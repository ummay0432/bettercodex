use super::ApiError;
use super::ApiResult;
use super::MAX_STREAM_EVENT_BYTES;
use bytes::Buf;
use bytes::BytesMut;

#[derive(Default)]
pub(super) struct SseDecoder {
    partial_line: BytesMut,
    pending_event: PendingSseEvent,
}

impl SseDecoder {
    pub(super) fn push(&mut self, chunk: &[u8]) -> ApiResult<Vec<String>> {
        let buffered_prefix = self.partial_line.len();
        self.partial_line.extend_from_slice(chunk);

        let Self {
            partial_line,
            pending_event,
        } = self;
        let mut events = Vec::new();
        let mut line_start = 0;
        for (chunk_offset, byte) in chunk.iter().enumerate() {
            if *byte != b'\n' {
                continue;
            }
            let newline = buffered_prefix + chunk_offset;
            if let Some(event) = pending_event.process_line(&partial_line[line_start..newline])? {
                events.push(event);
            }
            line_start = newline + 1;
        }
        partial_line.advance(line_start);
        if partial_line.len() > MAX_STREAM_EVENT_BYTES {
            return Err(ApiError::fatal("model sent an oversized SSE event"));
        }
        Ok(events)
    }

    pub(super) fn finish(&mut self) -> ApiResult<Vec<String>> {
        let mut events = Vec::new();
        if !self.partial_line.is_empty() {
            let line = std::mem::take(&mut self.partial_line);
            if let Some(event) = self.pending_event.process_line(&line)? {
                events.push(event);
            }
        }
        if let Some(event) = self.pending_event.take() {
            events.push(event);
        }
        Ok(events)
    }
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
