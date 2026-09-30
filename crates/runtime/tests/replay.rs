use pitcrew_interfaces::runtime::OutputChunk;
use pitcrew_runtime::replay::{DEFAULT_CAPACITY, ReplayBuffer};
use proptest::prelude::*;

#[test]
fn offsets_wraparound_and_edge_cases() {
    let mut buffer = ReplayBuffer::new(4);
    assert_eq!(buffer.read(99, usize::MAX), OutputChunk::default());
    buffer.append(b"abc");
    assert_eq!(
        buffer.read(1, 1),
        OutputChunk {
            offset: 1,
            data: b"b".to_vec(),
            end: 3,
            truncated: false
        }
    );
    buffer.append(b"de");
    assert_eq!(buffer.end(), 5);
    assert_eq!(
        buffer.read(0, usize::MAX),
        OutputChunk {
            offset: 1,
            data: b"bcde".to_vec(),
            end: 5,
            truncated: true
        }
    );
    assert_eq!(buffer.read(1, 4).data, b"bcde");
    assert!(!buffer.read(1, 4).truncated);
    assert_eq!(
        buffer.read(0, 0),
        OutputChunk {
            offset: 1,
            data: vec![],
            end: 5,
            truncated: true
        }
    );
    assert_eq!(
        buffer.read(u64::MAX, 8),
        OutputChunk {
            offset: 5,
            data: vec![],
            end: 5,
            truncated: false
        }
    );
    buffer.append(b"0123456789");
    assert_eq!(
        buffer.read(0, 4),
        OutputChunk {
            offset: 11,
            data: b"6789".to_vec(),
            end: 15,
            truncated: true
        }
    );
    buffer.append(b"");
    assert_eq!(buffer.end(), 15);
}

#[test]
fn zero_capacity_and_default_capacity() {
    let mut zero = ReplayBuffer::new(0);
    zero.append(b"abc");
    assert_eq!(
        zero.read(0, 5),
        OutputChunk {
            offset: 3,
            data: vec![],
            end: 3,
            truncated: true
        }
    );
    assert!(!zero.read(3, 0).truncated);
    let mut default = ReplayBuffer::default();
    default.append(&vec![42; DEFAULT_CAPACITY + 1]);
    let read = default.read(0, usize::MAX);
    assert_eq!(read.offset, 1);
    assert_eq!(read.data.len(), DEFAULT_CAPACITY);
    assert!(read.truncated);
}

proptest! {
    #![proptest_config(ProptestConfig {
        failure_persistence: Some(Box::new(proptest::test_runner::FileFailurePersistence::WithSource("proptest-regressions"))),
        .. ProptestConfig::default()
    })]
    #[test]
    fn matches_unbounded_vec_model(
        capacity in 0usize..128,
        operations in prop::collection::vec((prop::collection::vec(any::<u8>(), 0..256), any::<u64>(), 0usize..300), 0..150),
    ) {
        let mut buffer = ReplayBuffer::new(capacity);
        let mut model = Vec::new();
        for (bytes, request, max) in operations {
            model.extend(&bytes);
            buffer.append(&bytes);
            let end = model.len() as u64;
            let start = model.len().saturating_sub(capacity) as u64;
            // Exercise evicted, retained, end and future positions after every append.
            for from in [0, start.saturating_sub(1), start, end, u64::MAX, request % (end + 2)] {
                let offset = from.clamp(start, end);
                let stop = model.len().min(offset as usize + max);
                let expected = OutputChunk {
                    offset, data: model[offset as usize..stop].to_vec(), end, truncated: from < start,
                };
                prop_assert_eq!(buffer.read(from, max), expected);
                prop_assert_eq!(buffer.end(), end);
            }
        }
    }
}
