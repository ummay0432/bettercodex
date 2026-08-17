use super::ApiError;
use super::ApiResult;
use super::MAX_STREAM_EVENT_BYTES;
use memchr::memchr2;

#[derive(Default)]
pub(super) struct SseDecoder {
    partial_line: Vec<u8>,
    pending_event: PendingSseEvent,
    skip_leading_lf: bool,
    saw_first_line: bool,
}

impl SseDecoder {
    pub(super) fn push(&mut self, chunk: &[u8], events: &mut Vec<String>) -> ApiResult<()> {
        let Self {
            partial_line,
            pending_event,
            skip_leading_lf,
            saw_first_line,
        } = self;

        let mut chunk = chunk;
        if *skip_leading_lf && !chunk.is_empty() {
            *skip_leading_lf = false;
            if let Some(remaining) = chunk.strip_prefix(b"\n") {
                chunk = remaining;
            }
        }

        let mut line_start = 0;
        while let Some(relative_end) = memchr2(b'\r', b'\n', &chunk[line_start..]) {
            let line_end = line_start + relative_end;
            if partial_line.is_empty() {
                process_decoder_line(
                    pending_event,
                    saw_first_line,
                    &chunk[line_start..line_end],
                    events,
                )?;
            } else {
                append_fragment(partial_line, &chunk[line_start..line_end])?;
                process_decoder_line(pending_event, saw_first_line, partial_line, events)?;
                partial_line.clear();
            }

            if chunk[line_end] == b'\r' && chunk.get(line_end + 1) == Some(&b'\n') {
                line_start = line_end + 2;
            } else {
                line_start = line_end + 1;
                if chunk[line_end] == b'\r' && line_start == chunk.len() {
                    *skip_leading_lf = true;
                }
            }
        }
        append_fragment(partial_line, &chunk[line_start..])?;
        Ok(())
    }

    pub(super) fn finish(&mut self, events: &mut Vec<String>) -> ApiResult<()> {
        if !self.partial_line.is_empty() {
            let line = std::mem::take(&mut self.partial_line);
            process_decoder_line(
                &mut self.pending_event,
                &mut self.saw_first_line,
                &line,
                events,
            )?;
        }
        if let Some(event) = self.pending_event.take() {
            events.push(event);
        }
        Ok(())
    }
}

fn process_decoder_line(
    pending_event: &mut PendingSseEvent,
    saw_first_line: &mut bool,
    line: &[u8],
    events: &mut Vec<String>,
) -> ApiResult<()> {
    let line = if *saw_first_line {
        line
    } else {
        *saw_first_line = true;
        line.strip_prefix(b"\xef\xbb\xbf").unwrap_or(line)
    };
    if let Some(event) = pending_event.process_line(line)? {
        events.push(event);
    }
    Ok(())
}

fn append_fragment(partial_line: &mut Vec<u8>, fragment: &[u8]) -> ApiResult<()> {
    if partial_line.len().saturating_add(fragment.len()) > MAX_STREAM_EVENT_BYTES {
        return Err(ApiError::fatal("model sent an oversized SSE event"));
    }
    partial_line.extend_from_slice(fragment);
    Ok(())
}

#[derive(Default)]
struct PendingSseEvent {
    data: Option<String>,
}

impl PendingSseEvent {
    fn process_line(&mut self, line: &[u8]) -> ApiResult<Option<String>> {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        if line.len() > MAX_STREAM_EVENT_BYTES {
            return Err(ApiError::fatal("model sent an oversized SSE event"));
        }
        let line =
            std::str::from_utf8(line).map_err(|_| ApiError::fatal("SSE stream was not UTF-8"))?;
        if line.is_empty() {
            return Ok(self.take());
        }
        let Some(data) = line.strip_prefix("data:") else {
            return Ok(None);
        };
        let data = data.strip_prefix(' ').unwrap_or(data);
        let event_bytes = self
            .data
            .as_ref()
            .map_or(data.len(), |event| event.len() + 1 + data.len());
        if event_bytes > MAX_STREAM_EVENT_BYTES {
            return Err(ApiError::fatal("model sent an oversized SSE event"));
        }

        if let Some(event) = &mut self.data {
            event.push('\n');
            event.push_str(data);
        } else {
            self.data = Some(data.to_owned());
        }
        Ok(None)
    }

    fn take(&mut self) -> Option<String> {
        self.data.take()
    }
}
