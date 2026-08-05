use crate::skills::Skill;
use crate::skills::is_mention_name_byte;
use codex_utils_fuzzy_match::fuzzy_match;
use ratatui::style::Color;
use ratatui::style::Modifier;
use ratatui::style::Style;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::text::Span;
use std::collections::HashSet;
use std::ops::Range;
use unicode_width::UnicodeWidthChar;

const MAX_POPUP_ROWS: usize = 8;
const MAX_DISPLAY_NAME_WIDTH: usize = 28;

#[derive(Clone, Debug, PartialEq, Eq)]
struct ActiveToken {
    range: Range<usize>,
    query: String,
}

#[derive(Clone, Debug)]
struct SkillMatch {
    skill_index: usize,
    display_name_indices: Option<Vec<usize>>,
    score: i32,
}

#[derive(Debug, Default)]
pub(super) struct SkillPopup {
    token: Option<ActiveToken>,
    dismissed_token: Option<ActiveToken>,
    matches: Vec<SkillMatch>,
    selected: Option<usize>,
    first_visible: usize,
}

impl SkillPopup {
    pub(super) fn sync(
        &mut self,
        text: &str,
        cursor: usize,
        bound_ranges: &[Range<usize>],
        skills: &[Skill],
    ) {
        let mut token = active_token(text, cursor, bound_ranges, skills);
        if self.dismissed_token.as_ref() == token.as_ref() {
            token = None;
        } else {
            self.dismissed_token = None;
        }
        if token == self.token {
            return;
        }
        self.token = token;
        self.matches = self
            .token
            .as_ref()
            .map_or_else(Vec::new, |token| matching_skills(skills, &token.query));
        self.selected = (!self.matches.is_empty()).then_some(0);
        self.first_visible = 0;
    }

    pub(super) fn is_active(&self) -> bool {
        self.token.is_some()
    }

    pub(super) fn dismiss(&mut self) {
        self.dismissed_token = self.token.clone();
        self.hide();
    }

    pub(super) fn hide(&mut self) {
        self.token = None;
        self.matches.clear();
        self.selected = None;
        self.first_visible = 0;
    }

    pub(super) fn move_up(&mut self) {
        if self.matches.is_empty() {
            return;
        }
        self.selected = Some(match self.selected {
            Some(0) | None => self.matches.len() - 1,
            Some(selected) => selected - 1,
        });
        self.ensure_selected_visible();
    }

    pub(super) fn move_down(&mut self) {
        if self.matches.is_empty() {
            return;
        }
        self.selected = Some(match self.selected {
            Some(selected) if selected + 1 < self.matches.len() => selected + 1,
            Some(_) | None => 0,
        });
        self.ensure_selected_visible();
    }

    pub(super) fn selected_skill(&self, skills: &[Skill]) -> Option<(Range<usize>, Skill)> {
        let token = self.token.as_ref()?;
        let selected = self.selected?;
        let skill = skills.get(self.matches.get(selected)?.skill_index)?.clone();
        Some((token.range.clone(), skill))
    }

    pub(super) fn height(&self) -> u16 {
        if !self.is_active() {
            return 0;
        }
        u16::try_from(self.matches.len().clamp(1, MAX_POPUP_ROWS))
            .unwrap_or(u16::MAX)
            .saturating_add(2)
    }

    pub(super) fn lines(&self, skills: &[Skill]) -> Vec<Line<'static>> {
        if !self.is_active() {
            return Vec::new();
        }
        let visible_end = self
            .first_visible
            .saturating_add(MAX_POPUP_ROWS)
            .min(self.matches.len());
        let visible = &self.matches[self.first_visible..visible_end];
        let name_width = visible
            .iter()
            .filter_map(|matched| skills.get(matched.skill_index))
            .map(|skill| {
                display_width(&truncate_width(
                    skill.display_name(),
                    MAX_DISPLAY_NAME_WIDTH,
                ))
            })
            .max()
            .unwrap_or(1);
        let mut lines = if visible.is_empty() {
            vec![Line::from("  no matches").dim().italic()]
        } else {
            visible
                .iter()
                .enumerate()
                .filter_map(|(visible_index, matched)| {
                    let skill = skills.get(matched.skill_index)?;
                    let selected = self.selected == Some(self.first_visible + visible_index);
                    Some(skill_line(skill, matched, selected, name_width))
                })
                .collect()
        };
        lines.push(Line::default());
        lines.push(Line::from(vec![
            "  Press ".into(),
            "enter".bold(),
            " to insert or ".dim(),
            "esc".bold(),
            " to close".dim(),
        ]));
        lines
    }

    fn ensure_selected_visible(&mut self) {
        let Some(selected) = self.selected else {
            return;
        };
        if selected < self.first_visible {
            self.first_visible = selected;
        } else if selected >= self.first_visible.saturating_add(MAX_POPUP_ROWS) {
            self.first_visible = selected.saturating_add(1).saturating_sub(MAX_POPUP_ROWS);
        }
    }
}

fn matching_skills(skills: &[Skill], query: &str) -> Vec<SkillMatch> {
    let mut matches = skills
        .iter()
        .enumerate()
        .filter_map(|(skill_index, skill)| {
            if query.is_empty() {
                return Some(SkillMatch {
                    skill_index,
                    display_name_indices: None,
                    score: 0,
                });
            }
            if let Some((indices, score)) = fuzzy_match(skill.display_name(), query) {
                return Some(SkillMatch {
                    skill_index,
                    display_name_indices: Some(indices),
                    score,
                });
            }
            (skill.display_name() != skill.name())
                .then(|| fuzzy_match(skill.name(), query))
                .flatten()
                .map(|(_, score)| SkillMatch {
                    skill_index,
                    display_name_indices: None,
                    score,
                })
        })
        .collect::<Vec<_>>();
    matches.sort_by(|left, right| {
        left.display_name_indices
            .is_none()
            .cmp(&right.display_name_indices.is_none())
            .then_with(|| left.score.cmp(&right.score))
            .then_with(|| {
                skills[left.skill_index]
                    .display_name()
                    .cmp(skills[right.skill_index].display_name())
            })
            .then_with(|| {
                skills[left.skill_index]
                    .path()
                    .cmp(skills[right.skill_index].path())
            })
    });
    matches
}

fn active_token(
    text: &str,
    cursor: usize,
    bound_ranges: &[Range<usize>],
    skills: &[Skill],
) -> Option<ActiveToken> {
    if skills.is_empty() {
        return None;
    }
    let cursor = previous_char_boundary(text, cursor.min(text.len()));
    let line_start = text[..cursor].rfind('\n').map_or(0, |index| index + 1);
    let line_end = text[cursor..]
        .find('\n')
        .map_or(text.len(), |index| cursor + index);
    let start = text[line_start..cursor]
        .char_indices()
        .rfind(|(_, character)| character.is_whitespace())
        .map_or(line_start, |(index, character)| {
            line_start + index + character.len_utf8()
        });
    let end = cursor
        + text[cursor..line_end]
            .char_indices()
            .find(|(_, character)| character.is_whitespace())
            .map_or(line_end - cursor, |(index, _)| index);
    let range = start..end;
    if bound_ranges
        .iter()
        .any(|bound| bound.start < range.end && range.start < bound.end)
    {
        return None;
    }
    let query = text.get(range.clone())?.strip_prefix('$')?;
    if !query
        .as_bytes()
        .iter()
        .all(|byte| is_mention_name_byte(*byte))
        || !dollar_query_is_completable(query, skills)
    {
        return None;
    }
    Some(ActiveToken {
        range,
        query: query.to_string(),
    })
}

fn dollar_query_is_completable(query: &str, skills: &[Skill]) -> bool {
    if query.is_empty() {
        return true;
    }
    let uppercase_shell_variable = query.bytes().all(|byte| !byte.is_ascii_lowercase())
        && is_common_environment_variable(query);
    if uppercase_shell_variable || query.bytes().all(|byte| byte.is_ascii_digit()) {
        return false;
    }
    if matches!(query, "-" | "_") {
        return false;
    }
    if query
        .as_bytes()
        .first()
        .is_some_and(|byte| *byte == b'-' || byte.is_ascii_digit())
    {
        return skills.iter().any(|skill| {
            fuzzy_match(skill.name(), query).is_some()
                || fuzzy_match(skill.display_name(), query).is_some()
        });
    }
    true
}

fn is_common_environment_variable(name: &str) -> bool {
    matches!(
        name.to_ascii_uppercase().as_str(),
        "PATH"
            | "HOME"
            | "USER"
            | "SHELL"
            | "PWD"
            | "TMPDIR"
            | "TEMP"
            | "TMP"
            | "LANG"
            | "TERM"
            | "XDG_CONFIG_HOME"
    )
}

fn skill_line(
    skill: &Skill,
    matched: &SkillMatch,
    selected: bool,
    name_width: usize,
) -> Line<'static> {
    let display_name = truncate_width(skill.display_name(), MAX_DISPLAY_NAME_WIDTH);
    let matched_indices = matched
        .display_name_indices
        .as_ref()
        .map(|indices| indices.iter().copied().collect::<HashSet<_>>())
        .unwrap_or_default();
    let mut spans = vec![Span::from("  ")];
    for (index, character) in display_name.chars().enumerate() {
        let span = Span::from(character.to_string());
        spans.push(if matched_indices.contains(&index) {
            span.bold()
        } else {
            span
        });
    }
    let padding = name_width
        .saturating_sub(display_width(&display_name))
        .saturating_add(2);
    spans.push(Span::from(" ".repeat(padding)));
    spans.push(Span::from("[Skill] ").dim());
    spans.push(Span::from(skill.display_description().to_string()).dim());
    if selected {
        let style = Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD);
        for span in &mut spans {
            span.style = style;
        }
    }
    Line::from(spans)
}

fn truncate_width(value: &str, max_width: usize) -> String {
    let mut result = String::new();
    let mut width = 0_usize;
    for character in value.chars() {
        let character_width = character.width().unwrap_or(0);
        if width.saturating_add(character_width) > max_width {
            break;
        }
        result.push(character);
        width = width.saturating_add(character_width);
    }
    result
}

fn display_width(value: &str) -> usize {
    value
        .chars()
        .map(|character| character.width().unwrap_or(0))
        .sum()
}

fn previous_char_boundary(text: &str, mut index: usize) -> usize {
    while !text.is_char_boundary(index) {
        index -= 1;
    }
    index
}

#[cfg(test)]
#[path = "skill_popup_tests.rs"]
mod tests;
