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
    let snapshot = output.take();
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

    let snapshot = output.take();

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

    assert_eq!(output.take().text, "before\u{fffd}after");
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
