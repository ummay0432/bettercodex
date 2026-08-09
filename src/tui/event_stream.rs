use crossterm::event::Event;
use futures_util::Stream;
use std::io;
use std::pin::Pin;
use std::task::Context;
use std::task::Poll;

/// Crossterm expects Win32 input records. Reassert record mode around each
/// poll because another console client can restore VT input while we run.
pub(super) struct EventStream(crossterm::event::EventStream);

impl EventStream {
    pub(super) fn new() -> Self {
        Self(crossterm::event::EventStream::new())
    }
}

impl Stream for EventStream {
    type Item = io::Result<Event>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        #[cfg(windows)]
        let _ = super::windows_console::ensure_input_record_mode();

        let result = Pin::new(&mut self.0).poll_next(cx);

        #[cfg(windows)]
        if result.is_pending() {
            let _ = super::windows_console::ensure_input_record_mode();
        }

        result
    }
}
