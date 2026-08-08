use crate::quality_loop::DEFAULT_ITERATIONS;
use crate::skills::is_mention_name_byte;
use anyhow::Result;
use anyhow::anyhow;
use serde::Deserialize;
use serde::Serialize;
use std::collections::BTreeSet;
use std::ops::Range;

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum TriggerKind {
    Slash,
    Inline,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub(crate) struct ConsumedTrigger {
    pub(crate) kind: TriggerKind,
    pub(crate) start: usize,
    pub(crate) end: usize,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub(crate) struct ConsumedCount {
    pub(crate) value: usize,
    pub(crate) start: usize,
    pub(crate) end: usize,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub(crate) struct LoopInvocation {
    pub(crate) iterations: usize,
    pub(crate) triggers: Vec<ConsumedTrigger>,
    pub(crate) counts: Vec<ConsumedCount>,
}

#[derive(Clone, Debug)]
struct Trigger {
    kind: TriggerKind,
    span: Range<usize>,
}

#[derive(Clone, Debug)]
struct CountPhrase {
    span: Range<usize>,
    value: std::result::Result<usize, CountError>,
}

#[derive(Clone, Copy, Debug)]
enum CountError {
    Zero,
    Overflow,
}

#[cfg(test)]
pub(crate) fn parse_invocation(text: &str, has_attachment: bool) -> Result<Option<LoopInvocation>> {
    parse_invocation_with_mode(text, has_attachment, true)
}

pub(crate) fn parse_invocation_with_mode(
    text: &str,
    has_attachment: bool,
    allow_slash: bool,
) -> Result<Option<LoopInvocation>> {
    let triggers = find_triggers(text, allow_slash);
    if triggers.is_empty() {
        return Ok(None);
    }

    let phrases = find_count_phrases(text);
    let mut adjacent = BTreeSet::new();
    for trigger in &triggers {
        for (index, phrase) in phrases.iter().enumerate() {
            if phrase_is_adjacent(text, trigger, phrase) {
                adjacent.insert(index);
            }
        }
        reject_malformed_local_count(text, trigger, &phrases)?;
    }

    let mut values = BTreeSet::new();
    let mut counts = Vec::new();
    for index in adjacent {
        let phrase = &phrases[index];
        let value = match phrase.value {
            Ok(value) => value,
            Err(CountError::Zero) => {
                return Err(anyhow!("loop iteration count must be positive"));
            }
            Err(CountError::Overflow) => {
                return Err(anyhow!("loop iteration count is too large"));
            }
        };
        values.insert(value);
        counts.push(ConsumedCount {
            value,
            start: phrase.span.start,
            end: phrase.span.end,
        });
    }
    if values.len() > 1 {
        return Err(anyhow!(
            "loop invocation contains conflicting iteration counts"
        ));
    }
    let iterations = values.into_iter().next().unwrap_or(DEFAULT_ITERATIONS);

    let mut consumed = triggers
        .iter()
        .map(|trigger| trigger.span.clone())
        .chain(counts.iter().map(|count| count.start..count.end))
        .collect::<Vec<_>>();
    consumed.sort_by_key(|span| span.start);
    if !has_attachment && !has_task_text(text, &consumed) {
        return Err(anyhow!(
            "`$loop` and `/loop` require task text or an attachment"
        ));
    }

    Ok(Some(LoopInvocation {
        iterations,
        triggers: triggers
            .into_iter()
            .map(|trigger| ConsumedTrigger {
                kind: trigger.kind,
                start: trigger.span.start,
                end: trigger.span.end,
            })
            .collect(),
        counts,
    }))
}

fn find_triggers(text: &str, allow_slash: bool) -> Vec<Trigger> {
    let bytes = text.as_bytes();
    let mut triggers = Vec::new();
    let leading = text.len().saturating_sub(text.trim_start().len());
    if allow_slash
        && bytes.get(leading..leading.saturating_add(5)) == Some(b"/loop")
        && bytes
            .get(leading.saturating_add(5))
            .is_none_or(|byte| !is_mention_name_byte(*byte))
    {
        triggers.push(Trigger {
            kind: TriggerKind::Slash,
            span: leading..leading + 5,
        });
    }

    let mut cursor = 0;
    while let Some(relative) = text[cursor..].find("$loop") {
        let start = cursor + relative;
        let end = start + 5;
        if bytes
            .get(end)
            .is_none_or(|byte| !is_mention_name_byte(*byte))
        {
            triggers.push(Trigger {
                kind: TriggerKind::Inline,
                span: start..end,
            });
        }
        cursor = end;
    }
    triggers.sort_by_key(|trigger| trigger.span.start);
    triggers
}

fn find_count_phrases(text: &str) -> Vec<CountPhrase> {
    let bytes = text.as_bytes();
    let mut phrases = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        if !bytes[index].is_ascii_digit()
            || index > 0
                && (is_mention_name_byte(bytes[index - 1])
                    || matches!(bytes[index - 1], b'+' | b'-' | b'.'))
        {
            index += 1;
            continue;
        }
        let start = index;
        while bytes.get(index).is_some_and(u8::is_ascii_digit) {
            index += 1;
        }
        let digits_end = index;
        while bytes.get(index).is_some_and(u8::is_ascii_whitespace) {
            index += 1;
        }
        let unit_start = index;
        while bytes.get(index).is_some_and(u8::is_ascii_alphabetic) {
            index += 1;
        }
        let unit = text[unit_start..index].to_ascii_lowercase();
        if !matches!(unit.as_str(), "x" | "times" | "iterations")
            || bytes
                .get(index)
                .is_some_and(|byte| is_mention_name_byte(*byte))
        {
            index = digits_end.max(start + 1);
            continue;
        }
        let value = match text[start..digits_end].parse::<usize>() {
            Ok(0) => Err(CountError::Zero),
            Ok(value) => Ok(value),
            Err(_) => Err(CountError::Overflow),
        };
        phrases.push(CountPhrase {
            span: start..index,
            value,
        });
    }
    phrases
}

fn phrase_is_adjacent(text: &str, trigger: &Trigger, phrase: &CountPhrase) -> bool {
    match trigger.kind {
        TriggerKind::Slash => {
            phrase.span.start >= trigger.span.end
                && only_count_separators(&text[trigger.span.end..phrase.span.start])
        }
        TriggerKind::Inline => {
            (phrase.span.end <= trigger.span.start
                && only_count_separators(&text[phrase.span.end..trigger.span.start]))
                || (phrase.span.start >= trigger.span.end
                    && only_count_separators(&text[trigger.span.end..phrase.span.start]))
        }
    }
}

fn only_count_separators(value: &str) -> bool {
    value.chars().all(|character| {
        character.is_whitespace()
            || character.is_ascii_punctuation() && !matches!(character, '$' | '/' | '+' | '-' | '.')
    })
}

fn reject_malformed_local_count(
    text: &str,
    trigger: &Trigger,
    phrases: &[CountPhrase],
) -> Result<()> {
    let has_after = phrases.iter().any(|phrase| {
        phrase.span.start >= trigger.span.end
            && only_count_separators(&text[trigger.span.end..phrase.span.start])
    });
    if !has_after {
        let suffix = local_suffix(&text[trigger.span.end..]);
        if looks_like_malformed_count(suffix) {
            return Err(anyhow!("malformed loop iteration count after trigger"));
        }
    }
    if trigger.kind == TriggerKind::Inline {
        let has_before = phrases.iter().any(|phrase| {
            phrase.span.end <= trigger.span.start
                && only_count_separators(&text[phrase.span.end..trigger.span.start])
        });
        if !has_before {
            let prefix = local_prefix(&text[..trigger.span.start]);
            if looks_like_malformed_count_before(prefix) {
                return Err(anyhow!("malformed loop iteration count before trigger"));
            }
        }
    }
    Ok(())
}

fn local_suffix(value: &str) -> &str {
    let start = value
        .char_indices()
        .find(|(_, character)| !is_soft_separator(*character))
        .map_or(value.len(), |(index, _)| index);
    let rest = &value[start..];
    let end = rest
        .char_indices()
        .take(96)
        .find(|(_, character)| {
            matches!(
                *character,
                ',' | ';' | ':' | '(' | ')' | '[' | ']' | '{' | '}'
            )
        })
        .map_or(rest.len(), |(index, _)| index);
    &rest[..end]
}

fn local_prefix(value: &str) -> &str {
    let end = value
        .char_indices()
        .rev()
        .find(|(_, character)| !is_soft_separator(*character))
        .map_or(0, |(index, character)| index + character.len_utf8());
    let rest = &value[..end];
    let start = rest
        .char_indices()
        .rev()
        .take(96)
        .find(|(_, character)| {
            matches!(
                *character,
                ',' | ';' | ':' | '(' | ')' | '[' | ']' | '{' | '}'
            )
        })
        .map_or(0, |(index, character)| index + character.len_utf8());
    &rest[start..]
}

fn is_soft_separator(character: char) -> bool {
    character.is_whitespace()
        || character.is_ascii_punctuation() && !matches!(character, '+' | '-' | '.' | '$' | '/')
}

fn looks_like_malformed_count(value: &str) -> bool {
    let mut words = value.split_whitespace();
    let Some(first) = words.next() else {
        return false;
    };
    if first.is_empty() {
        return false;
    }
    let first_lower = first.to_ascii_lowercase();
    let starts_count_like = first_lower
        .as_bytes()
        .first()
        .is_some_and(|byte| byte.is_ascii_digit() || matches!(*byte, b'+' | b'-' | b'.'));
    if !starts_count_like {
        return matches!(first_lower.as_str(), "x" | "times" | "iterations");
    }
    if matches!(first_lower.as_str(), "+" | "-" | ".") {
        let remainder = words.collect::<Vec<_>>().join(" ");
        return !remainder.is_empty() && looks_like_malformed_count(&remainder);
    }
    if looks_like_count_token(&first_lower) {
        return true;
    }
    words.next().is_some_and(|unit| {
        let unit = unit.to_ascii_lowercase();
        unit.starts_with('x') || unit.starts_with("time") || unit.starts_with("iteration")
    })
}

fn looks_like_count_token(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    if lower
        .strip_suffix('x')
        .or_else(|| lower.strip_suffix("times"))
        .or_else(|| lower.strip_suffix("iterations"))
        .is_some_and(|number| {
            number
                .as_bytes()
                .first()
                .is_some_and(|byte| byte.is_ascii_digit() || matches!(*byte, b'+' | b'-' | b'.'))
        })
    {
        return true;
    }
    let Some(first_unit) = lower
        .char_indices()
        .find(|(_, character)| character.is_ascii_alphabetic())
        .map(|(index, _)| index)
    else {
        return false;
    };
    let unit = &lower[first_unit..];
    unit.starts_with('x') || unit.starts_with("time") || unit.starts_with("iteration")
}

fn looks_like_malformed_count_before(value: &str) -> bool {
    let words = value.split_whitespace().collect::<Vec<_>>();
    let Some(last) = words.last() else {
        return false;
    };
    let unit = last.to_ascii_lowercase();
    let candidate = if matches!(unit.as_str(), "x" | "times" | "iterations") {
        let Some(number) = words.get(words.len().saturating_sub(2)) else {
            return false;
        };
        format!("{number} {last}")
    } else {
        (*last).to_string()
    };
    candidate
        .as_bytes()
        .first()
        .is_some_and(|byte| byte.is_ascii_digit() || matches!(*byte, b'+' | b'-' | b'.'))
        && looks_like_malformed_count(&candidate)
}

fn has_task_text(text: &str, consumed: &[Range<usize>]) -> bool {
    let mut remaining = String::with_capacity(text.len());
    let mut cursor = 0;
    for span in consumed {
        if span.start > cursor {
            remaining.push_str(&text[cursor..span.start]);
        }
        remaining.push(' ');
        cursor = cursor.max(span.end);
    }
    remaining.push_str(&text[cursor..]);
    remaining
        .chars()
        .any(|character| !character.is_whitespace() && !character.is_ascii_punctuation())
}
