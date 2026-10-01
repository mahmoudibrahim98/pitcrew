//! `pitcrew_remote::askpass::classify` on arbitrary prompts and hints. The prompt text of
//! keyboard-interactive authentication is the server's, so it is attacker-controlled; the class
//! picks the dialog the user sees (a password box, a host-key trust dialog, a yes/no).
//!
//! Input: the hint and the prompt, separated by the first NUL byte (no NUL: no hint).
//!
//! Checks, besides "no panic":
//! - ssh's own hint wins: `confirm` is always a yes/no, `none` always a notice;
//! - without those hints, a notice never appears, and a yes/no only for OpenSSH's own
//!   "Accept updated hostkeys?" question;
//! - text marked as the server's (OpenSSH's `(user@host) ` prefix) is never classed as a host-key
//!   question, a key passphrase or a yes/no: only as a password or a one-time code;
//! - a host-key class needs the words of OpenSSH's host-key question, a passphrase the word
//!   "passphrase".
#![no_main]

use libfuzzer_sys::fuzz_target;
use pitcrew_remote::PromptKind;
use pitcrew_remote::askpass::classify;

fuzz_target!(|input: &[u8]| {
    let text = String::from_utf8_lossy(input);
    let (hint, prompt) = match text.split_once('\0') {
        Some((hint, prompt)) => (Some(hint), prompt),
        None => (None, text.as_ref()),
    };
    let kind = classify(prompt, hint);
    assert_eq!(classify(prompt, hint), kind, "not deterministic");
    match hint {
        Some("confirm") => assert_eq!(kind, PromptKind::Confirm),
        Some("none") => assert_eq!(kind, PromptKind::Notice),
        _ => {
            let lower = prompt.to_lowercase();
            assert_ne!(kind, PromptKind::Notice, "a notice without ssh's hint");
            if kind == PromptKind::Confirm {
                assert!(lower.starts_with("accept updated hostkeys?"));
            }
            if kind == PromptKind::HostKey {
                assert!(
                    lower.contains("continue connecting") || lower.contains("authenticity of host")
                );
            }
            if kind == PromptKind::Passphrase {
                assert!(lower.contains("passphrase"));
            }
            if prompt.starts_with('(') {
                assert!(
                    matches!(kind, PromptKind::Password | PromptKind::Otp),
                    "server text classed as {kind:?}: {prompt:?}"
                );
            }
            // The same text sent by a server, behind OpenSSH's prefix.
            let served = classify(&format!("(sam@hpc-login) {prompt}"), hint);
            assert!(
                matches!(served, PromptKind::Password | PromptKind::Otp),
                "server text classed as {served:?}: {prompt:?}"
            );
        }
    }
    assert_eq!(
        kind.is_secret(),
        matches!(
            kind,
            PromptKind::Password | PromptKind::Passphrase | PromptKind::Otp
        )
    );
});
