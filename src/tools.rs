//! The fixed direct tool stack exposed through the Responses API.

use crate::events::AgentEvent;
use crate::process_runtime::LiveOutputAction;
use crate::process_runtime::OutputStream;
use crate::protocol::FileChange;
use crate::protocol::FunctionCallOutputContentItem;
use crate::protocol::ImageDetail;
use crate::protocol::ToolFileChange;
use crate::truncation::TruncationPolicy;
use crate::truncation::approx_bytes_for_tokens;
use anyhow::Context;
use anyhow::Result;
use anyhow::anyhow;
use serde::Deserialize;
use serde_json::Value;
use serde_json::json;
use std::borrow::Cow;
use std::collections::HashSet;
use std::fs::File;
use std::io::BufRead;
use std::io::BufReader;
use std::io::Read;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;
use std::sync::LazyLock;
use std::time::Duration;
use std::time::Instant;
use tokio::sync::mpsc::UnboundedSender;
use tokio_util::sync::CancellationToken;

const MAX_MODEL_VISIBLE_TOOL_OUTPUT_TOKENS: usize = 10_000;
const MAX_READ_LINES: usize = 2_000;
// Leave room for the continuation marker inside the 10,000-token tool-output bound.
const MAX_READ_BYTES: usize = 39_000;
const MAX_EDIT_FILE_BYTES: usize = 64 * 1024 * 1024;
const MAX_FORWARDED_LIVE_OUTPUT_BYTES: usize = 128 * 1024;
const MAX_FILE_CHANGE_PREVIEW_BYTES: usize = 2 * 1024 * 1024;
// Myers diff work grows with the edit distance, so a byte bound alone does not keep complete
// rewrites responsive when both versions contain many short, unrelated lines.
const MAX_FILE_CHANGE_DIFF_LINES: usize = 8_000;
const MAX_TIMEOUT_SECONDS: f64 = i32::MAX as f64 / 1_000.0;
const READ_IMAGE_INVALID_MESSAGE: &str =
    "unable to process image: invalid or unsupported image data";

const BASH_NAME: &str = "bash";
const READ_NAME: &str = "read";
const WRITE_NAME: &str = "write";
const EDIT_NAME: &str = "edit";

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ToolCall {
    call_id: String,
    name: String,
    arguments: String,
}

impl ToolCall {
    pub(crate) fn from_response_item(item: &Value) -> Option<Self> {
        if item.get("type")?.as_str()? != "function_call" {
            return None;
        }
        let name = item.get("name")?.as_str()?;
        let name = match item.get("namespace") {
            None | Some(Value::Null) => name.to_string(),
            Some(Value::String(namespace)) if namespace.is_empty() || namespace == "functions" => {
                name.to_string()
            }
            Some(Value::String(namespace)) => format!("{namespace}.{name}"),
            Some(_) => return None,
        };
        Some(Self {
            call_id: item.get("call_id")?.as_str()?.to_string(),
            name,
            arguments: item.get("arguments")?.as_str()?.to_string(),
        })
    }

    pub(crate) fn supports_parallel_execution(&self) -> bool {
        matches!(self.name.as_str(), BASH_NAME | READ_NAME)
    }

    pub(crate) async fn execute(
        &self,
        runtime: &ToolRuntime,
        truncation_policy: TruncationPolicy,
        events: Option<UnboundedSender<AgentEvent>>,
        cancellation: CancellationToken,
    ) -> ToolResult {
        let input = parse_arguments(&self.name, &self.arguments);
        if let Some(events) = &events {
            let event_input = match &input {
                Ok(input) => input.clone(),
                Err(_) => Value::String(self.arguments.clone()),
            };
            let _ = events.send(AgentEvent::ToolStarted {
                call_id: self.call_id.clone(),
                name: self.name.clone(),
                input: Some(event_input),
            });
        }

        let started_at = Instant::now();
        let result = match input {
            Ok(input) => {
                runtime
                    .invoke(
                        &self.call_id,
                        &self.name,
                        input,
                        truncation_policy,
                        events.clone(),
                        cancellation,
                    )
                    .await
            }
            Err(error) => Err(error),
        };
        let result = match result {
            Ok(result) => result,
            Err(error) => ToolResult::error(format!("{error:#}"), truncation_policy),
        };

        if let Some(events) = events {
            let _ = events.send(AgentEvent::ToolCompleted {
                call_id: self.call_id.clone(),
                output: result.display.clone(),
                file_change: result.file_change.clone(),
                duration: started_at.elapsed(),
            });
        }
        result
    }

    pub(crate) fn into_output_item(self, output: ToolResult) -> (Value, ToolCompletion) {
        let ToolResult {
            body,
            display,
            file_change,
        } = output;
        let completion = ToolCompletion {
            call_id: self.call_id.clone(),
            error: display.err(),
            file_change,
        };
        (
            json!({
                "type": "function_call_output",
                "call_id": self.call_id,
                "output": body,
            }),
            completion,
        )
    }
}

pub(crate) struct ToolCompletion {
    pub(crate) call_id: String,
    pub(crate) error: Option<String>,
    pub(crate) file_change: Option<ToolFileChange>,
}

pub(crate) struct ToolResult {
    body: Value,
    display: std::result::Result<Value, String>,
    file_change: Option<ToolFileChange>,
}

impl ToolResult {
    fn text(text: String) -> Self {
        let text = bounded_text(
            text,
            TruncationPolicy::Tokens(MAX_MODEL_VISIBLE_TOOL_OUTPUT_TOKENS),
        );
        Self {
            body: Value::String(text.clone()),
            display: Ok(Value::String(text)),
            file_change: None,
        }
    }

    fn bash_output(
        stdout: String,
        stderr: String,
        exit_code: i32,
        policy: TruncationPolicy,
    ) -> Result<Self> {
        let max_bytes = bounded_output_bytes(policy);
        let structural_bytes = serde_json::to_string(&json!({
            "stdout": "",
            "stderr": "",
            "exit_code": exit_code,
        }))
        .context("failed to encode Bash output structure")?
        .len();
        if structural_bytes > max_bytes {
            return Err(anyhow!("Bash output structure exceeds its output bound"));
        }

        let (stdout_budget, stderr_budget) = split_string_budget(
            json_encoded_string_len(&stdout),
            json_encoded_string_len(&stderr),
            max_bytes.saturating_sub(structural_bytes),
        );
        let output = json!({
            "stdout": bounded_json_text_to_bytes(&stdout, stdout_budget),
            "stderr": bounded_json_text_to_bytes(&stderr, stderr_budget),
            "exit_code": exit_code,
        });
        let body =
            serde_json::to_string(&output).context("failed to encode bounded Bash output")?;
        if body.len() > max_bytes {
            return Err(anyhow!("Bash output could not be bounded safely"));
        }
        Ok(Self {
            body: Value::String(body),
            display: Ok(output),
            file_change: None,
        })
    }

    fn image(items: Vec<FunctionCallOutputContentItem>) -> Result<Self> {
        let body = Value::Array(
            items
                .into_iter()
                .map(serde_json::to_value)
                .collect::<std::result::Result<Vec<_>, _>>()?,
        );
        Ok(Self {
            body,
            display: Ok(json!({})),
            file_change: None,
        })
    }

    fn error(error: String, policy: TruncationPolicy) -> Self {
        let error = bounded_text(error, policy);
        Self {
            body: Value::String(error.clone()),
            display: Err(error),
            file_change: None,
        }
    }

    fn with_file_change(mut self, path: PathBuf, change: FileChange) -> Self {
        self.file_change = Some(ToolFileChange { path, change });
        self
    }
}

fn bounded_output_bytes(policy: TruncationPolicy) -> usize {
    policy.byte_budget().min(approx_bytes_for_tokens(
        MAX_MODEL_VISIBLE_TOOL_OUTPUT_TOKENS,
    ))
}

fn bounded_text(text: String, policy: TruncationPolicy) -> String {
    bounded_text_to_bytes(&text, bounded_output_bytes(policy))
}

fn bounded_text_to_bytes(text: &str, max_bytes: usize) -> String {
    truncate_string_to_budget(text, max_bytes, char::len_utf8)
}

fn bounded_json_text_to_bytes(text: &str, max_encoded_bytes: usize) -> String {
    truncate_string_to_budget(text, max_encoded_bytes, json_encoded_char_len)
}

fn split_string_budget(
    first_length: usize,
    second_length: usize,
    total_budget: usize,
) -> (usize, usize) {
    if first_length.saturating_add(second_length) <= total_budget {
        return (first_length, second_length);
    }
    let first_share = total_budget / 2;
    let second_share = total_budget.saturating_sub(first_share);
    if first_length <= first_share {
        (first_length, total_budget.saturating_sub(first_length))
    } else if second_length <= second_share {
        (total_budget.saturating_sub(second_length), second_length)
    } else {
        (first_share, second_share)
    }
}

fn truncate_string_to_budget(
    text: &str,
    max_bytes: usize,
    character_bytes: impl Fn(char) -> usize + Copy,
) -> String {
    if text.chars().map(character_bytes).sum::<usize>() <= max_bytes {
        return text.to_string();
    }
    if max_bytes < '…'.len_utf8() {
        return String::new();
    }

    let total_characters = text.chars().count();
    let largest_marker = format!("…{total_characters} chars truncated…");
    let marker_bytes = largest_marker.chars().map(character_bytes).sum::<usize>();
    if marker_bytes > max_bytes {
        return "…".to_string();
    }
    let retained_budget = max_bytes.saturating_sub(marker_bytes);
    let prefix_budget = retained_budget / 2;
    let suffix_budget = retained_budget.saturating_sub(prefix_budget);

    let mut prefix_end = 0_usize;
    let mut prefix_bytes = 0_usize;
    let mut retained_characters = 0_usize;
    for (index, character) in text.char_indices() {
        let encoded = character_bytes(character);
        if prefix_bytes.saturating_add(encoded) > prefix_budget {
            break;
        }
        prefix_bytes = prefix_bytes.saturating_add(encoded);
        prefix_end = index.saturating_add(character.len_utf8());
        retained_characters = retained_characters.saturating_add(1);
    }

    let mut suffix_start = text.len();
    let mut suffix_bytes = 0_usize;
    for (index, character) in text.char_indices().rev() {
        if index < prefix_end {
            break;
        }
        let encoded = character_bytes(character);
        if suffix_bytes.saturating_add(encoded) > suffix_budget {
            break;
        }
        suffix_bytes = suffix_bytes.saturating_add(encoded);
        suffix_start = index;
        retained_characters = retained_characters.saturating_add(1);
    }

    let removed = total_characters.saturating_sub(retained_characters);
    format!(
        "{}…{removed} chars truncated…{}",
        &text[..prefix_end],
        &text[suffix_start..]
    )
}

fn json_encoded_string_len(text: &str) -> usize {
    text.chars().map(json_encoded_char_len).sum()
}

fn json_encoded_char_len(character: char) -> usize {
    match character {
        '"' | '\\' | '\u{0008}' | '\t' | '\n' | '\u{000c}' | '\r' => 2,
        '\u{0000}'..='\u{001f}' => 6,
        _ => character.len_utf8(),
    }
}

pub(crate) struct ToolRuntime {
    cwd: PathBuf,
}

impl ToolRuntime {
    pub(crate) fn new(cwd: PathBuf) -> Self {
        Self { cwd }
    }

    #[allow(clippy::too_many_arguments)]
    async fn invoke(
        &self,
        call_id: &str,
        name: &str,
        input: Value,
        truncation_policy: TruncationPolicy,
        events: Option<UnboundedSender<AgentEvent>>,
        cancellation: CancellationToken,
    ) -> Result<ToolResult> {
        match name {
            BASH_NAME => {
                self.bash(call_id, input, truncation_policy, events, cancellation)
                    .await
            }
            READ_NAME => {
                let cwd = self.cwd.clone();
                blocking_tool(cancellation, move |cancellation| {
                    read(&cwd, input, &cancellation)
                })
                .await
            }
            WRITE_NAME => {
                let cwd = self.cwd.clone();
                blocking_tool(cancellation, move |cancellation| {
                    write(&cwd, input, &cancellation)
                })
                .await
            }
            EDIT_NAME => {
                let cwd = self.cwd.clone();
                blocking_tool(cancellation, move |cancellation| {
                    edit(&cwd, input, &cancellation)
                })
                .await
            }
            _ => Err(anyhow!("unknown tool `{name}`")),
        }
    }

    async fn bash(
        &self,
        call_id: &str,
        input: Value,
        truncation_policy: TruncationPolicy,
        events: Option<UnboundedSender<AgentEvent>>,
        cancellation: CancellationToken,
    ) -> Result<ToolResult> {
        let arguments: BashArgs = deserialize_arguments(input)?;
        let timeout = arguments.timeout.map(resolve_timeout).transpose()?;
        let live_call_id = call_id.to_string();
        let mut forwarded_live_bytes = 0_usize;
        let mut forward_live_output = |stream, mut chunk: String| {
            let Some(events) = &events else {
                return LiveOutputAction::Stop;
            };
            let omitted = crate::process_runtime::fit_live_output_budget(
                &mut chunk,
                &mut forwarded_live_bytes,
                MAX_FORWARDED_LIVE_OUTPUT_BYTES,
            );
            if !chunk.is_empty()
                && events
                    .send(AgentEvent::ToolOutputDelta {
                        call_id: live_call_id.clone(),
                        stream,
                        chunk,
                    })
                    .is_err()
            {
                return LiveOutputAction::Stop;
            }
            if omitted {
                let _ = events.send(AgentEvent::ToolOutputDelta {
                    call_id: live_call_id.clone(),
                    stream,
                    chunk: "\n… additional live output omitted …\n".to_string(),
                });
                LiveOutputAction::Stop
            } else {
                LiveOutputAction::Continue
            }
        };
        let on_output = events.is_some().then_some(
            &mut forward_live_output
                as &mut (dyn FnMut(OutputStream, String) -> LiveOutputAction + Send),
        );
        let output = crate::process_runtime::run_bash(
            &arguments.command,
            &self.cwd,
            timeout,
            cancellation,
            on_output,
        )
        .await?;

        ToolResult::bash_output(
            output.stdout,
            output.stderr,
            output.exit_code,
            truncation_policy,
        )
    }
}

async fn blocking_tool(
    cancellation: CancellationToken,
    operation: impl FnOnce(CancellationToken) -> Result<ToolResult> + Send + 'static,
) -> Result<ToolResult> {
    ensure_not_cancelled(&cancellation, "tool call")?;
    let operation_cancellation = cancellation.clone();
    tokio::task::spawn_blocking(move || operation(operation_cancellation))
        .await
        .context("blocking tool task failed")?
}

fn ensure_not_cancelled(cancellation: &CancellationToken, operation: &str) -> Result<()> {
    if cancellation.is_cancelled() {
        Err(anyhow!("{operation} was interrupted"))
    } else {
        Ok(())
    }
}

fn parse_arguments(name: &str, arguments: &str) -> Result<Value> {
    let input: Value = serde_json::from_str(arguments)
        .with_context(|| format!("failed to parse `{name}` function arguments"))?;
    if !input.is_object() {
        return Err(anyhow!("tool `{name}` expects a JSON object for arguments"));
    }
    Ok(input)
}

fn deserialize_arguments<T: for<'de> Deserialize<'de>>(input: Value) -> Result<T> {
    serde_json::from_value(input)
        .map_err(|error| anyhow!("failed to parse function arguments: {error}"))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BashArgs {
    command: String,
    timeout: Option<f64>,
}

fn resolve_timeout(seconds: f64) -> Result<Duration> {
    if !seconds.is_finite() || seconds <= 0.0 || seconds > MAX_TIMEOUT_SECONDS {
        return Err(anyhow!(
            "bash.timeout must be a finite number greater than 0 and no greater than {MAX_TIMEOUT_SECONDS} seconds"
        ));
    }
    Ok(Duration::from_secs_f64(seconds))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadArgs {
    path: String,
    offset: Option<usize>,
    limit: Option<usize>,
    detail: Option<String>,
}

fn read(cwd: &Path, input: Value, cancellation: &CancellationToken) -> Result<ToolResult> {
    ensure_not_cancelled(cancellation, READ_NAME)?;
    let arguments: ReadArgs = deserialize_arguments(input)?;
    let offset = arguments.offset.unwrap_or(1);
    if offset == 0 {
        return Err(anyhow!("read.offset is 1-indexed and must be at least 1"));
    }
    let limit = arguments.limit.unwrap_or(MAX_READ_LINES);
    if limit == 0 {
        return Err(anyhow!("read.limit must be at least 1"));
    }
    if limit > MAX_READ_LINES {
        return Err(anyhow!(
            "read.limit must be no greater than {MAX_READ_LINES}"
        ));
    }
    let path = resolve_path(cwd, &arguments.path);
    let file =
        open_for_read(&path).with_context(|| format!("unable to read `{}`", path.display()))?;
    let metadata = file
        .metadata()
        .with_context(|| format!("unable to inspect `{}`", path.display()))?;
    if !metadata.is_file() {
        return Err(anyhow!("read path `{}` is not a file", path.display()));
    }
    let mut reader = BufReader::new(file);
    let supported_image = matches!(
        image::guess_format(reader.fill_buf()?),
        Ok(image::ImageFormat::Png
            | image::ImageFormat::Jpeg
            | image::ImageFormat::Gif
            | image::ImageFormat::WebP)
    );
    if supported_image {
        if arguments.offset.is_some() || arguments.limit.is_some() {
            return Err(anyhow!(
                "read.offset and read.limit are only supported for text files"
            ));
        }
        let detail = match arguments.detail.as_deref() {
            None | Some("high") => ImageDetail::High,
            Some("original") => ImageDetail::Original,
            Some(detail) => {
                return Err(anyhow!(
                    "read.detail only supports `high` or `original`; omit `detail` for default high resized behavior, got `{detail}`"
                ));
            }
        };
        let limit = crate::input::MAX_TOTAL_IMAGE_BYTES;
        if metadata.len() > u64::try_from(limit).unwrap_or(u64::MAX) {
            return Err(anyhow!(
                "image at `{}` exceeds the {} MiB read image limit",
                path.display(),
                limit / (1024 * 1024)
            ));
        }
        let capacity = usize::try_from(metadata.len()).unwrap_or(limit).min(limit);
        let mut bytes = Vec::with_capacity(capacity);
        reader
            .take(u64::try_from(limit.saturating_add(1)).unwrap_or(u64::MAX))
            .read_to_end(&mut bytes)
            .with_context(|| format!("unable to read image at `{}`", path.display()))?;
        ensure_not_cancelled(cancellation, READ_NAME)?;
        if bytes.len() > limit {
            return Err(anyhow!(
                "image at `{}` exceeds the {} MiB read image limit",
                path.display(),
                limit / (1024 * 1024)
            ));
        }
        let mode = match detail {
            ImageDetail::High => {
                crate::image::PromptImageMode::ResizeWithLimits(crate::image::HIGH_DETAIL_LIMITS)
            }
            ImageDetail::Original => crate::image::PromptImageMode::Original,
            ImageDetail::Auto | ImageDetail::Low => unreachable!("read validates image detail"),
        };
        let image = crate::image::load_for_prompt_bytes(bytes.into(), mode)
            .map_err(|_| anyhow!(READ_IMAGE_INVALID_MESSAGE))?;
        ensure_not_cancelled(cancellation, READ_NAME)?;
        return ToolResult::image(vec![FunctionCallOutputContentItem::InputImage {
            image_url: image.into_data_url(),
            detail: Some(detail),
        }]);
    }

    if arguments.detail.is_some() {
        return Err(anyhow!("read.detail is only supported for image files"));
    }
    for _ in 1..offset {
        ensure_not_cancelled(cancellation, READ_NAME)?;
        if !skip_line(&mut reader, cancellation)? {
            return Err(anyhow!(
                "read offset {offset} is beyond the end of `{}`",
                path.display()
            ));
        }
    }

    let mut output = Vec::new();
    let mut emitted_lines = 0_usize;
    let mut truncated = false;
    while emitted_lines < limit {
        ensure_not_cancelled(cancellation, READ_NAME)?;
        let line = match read_bounded_line(&mut reader, MAX_READ_BYTES)? {
            BoundedLine::Eof => break,
            BoundedLine::Line(line) => line,
            BoundedLine::TooLong if emitted_lines > 0 => {
                truncated = true;
                break;
            }
            BoundedLine::TooLong => {
                return Err(anyhow!(
                    "a requested line exceeds the {MAX_READ_BYTES}-byte read limit; use bash for a bounded byte slice"
                ));
            }
        };
        if output.len().saturating_add(line.len()) > MAX_READ_BYTES {
            truncated = true;
            break;
        }
        output.extend_from_slice(&line);
        emitted_lines = emitted_lines.saturating_add(1);
    }
    if emitted_lines == limit && !reader.fill_buf()?.is_empty() {
        truncated = true;
    }
    if offset > 1 && emitted_lines == 0 {
        return Err(anyhow!(
            "read offset {offset} is beyond the end of `{}`",
            path.display()
        ));
    }
    let mut output = String::from_utf8(output)
        .with_context(|| format!("`{}` is not valid UTF-8 text", path.display()))?;
    if truncated {
        let next_offset = offset.saturating_add(emitted_lines);
        if !output.is_empty() && !output.ends_with('\n') {
            output.push('\n');
        }
        let line_unit = if limit == 1 { "line" } else { "lines" };
        output.push_str(&format!(
            "\n[Output bounded at {limit} {line_unit} or {MAX_READ_BYTES} bytes. Use offset={next_offset} to continue.]"
        ));
    }
    Ok(ToolResult::text(output))
}

fn skip_line(reader: &mut impl BufRead, cancellation: &CancellationToken) -> Result<bool> {
    let mut consumed = false;
    loop {
        ensure_not_cancelled(cancellation, READ_NAME)?;
        let buffer = reader.fill_buf()?;
        if buffer.is_empty() {
            return Ok(consumed);
        }
        consumed = true;
        let length = buffer
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(buffer.len(), |index| index + 1);
        let complete = buffer.get(length.saturating_sub(1)) == Some(&b'\n');
        reader.consume(length);
        if complete {
            return Ok(true);
        }
    }
}

enum BoundedLine {
    Eof,
    Line(Vec<u8>),
    TooLong,
}

fn read_bounded_line(reader: &mut impl BufRead, max_bytes: usize) -> Result<BoundedLine> {
    let mut line = Vec::new();
    loop {
        let buffer = reader.fill_buf()?;
        if buffer.is_empty() {
            return Ok(if line.is_empty() {
                BoundedLine::Eof
            } else {
                BoundedLine::Line(line)
            });
        }
        let length = buffer
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(buffer.len(), |index| index + 1);
        if line.len().saturating_add(length) > max_bytes {
            return Ok(BoundedLine::TooLong);
        }
        let complete = buffer.get(length.saturating_sub(1)) == Some(&b'\n');
        line.extend_from_slice(&buffer[..length]);
        reader.consume(length);
        if complete {
            return Ok(BoundedLine::Line(line));
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WriteArgs {
    path: String,
    content: String,
}

fn write(cwd: &Path, input: Value, cancellation: &CancellationToken) -> Result<ToolResult> {
    ensure_not_cancelled(cancellation, WRITE_NAME)?;
    let arguments: WriteArgs = deserialize_arguments(input)?;
    let path = resolve_path(cwd, &arguments.path);
    let write_path = resolve_symlink_write_path(&path);
    // Return the dedicated safety error before generic write context is added. The atomic helper
    // checks again after preview and parent-directory preparation in case the path changed.
    if std::fs::symlink_metadata(&write_path).is_ok_and(|metadata| !metadata.is_file()) {
        return Err(anyhow!(
            "write path `{}` is not a regular file",
            write_path.display()
        ));
    }
    let preview = write_preview(&write_path, &arguments.content);
    ensure_not_cancelled(cancellation, WRITE_NAME)?;
    let parent = write_path
        .parent()
        .ok_or_else(|| anyhow!("write path `{}` has no parent directory", path.display()))?;
    // Once the mutation starts, finish it so cancellation cannot report failure after creating
    // parent directories or completing the whole-file replacement.
    std::fs::create_dir_all(parent)
        .with_context(|| format!("unable to create `{}`", parent.display()))?;
    write_file_atomically(
        &write_path,
        arguments.content.as_bytes(),
        AtomicWriteMode::CreateOrReplace,
    )
    .with_context(|| format!("unable to write `{}`", path.display()))?;
    let result = write_result(&path, &arguments.content);
    let change = match preview {
        WritePreview::Add => Some(FileChange::Add {
            content: arguments.content,
        }),
        WritePreview::Update(original) => {
            let unified_diff = diffy::create_patch(&original, &arguments.content).to_string();
            (unified_diff.len() <= MAX_FILE_CHANGE_PREVIEW_BYTES).then_some(FileChange::Update {
                unified_diff,
                move_path: None,
            })
        }
        WritePreview::Omit => None,
    };
    Ok(match change {
        Some(change) => result.with_file_change(path, change),
        None => result,
    })
}

enum WritePreview {
    Add,
    Update(String),
    Omit,
}

fn write_preview(path: &Path, replacement: &str) -> WritePreview {
    // A preview must never make a write fail or read an arbitrarily large existing file. The write
    // itself retains its ordinary behavior when a bounded preview cannot be obtained.
    if replacement.len() > MAX_FILE_CHANGE_PREVIEW_BYTES {
        return WritePreview::Omit;
    }
    let file = match open_for_read(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return WritePreview::Add,
        Err(_) => return WritePreview::Omit,
    };
    if file
        .metadata()
        .is_ok_and(|metadata| metadata.len() > MAX_FILE_CHANGE_PREVIEW_BYTES as u64)
    {
        return WritePreview::Omit;
    }
    let mut bytes = Vec::new();
    if file
        .take((MAX_FILE_CHANGE_PREVIEW_BYTES as u64).saturating_add(1))
        .read_to_end(&mut bytes)
        .is_err()
        || bytes.len() > MAX_FILE_CHANGE_PREVIEW_BYTES
    {
        return WritePreview::Omit;
    }
    match String::from_utf8(bytes) {
        Ok(original) if write_diff_is_within_budget(&original, replacement) => {
            WritePreview::Update(original)
        }
        Ok(_) | Err(_) => WritePreview::Omit,
    }
}

fn write_diff_is_within_budget(original: &str, replacement: &str) -> bool {
    let original_lines = original
        .split_inclusive('\n')
        .take(MAX_FILE_CHANGE_DIFF_LINES.saturating_add(1))
        .count();
    if original_lines > MAX_FILE_CHANGE_DIFF_LINES {
        return false;
    }
    let remaining = MAX_FILE_CHANGE_DIFF_LINES.saturating_sub(original_lines);
    replacement
        .split_inclusive('\n')
        .take(remaining.saturating_add(1))
        .count()
        <= remaining
}

fn write_result(path: &Path, content: &str) -> ToolResult {
    ToolResult::text(format!(
        "Wrote {} bytes to {}",
        content.len(),
        path.display()
    ))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EditArgs {
    path: String,
    edits: Vec<Replacement>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Replacement {
    old_text: String,
    new_text: String,
}

fn edit(cwd: &Path, input: Value, cancellation: &CancellationToken) -> Result<ToolResult> {
    ensure_not_cancelled(cancellation, EDIT_NAME)?;
    let arguments: EditArgs = deserialize_arguments(input)?;
    if arguments.edits.is_empty() {
        return Err(anyhow!("edit.edits must contain at least one replacement"));
    }
    let path = resolve_path(cwd, &arguments.path);
    let write_path = resolve_symlink_write_path(&path);
    let file = open_for_read(&write_path)
        .with_context(|| format!("unable to read `{}` as UTF-8 text", path.display()))?;
    let metadata = file
        .metadata()
        .with_context(|| format!("unable to read `{}` as UTF-8 text", path.display()))?;
    if !metadata.is_file() {
        return Err(anyhow!("unable to read `{}` as UTF-8 text", path.display()));
    }
    if metadata.len() > MAX_EDIT_FILE_BYTES as u64 {
        return Err(anyhow!(
            "edit target `{}` exceeds the {} MiB edit limit; use bash for a bounded transformation",
            path.display(),
            MAX_EDIT_FILE_BYTES / (1024 * 1024)
        ));
    }
    let capacity = usize::try_from(metadata.len())
        .unwrap_or(MAX_EDIT_FILE_BYTES)
        .min(MAX_EDIT_FILE_BYTES);
    let mut bytes = Vec::with_capacity(capacity);
    file.take((MAX_EDIT_FILE_BYTES as u64).saturating_add(1))
        .read_to_end(&mut bytes)
        .with_context(|| format!("unable to read `{}` as UTF-8 text", path.display()))?;
    if bytes.len() > MAX_EDIT_FILE_BYTES {
        return Err(anyhow!(
            "edit target `{}` exceeds the {} MiB edit limit; use bash for a bounded transformation",
            path.display(),
            MAX_EDIT_FILE_BYTES / (1024 * 1024)
        ));
    }
    let original = String::from_utf8(bytes)
        .with_context(|| format!("unable to read `{}` as UTF-8 text", path.display()))?;
    ensure_not_cancelled(cancellation, EDIT_NAME)?;
    let normalized_original = normalize_line_endings(&original);
    let preferred_line_ending = preferred_line_ending(&original);
    let normalized_edits = arguments
        .edits
        .iter()
        .map(|edit| {
            (
                normalize_line_endings(&edit.old_text),
                restore_line_endings(
                    normalize_line_endings(&edit.new_text),
                    preferred_line_ending,
                ),
            )
        })
        .collect::<Vec<_>>();
    let mut ranges = Vec::with_capacity(normalized_edits.len());
    for (index, (old_text, _)) in normalized_edits.iter().enumerate() {
        ensure_not_cancelled(cancellation, EDIT_NAME)?;
        if old_text.is_empty() {
            return Err(anyhow!("edit.edits[{index}].oldText must not be empty"));
        }
        let start = match unique_match(&normalized_original, old_text) {
            UniqueMatch::Missing => {
                return Err(anyhow!(
                    "edit.edits[{index}].oldText must match exactly once in `{}`; found 0 matches",
                    path.display()
                ));
            }
            UniqueMatch::Unique(start) => start,
            UniqueMatch::Multiple => {
                return Err(anyhow!(
                    "edit.edits[{index}].oldText must match exactly once in `{}`; found multiple matches",
                    path.display()
                ));
            }
        };
        ranges.push((start, start + old_text.len(), index));
    }
    ranges.sort_unstable_by_key(|(start, _, _)| *start);
    for pair in ranges.windows(2) {
        if pair[0].1 > pair[1].0 {
            return Err(anyhow!(
                "edit replacements {} and {} overlap in `{}`",
                pair[0].2,
                pair[1].2,
                path.display()
            ));
        }
    }
    if matches!(&normalized_original, Cow::Owned(_)) {
        restore_original_range_offsets(&original, &mut ranges);
    }

    let updated_len = ranges
        .iter()
        .fold(original.len(), |length, (start, end, index)| {
            length - (end - start) + normalized_edits[*index].1.len()
        });
    let mut updated = String::with_capacity(updated_len);
    let mut original_start = 0_usize;
    for (start, end, index) in ranges {
        ensure_not_cancelled(cancellation, EDIT_NAME)?;
        updated.push_str(&original[original_start..start]);
        updated.push_str(&normalized_edits[index].1);
        original_start = end;
    }
    updated.push_str(&original[original_start..]);
    if updated == original {
        return Err(anyhow!(
            "edit replacements did not change `{}`",
            path.display()
        ));
    }
    let file_change = if original.len() <= MAX_FILE_CHANGE_PREVIEW_BYTES
        && updated.len() <= MAX_FILE_CHANGE_PREVIEW_BYTES
    {
        let unified_diff = diffy::create_patch(&original, &updated).to_string();
        (unified_diff.len() <= MAX_FILE_CHANGE_PREVIEW_BYTES).then_some(FileChange::Update {
            unified_diff,
            move_path: None,
        })
    } else {
        None
    };
    // Cancellation is honored throughout preparation. Once atomic replacement begins, finish it
    // so interruption cannot leave or report a partial edit.
    ensure_not_cancelled(cancellation, EDIT_NAME)?;
    write_file_atomically(
        &write_path,
        updated.as_bytes(),
        AtomicWriteMode::ReplaceExisting,
    )?;
    let result = ToolResult::text(format!(
        "Replaced {} block(s) in {}",
        arguments.edits.len(),
        path.display()
    ));
    Ok(match file_change {
        Some(change) => result.with_file_change(path, change),
        None => result,
    })
}

fn normalize_line_endings(text: &str) -> Cow<'_, str> {
    let bytes = text.as_bytes();
    if !bytes.contains(&b'\r') {
        return Cow::Borrowed(text);
    }

    let mut normalized = String::with_capacity(text.len());
    let mut segment_start = 0_usize;
    let mut index = 0_usize;
    while index < bytes.len() {
        if bytes[index] != b'\r' {
            index += 1;
            continue;
        }
        normalized.push_str(&text[segment_start..index]);
        normalized.push('\n');
        index += 1;
        if bytes.get(index) == Some(&b'\n') {
            index += 1;
        }
        segment_start = index;
    }
    normalized.push_str(&text[segment_start..]);
    Cow::Owned(normalized)
}

// Convert sorted, non-overlapping ranges from normalized LF offsets back to byte offsets in the
// original text. One forward scan avoids retaining one mapping entry for every CRLF in the file.
fn restore_original_range_offsets(original: &str, ranges: &mut [(usize, usize, usize)]) {
    let bytes = original.as_bytes();
    let mut original_offset = 0_usize;
    let mut normalized_offset = 0_usize;
    let mut advance_to = |target: usize| {
        while normalized_offset < target {
            original_offset += if bytes[original_offset] == b'\r'
                && bytes.get(original_offset + 1) == Some(&b'\n')
            {
                2
            } else {
                1
            };
            normalized_offset += 1;
        }
        debug_assert_eq!(normalized_offset, target);
        original_offset
    };

    for (start, end, _) in ranges {
        let normalized_start = *start;
        let normalized_end = *end;
        *start = advance_to(normalized_start);
        *end = advance_to(normalized_end);
    }
}

fn preferred_line_ending(text: &str) -> &'static str {
    let bytes = text.as_bytes();
    for (index, byte) in bytes.iter().enumerate() {
        match byte {
            b'\r' if bytes.get(index.saturating_add(1)) == Some(&b'\n') => return "\r\n",
            b'\r' => return "\r",
            b'\n' => return "\n",
            _ => {}
        }
    }
    "\n"
}

fn restore_line_endings(text: Cow<'_, str>, line_ending: &str) -> String {
    if line_ending == "\n" {
        text.into_owned()
    } else {
        text.replace('\n', line_ending)
    }
}

enum UniqueMatch {
    Missing,
    Unique(usize),
    Multiple,
}

fn unique_match(content: &str, pattern: &str) -> UniqueMatch {
    debug_assert!(!pattern.is_empty());
    let Some(start) = content.find(pattern) else {
        return UniqueMatch::Missing;
    };
    let next_start = start + content[start..].chars().next().map_or(1, char::len_utf8);
    // Uniqueness only depends on whether a second overlapping occurrence exists.
    if content[next_start..].contains(pattern) {
        UniqueMatch::Multiple
    } else {
        UniqueMatch::Unique(start)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum AtomicWriteMode {
    CreateOrReplace,
    ReplaceExisting,
}

// Resolve the final symlink component before reading and keep that target stable through the
// replacement. Missing targets are returned so `write` can create through a dangling symlink.
fn resolve_symlink_write_path(path: &Path) -> PathBuf {
    use std::os::unix::fs::MetadataExt;

    let root = path.to_path_buf();
    let mut current = root.clone();
    let mut visited = HashSet::new();
    loop {
        let metadata = match std::fs::symlink_metadata(&current) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return current,
            Err(_) => return root,
        };
        if !metadata.file_type().is_symlink() {
            return current;
        }
        if !visited.insert((metadata.dev(), metadata.ino())) {
            return root;
        }
        let target = match std::fs::read_link(&current) {
            Ok(target) => target,
            Err(_) => return root,
        };
        current = if target.is_absolute() {
            target
        } else if let Some(parent) = current.parent() {
            parent.join(target)
        } else {
            return root;
        };
    }
}

fn inspect_atomic_write_destination(
    path: &Path,
    mode: AtomicWriteMode,
) -> Result<Option<std::fs::Metadata>> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => Some(metadata),
        Err(error)
            if mode == AtomicWriteMode::CreateOrReplace
                && error.kind() == std::io::ErrorKind::NotFound =>
        {
            None
        }
        Err(error) => {
            return Err(error).with_context(|| format!("unable to inspect `{}`", path.display()));
        }
    };
    if metadata
        .as_ref()
        .is_some_and(|metadata| !metadata.is_file())
    {
        return Err(anyhow!(
            "write path `{}` is not a regular file",
            path.display()
        ));
    }
    Ok(metadata)
}

fn write_file_atomically(path: &Path, content: &[u8], mode: AtomicWriteMode) -> Result<()> {
    let metadata = inspect_atomic_write_destination(path, mode)?;
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("edit path `{}` has no parent directory", path.display()))?;
    // Keep the temporary basename independent of the destination basename. A destination can be
    // valid at the filesystem's NAME_MAX while a prefixed copy of that name is not.
    let temporary = parent.join(format!(
        ".bettercodex-{}-{}.tmp",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    let mut cleanup = TemporaryFile::new(temporary.clone());
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    if metadata.is_some() {
        use std::os::unix::fs::OpenOptionsExt as _;
        // Do not expose replacement content through the temporary path using broader default
        // permissions. The destination's ordinary mode is restored after writing so content is
        // never exposed through the staging path under broader access than necessary.
        options.mode(0o600);
    }
    let mut file = options
        .open(&temporary)
        .with_context(|| format!("unable to create `{}`", temporary.display()))?;
    file.write_all(content)?;
    if let Some(metadata) = metadata {
        use std::os::unix::fs::PermissionsExt as _;
        // Preserve ordinary access and executable bits, but retain the kernel's safety behavior of
        // clearing privilege bits when file contents are replaced.
        let mode = metadata.permissions().mode() & 0o777;
        file.set_permissions(std::fs::Permissions::from_mode(mode))?;
    }
    // Narrow the check-to-rename window so a special file created while the temporary file was
    // being populated is not silently replaced.
    let _ = inspect_atomic_write_destination(path, mode)?;
    crate::private_fs::replace_file(&temporary, path).with_context(|| {
        format!(
            "unable to atomically replace `{}` with `{}`",
            path.display(),
            temporary.display()
        )
    })?;
    cleanup.disarm();
    Ok(())
}

struct TemporaryFile {
    path: Option<PathBuf>,
}

impl TemporaryFile {
    fn new(path: PathBuf) -> Self {
        Self { path: Some(path) }
    }

    fn disarm(&mut self) {
        self.path = None;
    }
}

impl Drop for TemporaryFile {
    fn drop(&mut self) {
        if let Some(path) = &self.path {
            let _ = std::fs::remove_file(path);
        }
    }
}

fn open_for_read(path: &Path) -> std::io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;

    // Opening a FIFO read-only can block before its metadata is available for the regular-file
    // check. O_NONBLOCK is ignored for ordinary local files.
    let mut options = std::fs::OpenOptions::new();
    options.read(true).custom_flags(libc::O_NONBLOCK).open(path)
}

fn resolve_path(cwd: &Path, requested: &str) -> PathBuf {
    let requested = PathBuf::from(requested);
    if requested.is_absolute() {
        requested
    } else {
        cwd.join(requested)
    }
}

pub(crate) fn responses_api_specifications() -> &'static [Value] {
    &RESPONSES_API_SPECIFICATIONS
}

pub(crate) fn catalogue_text() -> &'static str {
    &CATALOGUE_TEXT
}

static TOOL_SPECIFICATIONS: LazyLock<Vec<Value>> = LazyLock::new(|| {
    vec![
        function_tool(
            BASH_NAME,
            "Execute a Bash command in the current working directory and wait for it to exit. Returns JSON with bounded `stdout` and `stderr` strings plus integer `exit_code`; runtime failures return plain error text. A timeout kills the process tree, and background children are terminated when the shell exits.",
            bash_schema(),
            bash_output_schema(),
        ),
        function_tool(
            READ_NAME,
            "Read bounded UTF-8 text or inspect a PNG, JPEG, GIF, or WebP image from a local file. Use `read` rather than shell commands to inspect a known file. Text reads include at most 2,000 lines or 39,000 bytes of file content and report the next offset when truncated; image reads accept files up to 50 MiB and return one image attachment. Failures return plain error text.",
            read_schema(),
            read_output_schema(),
        ),
        function_tool(
            WRITE_NAME,
            "Create or atomically replace a UTF-8 file, creating parent directories as needed. Use for new files or intentional whole-file rewrites; use `edit` for targeted changes. Returns a short confirmation or plain error text.",
            write_schema(),
            json!({"type": "string"}),
        ),
        function_tool(
            EDIT_NAME,
            "Atomically edit one UTF-8 file of at most 64 MiB with exact replacements. Every non-empty `oldText` must occur exactly once in the original file; replacements are matched independently, must not overlap, and either all apply or none do. Put multiple disjoint changes in one call and keep each `oldText` minimal but unique. Returns a short confirmation or plain error text.",
            edit_schema(),
            json!({"type": "string"}),
        ),
        json!({
            "type": "web_search",
            "external_web_access": true,
            "search_content_types": ["text", "image"],
        }),
    ]
});

static RESPONSES_API_SPECIFICATIONS: LazyLock<Vec<Value>> = LazyLock::new(|| {
    TOOL_SPECIFICATIONS
        .iter()
        .cloned()
        .map(|mut specification| {
            if let Some(specification) = specification.as_object_mut() {
                specification.remove("output_schema");
            }
            specification
        })
        .collect()
});

static CATALOGUE_TEXT: LazyLock<String> = LazyLock::new(|| {
    render_catalogue(&TOOL_SPECIFICATIONS)
        .unwrap_or_else(|error| format!("failed to render tool catalogue: {error}"))
});

fn render_catalogue(specifications: &[Value]) -> Result<String> {
    let mut output = String::from("# Tools\n");
    for specification in specifications {
        render_catalogue_tool(&mut output, specification)?;
    }
    output.truncate(output.trim_end_matches('\n').len());
    Ok(output)
}

fn render_catalogue_tool(output: &mut String, tool: &Value) -> Result<()> {
    if tool.get("type").and_then(Value::as_str) == Some("web_search") {
        output.push_str(
            "\n## Hosted web search\n\nThe Responses API can search and browse the live web using text and image results. URL citations in assistant output are displayed as clickable source links.\n",
        );
        return Ok(());
    }
    let name = tool
        .get("name")
        .and_then(Value::as_str)
        .context("function tool is missing its name")?;
    let description = tool
        .get("description")
        .and_then(Value::as_str)
        .context("tool is missing its description")?;
    let parameters = tool
        .get("parameters")
        .context("tool is missing its parameters schema")?;
    let output_schema = tool
        .get("output_schema")
        .context("tool is missing its output schema")?;

    output.push_str("\n## `");
    output.push_str(name);
    output.push_str("`\n\n");
    render_catalogue_description(output, description);
    output.push_str("\n\n### Input\n\n");
    render_catalogue_schema(output, parameters, 0)?;
    output.push_str("\n### Output\n\n");
    render_catalogue_schema(output, output_schema, 0)?;
    Ok(())
}

fn render_catalogue_description(output: &mut String, description: &str) {
    for (index, line) in description.trim().lines().enumerate() {
        if index > 0 {
            output.push('\n');
        }
        if let Some(heading) = line.strip_prefix("## ") {
            output.push_str("### ");
            output.push_str(heading);
        } else {
            output.push_str(line);
        }
    }
}

fn render_catalogue_schema(output: &mut String, schema: &Value, depth: usize) -> Result<()> {
    if schema.get("type").and_then(Value::as_str) != Some("object") {
        output.push_str("- Value — `");
        output.push_str(&catalogue_schema_type(schema));
        output.push('`');
        if let Some(description) = schema.get("description").and_then(Value::as_str) {
            output.push_str(" — ");
            output.push_str(description);
            output.push('\n');
        } else {
            output.push_str(".\n");
        }
        render_catalogue_nested_objects(output, schema, depth)?;
        return Ok(());
    }

    let Some(properties) = schema.get("properties").and_then(Value::as_object) else {
        output.push_str("No fields.\n");
        return Ok(());
    };
    let required = schema
        .get("required")
        .and_then(Value::as_array)
        .map(|required| {
            required
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    let mut names = required.clone();
    names.extend(
        properties
            .keys()
            .map(String::as_str)
            .filter(|name| !required.contains(name)),
    );
    for name in names {
        let property = properties
            .get(name)
            .with_context(|| format!("schema is missing property `{name}`"))?;
        render_catalogue_property(output, name, property, required.contains(&name), depth)?;
    }
    Ok(())
}

fn render_catalogue_property(
    output: &mut String,
    name: &str,
    schema: &Value,
    required: bool,
    depth: usize,
) -> Result<()> {
    output.push_str(&"  ".repeat(depth));
    output.push_str("- `");
    output.push_str(name);
    if !required {
        output.push('?');
    }
    output.push_str(": ");
    output.push_str(&catalogue_schema_type(schema));
    output.push('`');
    if required {
        output.push_str(" (required)");
    }
    if let Some(description) = schema.get("description").and_then(Value::as_str) {
        output.push_str(" — ");
        output.push_str(description);
    }
    output.push('\n');

    render_catalogue_nested_objects(output, schema, depth)
}

fn render_catalogue_nested_objects(
    output: &mut String,
    schema: &Value,
    depth: usize,
) -> Result<()> {
    if let Some(variants) = schema
        .get("anyOf")
        .or_else(|| schema.get("oneOf"))
        .and_then(Value::as_array)
    {
        for variant in variants {
            render_catalogue_nested_objects(output, variant, depth)?;
        }
        return Ok(());
    }
    let nested = if schema.get("type").and_then(Value::as_str) == Some("array") {
        schema.get("items")
    } else {
        Some(schema)
    };
    if nested
        .and_then(|nested| nested.get("type"))
        .and_then(Value::as_str)
        == Some("object")
    {
        render_catalogue_schema(
            output,
            nested.context("nested schema is missing")?,
            depth + 1,
        )?;
    }
    Ok(())
}

fn catalogue_schema_type(schema: &Value) -> String {
    if let Some(variants) = schema
        .get("anyOf")
        .or_else(|| schema.get("oneOf"))
        .and_then(Value::as_array)
    {
        return variants
            .iter()
            .map(catalogue_schema_type)
            .collect::<Vec<_>>()
            .join(" | ");
    }
    if let Some(values) = schema.get("enum").and_then(Value::as_array) {
        return values
            .iter()
            .map(|value| match value {
                Value::String(value) => format!("\"{value}\""),
                value => value.to_string(),
            })
            .collect::<Vec<_>>()
            .join(" | ");
    }

    match schema.get("type").and_then(Value::as_str) {
        Some("array") => {
            let item = schema
                .get("items")
                .map(catalogue_schema_type)
                .unwrap_or_else(|| "value".to_string());
            format!("{item}[]")
        }
        Some("object") => "object".to_string(),
        Some(kind) => kind.to_string(),
        None => "value".to_string(),
    }
}

fn function_tool(name: &str, description: &str, parameters: Value, output_schema: Value) -> Value {
    // Strict Responses schemas require every property to be required, representing optional
    // controls as nullable. Keep these controls truly optional unless evaluations justify changing
    // the call shape.
    json!({
        "type": "function",
        "name": name,
        "description": description,
        "strict": false,
        "parameters": parameters,
        "output_schema": output_schema,
    })
}

fn bash_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "command": {"type": "string", "description": "Bash command to execute."},
            "timeout": {"type": "number", "exclusiveMinimum": 0, "maximum": MAX_TIMEOUT_SECONDS, "description": "Positive seconds before killing the process tree. Optional; no timeout by default."},
        },
        "required": ["command"],
        "additionalProperties": false,
    })
}

fn bash_output_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "stdout": {"type": "string", "description": "Bounded standard output."},
            "stderr": {"type": "string", "description": "Bounded standard error."},
            "exit_code": {"type": "integer", "description": "Process exit status. Timeout returns 124 and interruption returns 130."},
        },
        "required": ["stdout", "stderr", "exit_code"],
        "additionalProperties": false,
    })
}

fn read_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "path": {"type": "string", "description": "Path to the file, relative to the working directory or absolute."},
            "offset": {"type": "integer", "minimum": 1, "description": "Text files only: 1-indexed line number to start reading from."},
            "limit": {"type": "integer", "minimum": 1, "maximum": MAX_READ_LINES, "description": "Text files only: maximum number of lines to read, up to 2,000."},
            "detail": {"type": "string", "enum": ["high", "original"], "description": "Image files only: detail level. Defaults to `high`; use `original` to preserve exact resolution."},
        },
        "required": ["path"],
        "additionalProperties": false,
    })
}

fn read_output_schema() -> Value {
    json!({
        "description": "Bounded UTF-8 text or one image content item.",
        "anyOf": [
            {"type": "string"},
            {
                "type": "array",
                "minItems": 1,
                "maxItems": 1,
                "items": {
                    "type": "object",
                    "properties": {
                        "type": {"type": "string", "enum": ["input_image"]},
                        "image_url": {"type": "string", "description": "Prepared image data URL."},
                        "detail": {"type": "string", "enum": ["high", "original"], "description": "Requested image detail level."},
                    },
                    "required": ["type", "image_url", "detail"],
                    "additionalProperties": false,
                },
            },
        ],
    })
}

fn write_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "path": {"type": "string", "description": "Path to create or replace, relative to the working directory or absolute."},
            "content": {"type": "string", "description": "Complete file content."},
        },
        "required": ["path", "content"],
        "additionalProperties": false,
    })
}

fn edit_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "path": {"type": "string", "description": "Path to the UTF-8 text file, relative to the working directory or absolute."},
            "edits": {
                "type": "array",
                "minItems": 1,
                "description": "Non-overlapping replacements matched independently against the original content.",
                "items": {
                    "type": "object",
                    "properties": {
                        "oldText": {"type": "string", "minLength": 1, "description": "Non-empty exact text that must occur once."},
                        "newText": {"type": "string", "description": "Replacement text."},
                    },
                    "required": ["oldText", "newText"],
                    "additionalProperties": false,
                },
            },
        },
        "required": ["path", "edits"],
        "additionalProperties": false,
    })
}

#[cfg(test)]
#[path = "tools_tests.rs"]
mod tests;
