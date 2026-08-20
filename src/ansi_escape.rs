//! Focused ANSI-to-Ratatui adapter retained from OpenAI Codex commit
//! 1669c2403f793d0230065397dfc25f52b844244e.

use ansi_to_tui::Error;
use ansi_to_tui::IntoText;
use ratatui::text::Line;
use ratatui::text::Text;

pub(crate) fn expand_tabs(input: &str) -> std::borrow::Cow<'_, str> {
    if input.contains('\t') {
        std::borrow::Cow::Owned(input.replace('\t', "    "))
    } else {
        std::borrow::Cow::Borrowed(input)
    }
}

/// Parses one ANSI-styled line, retaining the first line if the input contains
/// more than one.
pub(crate) fn ansi_escape_line(input: &str) -> Line<'static> {
    let input = expand_tabs(input);
    let text = ansi_escape(&input);
    match text.lines.as_slice() {
        [] => "".into(),
        [only] => only.clone(),
        [first, rest @ ..] => {
            tracing::warn!("ansi_escape_line: expected a single line, got {first:?} and {rest:?}");
            first.clone()
        }
    }
}

fn ansi_escape(input: &str) -> Text<'static> {
    match input.into_text() {
        Ok(mut text) => {
            for line in &mut text.lines {
                for span in &mut line.spans {
                    if span.content.chars().any(char::is_control) {
                        span.content = std::borrow::Cow::Owned(
                            span.content
                                .chars()
                                .filter(|character| !character.is_control())
                                .collect(),
                        );
                    }
                }
            }
            text
        }
        Err(Error::NomError(message)) => {
            tracing::error!(
                "ansi_to_tui NomError docs claim should never happen when parsing `{input}`: {message}"
            );
            panic!();
        }
        Err(Error::Utf8Error(error)) => {
            tracing::error!("Utf8Error: {error}");
            panic!();
        }
    }
}

