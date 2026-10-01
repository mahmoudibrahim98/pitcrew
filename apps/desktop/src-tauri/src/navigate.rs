//! Navigation from outside the window (`docs/build/contracts/desktop-gateway.md`, "Navigation from
//! outside the window"): deep links `pitcrew://…` and clicks on the app's own notifications. Each
//! becomes a typed [`NavigateTarget`], emitted to the main window as `gateway://navigate`; then the
//! window is shown and focused.
//!
//! - **Deep links come from any web page.** [`parse_link`] accepts only the contract's shapes, with
//!   ids that are bare ULIDs or a known display form; anything else is dropped and logged,
//!   shortened. A link only navigates: it never answers, moves or sends anything.
//! - **The raw text is parsed**, not a URL library's normalised form, so `..`, percent-encoding,
//!   queries and fragments are refused rather than resolved.
//! - **A link that launched the app** arrives before the UI listens. The [`Navigator`] holds the
//!   latest one until the main page asks for the workspace list (`gateway_workspaces`), which the
//!   UI does once it listens to the gateway's events.

use crate::app::MAIN;
use crate::gateway::error::shorten;
use crate::redact::redact;
use pitcrew_protocol::ids::{ProjectId, SessionId, TaskId, TaskKey, WorkspaceId, WorkstreamId};
use serde::Serialize;
use std::str::FromStr as _;
use std::sync::{Mutex, MutexGuard};
use tauri::{AppHandle, Emitter as _, EventTarget, Manager as _, Runtime};

/// The event the gateway emits with a [`NavigateTarget`].
pub const NAVIGATE_EVENT: &str = "gateway://navigate";

/// The deep-link scheme.
pub const SCHEME: &str = "pitcrew";

/// The longest deep link read, in bytes. The longest valid one is about 90.
pub const MAX_LINK: usize = 512;

/// Deep links read from one command line at most; the rest are ignored.
const MAX_LINKS_PER_LAUNCH: usize = 4;

/// Where a target points.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TargetKind {
    /// The workspace's Inbox.
    Inbox,
    /// A task.
    Task,
    /// A session.
    Session,
    /// A project.
    Project,
    /// A workstream.
    Workstream,
}

impl TargetKind {
    /// The kind's name in links and payloads.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Inbox => "inbox",
            Self::Task => "task",
            Self::Session => "session",
            Self::Project => "project",
            Self::Workstream => "workstream",
        }
    }
}

/// The payload of `gateway://navigate`: a place in the UI, not a path.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize)]
pub struct NavigateTarget {
    /// A workspace id: a bare ULID.
    pub workspace: String,
    /// What to open.
    pub kind: TargetKind,
    /// The id of what to open: a bare ULID, or for a task its key (`PAP-4`). Absent for `inbox`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
}

impl NavigateTarget {
    /// A workspace's Inbox.
    #[must_use]
    pub fn inbox(workspace: impl Into<String>) -> Self {
        Self {
            workspace: workspace.into(),
            kind: TargetKind::Inbox,
            id: None,
        }
    }

    /// A task, by its bare ULID or key.
    #[must_use]
    pub fn task(workspace: impl Into<String>, id: impl Into<String>) -> Self {
        Self {
            workspace: workspace.into(),
            kind: TargetKind::Task,
            id: Some(id.into()),
        }
    }
}

/// Why a deep link was refused, for the log.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum Refused {
    /// Over [`MAX_LINK`] bytes.
    #[error("the link is too long")]
    TooLong,
    /// Not `pitcrew://`.
    #[error("the link is not a pitcrew:// link")]
    Scheme,
    /// A character outside `A-Z a-z 0-9 / _ -` after the scheme: `.`, `%`, `?`, `#`, `@`, `:`,
    /// spaces, controls, anything not ASCII.
    #[error("the link holds a character a pitcrew link never has")]
    Character,
    /// Not one of the contract's shapes.
    #[error("the link is not one of the shapes PitCrew opens")]
    Shape,
    /// The workspace is not a ULID.
    #[error("the link's workspace is not a workspace id")]
    Workspace,
    /// The id is not a ULID or a known display form for its kind.
    #[error("the link's id is not an id of that kind")]
    Id,
}

/// Parses a deep link into a target, strictly: `pitcrew://w/<ws>/inbox`, or
/// `pitcrew://w/<ws>/<task|session|project|workstream>/<id>`. The scheme is matched without regard
/// to case; nothing else is.
///
/// Ids are normalised: the workspace and ULID ids to the bare upper-case ULID (`wsp_…`, `tsk_…`
/// and the like are accepted for their own kind); a task may also be named by its key, kept as
/// written.
///
/// # Errors
/// Why the link was refused.
pub fn parse_link(link: &str) -> Result<NavigateTarget, Refused> {
    if link.len() > MAX_LINK {
        return Err(Refused::TooLong);
    }
    let rest = link
        .get(..SCHEME.len() + 3)
        .filter(|head| head.eq_ignore_ascii_case("pitcrew://"))
        .and_then(|_| link.get(SCHEME.len() + 3..))
        .ok_or(Refused::Scheme)?;
    if !rest
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'/' | b'_' | b'-'))
    {
        return Err(Refused::Character);
    }
    let segments: Vec<&str> = rest.split('/').collect();
    if segments.iter().any(|s| s.is_empty()) {
        return Err(Refused::Shape);
    }
    let (workspace, kind, id) = match segments.as_slice() {
        ["w", workspace, "inbox"] => (*workspace, TargetKind::Inbox, None),
        ["w", workspace, kind, id] => {
            let kind = match *kind {
                "task" => TargetKind::Task,
                "session" => TargetKind::Session,
                "project" => TargetKind::Project,
                "workstream" => TargetKind::Workstream,
                _ => return Err(Refused::Shape),
            };
            (*workspace, kind, Some(*id))
        }
        _ => return Err(Refused::Shape),
    };
    let workspace = canonical(
        workspace,
        WorkspaceId::PREFIX,
        WorkspaceId::from_str(workspace)
            .ok()
            .map(|w| w.0.to_string()),
    )
    .ok_or(Refused::Workspace)?;
    let id = match id {
        None => None,
        Some(id) => Some(normalise_id(kind, id).ok_or(Refused::Id)?),
    };
    Ok(NavigateTarget {
        workspace,
        kind,
        id,
    })
}

/// A bare ULID for a ULID id of `kind` (bare or with its own prefix), or a task key as written.
fn normalise_id(kind: TargetKind, id: &str) -> Option<String> {
    let ulid = match kind {
        TargetKind::Inbox => return None,
        TargetKind::Task => canonical(
            id,
            TaskId::PREFIX,
            TaskId::from_str(id).ok().map(|t| t.0.to_string()),
        ),
        TargetKind::Session => canonical(
            id,
            SessionId::PREFIX,
            SessionId::from_str(id).ok().map(|s| s.0.to_string()),
        ),
        TargetKind::Project => canonical(
            id,
            ProjectId::PREFIX,
            ProjectId::from_str(id).ok().map(|p| p.0.to_string()),
        ),
        TargetKind::Workstream => canonical(
            id,
            WorkstreamId::PREFIX,
            WorkstreamId::from_str(id).ok().map(|w| w.0.to_string()),
        ),
    };
    match ulid {
        Some(ulid) => Some(ulid),
        None if kind == TargetKind::Task => TaskKey::from_str(id)
            .ok()
            .map(|key| key.to_string())
            .filter(|key| key == id),
        None => None,
    }
}

/// The parsed ULID as text, only if it is the same ULID as written (ignoring case, and the
/// kind's own `prefix_`): the ULID parser also takes overflowing and aliased forms.
fn canonical(id: &str, prefix: &str, parsed: Option<String>) -> Option<String> {
    let bare = id
        .strip_prefix(prefix)
        .and_then(|rest| rest.strip_prefix('_'))
        .unwrap_or(id);
    parsed.filter(|text| text.eq_ignore_ascii_case(bare))
}

/// Whether a command-line argument is meant as a deep link (and so is parsed, and logged if it
/// is refused). Other arguments are not PitCrew's business.
#[must_use]
pub fn looks_like_link(arg: &str) -> bool {
    arg.get(..SCHEME.len() + 1)
        .is_some_and(|head| head.eq_ignore_ascii_case("pitcrew:"))
}

/// The targets of the deep links among `args` (a command line, without the program's name),
/// in order. Refused links are logged, shortened and redacted, and dropped.
pub fn targets_in<I, S>(args: I) -> Vec<NavigateTarget>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut targets = Vec::new();
    for arg in args
        .into_iter()
        .filter(|a| looks_like_link(a.as_ref()))
        .take(MAX_LINKS_PER_LAUNCH)
    {
        let arg = arg.as_ref();
        match parse_link(arg) {
            Ok(target) => targets.push(target),
            Err(why) => {
                tracing::warn!(link = %shorten(&redact(arg)), reason = %why, "dropped a deep link");
            }
        }
    }
    targets
}

/// Delivers targets to the main window, holding the latest one while the page is not listening.
#[derive(Debug, Default)]
pub struct Navigator {
    state: Mutex<State>,
}

#[derive(Debug, Default)]
struct State {
    /// The main page has asked for the workspace list since it last started loading.
    listening: bool,
    /// The latest target that arrived while it was not.
    pending: Option<NavigateTarget>,
}

impl Navigator {
    /// Opens `target`: emits it to the main window (or holds it until the page listens), then
    /// shows and focuses the window.
    pub fn navigate<R: Runtime>(&self, app: &AppHandle<R>, target: NavigateTarget) {
        let now = {
            let mut state = self.lock();
            if state.listening {
                Some(target)
            } else {
                tracing::debug!(
                    kind = target.kind.name(),
                    "holding a target until the page listens"
                );
                state.pending = Some(target);
                None
            }
        };
        if let Some(target) = now {
            emit(app, &target);
        }
        focus_main(app);
    }

    /// The main page started loading: targets wait for it.
    pub fn page_started(&self) {
        self.lock().listening = false;
    }

    /// The main page asked for the workspace list, so it listens: a held target goes now.
    pub fn page_listening<R: Runtime>(&self, app: &AppHandle<R>) {
        let pending = {
            let mut state = self.lock();
            state.listening = true;
            state.pending.take()
        };
        if let Some(target) = pending {
            emit(app, &target);
        }
    }

    /// The target held for the page, if any (tests).
    #[must_use]
    pub fn pending(&self) -> Option<NavigateTarget> {
        self.lock().pending.clone()
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// Opens the deep links in a command line (`args` without the program's name): the last valid
/// one is navigated to; without one, the main window is only shown and focused.
pub fn open_links<R, I, S>(app: &AppHandle<R>, args: I)
where
    R: Runtime,
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let target = targets_in(args).pop();
    match (target, app.try_state::<Navigator>()) {
        (Some(target), Some(navigator)) => navigator.navigate(app, target),
        _ => focus_main(app),
    }
}

/// Emits `target` to the main window.
fn emit<R: Runtime>(app: &AppHandle<R>, target: &NavigateTarget) {
    let to = EventTarget::WebviewWindow {
        label: MAIN.to_owned(),
    };
    match app.emit_to(to, NAVIGATE_EVENT, target) {
        Ok(()) => tracing::info!(
            workspace = %target.workspace,
            kind = target.kind.name(),
            "navigate"
        ),
        Err(e) => tracing::warn!(error = %e, "cannot emit a navigation"),
    }
}

/// Shows, un-minimises and focuses the main window. The desktop may still keep another window in
/// front (focus-stealing prevention); then the window is shown without the focus.
pub fn focus_main<R: Runtime>(app: &AppHandle<R>) {
    if let Some(window) = app.get_webview_window(MAIN) {
        for (what, done) in [
            ("unminimize", window.unminimize()),
            ("show", window.show()),
            ("focus", window.set_focus()),
        ] {
            if let Err(e) = done {
                tracing::debug!(error = %e, what, "cannot bring the window forward");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WS: &str = "01JA0000000000000000000000";
    const TASK: &str = "01JB000000000000000TASK001";
    const SES: &str = "01JC0000000000000000000SES";

    fn ok(link: &str) -> NavigateTarget {
        parse_link(link).unwrap_or_else(|e| panic!("{link:?} was refused: {e}"))
    }

    #[test]
    fn the_contracts_shapes() {
        assert_eq!(
            ok(&format!("pitcrew://w/{WS}/inbox")),
            NavigateTarget::inbox(WS)
        );
        assert_eq!(
            ok(&format!("pitcrew://w/{WS}/task/{TASK}")),
            NavigateTarget::task(WS, TASK)
        );
        for (kind, name) in [
            (TargetKind::Session, "session"),
            (TargetKind::Project, "project"),
            (TargetKind::Workstream, "workstream"),
        ] {
            let t = ok(&format!("pitcrew://w/{WS}/{name}/{SES}"));
            assert_eq!((t.kind, t.id.as_deref()), (kind, Some(SES)), "{name}");
        }
        // Display forms, the task key, a lower-case ULID and the scheme in capitals.
        assert_eq!(ok(&format!("pitcrew://w/wsp_{WS}/inbox")).workspace, WS);
        assert_eq!(
            ok(&format!("pitcrew://w/{WS}/task/tsk_{TASK}"))
                .id
                .as_deref(),
            Some(TASK)
        );
        assert_eq!(
            ok(&format!("pitcrew://w/{WS}/session/ses_{SES}"))
                .id
                .as_deref(),
            Some(SES)
        );
        assert_eq!(
            ok(&format!("pitcrew://w/{WS}/task/PAP-4")).id.as_deref(),
            Some("PAP-4")
        );
        assert_eq!(
            ok(&format!("pitcrew://w/{}/inbox", WS.to_lowercase())).workspace,
            WS
        );
        assert_eq!(ok(&format!("PitCrew://w/{WS}/inbox")).workspace, WS);
    }

    #[test]
    fn hostile_links_are_refused() {
        use Refused::*;
        let long = format!("pitcrew://w/{WS}/task/{}", "A".repeat(MAX_LINK));
        let cases: Vec<(String, Refused)> = vec![
            // Other schemes, and near misses.
            (format!("http://w/{WS}/inbox"), Scheme),
            (format!("https://pitcrew.example/w/{WS}/inbox"), Scheme),
            ("javascript:alert(1)".into(), Scheme),
            (format!("file:///w/{WS}/inbox"), Scheme),
            (format!("pitcrewx://w/{WS}/inbox"), Scheme),
            (format!("pitcrew:w/{WS}/inbox"), Scheme),
            (format!("pitcrew:/w/{WS}/inbox"), Scheme),
            (format!(" pitcrew://w/{WS}/inbox"), Scheme),
            ("pitcrew".into(), Scheme),
            ("".into(), Scheme),
            (format!("ρitcrew://w/{WS}/inbox"), Scheme),
            // Dots, encodings, queries, fragments, authority parts.
            (format!("pitcrew://w/{WS}/inbox/../task/{TASK}"), Character),
            (format!("pitcrew://w/{WS}/task/{TASK}/.."), Character),
            (format!("pitcrew://w/{WS}/task/%2e%2e"), Character),
            (format!("pitcrew://w/{WS}/task/{TASK}%2Fanswer"), Character),
            (format!("pitcrew://w/{WS}/task/{TASK}%00"), Character),
            (format!("pitcrew://w/%{WS}/inbox"), Character),
            (format!("pitcrew://w/{WS}/inbox?answer=yes"), Character),
            (format!("pitcrew://w/{WS}/task/{TASK}?option=0"), Character),
            (format!("pitcrew://w/{WS}/inbox#x"), Character),
            (format!("pitcrew://user@w/{WS}/inbox"), Character),
            (format!("pitcrew://w:80/{WS}/inbox"), Character),
            (format!("pitcrew://w/{WS}/task/{TASK}\n"), Character),
            (format!("pitcrew://w/{WS}/task/{TASK}\0"), Character),
            (format!("pitcrew://w/{WS}/task/{TASK} "), Character),
            (format!("pitcrew://w/{WS}/task\\{TASK}"), Character),
            (format!("pitcrew://w/{WS}/inbox\u{202e}"), Character),
            (format!("pitcrew://ԝ/{WS}/inbox"), Character),
            (format!("pitcrew://w/{WS}/task/{TASK};x"), Character),
            // Unknown shapes.
            (format!("pitcrew://w/{WS}"), Shape),
            (format!("pitcrew://w/{WS}/"), Shape),
            (format!("pitcrew://w/{WS}/inbox/"), Shape),
            (format!("pitcrew://w//{WS}/inbox"), Shape),
            (format!("pitcrew:///w/{WS}/inbox"), Shape),
            (format!("pitcrew://w/{WS}/inbox/{TASK}"), Shape),
            (format!("pitcrew://w/{WS}/task"), Shape),
            (format!("pitcrew://w/{WS}/task/"), Shape),
            (format!("pitcrew://w/{WS}/tasks/{TASK}"), Shape),
            (format!("pitcrew://w/{WS}/TASK/{TASK}"), Shape),
            (format!("pitcrew://w/{WS}/ask/{TASK}"), Shape),
            (format!("pitcrew://w/{WS}/task/{TASK}/answer"), Shape),
            (format!("pitcrew://w/{WS}/task/{TASK}/move/done"), Shape),
            (format!("pitcrew://x/{WS}/inbox"), Shape),
            (format!("pitcrew://W/{WS}/inbox"), Shape),
            ("pitcrew://".into(), Shape),
            // Ids that are not ids of their kind.
            ("pitcrew://w/not-a-workspace/inbox".into(), Workspace),
            (format!("pitcrew://w/tsk_{WS}/inbox"), Workspace),
            (format!("pitcrew://w/{WS}0/inbox"), Workspace),
            (format!("pitcrew://w/{WS}/task/ses_{TASK}"), Id),
            (format!("pitcrew://w/{WS}/session/PAP-4"), Id),
            (format!("pitcrew://w/{WS}/project/PAP"), Id),
            (format!("pitcrew://w/{WS}/task/pap-4"), Id),
            (format!("pitcrew://w/{WS}/task/PAP-0"), Id),
            (format!("pitcrew://w/{WS}/task/PAP-04"), Id),
            // Forms the ULID parser takes but that are not the id as written: an overflow, and
            // Crockford's aliases.
            (
                format!("pitcrew://w/{WS}/workstream/{}", "Z".repeat(26)),
                Id,
            ),
            (format!("pitcrew://w/{}/inbox", "Z".repeat(26)), Workspace),
            (
                "pitcrew://w/O1JA0000000000000000000000/inbox".into(),
                Workspace,
            ),
            (
                "pitcrew://w/01JAOOOOOOOOOOOOOOOOOOOOOO/inbox".into(),
                Workspace,
            ),
            (
                format!("pitcrew://w/{WS}/session/01JC000000000000000000LSES"),
                Id,
            ),
            (
                format!("pitcrew://w/{WS}/project/01JC00000000000000000000I1"),
                Id,
            ),
            // Very long input.
            (long, TooLong),
            ("x".repeat(100_000), TooLong),
        ];
        for (link, why) in cases {
            assert_eq!(parse_link(&link), Err(why), "{link:?}");
        }
    }

    #[test]
    fn command_lines() {
        let args = [
            "--flag".to_owned(),
            format!("pitcrew://w/{WS}/inbox?x=1"),
            "https://example.com/".to_owned(),
            format!("pitcrew://w/{WS}/task/{TASK}"),
        ];
        assert_eq!(targets_in(&args), vec![NavigateTarget::task(WS, TASK)]);
        let many: Vec<String> = (0..10).map(|_| format!("pitcrew://w/{WS}/inbox")).collect();
        assert_eq!(targets_in(&many).len(), MAX_LINKS_PER_LAUNCH);
        assert!(looks_like_link("PITCREW:x"));
        assert!(!looks_like_link("pitcrew"));
        assert!(!looks_like_link("ïtcrew:"));
    }

    #[test]
    fn the_payload_is_the_contracts() {
        assert_eq!(
            serde_json::to_value(NavigateTarget::task(WS, TASK)).unwrap(),
            serde_json::json!({ "workspace": WS, "kind": "task", "id": TASK })
        );
        assert_eq!(
            serde_json::to_value(NavigateTarget::inbox(WS)).unwrap(),
            serde_json::json!({ "workspace": WS, "kind": "inbox" })
        );
        for kind in [
            TargetKind::Inbox,
            TargetKind::Task,
            TargetKind::Session,
            TargetKind::Project,
            TargetKind::Workstream,
        ] {
            assert_eq!(serde_json::to_value(kind).unwrap(), kind.name());
        }
    }
}
