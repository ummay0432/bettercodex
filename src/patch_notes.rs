//! Embedded, version-aware patch notes and their last-seen state.

use crate::state_file;
use anyhow::Context;
use anyhow::Result;
use anyhow::anyhow;
use serde::Deserialize;
use serde::Serialize;
use std::collections::HashSet;
use std::fmt;
use std::path::Path;

const CHANGELOG: &str = include_str!("../CHANGELOG.md");
const STATE_FILE_NAME: &str = "patch-notes.json";
const STATE_FORMAT_VERSION: u32 = 1;
const MAX_STATE_BYTES: usize = 4 * 1024;

// Patch-note tracking first ships after 0.1.3. A pre-existing session with no
// marker therefore came from 0.1.3 or earlier and should see the first notes.
const LEGACY_LAST_SEEN_VERSION: Version = Version::new(0, 1, 3);

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
struct Version {
    major: u64,
    minor: u64,
    patch: u64,
}

impl Version {
    const fn new(major: u64, minor: u64, patch: u64) -> Self {
        Self {
            major,
            minor,
            patch,
        }
    }

    fn parse(value: &str) -> Option<Self> {
        let mut components = value.split('.');
        let parse_component = |component: &str| {
            (!component.is_empty() && component.bytes().all(|byte| byte.is_ascii_digit()))
                .then(|| component.parse().ok())
                .flatten()
        };
        let version = Self::new(
            parse_component(components.next()?)?,
            parse_component(components.next()?)?,
            parse_component(components.next()?)?,
        );
        components.next().is_none().then_some(version)
    }
}

impl fmt::Display for Version {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

#[derive(Debug)]
struct Entry<'a> {
    version: Version,
    markdown: &'a str,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct State {
    version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_seen_version: Option<String>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            version: STATE_FORMAT_VERSION,
            last_seen_version: None,
        }
    }
}

/// Marks this build as seen and returns notes newer than the previous marker.
///
/// `had_saved_sessions` is sampled before the new session is created. It lets
/// the first release with this mechanism distinguish an existing 0.1.3 user
/// from a genuinely fresh installation, matching pi's fresh-install behavior.
pub(crate) fn for_startup(had_saved_sessions: bool) -> Result<Option<String>> {
    let home = crate::paths::bettercodex_home()
        .context("cannot save patch-note state because no bettercodex home is available")?;
    for_startup_at(
        &home.join(STATE_FILE_NAME),
        CHANGELOG,
        current_version()?,
        had_saved_sessions,
    )
}

/// Returns all released notes embedded in this build, oldest first so the
/// newest entry remains nearest the composer when inserted into scrollback.
pub(crate) fn released() -> Result<Option<String>> {
    notes_between(CHANGELOG, None, current_version()?)
}

fn current_version() -> Result<Version> {
    Version::parse(env!("CARGO_PKG_VERSION")).ok_or_else(|| {
        anyhow!(
            "bettercodex package version `{}` is not major.minor.patch",
            env!("CARGO_PKG_VERSION")
        )
    })
}

fn for_startup_at(
    state_path: &Path,
    changelog: &str,
    current: Version,
    had_saved_sessions: bool,
) -> Result<Option<String>> {
    let mut notes = None;
    state_file::update_json(state_path, MAX_STATE_BYTES, read_state, |state| {
        let last_seen = match state.last_seen_version.as_deref() {
            Some(version) => Version::parse(version).ok_or_else(|| {
                anyhow!(
                    "patch-note state contains invalid version `{version}` in {}",
                    state_path.display()
                )
            })?,
            None if had_saved_sessions => LEGACY_LAST_SEEN_VERSION,
            None => current,
        };

        notes = notes_between(changelog, Some(last_seen), current)?;
        if state.last_seen_version.is_none() || last_seen < current {
            state.last_seen_version = Some(current.to_string());
        }
        Ok(())
    })?;
    Ok(notes)
}

fn read_state(path: &Path) -> Result<State> {
    let state: State = state_file::read_json(path, MAX_STATE_BYTES)?.unwrap_or_default();
    if state.version != STATE_FORMAT_VERSION {
        return Err(anyhow!(
            "unsupported patch-note state version {}; expected {STATE_FORMAT_VERSION}",
            state.version
        ));
    }
    Ok(state)
}

fn notes_between(
    changelog: &str,
    last_seen: Option<Version>,
    current: Version,
) -> Result<Option<String>> {
    let mut entries = parse_changelog(changelog)?
        .into_iter()
        .filter(|entry| {
            entry.version <= current && last_seen.is_none_or(|last_seen| entry.version > last_seen)
        })
        .collect::<Vec<_>>();
    entries.sort_unstable_by_key(|entry| entry.version);
    let markdown = entries
        .into_iter()
        .map(|entry| entry.markdown)
        .collect::<Vec<_>>()
        .join("\n\n");
    Ok((!markdown.is_empty()).then_some(markdown))
}

fn parse_changelog(source: &str) -> Result<Vec<Entry<'_>>> {
    let mut entries = Vec::new();
    let mut versions = HashSet::new();
    let mut active = None::<(Version, usize)>;
    let mut offset = 0_usize;

    for segment in source.split_inclusive('\n') {
        let line = segment.trim_end_matches(['\r', '\n']);
        if let Some(heading) = line.strip_prefix("## ") {
            if let Some((version, start)) = active.take() {
                push_entry(source, start, offset, version, &mut versions, &mut entries)?;
            }
            active = parse_version_heading(heading)?.map(|version| (version, offset));
        }
        offset += segment.len();
    }

    if let Some((version, start)) = active {
        push_entry(
            source,
            start,
            source.len(),
            version,
            &mut versions,
            &mut entries,
        )?;
    }
    Ok(entries)
}

fn parse_version_heading(heading: &str) -> Result<Option<Version>> {
    let token = if let Some(bracketed) = heading.strip_prefix('[') {
        bracketed
            .split_once(']')
            .map(|(version, _)| version)
            .ok_or_else(|| anyhow!("patch-note heading has no closing bracket: `## {heading}`"))?
    } else {
        heading.split_whitespace().next().unwrap_or_default()
    };
    if token == "Unreleased"
        || !token
            .bytes()
            .next()
            .is_some_and(|byte| byte.is_ascii_digit())
    {
        return Ok(None);
    }
    Version::parse(token)
        .map(Some)
        .ok_or_else(|| anyhow!("patch-note heading has invalid version `{token}`"))
}

fn push_entry<'a>(
    source: &'a str,
    start: usize,
    end: usize,
    version: Version,
    versions: &mut HashSet<Version>,
    entries: &mut Vec<Entry<'a>>,
) -> Result<()> {
    if !versions.insert(version) {
        return Err(anyhow!("patch notes contain duplicate version {version}"));
    }
    let markdown = source[start..end].trim();
    if !markdown.is_empty() {
        entries.push(Entry { version, markdown });
    }
    Ok(())
}

#[cfg(test)]
#[path = "patch_notes_tests.rs"]
mod tests;
