use super::*;

fn push_events(decoder: &mut SseDecoder, chunk: &[u8]) -> ApiResult<Vec<String>> {
    let mut events = Vec::new();
    decoder.push(chunk, &mut events)?;
    Ok(events)
}

fn finish_events(decoder: &mut SseDecoder) -> ApiResult<Vec<String>> {
    let mut events = Vec::new();
    decoder.finish(&mut events)?;
    Ok(events)
}

#[test]
fn handles_split_crlf_and_multiline_frames() {
    let mut decoder = SseDecoder::default();
    assert_eq!(
        push_events(&mut decoder, b"event: ignored\r\nda").unwrap(),
        Vec::<String>::new()
    );
    assert_eq!(
        push_events(
            &mut decoder,
            b"ta: {\"type\":\r\ndata: \"response.created\"}\r\n\r\n",
        )
        .unwrap(),
        vec!["{\"type\":\n\"response.created\"}".to_string()]
    );
}

#[test]
fn preserves_empty_data_lines_and_unterminated_events() {
    let mut decoder = SseDecoder::default();
    assert_eq!(
        push_events(&mut decoder, b"data:\ndata: second\n\n").unwrap(),
        ["\nsecond"]
    );
    assert!(
        push_events(&mut decoder, b"data: trailing\r")
            .unwrap()
            .is_empty()
    );
    assert_eq!(finish_events(&mut decoder).unwrap(), ["trailing"]);
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
        decoded.extend(push_events(&mut decoder, chunk).unwrap());
    }

    assert_eq!(decoded, [event]);
}

#[test]
fn bounds_each_event_instead_of_the_transport_chunk() {
    let event = format!("data: {{\"payload\":\"{}\"}}\n\n", "x".repeat(1024));
    let chunk = event.repeat(MAX_STREAM_EVENT_BYTES / event.len() + 1);
    assert!(chunk.len() > MAX_STREAM_EVENT_BYTES);

    let mut decoder = SseDecoder::default();
    let events = push_events(&mut decoder, chunk.as_bytes()).unwrap();
    assert_eq!(events.len(), chunk.len() / event.len());
}

#[test]
fn rejects_an_oversized_non_data_line() {
    let line = format!("event: {}\n\n", "x".repeat(MAX_STREAM_EVENT_BYTES));
    let mut decoder = SseDecoder::default();

    let error = push_events(&mut decoder, line.as_bytes()).unwrap_err();
    assert!(error.to_string().contains("oversized SSE event"));
}

#[test]
fn rejects_an_oversized_fragmented_line() {
    let line = vec![b'x'; MAX_STREAM_EVENT_BYTES];
    let mut decoder = SseDecoder::default();

    assert!(push_events(&mut decoder, &line).unwrap().is_empty());
    let error = push_events(&mut decoder, b"x").unwrap_err();
    assert!(error.to_string().contains("oversized SSE event"));
}

#[test]
fn rejects_oversized_multiline_data() {
    let line = format!("data: {}\n", "x".repeat(MAX_STREAM_EVENT_BYTES / 2));
    let mut decoder = SseDecoder::default();

    assert!(
        push_events(&mut decoder, line.as_bytes())
            .unwrap()
            .is_empty()
    );
    let error = push_events(&mut decoder, line.as_bytes()).unwrap_err();
    assert!(error.to_string().contains("oversized SSE event"));
}

#[test]
fn rejects_invalid_utf8_after_reassembling_a_line() {
    let mut decoder = SseDecoder::default();
    let mut events = Vec::new();

    decoder.push(b"data: \xf0\x9f", &mut events).unwrap();
    let error = decoder.push(b"\xff\n\n", &mut events).unwrap_err();

    assert!(events.is_empty());
    assert!(error.to_string().contains("SSE stream was not UTF-8"));
}

#[test]
fn handles_every_chunk_boundary_without_buffering_complete_lines() {
    let stream = "event: ignored\r\ndata: α\r\ndata: β\r\n\r\ndata: trailing\n\n";
    let expected = ["α\nβ", "trailing"];

    for split in 0..=stream.len() {
        let mut decoder = SseDecoder::default();
        let mut events = push_events(&mut decoder, &stream.as_bytes()[..split]).unwrap();
        events.extend(push_events(&mut decoder, &stream.as_bytes()[split..]).unwrap());
        events.extend(finish_events(&mut decoder).unwrap());
        assert_eq!(events, expected, "failed at byte split {split}");
    }

    for chunk_bytes in 1..=stream.len() {
        let mut decoder = SseDecoder::default();
        let mut events = Vec::new();
        for chunk in stream.as_bytes().chunks(chunk_bytes) {
            events.extend(push_events(&mut decoder, chunk).unwrap());
        }
        events.extend(finish_events(&mut decoder).unwrap());
        assert_eq!(events, expected, "failed with {chunk_bytes}-byte chunks");
    }

    let mut decoder = SseDecoder::default();
    assert_eq!(
        push_events(&mut decoder, stream.as_bytes()).unwrap(),
        expected
    );
    assert_eq!(
        decoder.partial_line.capacity(),
        0,
        "complete transport lines should not enter the fragment buffer"
    );
}

#[test]
#[ignore = "manual performance measurement"]
fn benchmark_sse_decoder_workloads() {
    const EVENT_PAYLOAD_BYTES: usize = 512;
    const EVENTS_PER_WORKLOAD: usize = 200_000;
    const EVENTS_PER_BATCH: usize = 64;
    const FRAGMENT_BYTES: usize = 31;

    let frame = format!(
        "data: {{\"type\":\"response.output_text.delta\",\"delta\":\"{}\"}}\n\n",
        "x".repeat(EVENT_PAYLOAD_BYTES),
    );
    let batch = frame.repeat(EVENTS_PER_BATCH);

    benchmark_decoder(
        "one event per chunk",
        frame.len() * EVENTS_PER_WORKLOAD,
        EVENTS_PER_WORKLOAD,
        || {
            let mut decoder = SseDecoder::default();
            let mut events = Vec::new();
            for _ in 0..EVENTS_PER_WORKLOAD {
                decoder
                    .push(std::hint::black_box(frame.as_bytes()), &mut events)
                    .unwrap();
                assert_eq!(events.len(), 1);
                std::hint::black_box(&events);
                events.clear();
            }
        },
    );
    benchmark_decoder(
        "64 events per chunk",
        frame.len() * EVENTS_PER_WORKLOAD,
        EVENTS_PER_WORKLOAD,
        || {
            let mut decoder = SseDecoder::default();
            let mut events = Vec::new();
            for _ in 0..EVENTS_PER_WORKLOAD / EVENTS_PER_BATCH {
                decoder
                    .push(std::hint::black_box(batch.as_bytes()), &mut events)
                    .unwrap();
                assert_eq!(events.len(), EVENTS_PER_BATCH);
                std::hint::black_box(&events);
                events.clear();
            }
        },
    );
    benchmark_decoder(
        "31-byte fragments",
        frame.len() * EVENTS_PER_WORKLOAD,
        EVENTS_PER_WORKLOAD,
        || {
            let mut decoder = SseDecoder::default();
            let mut events = Vec::new();
            for _ in 0..EVENTS_PER_WORKLOAD {
                let mut decoded = 0;
                for fragment in frame.as_bytes().chunks(FRAGMENT_BYTES) {
                    decoder
                        .push(std::hint::black_box(fragment), &mut events)
                        .unwrap();
                    decoded += events.len();
                    std::hint::black_box(&events);
                    events.clear();
                }
                assert_eq!(decoded, 1);
            }
        },
    );
}

fn benchmark_decoder(
    workload: &str,
    decoded_bytes: usize,
    decoded_events: usize,
    run: impl FnOnce(),
) {
    let started = std::time::Instant::now();
    run();
    let elapsed = started.elapsed();
    let mib_per_second = decoded_bytes as f64 / (1024.0 * 1024.0) / elapsed.as_secs_f64();
    let nanoseconds_per_event = elapsed.as_nanos() as f64 / decoded_events as f64;
    eprintln!(
        "{workload}: {mib_per_second:.1} MiB/s, {nanoseconds_per_event:.1} ns/event ({elapsed:?})"
    );
}
