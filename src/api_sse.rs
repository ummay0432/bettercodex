use super::ApiError;
use super::ApiResult;
use super::MAX_STREAM_EVENT_BYTES;
use memchr::memchr;
use memchr::memchr_iter;

#[derive(Default)]
pub(super) struct SseDecoder {
    partial_line: Vec<u8>,
    pending_event: PendingSseEvent,
}

impl SseDecoder {
    pub(super) fn push(&mut self, chunk: &[u8], events: &mut Vec<String>) -> ApiResult<()> {
        let Self {
            partial_line,
            pending_event,
        } = self;

        let chunk = if partial_line.is_empty() {
            chunk
        } else if let Some(newline) = memchr(b'\n', chunk) {
            append_fragment(partial_line, &chunk[..newline])?;
            if let Some(event) = pending_event.process_line(partial_line)? {
                events.push(event);
            }
            partial_line.clear();
            &chunk[newline + 1..]
        } else {
            append_fragment(partial_line, chunk)?;
            return Ok(());
        };

        let mut line_start = 0;
        for newline in memchr_iter(b'\n', chunk) {
            if let Some(event) = pending_event.process_line(&chunk[line_start..newline])? {
                events.push(event);
            }
            line_start = newline + 1;
        }
        append_fragment(partial_line, &chunk[line_start..])?;
        Ok(())
    }

    pub(super) fn finish(&mut self, events: &mut Vec<String>) -> ApiResult<()> {
        if !self.partial_line.is_empty() {
            let line = std::mem::take(&mut self.partial_line);
            if let Some(event) = self.pending_event.process_line(&line)? {
                events.push(event);
            }
        }
        if let Some(event) = self.pending_event.take() {
            events.push(event);
        }
        Ok(())
    }
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

#[cfg(test)]
#[path = "api_sse_tests.rs"]
mod tests;
