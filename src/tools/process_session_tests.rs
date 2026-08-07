use super::*;

#[derive(Debug)]
struct NoopKiller;

impl ChildKiller for NoopKiller {
    fn kill(&mut self) -> std::io::Result<()> {
        Ok(())
    }

    fn clone_killer(&self) -> Box<dyn ChildKiller + Send + Sync> {
        Box::new(Self)
    }
}

#[test]
fn retained_output_keeps_head_and_tail() {
    let mut output = PendingOutput::default();
    let bytes = vec![b'x'; RETAINED_HEAD_BYTES + RETAINED_TAIL_BYTES + 17];
    output.append(&bytes);
    let snapshot = output.take(SnapshotBoundary::Final);
    let rendered = snapshot.text;
    assert_eq!(snapshot.total_bytes, bytes.len());
    assert_eq!(snapshot.omitted_bytes, 17);
    assert!(rendered.starts_with(&"x".repeat(RETAINED_HEAD_BYTES)));
    assert!(rendered.contains("17 bytes omitted"));
    assert!(rendered.ends_with(&"x".repeat(RETAINED_TAIL_BYTES)));
}

#[test]
fn retained_output_ring_preserves_order_across_many_reads() {
    let omitted_bytes = RETAINED_TAIL_BYTES / 2 + 137;
    let input_bytes = RETAINED_HEAD_BYTES + RETAINED_TAIL_BYTES + omitted_bytes;
    let bytes = (0..input_bytes)
        .map(|index| b'a' + u8::try_from(index % 26).unwrap())
        .collect::<Vec<_>>();
    let mut output = PendingOutput::default();
    for chunk in bytes.chunks(8_191) {
        output.append(chunk);
    }

    let snapshot = output.take(SnapshotBoundary::Final);

    let expected = format!(
        "{}\n... {omitted_bytes} bytes omitted ...\n{}",
        String::from_utf8_lossy(&bytes[..RETAINED_HEAD_BYTES]),
        String::from_utf8_lossy(&bytes[bytes.len() - RETAINED_TAIL_BYTES..]),
    );
    assert_eq!(
        snapshot,
        PendingOutputSnapshot {
            text: expected,
            total_bytes: input_bytes,
            omitted_bytes,
        }
    );
}

#[test]
fn retained_output_replaces_invalid_utf8() {
    let mut output = PendingOutput::default();
    output.append(b"before\x80after");

    assert_eq!(
        output.take(SnapshotBoundary::Intermediate).text,
        "before\u{fffd}after"
    );
}

#[test]
fn process_snapshots_join_utf8_split_across_poll_boundaries() {
    let session = ProcessSession::new(None, Box::new(NoopKiller), 1, ProcessMode::Piped, None);
    let character = "\u{10348}".as_bytes();
    session.append(&[0x80]);
    session.append(&character[..2]);

    let first = session.snapshot().unwrap();
    assert_eq!(first.output, "\u{fffd}");
    assert_eq!(first.total_bytes, 1);

    session.append(&character[2..]);
    let second = session.snapshot().unwrap();
    assert_eq!(second.output, "\u{10348}");
    assert_eq!(second.total_bytes, character.len());
}

#[test]
fn final_snapshot_exposes_an_incomplete_utf8_suffix_lossily() {
    let session = ProcessSession::new(None, Box::new(NoopKiller), 1, ProcessMode::Piped, None);
    session.append(&[0xf0, 0x90]);
    session.reader_finished(None);

    let snapshot = session.snapshot().unwrap();
    assert_eq!(snapshot.output, "\u{fffd}");
    assert_eq!(snapshot.total_bytes, 2);
}

#[tokio::test]
async fn exited_process_uses_codex_output_close_grace() {
    let session = ProcessSession::new(None, Box::new(NoopKiller), 1, ProcessMode::Piped, None);
    session.exited(0, None);
    let started = Instant::now();

    session.wait(Duration::from_secs(5)).await;

    assert!(started.elapsed() < Duration::from_secs(1));
    let snapshot = session.snapshot().unwrap();
    assert!(snapshot.has_exited());
    assert_eq!(snapshot.exit_code, Some(0));
}
