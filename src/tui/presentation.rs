use std::collections::VecDeque;
use std::time::Duration;
use std::time::Instant;

use unicode_segmentation::UnicodeSegmentation;

// Keep cosmetic presentation within Codex's 120 FPS ceiling while revealing enough text per
// frame to stay ahead of normal model output. Queue pressure raises the batch size so even one
// unusually large delta converges within a tightly bounded interval instead of delaying tools or
// turn completion for seconds.
/// Minimum interval between presentation frames, matching Codex's 120 FPS ceiling.
pub(super) const MIN_FRAME_INTERVAL: Duration = Duration::from_nanos(8_333_334);
// Keep ordinary reveals smaller than a terminal line while avoiding a full Markdown/layout/draw
// pass for every few characters. At 120 FPS this still presents an 80-column line over several
// distinct frames, but bounds the steady-state render work to one pass per token-sized chunk.
const SMOOTH_GRAPHEMES_PER_FRAME: usize = 12;
const CATCH_UP_GRAPHEMES: usize = 48;
const CATCH_UP_ITEMS: usize = 48;
const CATCH_UP_AGE: Duration = Duration::from_millis(80);
const MAX_PRESENTATION_LAG: Duration = Duration::from_millis(200);
const COMPACT_REVEALED_BYTES: usize = 64 * 1024;

#[derive(Debug)]
struct PendingBoundary {
    end: usize,
    enqueued_at: Instant,
    /// Unrevealed graphemes that started in this arrival.
    ///
    /// A grapheme may extend through later arrivals (for example, a split emoji ZWJ sequence).
    /// Charging it to the arrival containing its start preserves that earlier deadline without
    /// splitting the grapheme on screen.
    graphemes: usize,
}

#[derive(Debug, Default)]
pub(super) struct AssistantPresentation {
    text: String,
    revealed_bytes: usize,
    pending_graphemes: usize,
    last_grapheme_start: usize,
    boundaries: VecDeque<PendingBoundary>,
}

impl AssistantPresentation {
    pub(super) fn enqueue(&mut self, text: String, received_at: Instant) {
        if text.is_empty() {
            return;
        }
        let previous_len = self.text.len();
        let previous_last_grapheme_start = self.last_grapheme_start;
        self.text.push_str(&text);

        // Appending can only extend the grapheme that previously ended the string; earlier
        // boundaries are stable. Re-segment that trailing grapheme together with the new text so
        // each byte is examined once in ordinary streams rather than once per presentation frame.
        let (graphemes, last_grapheme_start) = if previous_len == 0 {
            grapheme_summary(&self.text, /*start*/ 0)
        } else {
            let (suffix_graphemes, last_grapheme_start) =
                grapheme_summary(&self.text, previous_last_grapheme_start);
            debug_assert!(suffix_graphemes > 0);
            (suffix_graphemes.saturating_sub(1), last_grapheme_start)
        };
        self.pending_graphemes = self.pending_graphemes.saturating_add(graphemes);
        self.last_grapheme_start = last_grapheme_start;
        self.boundaries.push_back(PendingBoundary {
            end: self.text.len(),
            enqueued_at: received_at,
            graphemes,
        });
    }

    pub(super) fn is_pending(&self) -> bool {
        self.pending_graphemes > 0
    }

    pub(super) fn clear(&mut self) {
        self.text.clear();
        self.revealed_bytes = 0;
        self.pending_graphemes = 0;
        self.last_grapheme_start = 0;
        self.boundaries.clear();
    }

    pub(super) fn take_all(&mut self) -> String {
        let revealed = self.text[self.revealed_bytes..].to_string();
        self.clear();
        revealed
    }

    pub(super) fn reveal(&mut self, now: Instant) -> String {
        if self.pending_graphemes == 0 {
            self.clear();
            return String::new();
        }

        let oldest_age = self
            .boundaries
            .front()
            .map(|boundary| now.saturating_duration_since(boundary.enqueued_at))
            .unwrap_or_default();
        let pressured = self.pending_graphemes >= CATCH_UP_GRAPHEMES || oldest_age >= CATCH_UP_AGE;
        let budget = if pressured {
            // Treat each arrival as a deadline-ordered prefix. A small old chunk can therefore
            // catch up immediately without making a large, newly arrived chunk inherit its age
            // and appear in one plop.
            let mut prefix_graphemes = 0_usize;
            self.boundaries
                .iter()
                .map(|boundary| {
                    prefix_graphemes = prefix_graphemes.saturating_add(boundary.graphemes);
                    let age = now.saturating_duration_since(boundary.enqueued_at);
                    deadline_batch_size(prefix_graphemes, age)
                })
                .max()
                .unwrap_or_default()
        } else {
            SMOOTH_GRAPHEMES_PER_FRAME
        }
        .max(SMOOTH_GRAPHEMES_PER_FRAME)
        .min(self.pending_graphemes);

        let end = reveal_end(&self.text, self.revealed_bytes, budget);
        self.consume_boundaries(budget, end);
        self.pending_graphemes = self.pending_graphemes.saturating_sub(budget);
        let revealed = self.text[self.revealed_bytes..end].to_string();
        self.revealed_bytes = end;
        while self
            .boundaries
            .front()
            .is_some_and(|boundary| boundary.graphemes == 0 && boundary.end <= self.revealed_bytes)
        {
            self.boundaries.pop_front();
        }
        if self.pending_graphemes == 0 {
            debug_assert_eq!(self.revealed_bytes, self.text.len());
            self.clear();
        } else if self.revealed_bytes >= COMPACT_REVEALED_BYTES {
            let revealed_bytes = self.revealed_bytes;
            self.text.drain(..revealed_bytes);
            for boundary in &mut self.boundaries {
                boundary.end = boundary.end.saturating_sub(revealed_bytes);
            }
            self.last_grapheme_start = self.last_grapheme_start.saturating_sub(revealed_bytes);
            self.revealed_bytes = 0;
        }
        revealed
    }

    fn consume_boundaries(&mut self, mut graphemes: usize, revealed_end: usize) {
        while graphemes > 0 {
            let boundary = match self.boundaries.front_mut() {
                Some(boundary) => boundary,
                None => unreachable!("pending graphemes retain an arrival boundary"),
            };
            if boundary.graphemes == 0 {
                debug_assert!(boundary.end <= revealed_end);
                self.boundaries.pop_front();
                continue;
            }
            let consumed = graphemes.min(boundary.graphemes);
            boundary.graphemes -= consumed;
            graphemes -= consumed;
        }
    }
}

/// Locate the end of the next presentation batch without traversing text beyond that batch.
fn reveal_end(text: &str, start: usize, graphemes: usize) -> usize {
    let ascii_end = start.saturating_add(graphemes);
    if ascii_end <= text.len()
        && text.is_char_boundary(ascii_end)
        && text.as_bytes()[start..ascii_end].is_ascii()
        && !text.as_bytes()[start..ascii_end].contains(&b'\r')
        // A following non-ASCII scalar may extend the candidate's final ASCII character (for
        // example, a combining accent). An ASCII successor cannot do so once CR is excluded.
        && (ascii_end == text.len() || text.as_bytes()[ascii_end].is_ascii())
    {
        return ascii_end;
    }

    start.saturating_add(
        text[start..]
            .graphemes(/*is_extended*/ true)
            .take(graphemes)
            .map(str::len)
            .sum::<usize>(),
    )
}

/// Count graphemes in a suffix that starts on a known grapheme boundary and return the final
/// grapheme's absolute byte offset.
fn grapheme_summary(text: &str, start: usize) -> (usize, usize) {
    let suffix = &text[start..];
    // Model-authored Markdown is overwhelmingly ASCII. Every ASCII byte except the second half of
    // CRLF is its own grapheme, so the ordinary LF-only case needs no Unicode state-machine walk.
    if suffix.is_ascii() && !suffix.as_bytes().contains(&b'\r') {
        debug_assert!(!suffix.is_empty());
        return (suffix.len(), text.len().saturating_sub(1));
    }

    let mut graphemes = 0_usize;
    let mut last_start = start;
    for (offset, _) in suffix.grapheme_indices(/*is_extended*/ true) {
        graphemes += 1;
        last_start = start.saturating_add(offset);
    }
    debug_assert!(graphemes > 0);
    (graphemes, last_start)
}

/// Number of discrete transcript items to reveal this frame while keeping every queued item
/// inside the same cosmetic latency bound as assistant text.
pub(super) fn item_reveal_budget(
    now: Instant,
    enqueued_at: impl Iterator<Item = Instant>,
) -> usize {
    let mut prefix_items = 0_usize;
    let mut oldest_age = Duration::ZERO;
    let budget = enqueued_at
        .map(|enqueued_at| {
            prefix_items = prefix_items.saturating_add(1);
            let age = now.saturating_duration_since(enqueued_at);
            oldest_age = oldest_age.max(age);
            deadline_batch_size(prefix_items, age)
        })
        .max()
        .unwrap_or_default();
    if prefix_items == 0 {
        0
    } else if prefix_items < CATCH_UP_ITEMS && oldest_age < CATCH_UP_AGE {
        // Preserve a visible frame boundary between ordinary transcript items. Only a genuinely
        // large or aging queue should trade that fluency for catch-up throughput.
        1
    } else {
        budget.max(1)
    }
}

fn deadline_batch_size(prefix_items: usize, age: Duration) -> usize {
    let frame_nanos = MIN_FRAME_INTERVAL.as_nanos();
    let remaining = MAX_PRESENTATION_LAG.saturating_sub(age);
    let remaining_frames = remaining
        .as_nanos()
        .saturating_add(frame_nanos.saturating_sub(1))
        / frame_nanos;
    let remaining_frames = usize::try_from(remaining_frames.max(1)).unwrap_or(usize::MAX);
    prefix_items.saturating_add(remaining_frames.saturating_sub(1)) / remaining_frames
}
