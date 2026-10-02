//! The ssh the prompt rule needs (the contract's "Prompts").
//!
//! The rule that a prompt's kind says who asks relies on OpenSSH 8.4 or newer: it marks the text a
//! server writes (keyboard-interactive) with a leading `(user@host)`, and it honours
//! `SSH_ASKPASS_REQUIRE=force`. An older ssh hands server text over unmarked, so a server could
//! ask for "the passphrase for key …" and pass for this computer asking.
//!
//! An older ssh would not use askpass at all: before 8.4 it asks through askpass only with
//! `DISPLAY` set, which ssh's minimal environment leaves out, and with no terminal it reads an
//! empty answer and sends it, up to three empty passwords that `pam_faillock` or fail2ban count
//! as failed logins.
//!
//! So `ssh -V` is asked once per ssh program ([`SshVersions`]), before ssh is built, and only
//! a verdict of 8.4 or newer gives ssh prompts ([`SshCheck::prompts_allowed`]). Any other ssh
//! (older, one whose version cannot be told, or one not asked yet) runs in `BatchMode`: it never
//! asks, so passwords, keyboard-interactive and unknown host keys are refused, the call fails
//! with [`NEEDED`] ([`Verdict::refusal`]), and keys that need no prompt still work. The verdict
//! is kept for the app's life, so the message says to restart PitCrew after updating ssh.
//! [`GatedPrompts`] stays in front of the prompt hub as a second layer: a prompt that reaches it
//! from an ssh not judged fit is refused, never shown or answered (ssh is stopped before it can
//! send anything).

use super::prompt::PromptHub;
use pitcrew_remote::{PromptCancel, PromptFuture, PromptHandler, PromptRequest, Reply};
use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::OnceCell;

/// What a refused prompt's error says.
pub const NEEDED: &str = "ssh 8.4 or newer is needed to sign in from the app";

/// The oldest OpenSSH whose prompts are answered.
const OLDEST: (u32, u32) = (8, 4);

/// How long `ssh -V` may take.
const VERSION_WAIT: Duration = Duration::from_secs(10);

/// What `ssh -V` said about prompts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// OpenSSH 8.4 or newer (the version, e.g. `OpenSSH 9.6`): prompts are answered.
    Fit(String),
    /// An older OpenSSH (e.g. `OpenSSH 8.1`): prompts are refused.
    TooOld(String),
    /// Its version could not be told (why): prompts are refused.
    Unknown(String),
}

impl Verdict {
    /// The verdict on `ssh -V`'s output: `OpenSSH_9.6p1 Ubuntu-3ubuntu13.5, OpenSSL 3.0.13 …`,
    /// or Windows' `OpenSSH_for_Windows_9.5p2, LibreSSL 3.8.2`.
    #[must_use]
    pub fn of(output: &str) -> Self {
        match openssh_version(output) {
            Some(version) => {
                let said = format!("OpenSSH {}.{}", version.0, version.1);
                if version >= OLDEST {
                    Self::Fit(said)
                } else {
                    Self::TooOld(said)
                }
            }
            None => {
                let first = output.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
                Self::Unknown(if first.trim().is_empty() {
                    "`ssh -V` said nothing".to_owned()
                } else {
                    format!("`ssh -V` said {:?}", super::tidy(first))
                })
            }
        }
    }

    /// Whether prompts are shown and answered.
    #[must_use]
    pub fn allows_prompts(&self) -> bool {
        matches!(self, Self::Fit(_))
    }

    /// Why prompts are refused, for people; `None` when they are not.
    #[must_use]
    pub fn refusal(&self) -> Option<String> {
        match self {
            Self::Fit(_) => None,
            Self::TooOld(version) => Some(format!(
                "{NEEDED}; this computer's ssh is {version} (restart PitCrew after updating ssh)"
            )),
            Self::Unknown(why) => Some(format!(
                "{NEEDED}; this computer's ssh version cannot be told ({why}; restart PitCrew \
                 after updating ssh)"
            )),
        }
    }
}

/// `(major, minor)` of the first `OpenSSH_<major>.<minor>` in `text`.
fn openssh_version(text: &str) -> Option<(u32, u32)> {
    let at = text.find("OpenSSH_")? + "OpenSSH_".len();
    let rest = text.get(at..)?;
    let word = rest
        .split(|c: char| c.is_whitespace() || c == ',')
        .next()
        .unwrap_or("");
    // `9.6p1`, or `for_Windows_9.5p2`.
    let version = word.get(word.find(|c: char| c.is_ascii_digit())?..)?;
    let (major, tail) = version.split_once('.')?;
    let minor: String = tail.chars().take_while(char::is_ascii_digit).collect();
    Some((major.parse().ok()?, minor.parse().ok()?))
}

/// `ssh -V`'s verdicts, asked once per ssh program.
#[derive(Default)]
pub struct SshVersions {
    asked: Mutex<HashMap<PathBuf, Arc<OnceCell<Verdict>>>>,
}

impl fmt::Debug for SshVersions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SshVersions").finish_non_exhaustive()
    }
}

impl SshVersions {
    /// `program`'s verdict, asking `program -V` the first time.
    pub async fn verdict(&self, program: &Path) -> Verdict {
        let cell = self.cell(program);
        cell.get_or_init(|| ask(program.to_path_buf()))
            .await
            .clone()
    }

    /// `program`'s verdict, if it has been asked already.
    #[must_use]
    pub fn known(&self, program: &Path) -> Option<Verdict> {
        self.cell(program).get().cloned()
    }

    fn cell(&self, program: &Path) -> Arc<OnceCell<Verdict>> {
        let mut asked = self
            .asked
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        Arc::clone(asked.entry(program.to_path_buf()).or_default())
    }
}

/// Runs `program -V`, with only the environment ssh gets, and judges what it says.
async fn ask(program: PathBuf) -> Verdict {
    let mut command = tokio::process::Command::new(&program);
    command
        .arg("-V")
        .env_clear()
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    for name in pitcrew_remote::MINIMAL_ENV {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    #[cfg(windows)]
    {
        // CREATE_NO_WINDOW, as for every ssh call: no console flashes up.
        command.creation_flags(0x0800_0000);
    }
    let verdict = match tokio::time::timeout(VERSION_WAIT, command.output()).await {
        Ok(Ok(out)) => {
            // OpenSSH writes its version on stderr.
            let mut said = String::from_utf8_lossy(&out.stderr).into_owned();
            said.push('\n');
            said.push_str(&String::from_utf8_lossy(&out.stdout));
            Verdict::of(&said)
        }
        Ok(Err(e)) => Verdict::Unknown(format!(
            "`ssh -V` did not run: {}",
            super::tidy(&e.to_string())
        )),
        Err(_) => Verdict::Unknown(format!(
            "`ssh -V` did not answer within {} s",
            VERSION_WAIT.as_secs()
        )),
    };
    match &verdict {
        Verdict::Fit(version) => tracing::info!(%version, "ssh's prompts are answered in the app"),
        other => tracing::warn!(verdict = ?other, "ssh's prompts are refused: {NEEDED}"),
    }
    verdict
}

/// One ssh program's verdict, as a link's follower and the errors need it.
#[derive(Clone, Debug)]
pub struct SshCheck {
    versions: Arc<SshVersions>,
    /// `None`: no ssh is run at all (the configured one was refused).
    program: Option<PathBuf>,
}

impl SshCheck {
    /// `program`'s verdict in `versions`.
    #[must_use]
    pub fn new(versions: Arc<SshVersions>, program: Option<PathBuf>) -> Self {
        Self { versions, program }
    }

    /// Asks `ssh -V` now if it has not been asked yet.
    pub async fn ask(&self) {
        if let Some(program) = &self.program {
            self.versions.verdict(program).await;
        }
    }

    /// Why prompts are refused, asking `ssh -V` first if need be; `None` when they are not.
    pub async fn refusal(&self) -> Option<String> {
        let program = self.program.as_ref()?;
        self.versions.verdict(program).await.refusal()
    }

    /// Why prompts are refused, if `ssh -V` has been asked; `None` when they are not, or it has
    /// not been asked.
    #[must_use]
    pub fn known_refusal(&self) -> Option<String> {
        let program = self.program.as_ref()?;
        self.versions.known(program)?.refusal()
    }

    /// Whether ssh may be given prompts: only once `ssh -V` was asked and said 8.4 or newer.
    /// Otherwise ssh runs in `BatchMode`, so it never asks (an older ssh would not use askpass,
    /// and would send empty passwords instead).
    #[must_use]
    pub fn prompts_allowed(&self) -> bool {
        self.program
            .as_ref()
            .and_then(|program| self.versions.known(program))
            .is_some_and(|verdict| verdict.allows_prompts())
    }
}

/// The prompt handler every remote ssh call gets: the [`PromptHub`], behind the version gate.
pub struct GatedPrompts {
    program: PathBuf,
    versions: Arc<SshVersions>,
    hub: Arc<PromptHub>,
}

impl fmt::Debug for GatedPrompts {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GatedPrompts")
            .field("program", &self.program)
            .finish_non_exhaustive()
    }
}

impl GatedPrompts {
    /// Prompts from `program` go to `hub` once `versions` says it is new enough.
    #[must_use]
    pub fn new(program: PathBuf, versions: Arc<SshVersions>, hub: Arc<PromptHub>) -> Self {
        Self {
            program,
            versions,
            hub,
        }
    }
}

impl PromptHandler for GatedPrompts {
    fn prompt(&self, request: PromptRequest, cancel: PromptCancel) -> PromptFuture<'_> {
        Box::pin(async move {
            let verdict = tokio::select! {
                verdict = self.versions.verdict(&self.program) => verdict,
                () = cancel.cancelled() => return Reply::Cancel,
            };
            if verdict.allows_prompts() {
                return self.hub.prompt(request, cancel).await;
            }
            // Never shown, never answered: ssh is stopped before it sends anything.
            tracing::warn!(host = %request.host, verdict = ?verdict, "an ssh prompt was refused: {NEEDED}");
            Reply::Cancel
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_are_read_from_ssh_v() {
        for (said, verdict) in [
            (
                "OpenSSH_9.6p1 Ubuntu-3ubuntu13.5, OpenSSL 3.0.13 30 Jan 2024",
                Verdict::Fit("OpenSSH 9.6".into()),
            ),
            (
                "OpenSSH_for_Windows_9.5p2, LibreSSL 3.8.2",
                Verdict::Fit("OpenSSH 9.5".into()),
            ),
            (
                "OpenSSH_8.4p1, OpenSSL 1.1.1k",
                Verdict::Fit("OpenSSH 8.4".into()),
            ),
            (
                "OpenSSH_10.0p2, OpenSSL 3.5.0",
                Verdict::Fit("OpenSSH 10.0".into()),
            ),
            (
                "OpenSSH_8.1p1, OpenSSL 1.1.1k  FIPS 25 Mar 2021",
                Verdict::TooOld("OpenSSH 8.1".into()),
            ),
            (
                "OpenSSH_for_Windows_8.1p1, LibreSSL 3.0.2",
                Verdict::TooOld("OpenSSH 8.1".into()),
            ),
            (
                "OpenSSH_for_Windows_7.7p1, LibreSSL 2.6.5",
                Verdict::TooOld("OpenSSH 7.7".into()),
            ),
        ] {
            assert_eq!(Verdict::of(said), verdict, "{said}");
        }
        // Anything else cannot be told, and refuses prompts too.
        for said in [
            "",
            "Dropbear v2022.83",
            "OpenSSH_x.y",
            "plink: Release 0.81",
        ] {
            let verdict = Verdict::of(said);
            assert!(
                matches!(verdict, Verdict::Unknown(_)),
                "{said}: {verdict:?}"
            );
            assert!(!verdict.allows_prompts());
            assert!(verdict.refusal().unwrap().starts_with(NEEDED));
        }
        let old = Verdict::of("OpenSSH_8.1p1");
        assert!(!old.allows_prompts());
        assert_eq!(
            old.refusal().as_deref(),
            Some(
                "ssh 8.4 or newer is needed to sign in from the app; this computer's ssh is \
                 OpenSSH 8.1 (restart PitCrew after updating ssh)"
            )
        );
        assert_eq!(Verdict::of("OpenSSH_9.6p1").refusal(), None);
        // What ssh said is tidied before it goes into a message.
        let Verdict::Unknown(why) = Verdict::of("evil\u{1b}[31m\u{202e}") else {
            panic!("not unknown");
        };
        assert!(
            !why.contains('\u{1b}') && !why.contains('\u{202e}'),
            "{why}"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn ssh_v_is_asked_once_per_program() {
        use std::os::unix::fs::PermissionsExt as _;
        let tmp = tempfile::tempdir().unwrap();
        let count = tmp.path().join("count");
        let program = tmp.path().join("ssh");
        std::fs::write(
            &program,
            format!(
                "#!/bin/sh\necho x >> '{}'\necho 'OpenSSH_8.1p1, OpenSSL 1.1.1k' >&2\n",
                count.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
        let versions = Arc::new(SshVersions::default());
        assert_eq!(versions.known(&program), None);
        let check = SshCheck::new(Arc::clone(&versions), Some(program.clone()));
        assert_eq!(check.known_refusal(), None, "not asked yet");
        assert!(!check.prompts_allowed(), "not asked yet: no prompts");
        for _ in 0..3 {
            assert_eq!(
                versions.verdict(&program).await,
                Verdict::TooOld("OpenSSH 8.1".into())
            );
        }
        assert!(check.known_refusal().unwrap().contains("OpenSSH 8.1"));
        assert!(!check.prompts_allowed(), "too old: no prompts");
        assert_eq!(std::fs::read_to_string(&count).unwrap().lines().count(), 1);
        // A program that is not there cannot be told.
        let missing = tmp.path().join("nowhere");
        assert!(matches!(
            versions.verdict(&missing).await,
            Verdict::Unknown(_)
        ));
        assert!(!SshCheck::new(Arc::clone(&versions), Some(missing)).prompts_allowed());
        // A new enough one gets prompts, once asked.
        let fit = tmp.path().join("ssh-new");
        std::fs::write(
            &fit,
            "#!/bin/sh\necho 'OpenSSH_9.6p1, OpenSSL 3.0.13' >&2\n",
        )
        .unwrap();
        std::fs::set_permissions(&fit, std::fs::Permissions::from_mode(0o700)).unwrap();
        let check = SshCheck::new(Arc::clone(&versions), Some(fit));
        assert!(!check.prompts_allowed());
        check.ask().await;
        assert!(check.prompts_allowed());
        assert_eq!(SshCheck::new(versions, None).refusal().await, None);
    }
}
