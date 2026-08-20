//! The fixed direct tool stack exposed through the Responses API.

use crate::ask_user_question::AskUserQuestionArgs;
use crate::ask_user_question::AskUserQuestionRequester;
use crate::ask_user_question::MAX_HEADER_CHARS;
use crate::ask_user_question::MAX_OPTION_DESCRIPTION_CHARS;
use crate::ask_user_question::MAX_OPTION_LABEL_CHARS;
use crate::ask_user_question::MAX_OPTIONS;
use crate::ask_user_question::MAX_PREVIEW_CHARS;
use crate::ask_user_question::MAX_QUESTION_CHARS;
use crate::ask_user_question::MAX_QUESTIONS;
use crate::ask_user_question::MIN_OPTIONS;
use crate::ask_user_question::TOOL_NAME as ASK_USER_QUESTION_NAME;
use crate::deepwork::CoordinateSpecialistArgs;
use crate::deepwork::DeepworkRequester;
use crate::deepwork::SpecialistRole;
use crate::deepwork::TOOL_NAME as COORDINATE_SPECIALIST_NAME;
use crate::events::AgentEvent;
use crate::private_fs::AnchoredPath;
use crate::private_fs::DirectoryHandle;
use crate::private_fs::FileObjectIdentity;
use crate::private_fs::FileSnapshot;
use crate::process_runtime::LiveOutputAction;
use crate::process_runtime::OutputStream;
use crate::protocol::FileChange;
use crate::protocol::FunctionCallOutputContentItem;
use crate::protocol::ImageDetail;
use crate::protocol::ToolFileChange;
use crate::rollout::MAX_TOOL_PRE_STATE_HASH_BYTES;
use crate::rollout::ToolContentDigest;
use crate::rollout::ToolLifecycleJournal;
use crate::rollout::ToolMutationEvidence;
use crate::rollout::ToolPathResolutionEvidence;
use crate::rollout::ToolStagingEvidence;
use crate::rollout::ToolSymlinkEvidence;
use crate::rollout::ToolTargetPreState;
use crate::truncation::TruncationPolicy;
use crate::truncation::approx_bytes_for_tokens;
use anyhow::Context;
use anyhow::Result;
use anyhow::anyhow;
use serde::Deserialize;
use serde_json::Value;
use serde_json::json;
use similar::TextDiff;
use std::borrow::Cow;
use std::collections::HashSet;
use std::ffi::OsStr;
use std::ffi::OsString;
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
// Leave room for the continuation marker inside the 10,000-token tool-output bound.
const MAX_READ_BYTES: usize = 39_000;
const MAX_EDIT_FILE_BYTES: usize = 64 * 1024 * 1024;
const MAX_FORWARDED_LIVE_OUTPUT_BYTES: usize = 128 * 1024;
const MAX_FILE_CHANGE_PREVIEW_BYTES: usize = 2 * 1024 * 1024;
// `similar` tokenizes both inputs before its deadline starts. Keep that eager allocation bounded
// while leaving ample headroom for ordinary multi-thousand-line source files.
const MAX_FILE_CHANGE_DIFF_LINES: usize = 200_000;
// Match current upstream Codex's deadline for interactive file diffs.
const MAX_FILE_CHANGE_DIFF_DURATION: Duration = Duration::from_millis(100);
const MAX_WRITE_SYMLINK_HOPS: usize = 64;
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
        lifecycle: Option<ToolLifecycleJournal>,
    ) -> ToolResult {
        let input = ensure_not_cancelled(&cancellation, &self.name)
            .and_then(|()| parse_arguments(&self.name, &self.arguments));
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
                        lifecycle.clone(),
                    )
                    .await
            }
            Err(error) => Err(error),
        };
        let result = match result {
            Ok(result) => result,
            Err(error) => {
                let requires_inspection = error.is::<MutationOutcomeUnknown>();
                ToolResult::error(format!("{error:#}"), truncation_policy)
                    .requiring_inspection(requires_inspection)
            }
        };

        if matches!(self.name.as_str(), BASH_NAME | WRITE_NAME | EDIT_NAME)
            && let Some(lifecycle) = &lifecycle
            && let Err(error) = lifecycle
                .record_finished_async(
                    &self.call_id,
                    result.body.clone(),
                    result.display.as_ref().err().cloned(),
                    result.file_change.clone(),
                    result.requires_inspection,
                )
                .await
        {
            tracing::warn!(
                %error,
                call_id = %self.call_id,
                tool = %self.name,
                "failed to record tool completion lifecycle"
            );
        }

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
            requires_inspection,
        } = output;
        let (error, inspection) = match display {
            Err(error) => (Some(error), None),
            Ok(Value::String(message)) if requires_inspection => (None, Some(message)),
            Ok(output) if requires_inspection => (None, Some(output.to_string())),
            Ok(_) => (None, None),
        };
        let completion = ToolCompletion {
            call_id: self.call_id.clone(),
            error,
            file_change,
            inspection,
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
    pub(crate) inspection: Option<String>,
}

pub(crate) struct ToolResult {
    body: Value,
    display: std::result::Result<Value, String>,
    file_change: Option<ToolFileChange>,
    requires_inspection: bool,
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
            requires_inspection: false,
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
            requires_inspection: false,
        })
    }

    fn structured(output: Value, policy: TruncationPolicy) -> Result<Self> {
        let body =
            serde_json::to_string(&output).context("failed to encode structured tool output")?;
        if body.len() > bounded_output_bytes(policy) {
            return Err(anyhow!("structured tool output exceeds its output bound"));
        }
        Ok(Self {
            body: Value::String(body),
            display: Ok(output),
            file_change: None,
            requires_inspection: false,
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
            requires_inspection: false,
        })
    }

    fn error(error: String, policy: TruncationPolicy) -> Self {
        let error = bounded_text(error, policy);
        Self {
            body: Value::String(error.clone()),
            display: Err(error),
            file_change: None,
            requires_inspection: false,
        }
    }

    fn with_file_change(mut self, path: PathBuf, change: FileChange) -> Self {
        self.file_change = Some(ToolFileChange { path, change });
        self
    }

    fn requiring_inspection(mut self, required: bool) -> Self {
        self.requires_inspection = required;
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
    ask_user_question: Option<AskUserQuestionRequester>,
    ask_user_question_enabled: bool,
    deepwork: Option<DeepworkRequester>,
    specialist_coordination_enabled: bool,
}

impl ToolRuntime {
    pub(crate) fn new(cwd: PathBuf) -> Self {
        Self {
            cwd,
            ask_user_question: None,
            ask_user_question_enabled: false,
            deepwork: None,
            specialist_coordination_enabled: false,
        }
    }

    pub(crate) fn set_ask_user_question_requester(&mut self, requester: AskUserQuestionRequester) {
        self.ask_user_question = Some(requester);
    }

    pub(crate) fn set_ask_user_question_enabled(&mut self, enabled: bool) {
        self.ask_user_question_enabled = enabled;
    }

    pub(crate) fn ask_user_question_activated(&self) -> bool {
        self.ask_user_question_enabled
    }

    pub(crate) fn ask_user_question_enabled(&self) -> bool {
        self.ask_user_question_activated() && self.ask_user_question.is_some()
    }

    pub(crate) fn set_deepwork_requester(&mut self, requester: DeepworkRequester) {
        self.deepwork = Some(requester);
    }

    pub(crate) fn deepwork_requester(&self) -> Option<DeepworkRequester> {
        self.deepwork.clone()
    }

    pub(crate) fn set_specialist_coordination_enabled(&mut self, enabled: bool) {
        self.specialist_coordination_enabled = enabled;
    }

    pub(crate) fn specialist_coordination_activated(&self) -> bool {
        self.specialist_coordination_enabled
    }

    pub(crate) fn specialist_coordination_enabled(&self) -> bool {
        self.specialist_coordination_activated() && self.deepwork.is_some()
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
        lifecycle: Option<ToolLifecycleJournal>,
    ) -> Result<ToolResult> {
        match name {
            BASH_NAME => {
                self.bash(
                    call_id,
                    input,
                    truncation_policy,
                    events,
                    cancellation,
                    lifecycle.as_ref(),
                )
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
                let call_id = call_id.to_string();
                blocking_tool(cancellation, move |cancellation| {
                    write_with_lifecycle(&cwd, input, &cancellation, &call_id, lifecycle.as_ref())
                })
                .await
            }
            EDIT_NAME => {
                let cwd = self.cwd.clone();
                let call_id = call_id.to_string();
                blocking_tool(cancellation, move |cancellation| {
                    edit_with_lifecycle(&cwd, input, &cancellation, &call_id, lifecycle.as_ref())
                })
                .await
            }
            ASK_USER_QUESTION_NAME => {
                if !self.ask_user_question_enabled() {
                    return Err(anyhow!("unknown tool `{name}`"));
                }
                let arguments: AskUserQuestionArgs = deserialize_arguments(input)?;
                arguments.validate()?;
                let requester = self
                    .ask_user_question
                    .as_ref()
                    .context("ask_user_question requester disappeared after tool admission")?;
                let response = requester
                    .request(call_id.to_string(), arguments, &cancellation)
                    .await?;
                ToolResult::structured(serde_json::to_value(response)?, truncation_policy)
            }
            COORDINATE_SPECIALIST_NAME => {
                if !self.specialist_coordination_enabled() {
                    return Err(anyhow!("unknown tool `{name}`"));
                }
                let arguments: CoordinateSpecialistArgs = deserialize_arguments(input)?;
                arguments.validate()?;
                let requester = self
                    .deepwork
                    .as_ref()
                    .context("deepwork requester disappeared after specialist tool admission")?;
                let response = requester.coordinate(arguments, &cancellation).await?;
                ToolResult::structured(serde_json::to_value(response)?, truncation_policy)
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
        lifecycle: Option<&ToolLifecycleJournal>,
    ) -> Result<ToolResult> {
        let arguments: BashArgs = deserialize_arguments(input)?;
        let timeout = arguments.timeout.map(resolve_timeout).transpose()?;
        ensure_not_cancelled(&cancellation, BASH_NAME)?;
        if let Some(lifecycle) = lifecycle {
            lifecycle.record_started_async(call_id).await?;
        }
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

#[derive(Debug)]
struct MutationOutcomeUnknown(String);

impl std::fmt::Display for MutationOutcomeUnknown {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for MutationOutcomeUnknown {}

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
    let line_limit = arguments.limit;
    if line_limit == Some(0) {
        return Err(anyhow!("read.limit must be at least 1"));
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
    while line_limit.is_none_or(|limit| emitted_lines < limit) {
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
    if line_limit.is_some_and(|limit| emitted_lines == limit) && !reader.fill_buf()?.is_empty() {
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
        let bound = line_limit.map_or_else(
            || format!("{MAX_READ_BYTES} bytes"),
            |limit| {
                let line_unit = if limit == 1 { "line" } else { "lines" };
                format!("{limit} {line_unit} or {MAX_READ_BYTES} bytes")
            },
        );
        output.push_str(&format!(
            "\n[Output bounded at {bound}. Use offset={next_offset} to continue.]"
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

fn write_with_lifecycle(
    cwd: &Path,
    input: Value,
    cancellation: &CancellationToken,
    call_id: &str,
    lifecycle: Option<&ToolLifecycleJournal>,
) -> Result<ToolResult> {
    ensure_not_cancelled(cancellation, WRITE_NAME)?;
    let arguments: WriteArgs = deserialize_arguments(input)?;
    ensure_not_cancelled(cancellation, WRITE_NAME)?;
    let path = resolve_path(cwd, &arguments.path);
    let resolved = resolve_symlink_write_path(&path)?;
    ensure_not_cancelled(cancellation, WRITE_NAME)?;
    let write_path = resolved.target().to_path_buf();
    let parent = write_path
        .parent()
        .ok_or_else(|| anyhow!("write path `{}` has no parent directory", path.display()))?;
    let missing_parent = highest_missing_parent(parent);
    let mut target = match AnchoredPath::open(&write_path) {
        Ok(target) => Some(target),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound && missing_parent.is_some() => {
            None
        }
        Err(error) => {
            return Err(error)
                .with_context(|| format!("unable to access `{}`", write_path.display()));
        }
    };
    if let Some(lifecycle) = lifecycle {
        lifecycle
            .record_started(call_id)
            .context("write mutation was not attempted because lifecycle start was not saved")?;
    }
    let WritePreparation {
        preview,
        pre_state,
        expectation,
    } = write_preparation(
        &write_path,
        target.as_ref(),
        &arguments.content,
        lifecycle.is_some(),
        cancellation,
    )?;
    ensure_not_cancelled(cancellation, WRITE_NAME)?;

    let staging_name = atomic_staging_name();
    let mut evidence = if lifecycle.is_some() {
        Some(ToolMutationEvidence {
            target: write_path.clone(),
            target_parent: target.as_ref().map(AnchoredPath::parent_identity),
            path_resolution: resolved.lifecycle_evidence(),
            pre_state,
            post_state: content_digest_cancellable(
                arguments.content.as_bytes(),
                cancellation,
                WRITE_NAME,
            )?,
            staging: Some(ToolStagingEvidence {
                name: staging_name.clone(),
                directory: None,
                content: None,
            }),
            missing_parent: missing_parent.clone(),
        })
    } else {
        None
    };
    if let (Some(lifecycle), Some(evidence)) = (lifecycle, evidence.as_ref()) {
        lifecycle
            .record_mutation_prepared(call_id, evidence.clone())
            .context("write mutation was not attempted because lifecycle evidence was not saved")?;
    }
    // Recheck after lifecycle I/O. Once parent creation or private staging starts, finish the
    // attempt so cancellation cannot be reported after a committed replacement.
    ensure_not_cancelled(cancellation, WRITE_NAME)?;
    let mutation = (|| -> Result<AtomicWriteOutcome> {
        if target.is_none() {
            verify_resolved_write_path(&resolved)?;
            let anchored = AnchoredPath::create_parent_directories(&write_path).with_context(|| {
                format!(
                    "atomic replacement of `{}` was not committed because `{}` could not be created",
                    write_path.display(),
                    parent.display()
                )
            })?;
            if let (Some(lifecycle), Some(evidence)) = (lifecycle, evidence.as_mut()) {
                evidence.target_parent = Some(anchored.parent_identity());
                lifecycle
                    .record_mutation_prepared(call_id, evidence.clone())
                    .context("atomic replacement was not committed after parent creation because refined lifecycle evidence was not saved")?;
            }
            target = Some(anchored);
        }
        let anchored = target
            .as_ref()
            .ok_or_else(|| anyhow!("write target is not anchored before replacement"))?;
        let outcome = write_file_atomically(
            &resolved,
            anchored,
            arguments.content.as_bytes(),
            expectation,
            &staging_name,
            |staging| {
                if let (Some(lifecycle), Some(evidence)) = (lifecycle, evidence.as_mut()) {
                    evidence.staging = Some(staging);
                    lifecycle.record_mutation_prepared(call_id, evidence.clone())?;
                }
                Ok(())
            },
        )
        .with_context(|| format!("unable to write `{}`", path.display()))?;
        Ok(outcome)
    })();
    let outcome = match mutation {
        Ok(outcome) => outcome,
        Err(error) => {
            if let Some(parent) = missing_parent.as_deref().filter(|parent| {
                std::fs::symlink_metadata(parent).is_ok_and(|value| value.is_dir())
            }) {
                return Err(error.context(format!(
                    "the directory `{}` may remain from parent creation",
                    parent.display()
                )));
            }
            return Err(error);
        }
    };

    let result = write_result(&path, &arguments.content, &outcome);
    let change = if outcome.path_requires_inspection() {
        None
    } else {
        match preview {
            WritePreview::Add => Some(FileChange::Add {
                content: arguments.content,
            }),
            WritePreview::Update(change) => Some(change),
            WritePreview::Omit => None,
        }
    };
    Ok(match change {
        Some(change) => result.with_file_change(path, change),
        None => result,
    })
}

struct WritePreparation {
    preview: WritePreview,
    pre_state: ToolTargetPreState,
    expectation: AtomicDestinationExpectation,
}

enum WritePreview {
    Add,
    Update(FileChange),
    Omit,
}

#[derive(Clone, Copy)]
enum AtomicDestinationExpectation {
    Absent,
    Existing(FileSnapshot),
}

fn write_preparation(
    path: &Path,
    target: Option<&AnchoredPath>,
    replacement: &str,
    capture_state: bool,
    cancellation: &CancellationToken,
) -> Result<WritePreparation> {
    let Some(target) = target else {
        return Ok(absent_write_preparation(replacement));
    };
    if !target.parent_path_is_current().unwrap_or(false) {
        return Err(target_changed_before_commit(WRITE_NAME, path));
    }
    let metadata = target
        .entry_metadata()
        .with_context(|| format!("unable to inspect `{}`", path.display()))?;
    let Some(metadata) = metadata else {
        return Ok(absent_write_preparation(replacement));
    };
    if !metadata.is_file() {
        return Err(anyhow!(
            "write path `{}` is not a regular file",
            path.display()
        ));
    }
    let snapshot = metadata.snapshot();
    let expectation = AtomicDestinationExpectation::Existing(snapshot);

    // Diff previews stay capped at 2 MiB, while lifecycle hashing has a separate 64 MiB I/O
    // budget. Both use the same descriptor snapshot when enabled, and neither is required for a
    // valid replacement of an otherwise writable regular file.
    let capture_preview = snapshot.byte_len() <= MAX_FILE_CHANGE_PREVIEW_BYTES as u64
        && replacement.len() <= MAX_FILE_CHANGE_PREVIEW_BYTES
        && !replacement.contains('\0');
    let capture_digest = capture_state && snapshot.byte_len() <= MAX_TOOL_PRE_STATE_HASH_BYTES;
    if !capture_preview && !capture_digest {
        return Ok(unknown_existing_write_preparation(expectation));
    }

    let mut file = match target.open_for_read() {
        Ok(file) => file,
        Err(_) => return Ok(unknown_existing_write_preparation(expectation)),
    };
    match file.metadata() {
        Ok(metadata)
            if metadata.is_file() && crate::private_fs::file_snapshot(&metadata) == snapshot => {}
        Ok(_) | Err(_) => return Err(target_changed_before_commit(WRITE_NAME, path)),
    }

    let (pre_state, preview) = if capture_preview {
        // A previewable target is at most 2 MiB, so one exact bounded read serves both preview and
        // lifecycle digest without an avoidable second pass.
        let bytes = match read_exact_cancellable(
            &mut file,
            snapshot.byte_len(),
            cancellation,
            WRITE_NAME,
        ) {
            Ok(bytes) => bytes,
            Err(_) => {
                ensure_not_cancelled(cancellation, WRITE_NAME)?;
                return Ok(unknown_existing_write_preparation(expectation));
            }
        };
        ensure_target_snapshot_is_current(WRITE_NAME, path, target, &file, snapshot)?;
        let pre_state = if capture_digest {
            ToolTargetPreState::Digest(ToolContentDigest::from_bytes(&bytes))
        } else {
            ToolTargetPreState::Unknown
        };
        let preview = match String::from_utf8(bytes) {
            Ok(original) => bounded_update_file_change(&original, replacement)
                .map_or(WritePreview::Omit, WritePreview::Update),
            Err(_) => WritePreview::Omit,
        };
        (pre_state, preview)
    } else {
        debug_assert!(capture_digest);
        let mut cancellable = CancellableReader::new(&mut file, cancellation, WRITE_NAME);
        let mut bounded = (&mut cancellable).take(snapshot.byte_len());
        let digest = match ToolContentDigest::from_reader(&mut bounded) {
            Ok(digest) if digest.byte_len() == snapshot.byte_len() => digest,
            Ok(_) | Err(_) => {
                ensure_not_cancelled(cancellation, WRITE_NAME)?;
                return Ok(unknown_existing_write_preparation(expectation));
            }
        };
        ensure_not_cancelled(cancellation, WRITE_NAME)?;
        ensure_target_snapshot_is_current(WRITE_NAME, path, target, &file, snapshot)?;
        (ToolTargetPreState::Digest(digest), WritePreview::Omit)
    };

    Ok(WritePreparation {
        preview,
        pre_state,
        expectation,
    })
}

fn absent_write_preparation(replacement: &str) -> WritePreparation {
    WritePreparation {
        preview: if replacement.len() <= MAX_FILE_CHANGE_PREVIEW_BYTES
            && !replacement.contains('\0')
        {
            WritePreview::Add
        } else {
            WritePreview::Omit
        },
        pre_state: ToolTargetPreState::Absent,
        expectation: AtomicDestinationExpectation::Absent,
    }
}

fn unknown_existing_write_preparation(
    expectation: AtomicDestinationExpectation,
) -> WritePreparation {
    WritePreparation {
        preview: WritePreview::Omit,
        pre_state: ToolTargetPreState::Unknown,
        expectation,
    }
}

fn ensure_target_snapshot_is_current(
    tool: &str,
    path: &Path,
    target: &AnchoredPath,
    file: &File,
    expected: FileSnapshot,
) -> Result<()> {
    let opened_is_current = file.metadata().is_ok_and(|metadata| {
        metadata.is_file() && crate::private_fs::file_snapshot(&metadata) == expected
    });
    let path_is_current = target.entry_metadata().is_ok_and(|metadata| {
        metadata.is_some_and(|metadata| metadata.is_file() && metadata.snapshot() == expected)
    });
    if opened_is_current && path_is_current && target.parent_path_is_current().unwrap_or(false) {
        Ok(())
    } else {
        Err(target_changed_before_commit(tool, path))
    }
}

fn target_changed_before_commit(tool: &str, path: &Path) -> anyhow::Error {
    anyhow!(
        "{tool} target `{}` changed while the replacement was being prepared; atomic replacement was not committed",
        path.display()
    )
}

fn read_exact_cancellable(
    reader: &mut impl Read,
    bytes: u64,
    cancellation: &CancellationToken,
    tool: &str,
) -> std::io::Result<Vec<u8>> {
    let capacity = usize::try_from(bytes).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::FileTooLarge,
            "file is too large to fit in memory",
        )
    })?;
    let mut output = Vec::with_capacity(capacity);
    let mut buffer = [0_u8; 64 * 1024];
    let mut remaining = bytes;
    while remaining > 0 {
        cancellation_checkpoint(cancellation, tool)?;
        let limit = usize::try_from(remaining.min(buffer.len() as u64)).unwrap_or(buffer.len());
        let read = reader.read(&mut buffer[..limit])?;
        if read == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "file changed while it was being read",
            ));
        }
        output.extend_from_slice(&buffer[..read]);
        remaining -= u64::try_from(read).unwrap_or(remaining);
    }
    cancellation_checkpoint(cancellation, tool)?;
    Ok(output)
}

struct CancellableReader<'a, R> {
    reader: &'a mut R,
    cancellation: &'a CancellationToken,
    tool: &'a str,
}

impl<'a, R> CancellableReader<'a, R> {
    fn new(reader: &'a mut R, cancellation: &'a CancellationToken, tool: &'a str) -> Self {
        Self {
            reader,
            cancellation,
            tool,
        }
    }
}

impl<R: Read> Read for CancellableReader<'_, R> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        cancellation_checkpoint(self.cancellation, self.tool)?;
        self.reader.read(buffer)
    }
}

fn content_digest_cancellable(
    bytes: &[u8],
    cancellation: &CancellationToken,
    tool: &str,
) -> Result<ToolContentDigest> {
    ToolContentDigest::from_bytes_with_checkpoint(bytes, || {
        ensure_not_cancelled(cancellation, tool)
    })
}

fn cancellation_checkpoint(cancellation: &CancellationToken, tool: &str) -> std::io::Result<()> {
    if cancellation.is_cancelled() {
        Err(std::io::Error::new(
            std::io::ErrorKind::Interrupted,
            format!("{tool} was interrupted"),
        ))
    } else {
        Ok(())
    }
}

fn highest_missing_parent(parent: &Path) -> Option<PathBuf> {
    let mut current = Some(parent);
    let mut highest_missing = None;
    while let Some(candidate) = current {
        match std::fs::symlink_metadata(candidate) {
            Ok(_) => break,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                highest_missing = Some(candidate.to_path_buf());
            }
            Err(_) => return None,
        }
        current = candidate.parent();
    }
    highest_missing
}

fn bounded_update_file_change(original: &str, replacement: &str) -> Option<FileChange> {
    if original.len() > MAX_FILE_CHANGE_PREVIEW_BYTES
        || replacement.len() > MAX_FILE_CHANGE_PREVIEW_BYTES
        || original.contains('\0')
        || replacement.contains('\0')
    {
        return None;
    }
    let original_lines = bounded_diff_line_count(original, MAX_FILE_CHANGE_DIFF_LINES)?;
    let remaining_lines = MAX_FILE_CHANGE_DIFF_LINES.checked_sub(original_lines)?;
    bounded_diff_line_count(replacement, remaining_lines)?;

    // Match upstream Codex's deadline-aware Myers diff without suppressing ordinary large files.
    // Serialize through a bounded writer so an oversized preview is never fully allocated merely
    // to discover that it exceeds the display contract.
    let diff = TextDiff::configure()
        .timeout(MAX_FILE_CHANGE_DIFF_DURATION)
        .diff_lines(original, replacement);
    let mut output = BoundedFileChangePreview::default();
    diff.unified_diff()
        .header("original", "modified")
        .to_writer(&mut output)
        .ok()?;
    Some(FileChange::Update {
        unified_diff: String::from_utf8(output.bytes).ok()?,
        move_path: None,
    })
}

fn bounded_diff_line_count(text: &str, limit: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut count = 0_usize;
    let mut index = 0_usize;
    while index < bytes.len() {
        match bytes[index] {
            b'\r' => {
                count = count.checked_add(1)?;
                if count > limit {
                    return None;
                }
                index += usize::from(bytes.get(index + 1) == Some(&b'\n')) + 1;
            }
            b'\n' => {
                count = count.checked_add(1)?;
                if count > limit {
                    return None;
                }
                index += 1;
            }
            _ => index += 1,
        }
    }
    if bytes
        .last()
        .is_some_and(|last| !matches!(last, b'\r' | b'\n'))
    {
        count = count.checked_add(1)?;
    }
    (count <= limit).then_some(count)
}

#[derive(Default)]
struct BoundedFileChangePreview {
    bytes: Vec<u8>,
}

impl Write for BoundedFileChangePreview {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        let Some(new_len) = self.bytes.len().checked_add(buffer.len()) else {
            return Err(std::io::Error::other("file-change preview is too large"));
        };
        if new_len > MAX_FILE_CHANGE_PREVIEW_BYTES {
            return Err(std::io::Error::other("file-change preview is too large"));
        }
        self.bytes
            .try_reserve(buffer.len())
            .map_err(|_| std::io::Error::other("file-change preview allocation failed"))?;
        self.bytes.extend_from_slice(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn write_result(path: &Path, content: &str, outcome: &AtomicWriteOutcome) -> ToolResult {
    let mut message = if outcome.path_requires_inspection() {
        format!(
            "An atomic replacement attempt for {} bytes at {} completed in the pinned parent directory; the final path requires inspection",
            content.len(),
            path.display()
        )
    } else {
        format!("Wrote {} bytes to {}", content.len(), path.display())
    };
    append_atomic_write_warnings(&mut message, outcome);
    ToolResult::text(message).requiring_inspection(outcome.requires_inspection())
}

fn append_atomic_write_warnings(message: &mut String, outcome: &AtomicWriteOutcome) {
    if let Some(path) = &outcome.cleanup_residue {
        message.push_str(&format!(
            ". Atomic replacement committed, but the private staging directory `{}` may remain",
            path.display()
        ));
    }
    if let Some(parent) = &outcome.rebound_parent {
        message.push_str(&format!(
            ". Atomic replacement committed in the pinned parent directory, but `{}` changed during commit; inspect the requested path before retrying",
            parent.display()
        ));
    }
    if let Some(error) = &outcome.commit_reported_error {
        message.push_str(&format!(
            ". The intended file state is present at the destination, but the commit operation reported `{error}`; inspect it before retrying",
        ));
    }
    if outcome.requested_path_changed_after_commit {
        message.push_str(
            ". Atomic replacement committed, but the requested symlink route changed immediately afterward; inspect it before retrying",
        );
    }
    if outcome.target_changed_after_commit {
        message.push_str(
            ". Atomic replacement committed, but the target entry changed immediately afterward; inspect it before retrying",
        );
    }
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

fn edit_with_lifecycle(
    cwd: &Path,
    input: Value,
    cancellation: &CancellationToken,
    call_id: &str,
    lifecycle: Option<&ToolLifecycleJournal>,
) -> Result<ToolResult> {
    ensure_not_cancelled(cancellation, EDIT_NAME)?;
    let arguments: EditArgs = deserialize_arguments(input)?;
    ensure_not_cancelled(cancellation, EDIT_NAME)?;
    if arguments.edits.is_empty() {
        return Err(anyhow!("edit.edits must contain at least one replacement"));
    }
    let path = resolve_path(cwd, &arguments.path);
    let resolved = resolve_symlink_write_path(&path)?;
    ensure_not_cancelled(cancellation, EDIT_NAME)?;
    let write_path = resolved.target().to_path_buf();
    if let Some(lifecycle) = lifecycle {
        lifecycle
            .record_started(call_id)
            .context("edit mutation was not attempted because lifecycle start was not saved")?;
    }

    let target = AnchoredPath::open(&write_path)
        .with_context(|| format!("unable to read `{}` as UTF-8 text", path.display()))?;
    if !target.parent_path_is_current().unwrap_or(false) {
        return Err(target_changed_before_commit(EDIT_NAME, &path));
    }
    let metadata = target
        .entry_metadata()
        .with_context(|| format!("unable to read `{}` as UTF-8 text", path.display()))?
        .filter(|metadata| metadata.is_file())
        .ok_or_else(|| anyhow!("unable to read `{}` as UTF-8 text", path.display()))?;
    let snapshot = metadata.snapshot();
    if snapshot.byte_len() > MAX_EDIT_FILE_BYTES as u64 {
        return Err(anyhow!(
            "edit target `{}` exceeds the {} MiB edit limit; use bash for a bounded transformation",
            path.display(),
            MAX_EDIT_FILE_BYTES / (1024 * 1024)
        ));
    }
    let mut file = target
        .open_for_read()
        .with_context(|| format!("unable to read `{}` as UTF-8 text", path.display()))?;
    match file.metadata() {
        Ok(metadata)
            if metadata.is_file() && crate::private_fs::file_snapshot(&metadata) == snapshot => {}
        Ok(_) | Err(_) => return Err(target_changed_before_commit(EDIT_NAME, &path)),
    }
    let bytes = read_exact_cancellable(&mut file, snapshot.byte_len(), cancellation, EDIT_NAME);
    ensure_not_cancelled(cancellation, EDIT_NAME)?;
    let bytes =
        bytes.with_context(|| format!("unable to read `{}` as UTF-8 text", path.display()))?;
    ensure_target_snapshot_is_current(EDIT_NAME, &path, &target, &file, snapshot)?;
    let original = String::from_utf8(bytes)
        .with_context(|| format!("unable to read `{}` as UTF-8 text", path.display()))?;
    ensure_not_cancelled(cancellation, EDIT_NAME)?;

    let normalized_original = normalize_line_endings(&original);
    let preferred_line_ending = preferred_line_ending(&original);
    let mut normalized_edits = Vec::new();
    normalized_edits
        .try_reserve_exact(arguments.edits.len())
        .map_err(|_| anyhow!("edit arguments for `{}` are too large", path.display()))?;
    for edit in &arguments.edits {
        ensure_not_cancelled(cancellation, EDIT_NAME)?;
        normalized_edits.push((
            normalize_line_endings(&edit.old_text),
            restore_line_endings(
                normalize_line_endings(&edit.new_text),
                preferred_line_ending,
            ),
        ));
    }
    let mut ranges = Vec::new();
    ranges
        .try_reserve_exact(normalized_edits.len())
        .map_err(|_| anyhow!("edit arguments for `{}` are too large", path.display()))?;
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
        .try_fold(original.len(), |length, (start, end, index)| {
            length
                .checked_sub(end - start)?
                .checked_add(normalized_edits[*index].1.len())
        })
        .ok_or_else(|| anyhow!("edit result for `{}` is too large", path.display()))?;
    let mut updated = String::new();
    updated
        .try_reserve_exact(updated_len)
        .map_err(|_| anyhow!("edit result for `{}` is too large", path.display()))?;
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
    let file_change = bounded_update_file_change(&original, &updated);
    ensure_not_cancelled(cancellation, EDIT_NAME)?;
    let staging_name = atomic_staging_name();
    let mut evidence = if lifecycle.is_some() {
        Some(ToolMutationEvidence {
            target: write_path,
            target_parent: Some(target.parent_identity()),
            path_resolution: resolved.lifecycle_evidence(),
            pre_state: ToolTargetPreState::Digest(content_digest_cancellable(
                original.as_bytes(),
                cancellation,
                EDIT_NAME,
            )?),
            post_state: content_digest_cancellable(updated.as_bytes(), cancellation, EDIT_NAME)?,
            staging: Some(ToolStagingEvidence {
                name: staging_name.clone(),
                directory: None,
                content: None,
            }),
            missing_parent: None,
        })
    } else {
        None
    };
    if let (Some(lifecycle), Some(evidence)) = (lifecycle, evidence.as_ref()) {
        lifecycle
            .record_mutation_prepared(call_id, evidence.clone())
            .context("edit mutation was not attempted because lifecycle evidence was not saved")?;
    }
    // Lifecycle recording may perform I/O, so honor a cancellation that arrived before staging.
    // After staging starts, the helper either commits once or reports that it did not commit.
    ensure_not_cancelled(cancellation, EDIT_NAME)?;
    let outcome = write_file_atomically(
        &resolved,
        &target,
        updated.as_bytes(),
        AtomicDestinationExpectation::Existing(snapshot),
        &staging_name,
        |staging| {
            if let (Some(lifecycle), Some(evidence)) = (lifecycle, evidence.as_mut()) {
                evidence.staging = Some(staging);
                lifecycle.record_mutation_prepared(call_id, evidence.clone())?;
            }
            Ok(())
        },
    )?;
    let mut message = if outcome.path_requires_inspection() {
        format!(
            "An atomic edit replacement for {} block(s) at {} completed in the pinned parent directory; the final path requires inspection",
            arguments.edits.len(),
            path.display()
        )
    } else {
        format!(
            "Replaced {} block(s) in {}",
            arguments.edits.len(),
            path.display()
        )
    };
    append_atomic_write_warnings(&mut message, &outcome);
    let result = ToolResult::text(message).requiring_inspection(outcome.requires_inspection());
    Ok(
        match file_change.filter(|_| !outcome.path_requires_inspection()) {
            Some(change) => result.with_file_change(path, change),
            None => result,
        },
    )
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

fn restore_line_endings<'a>(text: Cow<'a, str>, line_ending: &str) -> Cow<'a, str> {
    if line_ending == "\n" {
        text
    } else {
        Cow::Owned(text.replace('\n', line_ending))
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

struct ResolvedWritePath {
    requested: PathBuf,
    target: PathBuf,
    symlinks: Vec<ToolSymlinkEvidence>,
}

impl ResolvedWritePath {
    fn target(&self) -> &Path {
        &self.target
    }

    fn is_current(&self) -> bool {
        self.symlinks.iter().all(ToolSymlinkEvidence::is_current)
    }

    fn lifecycle_evidence(&self) -> Option<ToolPathResolutionEvidence> {
        (!self.symlinks.is_empty()).then(|| ToolPathResolutionEvidence {
            requested: self.requested.clone(),
            symlinks: self.symlinks.clone(),
        })
    }
}

// Resolve final symlink components before reading and retain their identities through commit.
// Parent-directory components are covered separately by `AnchoredPath`'s pinned descriptor. These
// checks narrow ordinary rebinding races but cannot turn POSIX rename into compare-and-replace.
fn resolve_symlink_write_path(path: &Path) -> Result<ResolvedWritePath> {
    let requested = path.to_path_buf();
    let mut current = requested.clone();
    let mut symlinks = Vec::new();
    let mut visited = HashSet::new();
    let mut hops = 0_usize;
    loop {
        let metadata = match std::fs::symlink_metadata(&current) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(ResolvedWritePath {
                    requested,
                    target: current,
                    symlinks,
                });
            }
            Err(error) => {
                return Err(error).with_context(|| {
                    format!(
                        "unable to resolve write path `{}`; mutation was not attempted",
                        requested.display()
                    )
                });
            }
        };
        if !metadata.file_type().is_symlink() {
            return Ok(ResolvedWritePath {
                requested,
                target: current,
                symlinks,
            });
        }
        let snapshot = crate::private_fs::file_snapshot(&metadata);
        if hops >= MAX_WRITE_SYMLINK_HOPS || !visited.insert(snapshot.object_identity()) {
            return Err(anyhow!(
                "write path `{}` contains a symlink loop or exceeds {MAX_WRITE_SYMLINK_HOPS} final-component symlink hops; mutation was not attempted",
                requested.display()
            ));
        }
        hops += 1;
        let link_target = std::fs::read_link(&current).with_context(|| {
            format!(
                "unable to resolve symlink `{}`; mutation was not attempted",
                current.display()
            )
        })?;
        let link_is_current = std::fs::symlink_metadata(&current).is_ok_and(|metadata| {
            metadata.file_type().is_symlink()
                && crate::private_fs::file_snapshot(&metadata) == snapshot
        });
        if !link_is_current {
            return Err(anyhow!(
                "write path `{}` changed while its symlinks were being resolved; mutation was not attempted",
                requested.display()
            ));
        }
        symlinks.push(ToolSymlinkEvidence {
            path: current.clone(),
            snapshot,
        });
        current = if link_target.is_absolute() {
            link_target
        } else if let Some(parent) = current.parent() {
            parent.join(link_target)
        } else {
            return Err(anyhow!(
                "symlink `{}` has no parent path; mutation was not attempted",
                current.display()
            ));
        };
    }
}

fn verify_resolved_write_path(resolved: &ResolvedWritePath) -> Result<()> {
    if resolved.is_current() {
        Ok(())
    } else {
        Err(anyhow!(
            "requested write path `{}` changed before atomic replacement; atomic replacement was not committed",
            resolved.requested.display()
        ))
    }
}

fn verify_atomic_destination(
    target: &AnchoredPath,
    expectation: AtomicDestinationExpectation,
) -> Result<()> {
    let metadata = target
        .entry_metadata()
        .with_context(|| format!("unable to inspect `{}`", target.path().display()))?;
    if metadata.is_some_and(|metadata| !metadata.is_file()) {
        return Err(anyhow!(
            "write path `{}` is not a regular file",
            target.path().display()
        ));
    }
    let matches = match (expectation, metadata) {
        (AtomicDestinationExpectation::Absent, None) => true,
        (AtomicDestinationExpectation::Existing(expected), Some(metadata)) => {
            metadata.snapshot() == expected
        }
        (AtomicDestinationExpectation::Absent, Some(_))
        | (AtomicDestinationExpectation::Existing(_), None) => false,
    };
    if matches {
        Ok(())
    } else {
        Err(anyhow!(
            "write target `{}` changed before atomic replacement",
            target.path().display()
        ))
    }
}

fn verify_atomic_parent(target: &AnchoredPath) -> Result<()> {
    if target.parent_path_is_current().unwrap_or(false) {
        Ok(())
    } else {
        Err(anyhow!(
            "parent directory `{}` changed before atomic replacement of `{}`",
            target.parent_path().display(),
            target.path().display()
        ))
    }
}

fn target_matches_open_file(target: &AnchoredPath, file: &File, expected: FileSnapshot) -> bool {
    let opened = match file.metadata() {
        Ok(metadata) if metadata.is_file() => crate::private_fs::file_snapshot(&metadata),
        Ok(_) | Err(_) => return false,
    };
    opened.same_content_state(expected)
        && target.entry_metadata().is_ok_and(|metadata| {
            metadata.is_some_and(|metadata| metadata.is_file() && metadata.snapshot() == opened)
        })
}

struct AtomicWriteOutcome {
    cleanup_residue: Option<PathBuf>,
    rebound_parent: Option<PathBuf>,
    commit_reported_error: Option<String>,
    requested_path_changed_after_commit: bool,
    target_changed_after_commit: bool,
}

impl AtomicWriteOutcome {
    fn path_requires_inspection(&self) -> bool {
        self.rebound_parent.is_some()
            || self.commit_reported_error.is_some()
            || self.requested_path_changed_after_commit
            || self.target_changed_after_commit
    }

    // Cleanup residue does not invalidate the committed file change, but its warning still needs
    // to survive transcript projection so the operator can inspect and remove it.
    fn requires_inspection(&self) -> bool {
        self.cleanup_residue.is_some() || self.path_requires_inspection()
    }
}

fn atomic_staging_name() -> String {
    format!(
        ".bettercodex-{}-{}.tmp",
        std::process::id(),
        uuid::Uuid::new_v4()
    )
}

fn hard_link_can_fallback_to_rename(error: &std::io::Error) -> bool {
    error.kind() == std::io::ErrorKind::Unsupported
        || error.raw_os_error().is_some_and(|code| {
            [
                libc::EACCES,
                libc::EMLINK,
                libc::ENOSYS,
                libc::EOPNOTSUPP,
                libc::EPERM,
                libc::EXDEV,
            ]
            .contains(&code)
        })
}

fn write_file_atomically(
    resolved: &ResolvedWritePath,
    target: &AnchoredPath,
    content: &[u8],
    expectation: AtomicDestinationExpectation,
    staging_name: &str,
    mut record_staging: impl FnMut(ToolStagingEvidence) -> Result<()>,
) -> Result<AtomicWriteOutcome> {
    if let Err(error) = verify_resolved_write_path(resolved)
        .and_then(|()| verify_atomic_parent(target))
        .and_then(|()| verify_atomic_destination(target, expectation))
    {
        return Err(error.context(format!(
            "atomic replacement of `{}` was not committed",
            target.path().display()
        )));
    }

    let mut staging = PrivateStagingArea::new(target, staging_name);
    let mut commit_outcome_unknown = false;
    let mut commit_reported_error = None;
    let mut committed_file = None;
    let operation = (|| -> Result<()> {
        target
            .create_directory(&staging.basename, 0o700)
            .with_context(|| format!("unable to create `{}`", staging.path().display()))?;
        staging.directory_present = true;
        let created_directory = target
            .child_metadata(&staging.basename)
            .with_context(|| format!("unable to inspect `{}`", staging.path().display()))?
            .filter(|metadata| metadata.is_directory())
            .ok_or_else(|| {
                anyhow!(
                    "private staging path `{}` is not a directory",
                    staging.path().display()
                )
            })?;
        staging.directory_identity = Some(created_directory.object_identity());
        let directory = target
            .open_child_directory(&staging.basename)
            .with_context(|| format!("unable to open `{}`", staging.path().display()))?;
        use std::os::unix::fs::PermissionsExt as _;
        let directory_metadata = directory
            .metadata()
            .with_context(|| format!("unable to inspect `{}`", staging.path().display()))?;
        let directory_mode = directory_metadata.permissions().mode() & 0o777;
        if !directory_metadata.is_dir()
            || directory_mode & 0o077 != 0
            || directory_mode & 0o300 != 0o300
            || crate::private_fs::file_object_identity(&directory_metadata)
                != created_directory.object_identity()
        {
            // A process umask may only remove permissions from the requested 0700 mode. If it
            // removes owner write/search access, fail safely rather than repairing the directory
            // through a pathname that another actor could race with a symlink substitution.
            return Err(anyhow!(
                "private staging path `{}` is not an owner-writable private directory",
                staging.path().display()
            ));
        }
        staging.directory = Some(directory);
        record_staging(staging.evidence()?)?;

        let requested_mode = match expectation {
            AtomicDestinationExpectation::Absent => 0o666,
            AtomicDestinationExpectation::Existing(_) => 0o600,
        };
        let mut file = staging
            .directory()?
            .create_file(OsStr::new("content"), requested_mode)
            .with_context(|| format!("unable to create `{}/content`", staging.path().display()))?;
        staging.content_present = true;
        staging.content = Some(crate::private_fs::file_snapshot(
            &file.metadata().with_context(|| {
                format!("unable to inspect `{}/content`", staging.path().display())
            })?,
        ));
        record_staging(staging.evidence()?)?;
        file.write_all(content).with_context(|| {
            format!("unable to populate `{}/content`", staging.path().display())
        })?;
        if let AtomicDestinationExpectation::Existing(expected) = expectation {
            // Preserve ordinary access and executable bits while retaining the kernel's ordinary
            // safety behavior of clearing set-user-ID and set-group-ID on replaced content.
            file.set_permissions(std::fs::Permissions::from_mode(expected.ordinary_mode()))
                .with_context(|| {
                    format!(
                        "unable to set mode on `{}/content`",
                        staging.path().display()
                    )
                })?;
        }
        staging.content = Some(crate::private_fs::file_snapshot(
            &file.metadata().with_context(|| {
                format!("unable to inspect `{}/content`", staging.path().display())
            })?,
        ));
        record_staging(staging.evidence()?)?;

        // These checks detect ordinary concurrent replacement and parent rebinding, but are not a
        // hardened security boundary: POSIX rename cannot condition replacement on an expected
        // inode, so another actor can still race an existing target's final check-to-rename
        // interval. New destinations use `linkat` first so a concurrently created entry is never
        // overwritten on filesystems that support hard links.
        verify_resolved_write_path(resolved)?;
        verify_atomic_parent(target)?;
        verify_atomic_destination(target, expectation)?;
        staging.verify_content(&file)?;
        let committed_snapshot = staging
            .content
            .ok_or_else(|| anyhow!("private staging content identity is unavailable"))?;
        let mut source_was_moved = false;
        let (commit_operation, commit_result) = match expectation {
            AtomicDestinationExpectation::Absent => {
                match target.link_from(staging.directory()?, OsStr::new("content")) {
                    Ok(()) => ("link", Ok(())),
                    Err(error) if target_matches_open_file(target, &file, committed_snapshot) => {
                        ("link", Err(error))
                    }
                    Err(error) if hard_link_can_fallback_to_rename(&error) => {
                        // Some writable filesystems do not support hard links. Preserve the
                        // retained write behavior there, with the narrow race documented above.
                        verify_resolved_write_path(resolved)?;
                        verify_atomic_parent(target)?;
                        verify_atomic_destination(target, expectation)?;
                        staging.verify_content(&file)?;
                        source_was_moved = true;
                        (
                            "rename fallback",
                            target.rename_from(staging.directory()?, OsStr::new("content")),
                        )
                    }
                    Err(error) => ("link", Err(error)),
                }
            }
            AtomicDestinationExpectation::Existing(_) => {
                source_was_moved = true;
                (
                    "rename",
                    target.rename_from(staging.directory()?, OsStr::new("content")),
                )
            }
        };
        match commit_result {
            Ok(()) if source_was_moved => {
                staging.content_present = false;
                staging.content = None;
            }
            Ok(()) => {}
            Err(error) => {
                let source_is_missing = staging
                    .directory
                    .as_ref()
                    .and_then(|directory| directory.entry_metadata(OsStr::new("content")).ok())
                    .is_some_and(|metadata| metadata.is_none());
                if target_matches_open_file(target, &file, committed_snapshot) {
                    commit_reported_error = Some(format!("{commit_operation}: {error}"));
                    if source_is_missing {
                        staging.content_present = false;
                        staging.content = None;
                    }
                } else {
                    // Network filesystems can report a failed namespace operation after the
                    // server committed it. Source metadata can be stale as well, so only the
                    // intended state observed at the destination proves success after an error.
                    commit_outcome_unknown = true;
                    return Err(anyhow::Error::new(MutationOutcomeUnknown(format!(
                        "{commit_operation} for `{}` reported `{error}`, and the destination state does not prove whether atomic replacement committed; inspect the requested path before retrying",
                        target.path().display()
                    ))));
                }
            }
        }
        committed_file = Some((file, committed_snapshot));
        Ok(())
    })();

    if let Err(error) = operation {
        let cleanup_error = staging.cleanup().err();
        let mut error = if commit_outcome_unknown {
            error.context(format!(
                "atomic replacement of `{}` has an unknown outcome; inspect it before retrying",
                target.path().display()
            ))
        } else {
            error.context(format!(
                "atomic replacement of `{}` was not committed",
                target.path().display()
            ))
        };
        if let Some(cleanup_error) = cleanup_error {
            error = error.context(format!(
                "private staging directory `{}` may remain after cleanup failed: {cleanup_error}",
                staging.path().display()
            ));
        }
        return Err(error);
    }

    let cleanup_residue = staging.cleanup().err().map(|_| staging.path());
    drop(staging);
    // Cleanup performs additional namespace I/O, and unlinking the staging hard link can update
    // status-change time. Observe the live committed inode and all requested path routes only
    // after that work, immediately before reporting the result.
    let target_changed_after_commit = committed_file
        .as_ref()
        .is_none_or(|(file, expected)| !target_matches_open_file(target, file, *expected));
    let requested_path_changed_after_commit = !resolved.is_current();
    let rebound_parent = (!target.parent_path_is_current().unwrap_or(false))
        .then(|| target.parent_path().to_path_buf());
    Ok(AtomicWriteOutcome {
        cleanup_residue,
        rebound_parent,
        commit_reported_error,
        requested_path_changed_after_commit,
        target_changed_after_commit,
    })
}

struct PrivateStagingArea<'a> {
    target: &'a AnchoredPath,
    basename: OsString,
    directory: Option<DirectoryHandle>,
    directory_identity: Option<FileObjectIdentity>,
    content: Option<FileSnapshot>,
    directory_present: bool,
    content_present: bool,
}

impl<'a> PrivateStagingArea<'a> {
    fn new(target: &'a AnchoredPath, staging_name: &str) -> Self {
        Self {
            target,
            basename: OsString::from(staging_name),
            directory: None,
            directory_identity: None,
            content: None,
            directory_present: false,
            content_present: false,
        }
    }

    fn path(&self) -> PathBuf {
        self.target.parent_path().join(&self.basename)
    }

    fn directory(&self) -> Result<&DirectoryHandle> {
        self.directory
            .as_ref()
            .ok_or_else(|| anyhow!("private staging directory is not open"))
    }

    fn evidence(&self) -> Result<ToolStagingEvidence> {
        let name = self
            .basename
            .to_str()
            .ok_or_else(|| anyhow!("private staging name is not UTF-8"))?
            .to_string();
        Ok(ToolStagingEvidence {
            name,
            directory: self.directory_identity,
            content: self.content,
        })
    }

    fn verify_content(&self, file: &File) -> Result<()> {
        let expected = self
            .content
            .ok_or_else(|| anyhow!("private staging content identity is unavailable"))?;
        let opened_is_current = file.metadata().is_ok_and(|metadata| {
            metadata.is_file() && crate::private_fs::file_snapshot(&metadata) == expected
        });
        let path_is_current = self
            .directory()?
            .entry_metadata(OsStr::new("content"))
            .is_ok_and(|metadata| {
                metadata
                    .is_some_and(|metadata| metadata.is_file() && metadata.snapshot() == expected)
            });
        if opened_is_current && path_is_current {
            Ok(())
        } else {
            Err(anyhow!(
                "private staging content changed before atomic replacement"
            ))
        }
    }

    fn cleanup(&mut self) -> std::io::Result<()> {
        let mut first_error = None;
        if self.content_present {
            let cleanup = (|| -> std::io::Result<()> {
                let directory = self.directory.as_ref().ok_or_else(|| {
                    std::io::Error::other("private staging directory is not open")
                })?;
                match (
                    self.content,
                    directory.entry_metadata(OsStr::new("content"))?,
                ) {
                    (Some(expected), Some(metadata))
                        if metadata.is_file()
                            && metadata.object_identity() == expected.object_identity() =>
                    {
                        // The open staging inode remains ours even if a partial write changed its
                        // size or timestamps before returning an error. Only a directory-entry
                        // substitution changes ownership enough to make unlinking unsafe.
                        directory.remove_file(OsStr::new("content"))?;
                    }
                    (Some(_), None) | (None, None) => {}
                    (Some(_), Some(_)) | (None, Some(_)) => {
                        return Err(std::io::Error::other(
                            "private staging content changed before cleanup",
                        ));
                    }
                }
                self.content_present = false;
                self.content = None;
                Ok(())
            })();
            if let Err(error) = cleanup {
                first_error = Some(error);
            }
        }
        if self.directory_present && !self.content_present {
            let cleanup = (|| -> std::io::Result<()> {
                let expected = self.directory_identity.ok_or_else(|| {
                    std::io::Error::other("private staging directory identity is unavailable")
                })?;
                match self.target.child_metadata(&self.basename)? {
                    Some(metadata)
                        if metadata.is_directory() && metadata.object_identity() == expected =>
                    {
                        self.target.remove_directory(&self.basename)?;
                    }
                    None => {}
                    Some(_) => {
                        return Err(std::io::Error::other(
                            "private staging directory changed before cleanup",
                        ));
                    }
                }
                self.directory_present = false;
                self.directory = None;
                self.directory_identity = None;
                Ok(())
            })();
            if let Err(error) = cleanup
                && first_error.is_none()
            {
                first_error = Some(error);
            }
        }
        first_error.map_or(Ok(()), Err)
    }
}

impl Drop for PrivateStagingArea<'_> {
    fn drop(&mut self) {
        let _ = self.cleanup();
    }
}

fn open_for_read(path: &Path) -> std::io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt as _;

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

pub(crate) fn responses_api_specifications_for(
    ask_user_question_enabled: bool,
    specialist_coordination_enabled: bool,
) -> &'static [Value] {
    match (ask_user_question_enabled, specialist_coordination_enabled) {
        (false, false) => &BASE_RESPONSES_API_SPECIFICATIONS,
        (true, false) => &QUESTION_RESPONSES_API_SPECIFICATIONS,
        (false, true) => &COORDINATION_RESPONSES_API_SPECIFICATIONS,
        (true, true) => &DEEPWORK_RESPONSES_API_SPECIFICATIONS,
    }
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
            "Read bounded UTF-8 text or inspect a PNG, JPEG, GIF, or WebP image from a local file. Use `read` rather than shell commands to inspect a known file. Text reads stop at 39,000 bytes or the optional `limit` in lines, whichever comes first, and report the next offset when truncated. Image reads accept files up to 50 MiB and return one image attachment. Failures return plain error text.",
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
        function_tool(
            ASK_USER_QUESTION_NAME,
            "Ask the user for decisions in an interactive terminal card. Wait for an explicit response to one to four related questions. Use this only when the answer materially affects the work and cannot be inferred safely. Provide two to six concise options per question; the UI automatically adds an Other free-text choice. Use multiSelect for questions where several options can apply, and defaultSelected only for genuinely recommended multi-select defaults. Add preview only when seeing the proposed content helps the user decide. Cancellation is returned explicitly and never chooses a highlighted or default option.",
            ask_user_question_schema(),
            ask_user_question_output_schema(),
        ),
        function_tool(
            COORDINATE_SPECIALIST_NAME,
            "Coordinate the active `$deepwork` run. This is a strict sequential pipeline, not a general delegation tool. Use `approve_interview` only after the user approves the task contract, then start, supervise, inspect, and explicitly accept each expected specialist. Use `wait` instead of polling; it blocks until a meaningful completion, blocker, interruption, or failure exists. Cancelling `wait` only stops Main from waiting; use `cancel` to interrupt the target specialist and pause its current stage. A completed turn remains available for review: send concrete corrections to the same session, or atomically accept and retire it with `retire`. After accepted `$evals` and `$manifest`, use `approve_readiness` only after the user approves the final execution contract. Revive direct amendments when prior context remains useful; replace stale or biased work. `status` recovers the canonical run state after resume. Never use this tool outside a user-invoked `$deepwork` run.",
            coordinate_specialist_schema(),
            coordinate_specialist_output_schema(),
        ),
        json!({
            "type": "web_search",
            "external_web_access": true,
            "search_content_types": ["text", "image"],
        }),
    ]
});

static DEEPWORK_RESPONSES_API_SPECIFICATIONS: LazyLock<Vec<Value>> =
    LazyLock::new(|| responses_specifications(true, true));

static QUESTION_RESPONSES_API_SPECIFICATIONS: LazyLock<Vec<Value>> =
    LazyLock::new(|| responses_specifications(true, false));

static COORDINATION_RESPONSES_API_SPECIFICATIONS: LazyLock<Vec<Value>> =
    LazyLock::new(|| responses_specifications(false, true));

static BASE_RESPONSES_API_SPECIFICATIONS: LazyLock<Vec<Value>> =
    LazyLock::new(|| responses_specifications(false, false));

fn responses_specifications(
    ask_user_question_enabled: bool,
    specialist_coordination_enabled: bool,
) -> Vec<Value> {
    TOOL_SPECIFICATIONS
        .iter()
        .filter(|specification| {
            specification_is_enabled(
                specification,
                ask_user_question_enabled,
                specialist_coordination_enabled,
            )
        })
        .cloned()
        .map(|mut specification| {
            if let Some(specification) = specification.as_object_mut() {
                specification.remove("output_schema");
            }
            specification
        })
        .collect()
}

fn specification_is_enabled(
    specification: &Value,
    ask_user_question_enabled: bool,
    specialist_coordination_enabled: bool,
) -> bool {
    let name = specification.get("name").and_then(Value::as_str);
    (ask_user_question_enabled || name != Some(ASK_USER_QUESTION_NAME))
        && (specialist_coordination_enabled || name != Some(COORDINATE_SPECIALIST_NAME))
}

static CATALOGUE_TEXT: LazyLock<String> = LazyLock::new(|| {
    let specifications = TOOL_SPECIFICATIONS
        .iter()
        .filter(|specification| specification_is_enabled(specification, false, false))
        .cloned()
        .collect::<Vec<_>>();
    render_catalogue(&specifications)
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
        if let Some(variants) = schema
            .get("anyOf")
            .or_else(|| schema.get("oneOf"))
            .and_then(Value::as_array)
        {
            output.push_str("One of these object shapes:\n\n");
            for (index, variant) in variants.iter().enumerate() {
                if index > 0 {
                    output.push('\n');
                }
                render_catalogue_schema(output, variant, depth)?;
            }
        } else {
            output.push_str("No fields.\n");
        }
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

    if let Some(variants) = schema
        .get("anyOf")
        .or_else(|| schema.get("oneOf"))
        .and_then(Value::as_array)
        && variants
            .iter()
            .all(|variant| variant.get("const").is_some())
    {
        for variant in variants {
            output.push_str(&"  ".repeat(depth + 1));
            output.push_str("- `");
            output.push_str(&catalogue_schema_type(variant));
            output.push('`');
            if let Some(description) = variant.get("description").and_then(Value::as_str) {
                output.push_str(" — ");
                output.push_str(description);
            }
            output.push('\n');
        }
        return Ok(());
    }

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
        && nested.is_some_and(|nested| {
            nested.get("properties").is_some()
                || nested.get("anyOf").is_some()
                || nested.get("oneOf").is_some()
        })
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
    if let Some(value) = schema.get("const") {
        return match value {
            Value::String(value) => format!("\"{value}\""),
            value => value.to_string(),
        };
    }
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
            "limit": {"type": "integer", "minimum": 1, "description": "Text files only: maximum number of lines to read. Omit to use only the byte bound."},
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

fn ask_user_question_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "questions": {
                "type": "array",
                "minItems": 1,
                "maxItems": MAX_QUESTIONS,
                "description": "Related questions shown together in one interactive card.",
                "items": {
                    "type": "object",
                    "properties": {
                        "question": {
                            "type": "string",
                            "minLength": 1,
                            "maxLength": MAX_QUESTION_CHARS,
                            "description": "Complete question shown to the user."
                        },
                        "header": {
                            "type": "string",
                            "minLength": 1,
                            "maxLength": MAX_HEADER_CHARS,
                            "description": "Short label for this question, at most 12 characters."
                        },
                        "options": {
                            "type": "array",
                            "minItems": MIN_OPTIONS,
                            "maxItems": MAX_OPTIONS,
                            "description": "Concise choices. Do not add Other; the UI supplies it.",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "label": {
                                        "type": "string",
                                        "minLength": 1,
                                        "maxLength": MAX_OPTION_LABEL_CHARS,
                                        "description": "Concise option label."
                                    },
                                    "description": {
                                        "type": "string",
                                        "minLength": 1,
                                        "maxLength": MAX_OPTION_DESCRIPTION_CHARS,
                                        "description": "Brief consequence or meaning of this option."
                                    },
                                    "preview": {
                                        "type": "string",
                                        "maxLength": MAX_PREVIEW_CHARS,
                                        "description": "Optional Markdown preview shown while this option is focused."
                                    },
                                    "defaultSelected": {
                                        "type": "boolean",
                                        "description": "Optional initial checkbox state. Valid only when multiSelect is true."
                                    }
                                },
                                "required": ["label", "description"],
                                "additionalProperties": false
                            }
                        },
                        "multiSelect": {
                            "type": "boolean",
                            "description": "Whether the user may choose multiple options. Defaults to false."
                        }
                    },
                    "required": ["question", "header", "options"],
                    "additionalProperties": false
                }
            }
        },
        "required": ["questions"],
        "additionalProperties": false
    })
}

fn ask_user_question_output_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "answers": {
                "type": "array",
                "description": "Answers in the same order as the submitted questions.",
                "items": {
                    "type": "object",
                    "properties": {
                        "question": {"type": "string", "description": "Original question text."},
                        "selectedOptions": {
                            "type": "array",
                            "items": {"type": "string"},
                            "description": "Selected supplied option labels."
                        },
                        "freeText": {
                            "type": "string",
                            "description": "The user's Other answer, when supplied."
                        }
                    },
                    "required": ["question", "selectedOptions"],
                    "additionalProperties": false
                }
            },
            "cancelled": {
                "type": "boolean",
                "description": "True only when the user explicitly cancelled the card."
            }
        },
        "required": ["answers", "cancelled"],
        "additionalProperties": false
    })
}

fn coordinate_specialist_schema() -> Value {
    let specialist = json!({
        "type": "string",
        "description": "Fixed pipeline specialist role.",
        "oneOf": [
            {"const": "evals", "description": SpecialistRole::Evals.description()},
            {"const": "manifest", "description": SpecialistRole::Manifest.description()},
            {"const": "worker", "description": SpecialistRole::Worker.description()},
            {"const": "reviewer", "description": SpecialistRole::Reviewer.description()}
        ]
    });
    json!({
        "type": "object",
        "description": "One deepwork coordination action.",
        "oneOf": [
            {
                "type": "object",
                "properties": {
                    "action": {"type": "string", "enum": ["status"], "description": "Return bounded canonical pipeline state."}
                },
                "required": ["action"],
                "additionalProperties": false
            },
            {
                "type": "object",
                "properties": {
                    "action": {"type": "string", "enum": ["approve_interview"]},
                    "contract": {"type": "string", "description": "User-approved task contract containing the literal `SUCCESS CRITERIA` heading followed by plain bullets."}
                },
                "required": ["action", "contract"],
                "additionalProperties": false
            },
            {
                "type": "object",
                "properties": {
                    "action": {"type": "string", "enum": ["approve_readiness"]},
                    "contract": {"type": "string", "description": "User-approved execution contract containing the literal `SUCCESS CRITERIA` heading followed by plain bullets."}
                },
                "required": ["action", "contract"],
                "additionalProperties": false
            },
            {
                "type": "object",
                "properties": {
                    "action": {"type": "string", "enum": ["start"]},
                    "specialist": specialist,
                    "handoff": {"type": "string", "description": "Complete bounded stage handoff preserving the accepted task, literal SUCCESS CRITERIA block, constraints, non-goals, prior accepted outputs, and exact deliverable."}
                },
                "required": ["action", "specialist", "handoff"],
                "additionalProperties": false
            },
            {
                "type": "object",
                "properties": {
                    "action": {"type": "string", "enum": ["send"]},
                    "session_id": {"type": "string", "description": "Stable specialist session UUID."},
                    "message": {"type": "string", "description": "Concrete follow-up direction for the same live specialist context."}
                },
                "required": ["action", "session_id", "message"],
                "additionalProperties": false
            },
            {
                "type": "object",
                "properties": {
                    "action": {"type": "string", "enum": ["wait"]},
                    "session_id": {"type": "string", "description": "Stable specialist session UUID."}
                },
                "required": ["action", "session_id"],
                "additionalProperties": false
            },
            {
                "type": "object",
                "properties": {
                    "action": {"type": "string", "enum": ["cancel"]},
                    "session_id": {"type": "string", "description": "Stable UUID of the working specialist to interrupt. The current stage remains paused and the session stays live for a later send or replacement."}
                },
                "required": ["action", "session_id"],
                "additionalProperties": false
            },
            {
                "type": "object",
                "properties": {
                    "action": {"type": "string", "enum": ["retire"]},
                    "session_id": {"type": "string", "description": "Stable specialist session UUID to accept and retire."},
                    "accepted_handoff": {"type": "string", "description": "Orchestrator-inspected stage output accepted for later handoffs."},
                    "artifacts": {"type": "array", "maxItems": 32, "items": {"type": "string"}, "description": "Existing regular pipeline artifact paths under the current numbered `.deepwork` workspace."},
                    "remaining_risks": {"type": "string", "description": "Known limitations or unresolved risks; omit when none."}
                },
                "required": ["action", "session_id", "accepted_handoff"],
                "additionalProperties": false
            },
            {
                "type": "object",
                "properties": {
                    "action": {"type": "string", "enum": ["revive"]},
                    "session_id": {"type": "string", "description": "Stable retired specialist session UUID."},
                    "message": {"type": "string", "description": "New user feedback or amendment that reopens this stage."}
                },
                "required": ["action", "session_id", "message"],
                "additionalProperties": false
            },
            {
                "type": "object",
                "properties": {
                    "action": {"type": "string", "enum": ["replace"]},
                    "session_id": {"type": "string", "description": "Stable prior specialist session UUID."},
                    "message": {"type": "string", "description": "Fresh redo brief containing the accepted handoff, current repository state, and new feedback."}
                },
                "required": ["action", "session_id", "message"],
                "additionalProperties": false
            }
        ]
    })
}

fn coordinate_specialist_output_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "action": {"type": "string"},
            "stage": {"type": "string", "enum": ["interview", "evals", "manifest", "readiness", "worker", "reviewer", "completed"]},
            "runIndex": {"type": "integer", "minimum": 0},
            "workspace": {"type": "string"},
            "message": {"type": "string"},
            "sessionId": {"type": "string"},
            "event": {"type": "object", "description": "Bounded meaningful specialist event, present for wait."},
            "state": {"type": "object", "description": "Bounded canonical pipeline state, present for status."}
        },
        "required": ["action", "stage", "runIndex", "workspace", "message"],
        "additionalProperties": false
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
