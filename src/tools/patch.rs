//! Local-filesystem port of Codex's `codex-apply-patch` parser and applier at
//! `1669c2403f793d0230065397dfc25f52b844244e`.
//!
//! The upstream crate is coupled to Codex's remote filesystem and sandbox
//! abstractions. bettercodex keeps the parser, fuzzy matching, sequential
//! semantics, and output contract while using `std::fs` with the invoking
//! user's permissions. Every operation is prepared against an in-memory view
//! before the first filesystem mutation, so a late invalid hunk cannot leave
//! the earlier half of a patch applied.

use anyhow::Context;
use anyhow::Result;
use anyhow::anyhow;
use std::borrow::Cow;
use std::collections::HashMap;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

const CANCELLATION_CHECK_INTERVAL: usize = 1_024;

pub(super) fn apply(root: &Path, input: &str, cancellation: &CancellationToken) -> Result<String> {
    ensure_not_cancelled(cancellation)?;
    let root = root
        .canonicalize()
        .with_context(|| format!("failed to resolve working directory {}", root.display()))?;
    let operations = parse(input, cancellation)?;
    if operations.is_empty() {
        return Err(anyhow!("No files were modified."));
    }
    let prepared = PreparedPatch::build(&root, operations, cancellation)?;
    // Cancellation is honored throughout parsing and preparation. Once the
    // first mutation starts, finish the short commit without interruption so
    // cancellation cannot itself strand a partially applied patch.
    ensure_not_cancelled(cancellation)?;
    prepared.commit()
}

struct PreparedPatch {
    mutations: Vec<Mutation>,
    added: Vec<PathBuf>,
    modified: Vec<PathBuf>,
    deleted: Vec<PathBuf>,
}

impl PreparedPatch {
    fn build(
        root: &Path,
        operations: Vec<Operation>,
        cancellation: &CancellationToken,
    ) -> Result<Self> {
        let mut files = VirtualFiles::default();
        let mut mutations = Vec::new();
        let mut added = Vec::new();
        let mut modified = Vec::new();
        let mut deleted = Vec::new();

        for operation in operations {
            ensure_not_cancelled(cancellation)?;
            match operation {
                Operation::Add { path, content } => {
                    let target = resolve(root, &path)?;
                    let content = Arc::<str>::from(content);
                    files.write(target.clone(), Arc::clone(&content));
                    mutations.push(Mutation::Write {
                        path: target,
                        content,
                    });
                    added.push(path);
                }
                Operation::Delete { path } => {
                    let target = resolve(root, &path)?;
                    files.delete(&target)?;
                    mutations.push(Mutation::Delete {
                        path: target,
                        error_context: DeleteErrorContext::DeleteFile,
                    });
                    deleted.push(path);
                }
                Operation::Update {
                    path,
                    move_to,
                    chunks,
                } => {
                    let source = resolve(root, &path)?;
                    let content = files.read_to_string(&source)?;
                    ensure_not_cancelled(cancellation)?;
                    let content =
                        Arc::<str>::from(apply_chunks(&content, &chunks, &path, cancellation)?);
                    ensure_not_cancelled(cancellation)?;

                    if let Some(destination_path) = move_to {
                        let destination = resolve(root, &destination_path)?;
                        if destination == source {
                            files.write(source.clone(), Arc::clone(&content));
                            mutations.push(Mutation::Write {
                                path: source,
                                content,
                            });
                            modified.push(path);
                        } else {
                            files.write(destination.clone(), Arc::clone(&content));
                            files.delete(&source)?;
                            mutations.push(Mutation::Write {
                                path: destination,
                                content,
                            });
                            mutations.push(Mutation::Delete {
                                path: source,
                                error_context: DeleteErrorContext::RemoveOriginal,
                            });
                            modified.push(destination_path);
                        }
                    } else {
                        files.write(source.clone(), Arc::clone(&content));
                        mutations.push(Mutation::Write {
                            path: source,
                            content,
                        });
                        modified.push(path);
                    }
                }
            }
        }

        Ok(Self {
            mutations,
            added,
            modified,
            deleted,
        })
    }

    fn commit(self) -> Result<String> {
        for mutation in self.mutations {
            match mutation {
                Mutation::Write { path, content } => write_file(&path, &content)?,
                Mutation::Delete {
                    path,
                    error_context,
                } => {
                    std::fs::remove_file(&path).with_context(|| match error_context {
                        DeleteErrorContext::DeleteFile => {
                            format!("Failed to delete file {}", path.display())
                        }
                        DeleteErrorContext::RemoveOriginal => {
                            format!("Failed to remove original {}", path.display())
                        }
                    })?;
                }
            }
        }

        let mut summary = String::from("Success. Updated the following files:\n");
        for path in self.added {
            summary.push_str(&format!("A {}\n", path.display()));
        }
        for path in self.modified {
            summary.push_str(&format!("M {}\n", path.display()));
        }
        for path in self.deleted {
            summary.push_str(&format!("D {}\n", path.display()));
        }
        Ok(summary)
    }
}

enum Mutation {
    Write {
        path: PathBuf,
        content: Arc<str>,
    },
    Delete {
        path: PathBuf,
        error_context: DeleteErrorContext,
    },
}

enum DeleteErrorContext {
    DeleteFile,
    RemoveOriginal,
}

#[derive(Default)]
struct VirtualFiles {
    files: HashMap<PathBuf, VirtualFile>,
}

impl VirtualFiles {
    fn read_to_string(&mut self, path: &Path) -> Result<Arc<str>> {
        if let Some(file) = self.files.get(path) {
            return match file {
                VirtualFile::Text(content) => Ok(Arc::clone(content)),
                VirtualFile::Missing => Err(missing_file_error())
                    .with_context(|| format!("Failed to read file to update {}", path.display())),
            };
        }

        let content = Arc::<str>::from(
            std::fs::read_to_string(path)
                .with_context(|| format!("Failed to read file to update {}", path.display()))?,
        );
        self.files
            .insert(path.to_path_buf(), VirtualFile::Text(Arc::clone(&content)));
        Ok(content)
    }

    fn write(&mut self, path: PathBuf, content: Arc<str>) {
        self.files.insert(path, VirtualFile::Text(content));
    }

    fn delete(&mut self, path: &Path) -> Result<()> {
        match self.files.get(path) {
            Some(VirtualFile::Text(_)) => {}
            Some(VirtualFile::Missing) => {
                return Err(missing_file_error())
                    .with_context(|| format!("Failed to delete file {}", path.display()));
            }
            None => {
                let metadata = std::fs::symlink_metadata(path)
                    .with_context(|| format!("Failed to delete file {}", path.display()))?;
                if metadata.file_type().is_dir() {
                    return Err(std::io::Error::from_raw_os_error(libc::EISDIR))
                        .with_context(|| format!("Failed to delete file {}", path.display()));
                }
            }
        }
        self.files.insert(path.to_path_buf(), VirtualFile::Missing);
        Ok(())
    }
}

enum VirtualFile {
    Text(Arc<str>),
    Missing,
}

fn missing_file_error() -> std::io::Error {
    std::io::Error::from_raw_os_error(libc::ENOENT)
}

fn ensure_not_cancelled(cancellation: &CancellationToken) -> Result<()> {
    if cancellation.is_cancelled() {
        Err(anyhow!("apply_patch was interrupted"))
    } else {
        Ok(())
    }
}

fn write_file(path: &Path, content: &str) -> Result<()> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent).with_context(|| {
            format!("Failed to create parent directories for {}", path.display())
        })?;
    }
    std::fs::write(path, content)
        .with_context(|| format!("Failed to write file {}", path.display()))
}

#[derive(Debug)]
enum Operation {
    Add {
        path: PathBuf,
        content: String,
    },
    Delete {
        path: PathBuf,
    },
    Update {
        path: PathBuf,
        move_to: Option<PathBuf>,
        chunks: Vec<Chunk>,
    },
}

#[derive(Debug)]
struct Chunk {
    anchor: Option<String>,
    lines: Vec<ChangeLine>,
    end_of_file: bool,
}

#[derive(Debug)]
enum ChangeLine {
    Context(String),
    Remove(String),
    Add(String),
}

fn parse(input: &str, cancellation: &CancellationToken) -> Result<Vec<Operation>> {
    ensure_not_cancelled(cancellation)?;
    let normalized = if input.contains("\r\n") {
        Cow::Owned(input.replace("\r\n", "\n"))
    } else {
        Cow::Borrowed(input)
    };
    let mut lines = normalized.trim().split('\n').collect::<Vec<_>>();
    if matches!(
        lines.first().map(|line| line.trim()),
        Some("<<EOF" | "<<'EOF'" | "<<\"EOF\"")
    ) && lines
        .last()
        .is_some_and(|line| line.trim_end().ends_with("EOF"))
        && lines.len() >= 4
    {
        lines = lines[1..lines.len() - 1].to_vec();
    }
    if lines.first().map(|line| line.trim()) != Some("*** Begin Patch") {
        return Err(anyhow!("patch must begin with `*** Begin Patch`"));
    }

    let mut index = 1;
    let mut operations = Vec::new();
    while index < lines.len() && lines[index].trim() != "*** End Patch" {
        ensure_not_cancelled(cancellation)?;
        let line = lines[index].trim();
        index += 1;
        if let Some(raw_path) = line.strip_prefix("*** Add File: ") {
            let path = patch_path(raw_path)?;
            let mut added = Vec::new();
            while index < lines.len() && !is_boundary(lines[index]) {
                if index.is_multiple_of(CANCELLATION_CHECK_INTERVAL) {
                    ensure_not_cancelled(cancellation)?;
                }
                let content = lines[index]
                    .strip_prefix('+')
                    .ok_or_else(|| anyhow!("added file lines must begin with `+`"))?;
                added.push(content);
                index += 1;
            }
            let mut content = added.join("\n");
            if !added.is_empty() {
                content.push('\n');
            }
            operations.push(Operation::Add { path, content });
        } else if let Some(raw_path) = line.strip_prefix("*** Delete File: ") {
            operations.push(Operation::Delete {
                path: patch_path(raw_path)?,
            });
        } else if let Some(raw_path) = line.strip_prefix("*** Update File: ") {
            let path = patch_path(raw_path)?;
            let move_to = lines
                .get(index)
                .map(|line| line.trim())
                .and_then(|line| line.strip_prefix("*** Move to: "))
                .map(patch_path)
                .transpose()?;
            if move_to.is_some() {
                index += 1;
            }
            let chunks = parse_chunks(&lines, &mut index, cancellation)?;
            if chunks.is_empty() {
                return Err(anyhow!("update operation for {} is empty", path.display()));
            }
            operations.push(Operation::Update {
                path,
                move_to,
                chunks,
            });
        } else {
            return Err(anyhow!("invalid patch operation `{line}`"));
        }
    }

    if lines.get(index).map(|line| line.trim()) != Some("*** End Patch") {
        return Err(anyhow!("patch must end with `*** End Patch`"));
    }
    index += 1;
    if lines[index..].iter().any(|line| !line.trim().is_empty()) {
        return Err(anyhow!("unexpected content after `*** End Patch`"));
    }
    Ok(operations)
}

fn parse_chunks(
    lines: &[&str],
    index: &mut usize,
    cancellation: &CancellationToken,
) -> Result<Vec<Chunk>> {
    let mut chunks = Vec::new();
    let mut current: Option<Chunk> = None;
    while *index < lines.len() && !is_boundary(lines[*index]) {
        if (*index).is_multiple_of(CANCELLATION_CHECK_INTERVAL) {
            ensure_not_cancelled(cancellation)?;
        }
        let line = lines[*index];
        *index += 1;
        let update_line = line.trim_end();
        if current.as_ref().is_some_and(|chunk| chunk.end_of_file) {
            if update_line.is_empty() {
                continue;
            }
            if update_line != "@@" && !update_line.starts_with("@@ ") {
                return Err(anyhow!(
                    "expected update hunk to start with a @@ context marker, got `{line}`"
                ));
            }
        }
        if update_line == "@@" || update_line.starts_with("@@ ") {
            if let Some(chunk) = current.take() {
                require_change_lines(&chunk)?;
                chunks.push(chunk);
            }
            current = Some(Chunk {
                anchor: update_line.strip_prefix("@@ ").map(str::to_string),
                lines: Vec::new(),
                end_of_file: false,
            });
            continue;
        }
        if update_line == "*** End of File" {
            let chunk = current
                .as_mut()
                .ok_or_else(|| anyhow!("`*** End of File` must follow a change"))?;
            chunk.end_of_file = true;
            continue;
        }

        let change = if line.is_empty() {
            ChangeLine::Context(String::new())
        } else {
            let (prefix, content) = line.split_at(1);
            match prefix {
                " " => ChangeLine::Context(content.to_string()),
                "+" => ChangeLine::Add(content.to_string()),
                "-" => ChangeLine::Remove(content.to_string()),
                _ => return Err(anyhow!("invalid change line `{line}`")),
            }
        };
        current
            .get_or_insert_with(|| Chunk {
                anchor: None,
                lines: Vec::new(),
                end_of_file: false,
            })
            .lines
            .push(change);
    }
    if let Some(chunk) = current {
        require_change_lines(&chunk)?;
        chunks.push(chunk);
    }
    Ok(chunks)
}

fn require_change_lines(chunk: &Chunk) -> Result<()> {
    if chunk.lines.is_empty() {
        return Err(anyhow!("update context contains no change lines"));
    }
    Ok(())
}

fn is_boundary(line: &str) -> bool {
    let line = line.trim();
    line == "*** End Patch"
        || line.starts_with("*** Add File: ")
        || line.starts_with("*** Delete File: ")
        || line.starts_with("*** Update File: ")
}

fn patch_path(raw: &str) -> Result<PathBuf> {
    if raw.is_empty() {
        return Err(anyhow!("patch path is empty"));
    }
    Ok(PathBuf::from(raw))
}

fn resolve(root: &Path, path: &Path) -> Result<PathBuf> {
    Ok(if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    })
}

struct Replacement<'a> {
    start: usize,
    old_len: usize,
    lines: Vec<&'a str>,
}

fn apply_chunks(
    content: &str,
    chunks: &[Chunk],
    path: &Path,
    cancellation: &CancellationToken,
) -> Result<String> {
    let mut original_lines = content.split('\n').collect::<Vec<_>>();
    if original_lines.last() == Some(&"") {
        original_lines.pop();
    }
    let mut replacements = Vec::with_capacity(chunks.len());
    let mut line_index = 0;
    for chunk in chunks {
        ensure_not_cancelled(cancellation)?;
        if let Some(anchor) = &chunk.anchor {
            line_index = seek_sequence(
                &original_lines,
                &[anchor.as_str()],
                line_index,
                false,
                cancellation,
            )?
            .map(|index| index + 1)
            .ok_or_else(|| anyhow!("Failed to find context '{anchor}' in {}", path.display()))?;
        }

        let old_lines = chunk
            .lines
            .iter()
            .filter_map(|line| match line {
                ChangeLine::Context(line) | ChangeLine::Remove(line) => Some(line.as_str()),
                ChangeLine::Add(_) => None,
            })
            .collect::<Vec<_>>();
        let new_lines = chunk
            .lines
            .iter()
            .filter_map(|line| match line {
                ChangeLine::Context(line) | ChangeLine::Add(line) => Some(line.as_str()),
                ChangeLine::Remove(_) => None,
            })
            .collect::<Vec<_>>();

        if old_lines.is_empty() {
            let insertion_index = if original_lines.last() == Some(&"") {
                original_lines.len() - 1
            } else {
                original_lines.len()
            };
            replacements.push(Replacement {
                start: insertion_index,
                old_len: 0,
                lines: new_lines,
            });
            continue;
        }

        let mut pattern = old_lines.as_slice();
        let mut replacement = new_lines.as_slice();
        let mut found = seek_sequence(
            &original_lines,
            pattern,
            line_index,
            chunk.end_of_file,
            cancellation,
        )?;
        if found.is_none() && pattern.last() == Some(&"") {
            pattern = &pattern[..pattern.len() - 1];
            if replacement.last() == Some(&"") {
                replacement = &replacement[..replacement.len() - 1];
            }
            found = seek_sequence(
                &original_lines,
                pattern,
                line_index,
                chunk.end_of_file,
                cancellation,
            )?;
        }

        let start = found.ok_or_else(|| {
            anyhow!(
                "Failed to find expected lines in {}:\n{}",
                path.display(),
                old_lines.join("\n"),
            )
        })?;
        replacements.push(Replacement {
            start,
            old_len: pattern.len(),
            lines: replacement.to_vec(),
        });
        line_index = start + pattern.len();
    }

    replacements.sort_by_key(|replacement| replacement.start);
    if replacements_overlap(&replacements) {
        return render_overlapping_replacements(
            content,
            &original_lines,
            &replacements,
            cancellation,
        );
    }
    render_replacements(content, &original_lines, &replacements, cancellation)
}

fn replacements_overlap(replacements: &[Replacement<'_>]) -> bool {
    let mut replaced_until = 0_usize;
    for replacement in replacements {
        if replacement.start < replaced_until {
            return true;
        }
        replaced_until = replacement.start.saturating_add(replacement.old_len);
    }
    false
}

fn render_overlapping_replacements(
    original: &str,
    original_lines: &[&str],
    replacements: &[Replacement<'_>],
    cancellation: &CancellationToken,
) -> Result<String> {
    let mut lines = original_lines.to_vec();
    for replacement in replacements.iter().rev() {
        ensure_not_cancelled(cancellation)?;
        let end = replacement.start.saturating_add(replacement.old_len);
        if end > lines.len() {
            return Err(anyhow!("patch chunks overlap incompatibly"));
        }
        lines.splice(replacement.start..end, replacement.lines.iter().copied());
    }
    render_replacements(original, &lines, &[], cancellation)
}

fn seek_sequence(
    lines: &[&str],
    pattern: &[&str],
    start: usize,
    end_of_file: bool,
    cancellation: &CancellationToken,
) -> Result<Option<usize>> {
    if pattern.is_empty() {
        return Ok(Some(start));
    }
    if pattern.len() > lines.len() {
        return Ok(None);
    }
    let search_start = if end_of_file {
        lines.len() - pattern.len()
    } else {
        start
    };
    let last = lines.len() - pattern.len();
    for comparison in [
        lines_equal as fn(&str, &str) -> bool,
        lines_equal_without_trailing_space,
        lines_equal_trimmed,
        lines_equal_normalized,
    ] {
        ensure_not_cancelled(cancellation)?;
        for index in search_start..=last {
            if index.is_multiple_of(CANCELLATION_CHECK_INTERVAL) {
                ensure_not_cancelled(cancellation)?;
            }
            if sequence_matches(
                &lines[index..index + pattern.len()],
                pattern,
                comparison,
                cancellation,
            )? {
                return Ok(Some(index));
            }
        }
    }
    Ok(None)
}

fn sequence_matches(
    lines: &[&str],
    pattern: &[&str],
    comparison: fn(&str, &str) -> bool,
    cancellation: &CancellationToken,
) -> Result<bool> {
    for (index, (line, expected)) in lines.iter().zip(pattern).enumerate() {
        if index > 0 && index.is_multiple_of(CANCELLATION_CHECK_INTERVAL) {
            ensure_not_cancelled(cancellation)?;
        }
        if !comparison(line, expected) {
            return Ok(false);
        }
    }
    Ok(true)
}

fn lines_equal(left: &str, right: &str) -> bool {
    left == right
}

fn lines_equal_without_trailing_space(left: &str, right: &str) -> bool {
    left.trim_end() == right.trim_end()
}

fn lines_equal_trimmed(left: &str, right: &str) -> bool {
    left.trim() == right.trim()
}

fn lines_equal_normalized(left: &str, right: &str) -> bool {
    normalized_characters(left).eq(normalized_characters(right))
}

fn normalized_characters(line: &str) -> impl Iterator<Item = char> + '_ {
    line.trim().chars().map(|character| match character {
        '\u{2010}' | '\u{2011}' | '\u{2012}' | '\u{2013}' | '\u{2014}' | '\u{2015}'
        | '\u{2212}' => '-',
        '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}' => '\'',
        '\u{201C}' | '\u{201D}' | '\u{201E}' | '\u{201F}' => '"',
        '\u{00A0}' | '\u{2002}' | '\u{2003}' | '\u{2004}' | '\u{2005}' | '\u{2006}'
        | '\u{2007}' | '\u{2008}' | '\u{2009}' | '\u{200A}' | '\u{202F}' | '\u{205F}'
        | '\u{3000}' => ' ',
        character => character,
    })
}

fn render_replacements(
    original: &str,
    original_lines: &[&str],
    replacements: &[Replacement<'_>],
    cancellation: &CancellationToken,
) -> Result<String> {
    let replacement_bytes = replacements
        .iter()
        .flat_map(|replacement| &replacement.lines)
        .fold(0_usize, |bytes, line| {
            bytes.saturating_add(line.len().saturating_add(1))
        });
    let mut rendered = String::with_capacity(original.len().saturating_add(replacement_bytes));
    let mut original_index = 0_usize;
    let mut rendered_lines = 0_usize;
    let mut last_line_was_empty = false;

    for replacement in replacements {
        if replacement.start < original_index
            || replacement.start.saturating_add(replacement.old_len) > original_lines.len()
        {
            return Err(anyhow!(
                "patch replacements overlap or exceed the source file"
            ));
        }
        for line in &original_lines[original_index..replacement.start] {
            push_rendered_line(
                &mut rendered,
                line,
                &mut rendered_lines,
                &mut last_line_was_empty,
            );
            if rendered_lines.is_multiple_of(CANCELLATION_CHECK_INTERVAL) {
                ensure_not_cancelled(cancellation)?;
            }
        }
        for line in &replacement.lines {
            push_rendered_line(
                &mut rendered,
                line,
                &mut rendered_lines,
                &mut last_line_was_empty,
            );
            if rendered_lines.is_multiple_of(CANCELLATION_CHECK_INTERVAL) {
                ensure_not_cancelled(cancellation)?;
            }
        }
        original_index = replacement.start.saturating_add(replacement.old_len);
    }
    for line in &original_lines[original_index..] {
        push_rendered_line(
            &mut rendered,
            line,
            &mut rendered_lines,
            &mut last_line_was_empty,
        );
        if rendered_lines.is_multiple_of(CANCELLATION_CHECK_INTERVAL) {
            ensure_not_cancelled(cancellation)?;
        }
    }
    ensure_not_cancelled(cancellation)?;
    if rendered_lines > 0 && !last_line_was_empty {
        rendered.push('\n');
    }
    Ok(rendered)
}

fn push_rendered_line(
    output: &mut String,
    line: &str,
    rendered_lines: &mut usize,
    last_line_was_empty: &mut bool,
) {
    if *rendered_lines > 0 {
        output.push('\n');
    }
    output.push_str(line);
    *rendered_lines = rendered_lines.saturating_add(1);
    *last_line_was_empty = line.is_empty();
}

#[cfg(test)]
#[path = "patch_tests.rs"]
mod tests;
