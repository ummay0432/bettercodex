use super::*;

#[test]
fn handles_split_crlf_and_multiline_frames() {
    let mut decoder = SseDecoder::default();
    assert_eq!(
        decoder.push(b"event: ignored\r\nda").unwrap(),
        Vec::<String>::new()
    );
    assert_eq!(
        decoder
            .push(b"ta: {\"type\":\r\ndata: \"response.created\"}\r\n\r\n")
            .unwrap(),
        vec!["{\"type\":\n\"response.created\"}".to_string()]
    );
}

#[test]
fn preserves_empty_data_lines_and_unterminated_events() {
    let mut decoder = SseDecoder::default();
    assert_eq!(
        decoder.push(b"data:\ndata: second\n\n").unwrap(),
        ["\nsecond"]
    );
    assert!(decoder.push(b"data: trailing\r").unwrap().is_empty());
    assert_eq!(decoder.finish().unwrap(), ["trailing"]);
}

#[test]
fn reassembles_a_large_line_from_small_chunks() {
    const PAYLOAD_BYTES: usize = 512 * 1024;
    const CHUNK_BYTES: usize = 127;
    let event = format!("{{\"payload\":\"{}\"}}", "x".repeat(PAYLOAD_BYTES));
    let frame = format!("data: {event}\n\n");
    let mut decoder = SseDecoder::default();
    let mut decoded = Vec::new();

    for chunk in frame.as_bytes().chunks(CHUNK_BYTES) {
        decoded.extend(decoder.push(chunk).unwrap());
    }

    assert_eq!(decoded, [event]);
}

#[test]
fn bounds_each_event_instead_of_the_transport_chunk() {
    let event = format!("data: {{\"payload\":\"{}\"}}\n\n", "x".repeat(1024));
    let chunk = event.repeat(MAX_STREAM_EVENT_BYTES / event.len() + 1);
    assert!(chunk.len() > MAX_STREAM_EVENT_BYTES);

    let mut decoder = SseDecoder::default();
    let events = decoder.push(chunk.as_bytes()).unwrap();
    assert_eq!(events.len(), chunk.len() / event.len());
}

#[test]
fn rejects_an_oversized_non_data_line() {
    let line = format!("event: {}\n\n", "x".repeat(MAX_STREAM_EVENT_BYTES));
    let mut decoder = SseDecoder::default();

    let error = decoder.push(line.as_bytes()).unwrap_err();
    assert!(error.to_string().contains("oversized SSE event"));
}

#[test]
fn rejects_an_oversized_fragmented_line() {
    let line = vec![b'x'; MAX_STREAM_EVENT_BYTES];
    let mut decoder = SseDecoder::default();

    assert!(decoder.push(&line).unwrap().is_empty());
    let error = decoder.push(b"x").unwrap_err();
    assert!(error.to_string().contains("oversized SSE event"));
}

#[test]
fn rejects_oversized_multiline_data() {
    let line = format!("data: {}\n", "x".repeat(MAX_STREAM_EVENT_BYTES / 2));
    let mut decoder = SseDecoder::default();

    assert!(decoder.push(line.as_bytes()).unwrap().is_empty());
    let error = decoder.push(line.as_bytes()).unwrap_err();
    assert!(error.to_string().contains("oversized SSE event"));
}
