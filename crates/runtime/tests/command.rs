use pitcrew_protocol::runner::Key;
use pitcrew_runtime::command::{Argument, Command, FormatError, quote_argument};
use pitcrew_runtime::keys::{pty_key, tmux_key};
use pitcrew_runtime::{PaneId, SessionId, WindowId};
use proptest::prelude::*;

#[test]
fn every_key_has_both_encodings() {
    let cases: &[(Key, &str, &[u8])] = &[
        (Key::Enter, "Enter", b"\r"),
        (Key::Escape, "Escape", b"\x1b"),
        (Key::Tab, "Tab", b"\t"),
        (Key::Up, "Up", b"\x1b[A"),
        (Key::Down, "Down", b"\x1b[B"),
        (Key::Left, "Left", b"\x1b[D"),
        (Key::Right, "Right", b"\x1b[C"),
        (Key::Backspace, "BSpace", b"\x7f"),
        (Key::CtrlC, "C-c", b"\x03"),
    ];
    for &(key, tmux, pty) in cases {
        assert_eq!(tmux_key(key), tmux);
        assert_eq!(pty_key(key), pty);
    }
}

#[test]
fn quoting_table() -> Result<(), FormatError> {
    for (input, expected) in [
        ("", "\"\""),
        ("; kill-server", "\"; kill-server\""),
        ("\\", "\"\\\\\""),
        ("\"'", "\"\\\"'\""),
        ("$(echo hi) $NAME", "\"\\$(echo hi) \\$NAME\""),
        ("\r\n\t\x1b\x7f", "\"\\015\\012\\011\\033\\177\""),
        ("#{pane_id} #(echo hi)", "\"#{pane_id} #(echo hi)\""),
        ("café 🦀", "\"café 🦀\""),
        ("~user", "\"\\~user\""),
    ] {
        assert_eq!(quote_argument(input)?, expected);
    }
    assert_eq!(quote_argument("a\0b"), Err(FormatError::Nul));
    Ok(())
}

#[test]
fn command_boundaries_and_typed_arguments() -> Result<(), FormatError> {
    for bad in [
        "",
        "send-keys;kill-server",
        "send-keys\nkill-server",
        "run shell",
        "-x",
        "x=y",
    ] {
        assert_eq!(Command::new(bad), Err(FormatError::InvalidCommandName));
    }
    assert_eq!(
        Command::send_literal(PaneId(3), "-F #{pane_id}")?.to_line(),
        "send-keys \"-l\" \"-t\" \"%3\" \"--\" \"-F #{pane_id}\"\n"
    );
    assert_eq!(
        Command::send_keys(PaneId(2), &[Key::Enter, Key::CtrlC])?.to_line(),
        "send-keys \"-t\" \"%2\" \"--\" \"Enter\" \"C-c\"\n"
    );
    assert_eq!(
        Command::new("display-message")?
            .arg(Argument::Window(WindowId(2)))?
            .arg(Argument::Session(SessionId(3)))?
            .arg(Argument::Number(80))?
            .to_line(),
        "display-message \"@2\" \"\\$3\" \"80\"\n"
    );
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig {
        failure_persistence: Some(Box::new(proptest::test_runner::FileFailurePersistence::WithSource("proptest-regressions"))),
        .. ProptestConfig::default()
    })]
    #[test]
    fn arbitrary_text_is_one_physical_command(characters in prop::collection::vec(any::<char>(), 0..1024)) {
        let text: String = characters.into_iter().collect();
        match Command::send_literal(PaneId(0), &text) {
            Ok(command) => {
                let line = command.to_line();
                prop_assert_eq!(line.bytes().filter(|&b| b == b'\n').count(), 1);
                prop_assert!(line.ends_with('\n'));
                prop_assert!(!line.contains('\r'));
                prop_assert!(!line.contains('\0'));
            }
            Err(error) => {
                prop_assert!(text.contains('\0'));
                prop_assert_eq!(error, FormatError::Nul);
            }
        }
    }
}
