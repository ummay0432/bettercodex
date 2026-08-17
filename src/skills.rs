use crate::paths;
use crate::repository;
use crate::skill_settings;
use crate::system_skills;
use crate::text::escape_cdata;
use crate::text::escape_xml_text;
use anyhow::Context;
use anyhow::Result;
use anyhow::anyhow;
use serde::Deserialize;
use serde_json::Value;
use serde_json::json;
use std::collections::HashMap;
use std::collections::HashSet;
use std::collections::VecDeque;
use std::fs::File;
use std::io::BufRead;
use std::io::BufReader;
use std::io::Read;
use std::ops::Range;
use std::path::Path;
use std::path::PathBuf;

const SKILL_FILE_NAME: &str = "SKILL.md";
const SKILLS_DIRECTORY: &str = "skills";
const PROJECT_DIRECTORY: &str = ".bcodex";
const PAPERCUT_SYSTEM_SKILL_NAME: &str = "papercut";
const MAX_NAME_CHARS: usize = 64;
const MAX_DESCRIPTION_CHARS: usize = 1_024;
const MAX_METADATA_BYTES: usize = 64 * 1024;
const MAX_SKILL_PROMPT_BYTES: usize = 8_000;
const MAX_SCAN_DEPTH: usize = 6;
const MAX_SCAN_DIRECTORIES: usize = 2_000;
const MAX_SCAN_ENTRIES: usize = 20_000;
const MAX_WARNINGS: usize = 32;
const APPROXIMATE_BYTES_PER_TOKEN: u64 = 4;
const SKILL_METADATA_CONTEXT_PERCENT: u64 = 2;
// Hard ceiling for the complete serialized Responses item, not only its decoded text body.
const MAX_SKILLS_CONTEXT_BYTES: usize = 39_000;

fn is_reserved_system_skill_name(name: &str) -> bool {
    name == "review"
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum SkillScope {
    System,
    Repository,
    User,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Skill {
    name: String,
    description: String,
    short_description: Option<String>,
    display_name: Option<String>,
    path: PathBuf,
    discovery_path: PathBuf,
    scope: SkillScope,
    enabled: bool,
    allow_implicit_invocation: bool,
}

impl Skill {
    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    pub(crate) fn display_name(&self) -> &str {
        self.display_name.as_deref().unwrap_or(&self.name)
    }

    pub(crate) fn display_description(&self) -> &str {
        self.short_description
            .as_deref()
            .unwrap_or(&self.description)
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    fn matches_path(&self, path: &Path) -> bool {
        self.path == path || self.discovery_path == path
    }

    pub(crate) fn is_enabled(&self) -> bool {
        self.enabled
    }

    pub(crate) fn allows_implicit_invocation(&self) -> bool {
        self.allow_implicit_invocation
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SkillUpdate {
    Enabled(bool),
    AllowImplicitInvocation(bool),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SkillSelection {
    name: String,
    path: PathBuf,
}

impl SkillSelection {
    pub(crate) fn new(name: impl Into<String>, path: impl Into<PathBuf>) -> Self {
        Self {
            name: name.into(),
            path: path.into(),
        }
    }

    pub(crate) fn name(&self) -> &str {
        &self.name
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SkillMention {
    selection: SkillSelection,
    range: Range<usize>,
}

impl SkillMention {
    pub(crate) fn new(selection: SkillSelection, range: Range<usize>) -> Self {
        Self { selection, range }
    }

    pub(crate) fn selection(&self) -> &SkillSelection {
        &self.selection
    }

    pub(crate) fn range(&self) -> &Range<usize> {
        &self.range
    }

    pub(crate) fn range_mut(&mut self) -> &mut Range<usize> {
        &mut self.range
    }

    pub(crate) fn shifted(mut self, offset: usize) -> Self {
        self.range.start = self.range.start.saturating_add(offset);
        self.range.end = self.range.end.saturating_add(offset);
        self
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct SkillCatalog {
    skills: Vec<Skill>,
    warnings: Vec<String>,
}

#[derive(Debug, Default)]
pub(crate) struct SkillInjectionOutcome {
    pub(crate) items: Vec<Value>,
    pub(crate) warnings: Vec<String>,
}

#[derive(Clone, Copy)]
struct SkillRoot<'a> {
    path: &'a Path,
    scope: SkillScope,
}

#[derive(Debug, Deserialize)]
struct SkillFrontmatter {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    metadata: SkillFrontmatterMetadata,
}

#[derive(Debug, Default, Deserialize)]
struct SkillFrontmatterMetadata {
    #[serde(default, rename = "short-description")]
    short_description: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct SkillMetadataFile {
    #[serde(default)]
    interface: Option<SkillInterfaceFile>,
    #[serde(default)]
    policy: Option<SkillPolicyFile>,
}

#[derive(Debug, Default, Deserialize)]
struct SkillInterfaceFile {
    #[serde(default)]
    display_name: Option<String>,
    #[serde(default)]
    short_description: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct SkillPolicyFile {
    #[serde(default)]
    allow_implicit_invocation: Option<bool>,
}

struct WarningCollector {
    warnings: Vec<String>,
    omitted: usize,
}

impl WarningCollector {
    fn new() -> Self {
        Self {
            warnings: Vec::new(),
            omitted: 0,
        }
    }

    fn push(&mut self, warning: impl Into<String>) {
        if self.warnings.len() < MAX_WARNINGS {
            self.warnings.push(bounded_warning(warning.into()));
        } else {
            self.omitted = self.omitted.saturating_add(1);
        }
    }

    fn finish(mut self) -> Vec<String> {
        if self.omitted > 0 {
            self.warnings.push(format!(
                "{} additional skill loading warning(s) omitted",
                self.omitted
            ));
        }
        self.warnings
    }
}

impl SkillCatalog {
    pub(crate) fn load(cwd: &Path) -> Self {
        let home = paths::bettercodex_home();
        Self::load_with_home(cwd, home.as_deref())
    }

    fn load_with_home(cwd: &Path, home: Option<&Path>) -> Self {
        let installation_warning = home.and_then(|home| {
            system_skills::install(home)
                .err()
                .map(|error| format!("Could not install bundled system skills: {error:#}"))
        });
        let roots = discovery_roots_with_home(cwd, home);
        let borrowed = roots
            .iter()
            .map(|(path, scope)| SkillRoot {
                path,
                scope: *scope,
            })
            .collect::<Vec<_>>();
        let mut catalog = Self::load_from_roots(&borrowed);
        if let Some(home) = home {
            catalog.apply_settings(&home.join(skill_settings::FILE_NAME));
        }
        if let Some(warning) = installation_warning
            && catalog.warnings.len() < MAX_WARNINGS
        {
            catalog.warnings.push(bounded_warning(warning));
        }
        catalog
    }

    fn load_from_roots(roots: &[SkillRoot<'_>]) -> Self {
        let mut warnings = WarningCollector::new();
        let mut discovered_paths = HashSet::new();
        let mut skills = Vec::new();

        for root in roots {
            for discovery_path in discover_skill_files(root.path, root.scope, &mut warnings) {
                let canonical = discovery_path
                    .canonicalize()
                    .unwrap_or_else(|_| discovery_path.clone());
                if discovered_paths.contains(&canonical) {
                    continue;
                }
                match load_skill(&canonical, discovery_path, root.scope) {
                    Ok((mut skill, metadata_warning)) => {
                        if is_reserved_system_skill_name(&skill.name)
                            && skill.scope != SkillScope::System
                        {
                            warnings.push(format!(
                                "Skipped reserved skill name `{}` at {}",
                                skill.name,
                                canonical.display()
                            ));
                            // Do not let a lower-trust alias claim the canonical path and suppress
                            // the bundled system skill when its root is scanned later.
                            continue;
                        }
                        discovered_paths.insert(canonical.clone());
                        if is_reserved_system_skill_name(&skill.name) {
                            skill.enabled = true;
                        }
                        skills.push(skill);
                        if let Some(warning) = metadata_warning {
                            warnings.push(warning);
                        }
                    }
                    Err(error) => {
                        discovered_paths.insert(canonical.clone());
                        warnings.push(format!(
                            "Skipped invalid skill {}: {error:#}",
                            canonical.display()
                        ));
                    }
                }
            }
        }

        skills.sort_by(|left, right| {
            left.scope
                .cmp(&right.scope)
                .then_with(|| left.name.cmp(&right.name))
                .then_with(|| left.path.cmp(&right.path))
        });
        Self {
            skills,
            warnings: warnings.finish(),
        }
    }

    pub(crate) fn skills(&self) -> &[Skill] {
        &self.skills
    }

    pub(crate) fn warnings(&self) -> &[String] {
        &self.warnings
    }

    pub(crate) fn catalogue_message(&self, context_window: u64) -> Option<Value> {
        let visible = self
            .skills
            .iter()
            .filter(|skill| skill.enabled && skill.allow_implicit_invocation)
            .collect::<Vec<_>>();
        if visible.is_empty() {
            return None;
        }

        let line_budget = context_window
            .saturating_mul(SKILL_METADATA_CONTEXT_PERCENT)
            .saturating_div(100)
            .saturating_mul(APPROXIMATE_BYTES_PER_TOKEN)
            .try_into()
            .unwrap_or(usize::MAX)
            .min(MAX_SKILLS_CONTEXT_BYTES);
        let empty_item = catalogue_item(String::new());
        let serialized_lines_budget = MAX_SKILLS_CONTEXT_BYTES
            .saturating_sub(serialized_value_bytes(&empty_item))
            .saturating_sub(json_string_content_bytes(
                "<available_skills>\n\n</available_skills>",
            ));
        let (mut lines, mut omitted) =
            render_catalog_lines(&visible, line_budget, serialized_lines_budget);
        if omitted > 0 {
            let mut cost = catalog_lines_cost(&lines);
            loop {
                let notice = format!(
                    "- Exceeded skills context budget; {omitted} additional skill(s) were omitted."
                );
                let mut candidate = cost;
                candidate.add_line(&notice, !lines.is_empty());
                if candidate.fits(line_budget, serialized_lines_budget) {
                    lines.push(notice);
                    break;
                }
                let Some(removed) = lines.pop() else {
                    break;
                };
                cost.remove_last_line(&removed, !lines.is_empty());
                omitted = omitted.saturating_add(1);
            }
        }
        let body = format!(
            "<available_skills>\n{}\n</available_skills>",
            lines.join("\n"),
        );
        let item = catalogue_item(body);
        let serialized_bytes = serialized_value_bytes(&item);
        debug_assert!(serialized_bytes <= MAX_SKILLS_CONTEXT_BYTES);
        (serialized_bytes <= MAX_SKILLS_CONTEXT_BYTES).then_some(item)
    }

    pub(crate) fn explicit_injections(
        &self,
        text: &str,
        structured: &[SkillSelection],
    ) -> SkillInjectionOutcome {
        let mut selected = Vec::new();
        let mut seen_paths = HashSet::new();
        let mut blocked_names = HashSet::new();
        let mut warnings = Vec::new();

        for selection in structured {
            blocked_names.insert(selection.name.as_str());
            let Some(skill) = self
                .skills
                .iter()
                .find(|skill| skill.name == selection.name && skill.matches_path(&selection.path))
            else {
                warnings.push(bounded_warning(format!(
                    "Selected skill `${}` is no longer available at {}",
                    selection.name,
                    selection.path.display()
                )));
                continue;
            };
            if !skill.enabled {
                warnings.push(bounded_warning(format!(
                    "Selected skill `${}` is disabled in {}",
                    selection.name,
                    settings_file_display()
                )));
                continue;
            }
            if seen_paths.insert(skill.path.clone()) {
                selected.push(skill);
            }
        }

        let mentions = extract_mentions(text);
        let linked_paths = mentions
            .paths
            .iter()
            .map(|path| Path::new(path.strip_prefix("skill://").unwrap_or(path)))
            .collect::<HashSet<_>>();
        for skill in self.skills.iter().filter(|skill| skill.enabled) {
            if linked_paths.iter().any(|path| skill.matches_path(path))
                && seen_paths.insert(skill.path.clone())
            {
                selected.push(skill);
            }
        }

        let mut counts = HashMap::<&str, usize>::new();
        for skill in self.skills.iter().filter(|skill| skill.enabled) {
            *counts.entry(skill.name.as_str()).or_default() += 1;
        }
        for skill in self.skills.iter().filter(|skill| skill.enabled) {
            if seen_paths.contains(&skill.path)
                || blocked_names.contains(skill.name.as_str())
                || !mentions.plain_names.contains(skill.name.as_str())
                || counts.get(skill.name.as_str()) != Some(&1)
            {
                continue;
            }
            seen_paths.insert(skill.path.clone());
            selected.push(skill);
        }

        let mut items = Vec::with_capacity(selected.len());
        for skill in selected {
            match read_skill_prompt(&skill.path) {
                Ok((contents, truncated)) => {
                    let path = escape_xml_text(&skill.path.to_string_lossy());
                    let truncation_notice = truncated.then(|| {
                        format!(
                            "\n<skill_truncated>The injected copy reached bettercodex's {MAX_SKILL_PROMPT_BYTES}-byte item limit. Read the complete SKILL.md at {path} before acting.</skill_truncated>"
                        )
                    });
                    if truncated {
                        warnings.push(bounded_warning(format!(
                            "Skill `{}` exceeded the {}-byte prompt limit and was truncated",
                            skill.name, MAX_SKILL_PROMPT_BYTES
                        )));
                    }
                    let name = escape_xml_text(&skill.name);
                    let description = escape_xml_text(
                        &skill
                            .description
                            .chars()
                            .take(MAX_DESCRIPTION_CHARS)
                            .collect::<String>(),
                    );
                    let prompt = format!(
                        "<skill_context>\n<name>{name}</name>\n<description>{description}</description>\n<path>{path}</path>\n<instructions><![CDATA[\n{}\n]]></instructions>{}\n</skill_context>",
                        escape_cdata(&contents),
                        truncation_notice.as_deref().unwrap_or_default(),
                    );
                    items.push(json!({
                        "type": "message",
                        "role": "user",
                        "content": [{"type": "input_text", "text": prompt}],
                    }));
                }
                Err(error) => warnings.push(bounded_warning(format!(
                    "Failed to load selected skill `{}` at {}: {error:#}",
                    skill.name,
                    skill.path.display()
                ))),
            }
        }

        SkillInjectionOutcome { items, warnings }
    }

    fn apply_settings(&mut self, path: &Path) {
        let settings = match skill_settings::read(path) {
            Ok(settings) => settings,
            Err(error) => {
                if self.warnings.len() < MAX_WARNINGS {
                    self.warnings.push(bounded_warning(format!(
                        "Could not load skill settings {}: {error:#}",
                        path.display()
                    )));
                }
                return;
            }
        };
        for skill in &mut self.skills {
            if is_reserved_system_skill_name(&skill.name) {
                skill.enabled = true;
                continue;
            }
            let Some(settings) = settings.skills.get(&skill.path) else {
                continue;
            };
            if let Some(enabled) = settings.enabled {
                skill.enabled = enabled;
            }
            if let Some(allow) = settings.allow_implicit_invocation {
                skill.allow_implicit_invocation = allow;
            }
        }
    }
}

pub(crate) fn save_skill_update(path: &Path, update: SkillUpdate) -> Result<()> {
    let home = paths::bettercodex_home().ok_or_else(|| {
        anyhow!("cannot save skill settings because no bettercodex home is available")
    })?;
    skill_settings::save(&home.join(skill_settings::FILE_NAME), path, update)
}

fn settings_file_display() -> String {
    paths::bettercodex_home().map_or_else(
        || "BCODEX_HOME/skills.json".to_string(),
        |home| home.join(skill_settings::FILE_NAME).display().to_string(),
    )
}

pub(crate) fn is_mention_name_byte(byte: u8) -> bool {
    matches!(byte, b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'_' | b'-' | b':')
}

pub(crate) fn explicitly_invokes_review(text: &str) -> bool {
    let mentions = extract_mentions(text);
    mentions.plain_names.contains("review") || mentions.linked_review
}

fn discovery_roots_with_home(
    cwd: &Path,
    bettercodex_home: Option<&Path>,
) -> Vec<(PathBuf, SkillScope)> {
    let project_root = repository::find_root(cwd).unwrap_or_else(|| cwd.to_path_buf());
    let mut directories = Vec::new();
    let mut directory = cwd;
    loop {
        directories.push(directory.to_path_buf());
        if directory == project_root {
            break;
        }
        let Some(parent) = directory.parent() else {
            break;
        };
        directory = parent;
    }
    directories.reverse();

    let mut roots = directories
        .into_iter()
        .map(|directory| {
            (
                directory.join(PROJECT_DIRECTORY).join(SKILLS_DIRECTORY),
                SkillScope::Repository,
            )
        })
        .collect::<Vec<_>>();
    if let Some(home) = bettercodex_home {
        roots.push((home.join(SKILLS_DIRECTORY), SkillScope::User));
        roots.push((system_skills::root(home), SkillScope::System));
    }
    roots
}

fn discover_skill_files(
    root: &Path,
    scope: SkillScope,
    warnings: &mut WarningCollector,
) -> Vec<PathBuf> {
    if !root.is_dir() {
        return Vec::new();
    }
    let mut files = Vec::new();
    let mut stack = vec![(root.to_path_buf(), 0_usize)];
    let mut visited = HashSet::new();
    let mut directories = 0_usize;
    let mut entries_seen = 0_usize;

    while let Some((directory, depth)) = stack.pop() {
        let canonical = directory
            .canonicalize()
            .unwrap_or_else(|_| directory.clone());
        if !visited.insert(canonical) {
            continue;
        }
        directories = directories.saturating_add(1);
        if directories > MAX_SCAN_DIRECTORIES {
            warnings.push(format!(
                "Skill scan reached its {}-directory limit under {}",
                MAX_SCAN_DIRECTORIES,
                root.display()
            ));
            break;
        }

        let read_dir = match std::fs::read_dir(&directory) {
            Ok(read_dir) => read_dir,
            Err(error) => {
                warnings.push(format!(
                    "Could not scan skill directory {}: {error}",
                    directory.display()
                ));
                continue;
            }
        };
        let mut entries = Vec::new();
        for entry in read_dir {
            entries_seen = entries_seen.saturating_add(1);
            if entries_seen > MAX_SCAN_ENTRIES {
                warnings.push(format!(
                    "Skill scan reached its {}-entry limit under {}",
                    MAX_SCAN_ENTRIES,
                    root.display()
                ));
                return files;
            }
            match entry {
                Ok(entry) => entries.push(entry),
                Err(error) => warnings.push(format!(
                    "Could not inspect an entry under {}: {error}",
                    directory.display()
                )),
            }
        }
        entries.sort_by_key(std::fs::DirEntry::file_name);

        let mut child_directories = Vec::new();
        for entry in entries {
            let path = entry.path();
            let file_type = match entry.file_type() {
                Ok(file_type) => file_type,
                Err(error) => {
                    warnings.push(format!("Could not inspect {}: {error}", path.display()));
                    continue;
                }
            };
            if file_type.is_file() && entry.file_name() == SKILL_FILE_NAME {
                files.push(path);
                continue;
            }

            let is_directory = if file_type.is_dir() {
                true
            } else if file_type.is_symlink() && scope != SkillScope::System {
                match std::fs::metadata(&path) {
                    Ok(metadata) => metadata.is_dir(),
                    Err(error) => {
                        warnings.push(format!("Could not inspect {}: {error}", path.display()));
                        false
                    }
                }
            } else {
                false
            };
            if is_directory
                && depth < MAX_SCAN_DEPTH
                && !entry.file_name().to_string_lossy().starts_with('.')
            {
                child_directories.push(path);
            }
        }
        child_directories.reverse();
        stack.extend(
            child_directories
                .into_iter()
                .map(|path| (path, depth.saturating_add(1))),
        );
    }
    files
}

fn load_skill(
    path: &Path,
    discovery_path: PathBuf,
    scope: SkillScope,
) -> Result<(Skill, Option<String>)> {
    let frontmatter = read_frontmatter(path)?;
    let parsed: SkillFrontmatter = parse_yaml_with_scalar_repair(&frontmatter)
        .with_context(|| format!("invalid YAML frontmatter in {}", path.display()))?;
    let fallback_name = path
        .parent()
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
        .map(sanitize_single_line)
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "skill".to_string());
    let name = parsed
        .name
        .as_deref()
        .map(sanitize_single_line)
        .filter(|name| !name.is_empty())
        .unwrap_or(fallback_name);
    if name.chars().count() > MAX_NAME_CHARS {
        return Err(anyhow!(
            "name exceeds the maximum length of {MAX_NAME_CHARS} characters"
        ));
    }
    let description = parsed
        .description
        .as_deref()
        .map(sanitize_single_line)
        .filter(|description| !description.is_empty())
        .ok_or_else(|| anyhow!("missing field `description`"))?;
    let frontmatter_short = parsed
        .metadata
        .short_description
        .as_deref()
        .map(sanitize_single_line)
        .filter(|description| !description.is_empty());

    let (metadata, metadata_warning) = load_optional_metadata(path);
    let interface = metadata.interface.unwrap_or_default();
    let display_name = bounded_optional(interface.display_name, MAX_NAME_CHARS);
    let short_description = bounded_optional(interface.short_description, MAX_DESCRIPTION_CHARS)
        .or_else(|| bounded_optional(frontmatter_short, MAX_DESCRIPTION_CHARS));
    let allow_implicit_invocation = metadata
        .policy
        .and_then(|policy| policy.allow_implicit_invocation)
        .unwrap_or(true);
    let enabled = scope != SkillScope::System || name.as_str() != PAPERCUT_SYSTEM_SKILL_NAME;

    Ok((
        Skill {
            name,
            description,
            short_description,
            display_name,
            path: path.to_path_buf(),
            discovery_path,
            scope,
            enabled,
            allow_implicit_invocation,
        },
        metadata_warning,
    ))
}

fn read_frontmatter(path: &Path) -> Result<String> {
    let file = File::open(path).with_context(|| format!("failed to read {}", path.display()))?;
    let mut reader = BufReader::new(file);
    let mut line = String::new();
    let mut bytes = 0_usize;
    let first_bytes = read_frontmatter_line(&mut reader, &mut line, &mut bytes)?;
    if first_bytes == 0 || line.trim() != "---" {
        return Err(anyhow!("missing YAML frontmatter delimited by ---"));
    }

    let mut lines = Vec::new();
    loop {
        let read = read_frontmatter_line(&mut reader, &mut line, &mut bytes)?;
        if read == 0 {
            return Err(anyhow!("missing closing YAML frontmatter delimiter ---"));
        }
        if line.trim() == "---" {
            break;
        }
        lines.push(line.trim_end_matches(['\r', '\n']).to_string());
    }
    if lines.is_empty() {
        return Err(anyhow!("empty YAML frontmatter"));
    }
    Ok(lines.join("\n"))
}

fn read_frontmatter_line(
    reader: &mut BufReader<File>,
    line: &mut String,
    bytes: &mut usize,
) -> Result<usize> {
    line.clear();
    let remaining = MAX_METADATA_BYTES.saturating_sub(*bytes);
    let read = std::io::Read::by_ref(reader)
        .take(remaining.saturating_add(1) as u64)
        .read_line(line)?;
    *bytes = bytes.saturating_add(read);
    if *bytes > MAX_METADATA_BYTES {
        return Err(anyhow!(
            "YAML frontmatter exceeds the {MAX_METADATA_BYTES}-byte limit"
        ));
    }
    Ok(read)
}

fn parse_yaml_with_scalar_repair(frontmatter: &str) -> Result<SkillFrontmatter, serde_yaml::Error> {
    match serde_yaml::from_str(frontmatter) {
        Ok(parsed) => Ok(parsed),
        Err(original) => match repair_frontmatter_scalar_fields(frontmatter) {
            Some(repaired) => serde_yaml::from_str(&repaired).map_err(|_| original),
            None => Err(original),
        },
    }
}

fn repair_frontmatter_scalar_fields(frontmatter: &str) -> Option<String> {
    let mut changed = false;
    let mut block_scalar_indent = None;
    let mut repaired = Vec::new();
    for line in frontmatter.lines() {
        let indent = line
            .chars()
            .take_while(|character| *character == ' ')
            .count();
        if let Some(block_indent) = block_scalar_indent {
            if line.trim().is_empty() || indent > block_indent {
                repaired.push(line.to_string());
                continue;
            }
            block_scalar_indent = None;
        }

        let Some((key, value)) = line.split_once(':') else {
            repaired.push(line.to_string());
            continue;
        };
        if key.trim().is_empty() || !value.chars().next().is_none_or(char::is_whitespace) {
            repaired.push(line.to_string());
            continue;
        }

        let trimmed_start = value.trim_start();
        let leading_whitespace = &value[..value.len() - trimmed_start.len()];
        let mut scalar = trimmed_start;
        let mut comment = "";
        for (index, character) in trimmed_start.char_indices() {
            if character == '#'
                && (index == 0
                    || trimmed_start[..index]
                        .chars()
                        .next_back()
                        .is_some_and(char::is_whitespace))
            {
                let comment_start = trimmed_start[..index].trim_end().len();
                scalar = &trimmed_start[..comment_start];
                comment = &trimmed_start[comment_start..];
                break;
            }
        }

        let scalar = scalar.trim_end();
        let Some(first) = scalar.chars().next() else {
            repaired.push(line.to_string());
            continue;
        };
        if matches!(first, '|' | '>') {
            block_scalar_indent = Some(indent);
            repaired.push(line.to_string());
            continue;
        }
        if matches!(first, '\'' | '"') {
            repaired.push(line.to_string());
            continue;
        }
        let has_colon_separator = scalar.char_indices().any(|(index, character)| {
            character == ':'
                && scalar[index + character.len_utf8()..]
                    .chars()
                    .next()
                    .is_some_and(char::is_whitespace)
        });
        let invalid_flow_scalar = matches!(first, '[' | '{' | '@' | '`')
            && serde_yaml::from_str::<serde_yaml::Value>(scalar).is_err();
        if !has_colon_separator && !invalid_flow_scalar {
            repaired.push(line.to_string());
            continue;
        }

        repaired.push(format!(
            "{key}:{leading_whitespace}'{}'{comment}",
            scalar.replace('\'', "''")
        ));
        changed = true;
    }
    changed.then(|| repaired.join("\n"))
}

fn load_optional_metadata(path: &Path) -> (SkillMetadataFile, Option<String>) {
    let Some(skill_directory) = path.parent() else {
        return (SkillMetadataFile::default(), None);
    };
    let metadata_path = skill_directory.join("agents").join("openai.yaml");
    if !metadata_path.is_file() {
        return (SkillMetadataFile::default(), None);
    }
    let result = (|| -> Result<SkillMetadataFile> {
        let file = File::open(&metadata_path)?;
        let mut bytes = Vec::new();
        file.take((MAX_METADATA_BYTES + 1) as u64)
            .read_to_end(&mut bytes)?;
        if bytes.len() > MAX_METADATA_BYTES {
            return Err(anyhow!(
                "metadata exceeds the {MAX_METADATA_BYTES}-byte limit"
            ));
        }
        Ok(serde_yaml::from_slice(&bytes)?)
    })();
    match result {
        Ok(metadata) => (metadata, None),
        Err(error) => (
            SkillMetadataFile::default(),
            Some(format!(
                "Ignoring optional skill metadata {}: {error:#}",
                metadata_path.display()
            )),
        ),
    }
}

fn read_skill_prompt(path: &Path) -> Result<(String, bool)> {
    let file = File::open(path).with_context(|| format!("failed to read {}", path.display()))?;
    let mut bytes = Vec::new();
    file.take((MAX_SKILL_PROMPT_BYTES + 1) as u64)
        .read_to_end(&mut bytes)?;
    let truncated = bytes.len() > MAX_SKILL_PROMPT_BYTES;
    bytes.truncate(MAX_SKILL_PROMPT_BYTES);
    match std::str::from_utf8(&bytes) {
        Ok(contents) => Ok((contents.to_string(), truncated)),
        Err(error) if error.error_len().is_none() => {
            bytes.truncate(error.valid_up_to());
            Ok((
                String::from_utf8(bytes)
                    .context("failed to retain the valid UTF-8 skill prefix")?,
                true,
            ))
        }
        Err(error) => Err(anyhow!("skill is not valid UTF-8: {error}")),
    }
}

#[derive(Clone, Copy, Default)]
struct CatalogCost {
    text_bytes: usize,
    serialized_bytes: usize,
}

impl CatalogCost {
    fn fits(self, text_budget: usize, serialized_budget: usize) -> bool {
        self.text_bytes <= text_budget && self.serialized_bytes <= serialized_budget
    }

    fn add_line(&mut self, line: &str, after_existing_line: bool) {
        self.text_bytes = self.text_bytes.saturating_add(line.len() + 1);
        if after_existing_line {
            // A newline in a JSON string is serialized as the two bytes `\\n`.
            self.serialized_bytes = self.serialized_bytes.saturating_add(2);
        }
        self.serialized_bytes = self
            .serialized_bytes
            .saturating_add(json_string_content_bytes(line));
    }

    fn remove_last_line(&mut self, line: &str, has_preceding_line: bool) {
        self.text_bytes = self.text_bytes.saturating_sub(line.len() + 1);
        self.serialized_bytes = self
            .serialized_bytes
            .saturating_sub(json_string_content_bytes(line));
        if has_preceding_line {
            self.serialized_bytes = self.serialized_bytes.saturating_sub(2);
        }
    }
}

fn render_catalog_lines(
    skills: &[&Skill],
    text_budget: usize,
    serialized_budget: usize,
) -> (Vec<String>, usize) {
    let minimum = skills
        .iter()
        .map(|skill| catalog_line(skill, ""))
        .collect::<Vec<_>>();
    let minimum_cost = catalog_lines_cost(&minimum);
    if !minimum_cost.fits(text_budget, serialized_budget) {
        let mut included = Vec::new();
        let mut cost = CatalogCost::default();
        for line in minimum {
            let mut candidate = cost;
            candidate.add_line(&line, !included.is_empty());
            if !candidate.fits(text_budget, serialized_budget) {
                break;
            }
            cost = candidate;
            included.push(line);
        }
        let omitted = skills.len().saturating_sub(included.len());
        return (included, omitted);
    }

    let full_descriptions = skills
        .iter()
        .map(|skill| {
            escape_xml_text(
                &skill
                    .description
                    .chars()
                    .take(MAX_DESCRIPTION_CHARS)
                    .collect::<String>(),
            )
            .chars()
            .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    let full = skills
        .iter()
        .zip(&full_descriptions)
        .map(|(skill, description)| catalog_line(skill, &description.iter().collect::<String>()))
        .collect::<Vec<_>>();
    if catalog_lines_cost(&full).fits(text_budget, serialized_budget) {
        return (full, 0);
    }

    // Distribute the remaining budget one character per skill at a time. Track both decoded UTF-8
    // and JSON-escaped costs so quote-, backslash-, or control-heavy metadata cannot exceed the
    // hard serialized item ceiling.
    let mut used = minimum_cost;
    let mut descriptions = vec![String::new(); skills.len()];
    let mut next_char = vec![0_usize; skills.len()];
    let mut pending = full_descriptions
        .iter()
        .enumerate()
        .filter_map(|(index, description)| (!description.is_empty()).then_some(index))
        .collect::<VecDeque<_>>();
    while let Some(index) = pending.pop_front() {
        let character = full_descriptions[index][next_char[index]];
        let separator_cost = usize::from(next_char[index] == 0);
        let candidate = CatalogCost {
            text_bytes: used
                .text_bytes
                .saturating_add(character.len_utf8())
                .saturating_add(separator_cost),
            serialized_bytes: used
                .serialized_bytes
                .saturating_add(json_escaped_char_bytes(character))
                .saturating_add(separator_cost),
        };
        if !candidate.fits(text_budget, serialized_budget) {
            continue;
        }
        descriptions[index].push(character);
        next_char[index] += 1;
        used = candidate;
        if next_char[index] < full_descriptions[index].len() {
            pending.push_back(index);
        }
    }
    (
        skills
            .iter()
            .zip(descriptions)
            .map(|(skill, description)| catalog_line(skill, &description))
            .collect(),
        0,
    )
}

fn catalog_line(skill: &Skill, description: &str) -> String {
    let path = truncate_bytes(
        &escape_xml_text(&skill.discovery_path.to_string_lossy().replace('\\', "/")),
        1_024,
    );
    let name = truncate_bytes(&escape_xml_text(&skill.name), 256);
    if description.is_empty() {
        format!("- {name}: (file: {path})")
    } else {
        format!("- {name}: {description} (file: {path})")
    }
}

fn catalog_lines_cost(lines: &[String]) -> CatalogCost {
    let mut cost = CatalogCost::default();
    for (index, line) in lines.iter().enumerate() {
        cost.add_line(line, index > 0);
    }
    cost
}

fn json_string_content_bytes(value: &str) -> usize {
    value.chars().fold(0_usize, |total, character| {
        total.saturating_add(json_escaped_char_bytes(character))
    })
}

fn json_escaped_char_bytes(character: char) -> usize {
    match character {
        '"' | '\\' | '\u{0008}' | '\u{000C}' | '\n' | '\r' | '\t' => 2,
        '\u{0000}'..='\u{001F}' => 6,
        _ => character.len_utf8(),
    }
}

fn catalogue_item(body: String) -> Value {
    json!({
        "type": "message",
        "role": "user",
        "content": [{"type": "input_text", "text": body}],
    })
}

fn serialized_value_bytes(value: &Value) -> usize {
    serde_json::to_vec(value).map_or(usize::MAX, |bytes| bytes.len())
}

fn truncate_bytes(value: &str, limit: usize) -> String {
    if value.len() <= limit {
        return value.to_string();
    }
    let mut end = limit;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_string()
}

fn bounded_optional(value: Option<String>, max_chars: usize) -> Option<String> {
    value
        .as_deref()
        .map(sanitize_single_line)
        .filter(|value| !value.is_empty() && value.chars().count() <= max_chars)
}

fn sanitize_single_line(raw: &str) -> String {
    raw.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn bounded_warning(warning: String) -> String {
    let mut characters = warning.chars();
    let bounded = characters.by_ref().take(512).collect::<String>();
    if characters.next().is_some() {
        format!("{bounded}…")
    } else {
        bounded
    }
}

struct Mentions<'a> {
    plain_names: HashSet<&'a str>,
    linked_review: bool,
    paths: HashSet<&'a str>,
}

fn extract_mentions(text: &str) -> Mentions<'_> {
    let bytes = text.as_bytes();
    let mut plain_names = HashSet::new();
    let mut linked_review = false;
    let mut paths = HashSet::new();
    let trimmed = text.trim_start();
    if let Some(rest) = trimmed.strip_prefix("/review")
        && rest
            .as_bytes()
            .first()
            .is_none_or(|byte| !is_mention_name_byte(*byte))
    {
        plain_names.insert("review");
    }
    let mut index = 0_usize;
    while index < bytes.len() {
        if bytes[index] == b'['
            && let Some((name, path, end)) = parse_linked_mention(text, index)
        {
            if !is_common_environment_variable(name) {
                linked_review |= name == "review";
                paths.insert(path);
            }
            index = end;
            continue;
        }
        if bytes[index] != b'$' {
            index += 1;
            continue;
        }
        let start = index + 1;
        let mut end = start;
        while bytes
            .get(end)
            .is_some_and(|byte| is_mention_name_byte(*byte))
        {
            end += 1;
        }
        if end > start {
            let name = &text[start..end];
            if !is_common_environment_variable(name) {
                plain_names.insert(name);
            }
        }
        index = end.max(index + 1);
    }
    Mentions {
        plain_names,
        linked_review,
        paths,
    }
}

fn parse_linked_mention(text: &str, start: usize) -> Option<(&str, &str, usize)> {
    let bytes = text.as_bytes();
    if bytes.get(start + 1) != Some(&b'$') {
        return None;
    }
    let name_start = start + 2;
    let mut name_end = name_start;
    while bytes
        .get(name_end)
        .is_some_and(|byte| is_mention_name_byte(*byte))
    {
        name_end += 1;
    }
    if name_end == name_start || bytes.get(name_end) != Some(&b']') {
        return None;
    }
    let mut path_start = name_end + 1;
    while bytes.get(path_start).is_some_and(u8::is_ascii_whitespace) {
        path_start += 1;
    }
    if bytes.get(path_start) != Some(&b'(') {
        return None;
    }
    let path_content_start = path_start + 1;
    let path_end = text[path_content_start..].find(')')? + path_content_start;
    let path = text[path_content_start..path_end].trim();
    if path.is_empty() {
        return None;
    }
    Some((&text[name_start..name_end], path, path_end + 1))
}

pub(crate) fn is_common_environment_variable(name: &str) -> bool {
    const COMMON_NAMES: [&str; 11] = [
        "PATH",
        "HOME",
        "USER",
        "SHELL",
        "PWD",
        "TMPDIR",
        "TEMP",
        "TMP",
        "LANG",
        "TERM",
        "XDG_CONFIG_HOME",
    ];

    COMMON_NAMES
        .iter()
        .any(|common| name.eq_ignore_ascii_case(common))
}

#[cfg(test)]
#[path = "skills_review_tests.rs"]
mod review_tests;
