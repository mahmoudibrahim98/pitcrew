use pitcrew_runtime::control::{
    CommandReply, ControlParser, Notification, PaneId, SessionId, WindowId,
};
use proptest::prelude::*;

fn cases() -> Vec<(Vec<u8>, Notification)> {
    use Notification::*;
    vec![
        (
            b"%output %3 hello\\015\\012\\000\\033\\134377\\377\xff\n".to_vec(),
            Output {
                pane: PaneId(3),
                data: b"hello\r\n\0\x1b\\377\xff\xff".to_vec(),
            },
        ),
        (
            b"%output %0   trailing  \n".to_vec(),
            Output {
                pane: PaneId(0),
                data: b"  trailing  ".to_vec(),
            },
        ),
        (
            b"%output %0 \n".to_vec(),
            Output {
                pane: PaneId(0),
                data: vec![],
            },
        ),
        (
            b"%output %0 \\12 \\400 \\999 \\08x \\q \\\n".to_vec(),
            Output {
                pane: PaneId(0),
                data: b"\\12 \\400 \\999 \\08x \\q \\".to_vec(),
            },
        ),
        (
            "%output %8 café 🦀\n".as_bytes().to_vec(),
            Output {
                pane: PaneId(8),
                data: "café 🦀".as_bytes().to_vec(),
            },
        ),
        (
            b"%extended-output %3 12 : hi\\012\n".to_vec(),
            ExtendedOutput {
                pane: PaneId(3),
                age: 12,
                extra: vec![],
                data: b"hi\n".to_vec(),
            },
        ),
        (
            b"%extended-output %3 0 future version=2 :  : \\134\n".to_vec(),
            ExtendedOutput {
                pane: PaneId(3),
                age: 0,
                extra: vec![b"future".to_vec(), b"version=2".to_vec()],
                data: b" : \\".to_vec(),
            },
        ),
        (
            b"%extended-output %3 0 :\n".to_vec(),
            ExtendedOutput {
                pane: PaneId(3),
                age: 0,
                extra: vec![],
                data: vec![],
            },
        ),
        (
            b"%window-add @12\n".to_vec(),
            WindowAdd {
                window: WindowId(12),
            },
        ),
        (
            b"%window-close @12\n".to_vec(),
            WindowClose {
                window: WindowId(12),
            },
        ),
        (
            b"%unlinked-window-add @2\n".to_vec(),
            UnlinkedWindowAdd {
                window: WindowId(2),
            },
        ),
        (
            b"%unlinked-window-close @2\n".to_vec(),
            UnlinkedWindowClose {
                window: WindowId(2),
            },
        ),
        (
            b"%window-renamed @4 name with spaces \xff\n".to_vec(),
            WindowRenamed {
                window: WindowId(4),
                name: b"name with spaces \xff".to_vec(),
            },
        ),
        (
            b"%session-changed $9 session name  \n".to_vec(),
            SessionChanged {
                session: SessionId(9),
                name: b"session name  ".to_vec(),
            },
        ),
        (b"%sessions-changed\n".to_vec(), SessionsChanged),
        (
            b"%layout-change @1 abcd,80x24,0,0,1 abcd,80x24,0,0,1 *Z\n".to_vec(),
            LayoutChange {
                window: WindowId(1),
                layout: b"abcd,80x24,0,0,1".to_vec(),
                visible_layout: b"abcd,80x24,0,0,1".to_vec(),
                flags: b"*Z".to_vec(),
            },
        ),
        (
            b"%layout-change @1 layout visible \n".to_vec(),
            LayoutChange {
                window: WindowId(1),
                layout: b"layout".to_vec(),
                visible_layout: b"visible".to_vec(),
                flags: vec![],
            },
        ),
        (b"%pause %0\n".to_vec(), Pause { pane: PaneId(0) }),
        (b"%continue %0\n".to_vec(), Continue { pane: PaneId(0) }),
        (b"%exit\n".to_vec(), Exit { reason: None }),
        (
            b"%exit server exited\n".to_vec(),
            Exit {
                reason: Some(b"server exited".to_vec()),
            },
        ),
        (
            b"%future args  \\134\xff\n".to_vec(),
            Other {
                name: "future".into(),
                args: b"args  \\134\xff".to_vec(),
            },
        ),
        (
            b"%future\n".to_vec(),
            Other {
                name: "future".into(),
                args: vec![],
            },
        ),
        (
            b"%begin 10 7 1\nfirst\n\n%output %0 literal\nraw\xff\r\n%end 10 7 1\n".to_vec(),
            CommandReply(pitcrew_runtime::control::CommandReply {
                time: 10,
                number: 7,
                flags: 1,
                failed: false,
                lines: vec![
                    b"first".to_vec(),
                    vec![],
                    b"%output %0 literal".to_vec(),
                    b"raw\xff\r".to_vec(),
                ],
            }),
        ),
        (
            b"%begin 10 8 1\nunknown command\n%error 10 8 1\n".to_vec(),
            CommandReply(pitcrew_runtime::control::CommandReply {
                time: 10,
                number: 8,
                flags: 1,
                failed: true,
                lines: vec![b"unknown command".to_vec()],
            }),
        ),
        (
            b"%begin 10 9 0\n%end 10 9 0\n".to_vec(),
            CommandReply(pitcrew_runtime::control::CommandReply {
                time: 10,
                number: 9,
                flags: 0,
                failed: false,
                lines: vec![],
            }),
        ),
    ]
}

#[test]
fn every_notification_at_every_single_split() {
    for (wire, expected) in cases() {
        for split in 0..=wire.len() {
            let mut parser = ControlParser::new();
            let mut actual = parser.feed(&wire[..split]);
            assert!(parser.feed(b"").is_empty());
            actual.extend(parser.feed(&wire[split..]));
            assert_eq!(actual, vec![expected.clone()], "split {split}: {wire:?}");
        }
        let mut parser = ControlParser::new();
        let actual: Vec<_> = wire
            .chunks(1)
            .flat_map(|chunk| parser.feed(chunk))
            .collect();
        assert_eq!(actual, vec![expected]);
    }
}

#[test]
fn only_matching_guards_finish_a_reply() {
    let mut parser = ControlParser::new();
    assert!(
        parser
            .feed(b"%begin 1 2 0\n%end 1 3 0\n%error 2 2 0\n%begin 1 5 0\n%end 1 2")
            .is_empty()
    );
    assert_eq!(
        parser.feed(b" 0\n"),
        vec![Notification::CommandReply(CommandReply {
            time: 1,
            number: 2,
            flags: 0,
            failed: false,
            lines: vec![
                b"%end 1 3 0".to_vec(),
                b"%error 2 2 0".to_vec(),
                b"%begin 1 5 0".to_vec()
            ],
        })]
    );
}

#[test]
fn malformed_records_are_preserved_and_do_not_break_later_records() {
    let lines: &[&[u8]] = &[
        b"%begin x 1 0",
        b"%end 1 1 0",
        b"%error 1 1 0",
        b"%window-add %1",
        b"%output %-1 nope",
        b"%pause %18446744073709551616",
        b"%continue %1 junk",
        b"%sessions-changed junk",
        b"%extended-output %1 nope : data",
        b"%extended-output %1 2 missing-colon",
        b"%layout-change @0 layout",
        b"plain line",
        b"",
    ];
    let mut parser = ControlParser::new();
    for line in lines {
        let mut wire = line.to_vec();
        wire.push(b'\n');
        assert!(matches!(
            parser.feed(&wire).as_slice(),
            [Notification::Other { .. }]
        ));
    }
    assert_eq!(
        parser.feed(b"%sessions-changed\n"),
        vec![Notification::SessionsChanged]
    );
}

fn encode_output(bytes: &[u8]) -> Vec<u8> {
    let mut encoded = Vec::new();
    for &byte in bytes {
        if byte < 32 || byte == b'\\' {
            encoded.extend(format!("\\{byte:03o}").as_bytes());
        } else {
            encoded.push(byte);
        }
    }
    encoded
}

proptest! {
    #![proptest_config(ProptestConfig {
        failure_persistence: Some(Box::new(proptest::test_runner::FileFailurePersistence::WithSource("proptest-regressions"))),
        .. ProptestConfig::default()
    })]
    #[test]
    fn arbitrary_chunking_matches_whole_stream(
        payload in prop::collection::vec(any::<u8>(), 0..2048),
        cuts in prop::collection::vec(any::<usize>(), 0..100),
    ) {
        let mut stream: Vec<u8> = cases().into_iter().flat_map(|(wire, _)| wire).collect();
        stream.extend(b"%output %44 ");
        stream.extend(encode_output(&payload));
        stream.push(b'\n');
        let expected = ControlParser::new().feed(&stream);
        prop_assert_eq!(expected.last(), Some(&Notification::Output { pane: PaneId(44), data: payload }));
        let mut boundaries: Vec<_> = cuts.into_iter().map(|cut| cut % (stream.len() + 1)).collect();
        boundaries.extend([0, stream.len()]);
        boundaries.sort_unstable();
        let mut parser = ControlParser::new();
        let mut actual = Vec::new();
        for pair in boundaries.windows(2) {
            actual.extend(parser.feed(&stream[pair[0]..pair[1]]));
        }
        prop_assert_eq!(actual, expected);
    }
}
