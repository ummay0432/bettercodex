use super::*;

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
