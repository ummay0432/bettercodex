//! Local-filesystem port of Codex's `codex-apply-patch` parser and applier at
//! `1669c2403f793d0230065397dfc25f52b844244e`.
//!
//! The upstream crate is coupled to Codex's remote filesystem and sandbox
//! abstractions. BetterCodex keeps the parser, fuzzy matching, sequential
//! application, and output semantics while using `std::fs` with the invoking
//! user's permissions.

use anyhow::Context;
use anyhow::Result;
use anyhow::anyhow;
use std::path::Path;
use std::path::PathBuf;

pub(super) fn apply(root: &Path, input: &str) -> Result<String> {
    let root = root
        .canonicalize()
        .with_context(|| format!("failed to resolve working directory {}", root.display()))?;
    let operations = parse(input)?;
    if operations.is_empty() {
        return Err(anyhow!("No files were modified."));
    }
    let mut added = Vec::new();
    let mut modified = Vec::new();
    let mut deleted = Vec::new();

    for operation in operations {
        match operation {
            Operation::Add { path, content } => {
                let target = resolve(&root, &path)?;
                write_file(&target, &content)?;
                added.push(path);
            }
            Operation::Delete { path } => {
                let target = resolve(&root, &path)?;
                std::fs::remove_file(&target)
                    .with_context(|| format!("Failed to delete file {}", target.display()))?;
                deleted.push(path);
            }
            Operation::Update {
                path,
                move_to,
                chunks,
            } => {
                let source = resolve(&root, &path)?;
                let content = std::fs::read_to_string(&source).with_context(|| {
                    format!("Failed to read file to update {}", source.display())
                })?;
                let mut content = FileText::new(content);
                apply_chunks(&mut content, &chunks, &path)?;

                if let Some(destination_path) = move_to {
                    let destination = resolve(&root, &destination_path)?;
                    if destination == source {
                        write_file(&source, &content.render())?;
                        modified.push(path);
                    } else {
                        write_file(&destination, &content.render())?;
                        std::fs::remove_file(&source).with_context(|| {
                            format!("Failed to remove original {}", source.display())
                        })?;
                        modified.push(destination_path);
                    }
                } else {
                    write_file(&source, &content.render())?;
                    modified.push(path);
                }
            }
        }
    }

    let mut summary = String::from("Success. Updated the following files:\n");
    for path in added {
        summary.push_str(&format!("A {}\n", path.display()));
    }
    for path in modified {
        summary.push_str(&format!("M {}\n", path.display()));
    }
    for path in deleted {
        summary.push_str(&format!("D {}\n", path.display()));
    }
    Ok(summary)
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

fn parse(input: &str) -> Result<Vec<Operation>> {
    let normalized = input.replace("\r\n", "\n");
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
        let line = lines[index].trim();
        index += 1;
        if let Some(raw_path) = line.strip_prefix("*** Add File: ") {
            let path = patch_path(raw_path)?;
            let mut added = Vec::new();
            while index < lines.len() && !is_boundary(lines[index]) {
                let content = lines[index]
                    .strip_prefix('+')
                    .ok_or_else(|| anyhow!("added file lines must begin with `+`"))?;
                added.push(content);
                index += 1;
            }
            operations.push(Operation::Add {
                path,
                content: if added.is_empty() {
                    String::new()
                } else {
                    format!("{}\n", added.join("\n"))
                },
            });
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
            let chunks = parse_chunks(&lines, &mut index)?;
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

fn parse_chunks(lines: &[&str], index: &mut usize) -> Result<Vec<Chunk>> {
    let mut chunks = Vec::new();
    let mut current: Option<Chunk> = None;
    while *index < lines.len() && !is_boundary(lines[*index]) {
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

#[derive(Clone, PartialEq)]
struct FileText {
    lines: Vec<String>,
}

impl FileText {
    fn new(content: String) -> Self {
        Self::parse(content)
    }

    fn parse(content: String) -> Self {
        let mut lines = content.split('\n').map(str::to_string).collect::<Vec<_>>();
        if lines.last().is_some_and(String::is_empty) {
            lines.pop();
        }
        Self { lines }
    }

    fn render(&self) -> String {
        let mut lines = self.lines.clone();
        if !lines.last().is_some_and(String::is_empty) {
            lines.push(String::new());
        }
        lines.join("\n")
    }
}

fn apply_chunks(file: &mut FileText, chunks: &[Chunk], path: &Path) -> Result<()> {
    let original_lines = file.lines.clone();
    let mut replacements = Vec::<(usize, usize, Vec<String>)>::new();
    let mut line_index = 0;
    for chunk in chunks {
        if let Some(anchor) = &chunk.anchor {
            line_index = seek_sequence(
                &original_lines,
                std::slice::from_ref(anchor),
                line_index,
                false,
            )
            .map(|index| index + 1)
            .ok_or_else(|| anyhow!("Failed to find context '{anchor}' in {}", path.display()))?;
        }

        let old_lines = chunk
            .lines
            .iter()
            .filter_map(|line| match line {
                ChangeLine::Context(line) | ChangeLine::Remove(line) => Some(line.clone()),
                ChangeLine::Add(_) => None,
            })
            .collect::<Vec<_>>();
        let new_lines = chunk
            .lines
            .iter()
            .filter_map(|line| match line {
                ChangeLine::Context(line) | ChangeLine::Add(line) => Some(line.clone()),
                ChangeLine::Remove(_) => None,
            })
            .collect::<Vec<_>>();

        if old_lines.is_empty() {
            let insertion_index = if original_lines.last().is_some_and(String::is_empty) {
                original_lines.len() - 1
            } else {
                original_lines.len()
            };
            replacements.push((insertion_index, 0, new_lines));
            continue;
        }

        let mut pattern = old_lines.as_slice();
        let mut replacement = new_lines.as_slice();
        let mut found = seek_sequence(&original_lines, pattern, line_index, chunk.end_of_file);
        if found.is_none() && pattern.last().is_some_and(String::is_empty) {
            pattern = &pattern[..pattern.len() - 1];
            if replacement.last().is_some_and(String::is_empty) {
                replacement = &replacement[..replacement.len() - 1];
            }
            found = seek_sequence(&original_lines, pattern, line_index, chunk.end_of_file);
        }

        let start = found.ok_or_else(|| {
            anyhow!(
                "Failed to find expected lines in {}:\n{}",
                path.display(),
                old_lines.join("\n"),
            )
        })?;
        replacements.push((start, pattern.len(), replacement.to_vec()));
        line_index = start + pattern.len();
    }

    replacements.sort_by_key(|(index, _, _)| *index);
    for (start, old_len, replacement) in replacements.into_iter().rev() {
        file.lines.splice(start..start + old_len, replacement);
    }
    Ok(())
}

fn seek_sequence(
    lines: &[String],
    pattern: &[String],
    start: usize,
    end_of_file: bool,
) -> Option<usize> {
    if pattern.is_empty() {
        return Some(start);
    }
    if pattern.len() > lines.len() {
        return None;
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
        for index in search_start..=last {
            if lines[index..index + pattern.len()]
                .iter()
                .zip(pattern)
                .all(|(line, expected)| comparison(line, expected))
            {
                return Some(index);
            }
        }
    }
    None
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
    normalize_line(left) == normalize_line(right)
}

fn normalize_line(line: &str) -> String {
    line.trim()
        .chars()
        .map(|character| match character {
            '\u{2010}' | '\u{2011}' | '\u{2012}' | '\u{2013}' | '\u{2014}' | '\u{2015}'
            | '\u{2212}' => '-',
            '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}' => '\'',
            '\u{201C}' | '\u{201D}' | '\u{201E}' | '\u{201F}' => '"',
            '\u{00A0}' | '\u{2002}' | '\u{2003}' | '\u{2004}' | '\u{2005}' | '\u{2006}'
            | '\u{2007}' | '\u{2008}' | '\u{2009}' | '\u{200A}' | '\u{202F}' | '\u{205F}'
            | '\u{3000}' => ' ',
            character => character,
        })
        .collect()
}

#[cfg(test)]
#[path = "patch_tests.rs"]
mod tests;
