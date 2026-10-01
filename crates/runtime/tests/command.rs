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
        Command::send_keys(PaneId(2), &[Key::Enter, Key::CtrlC])?
            .expect("nonempty keys")
            .to_line(),
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

#[test]
fn formats_modes_bytes_and_empty_keys() -> Result<(), FormatError> {
    assert_eq!(Command::send_keys(PaneId(0), &[])?, None);
    assert_eq!(Command::send_bytes(PaneId(0), &[])?, None);
    assert_eq!(
        Command::send_bytes(PaneId(2), b"\0\xff\x80\r")?
            .expect("bytes")
            .to_line(),
        "send-keys \"-H\" \"-t\" \"%2\" \"--\" \"00\" \"ff\" \"80\" \"0d\"\n"
    );
    assert_eq!(
        Command::pane_in_mode(PaneId(2))?.to_line(),
        "display-message \"-p\" \"-t\" \"%2\" \"#{pane_in_mode}\"\n"
    );
    assert_eq!(
        Command::cancel_copy_mode(PaneId(2))?.to_line(),
        "send-keys \"-X\" \"-t\" \"%2\" \"--\" \"cancel\"\n"
    );
    assert_eq!(
        Command::new("new-window")?
            .arg(Argument::Flag("-n"))?
            .arg(Argument::Name("#(touch sentinel) #{pane_id} ##"))?
            .to_line(),
        "new-window \"-n\" \"##(touch sentinel) ##{pane_id} ####\"\n"
    );
    assert_eq!(
        Command::new("display-message")?.arg(Argument::FormatLiteral("x\0y")),
        Err(FormatError::Nul)
    );
    Ok(())
}

#[test]
fn names_reject_all_c0_controls_and_del() {
    for byte in (0..=0x1f).chain([0x7f]) {
        let name = format!("prefix{}suffix", char::from(byte));
        for command in ["new-window", "rename-window"] {
            assert_eq!(
                Command::new(command)
                    .expect("command")
                    .arg(Argument::Name(&name)),
                Err(FormatError::ControlInName),
                "{command}, byte {byte:02x}"
            );
        }
    }
}

// Independent model of the relevant tmux double-quote lexer. Synthetic values
// make accidental variable/home expansion observable without reading the host.
fn lex_double_quote(input: &[u8]) -> Result<(Vec<u8>, &[u8]), &'static str> {
    if input.first() != Some(&b'"') {
        return Err("missing opening quote");
    }
    let mut output = Vec::new();
    let mut index = 1;
    while let Some(&byte) = input.get(index) {
        index += 1;
        match byte {
            b'"' => return Ok((output, &input[index..])),
            b'\n' | 0 => return Err("invalid physical line"),
            b'\\' => {
                let escaped = *input.get(index).ok_or("incomplete escape")?;
                index += 1;
                if (b'0'..=b'7').contains(&escaped) {
                    let mut value = u16::from(escaped - b'0');
                    for _ in 0..2 {
                        let digit = *input.get(index).ok_or("short octal")?;
                        if !(b'0'..=b'7').contains(&digit) {
                            return Err("invalid octal");
                        }
                        value = value * 8 + u16::from(digit - b'0');
                        index += 1;
                    }
                    output.push(u8::try_from(value).map_err(|_| "octal overflow")?);
                } else {
                    output.push(match escaped {
                        b'n' => b'\n',
                        b'r' => b'\r',
                        b't' => b'\t',
                        b'e' => 27,
                        other => other,
                    });
                }
            }
            b'$' => {
                let start = index;
                let braced = input.get(index) == Some(&b'{');
                if braced {
                    index += 1;
                }
                while input
                    .get(index)
                    .is_some_and(|b| b.is_ascii_alphanumeric() || *b == b'_')
                {
                    index += 1;
                }
                if braced {
                    if input.get(index) != Some(&b'}') {
                        return Err("invalid variable");
                    }
                    index += 1;
                }
                if start == index {
                    output.push(b'$');
                } else {
                    output.extend(b"synthetic-value");
                }
            }
            b'~' if output.is_empty() => {
                while input
                    .get(index)
                    .is_some_and(|b| *b != b'/' && *b != b'"' && !b.is_ascii_whitespace())
                {
                    index += 1;
                }
                output.extend(b"/synthetic/home");
            }
            other => output.push(other),
        }
    }
    Err("missing closing quote")
}

#[test]
fn lexer_catches_expansion_and_escape_regressions() {
    assert_eq!(
        lex_double_quote(br#""$FOO"tail"#).expect("lex"),
        (b"synthetic-value".to_vec(), b"tail".as_slice())
    );
    assert_eq!(
        lex_double_quote(br#""~user/path""#).expect("lex").0,
        b"/synthetic/home/path"
    );
    assert_eq!(
        lex_double_quote(br#""\134\012\"\$\~""#).expect("lex").0,
        b"\\\n\"$~"
    );
    for input in [
        "$FOO",
        "${FOO}",
        "~user/path",
        "\\",
        "\\012",
        "\"",
        "a\nb",
        "\x7f",
        "FOO=bar",
        "%hidden x",
    ] {
        let quoted = quote_argument(input).expect("quote");
        let (value, rest) = lex_double_quote(quoted.as_bytes()).expect("lex");
        assert_eq!(value, input.as_bytes());
        assert!(rest.is_empty());
    }
}

proptest! {
    #![proptest_config(ProptestConfig {
        failure_persistence: Some(Box::new(proptest::test_runner::FileFailurePersistence::WithSource("proptest-regressions"))),
        .. ProptestConfig::default()
    })]
    #[test]
    fn arbitrary_text_is_one_physical_command(characters in prop::collection::vec(prop_oneof![
        5 => prop::sample::select(vec!['$', '~', '\\', '"']),
        3 => (1u8..=0x1f).prop_map(char::from),
        1 => Just('\x7f'),
        3 => any::<char>(),
    ], 0..1024)) {
        let text: String = characters.into_iter().collect();
        match Command::send_literal(PaneId(0), &text) {
            Ok(command) => {
                let quoted = quote_argument(&text).expect("valid text");
                let (decoded, rest) = lex_double_quote(quoted.as_bytes()).expect("lex quoted text");
                prop_assert_eq!(decoded, text.as_bytes());
                prop_assert!(rest.is_empty());
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
