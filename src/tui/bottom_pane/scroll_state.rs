// Ported from OpenAI Codex rust-v0.147.0 (be6e8eac),
// codex-rs/tui/src/bottom_pane/scroll_state.rs.

/// Shared selection and scrolling state for Codex-style command menus.
#[derive(Clone, Copy, Debug, Default)]
pub(in crate::tui) struct ScrollState {
    pub(in crate::tui) selected_idx: Option<usize>,
    pub(in crate::tui) scroll_top: usize,
}

impl ScrollState {
    pub(in crate::tui) fn new() -> Self {
        Self::default()
    }

    pub(in crate::tui) fn clamp_selection(&mut self, len: usize) {
        if self.clear_if_empty(len) {
            return;
        }
        self.selected_idx = Some(self.selected_idx.unwrap_or(0).min(len - 1));
    }

    pub(in crate::tui) fn move_up_wrap(&mut self, len: usize) {
        if self.clear_if_empty(len) {
            return;
        }
        self.selected_idx = Some(match self.selected_idx {
            Some(index) if index > 0 => index - 1,
            Some(_) => len - 1,
            None => 0,
        });
    }

    pub(in crate::tui) fn move_down_wrap(&mut self, len: usize) {
        if self.clear_if_empty(len) {
            return;
        }
        self.selected_idx = Some(match self.selected_idx {
            Some(index) if index + 1 < len => index + 1,
            _ => 0,
        });
    }

    pub(in crate::tui) fn page_up_clamped(&mut self, len: usize, visible_rows: usize) {
        if self.clear_if_empty(len) {
            return;
        }
        let current = self.selected_idx.unwrap_or(0).min(len - 1);
        self.selected_idx = Some(current.saturating_sub(visible_rows.max(1)));
        self.ensure_visible(len, visible_rows);
    }

    pub(in crate::tui) fn page_down_clamped(&mut self, len: usize, visible_rows: usize) {
        if self.clear_if_empty(len) {
            return;
        }
        let current = self.selected_idx.unwrap_or(0).min(len - 1);
        self.selected_idx = Some(current.saturating_add(visible_rows.max(1)).min(len - 1));
        self.ensure_visible(len, visible_rows);
    }

    pub(in crate::tui) fn jump_top(&mut self, len: usize, visible_rows: usize) {
        if self.clear_if_empty(len) {
            return;
        }
        self.selected_idx = Some(0);
        self.ensure_visible(len, visible_rows);
    }

    pub(in crate::tui) fn jump_bottom(&mut self, len: usize, visible_rows: usize) {
        if self.clear_if_empty(len) {
            return;
        }
        self.selected_idx = Some(len - 1);
        self.ensure_visible(len, visible_rows);
    }

    pub(in crate::tui) fn ensure_visible(&mut self, len: usize, visible_rows: usize) {
        if len == 0 || visible_rows == 0 {
            self.scroll_top = 0;
            return;
        }
        if let Some(selected) = self.selected_idx {
            if selected < self.scroll_top {
                self.scroll_top = selected;
            } else {
                let bottom = self.scroll_top + visible_rows - 1;
                if selected > bottom {
                    self.scroll_top = selected + 1 - visible_rows;
                }
            }
        } else {
            self.scroll_top = 0;
        }
    }

    fn clear_if_empty(&mut self, len: usize) -> bool {
        if len != 0 {
            return false;
        }
        self.selected_idx = None;
        self.scroll_top = 0;
        true
    }
}
