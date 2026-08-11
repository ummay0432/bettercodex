use super::bottom_pane::selection_popup_common::MAX_POPUP_ROWS;
use super::bottom_pane::selection_popup_common::measure_text_height;
use super::bottom_pane::selection_popup_common::menu_surface_padding_height;
use super::bottom_pane::selection_popup_common::render_menu_surface;
use super::clipboard_paste;
use super::context_window::ContextAction;
use super::context_window::ContextWindowView;
use super::editor;
use super::editor::Editor;
use super::file_search::FileSearchPopup;
use super::file_search::FileSearchUpdate;
use super::file_search::is_horizontal_whitespace;
use super::markdown;
use super::markdown_cache::MarkdownRenderCache;
use super::model_picker::ModelPicker;
use super::model_picker::ModelPickerAction;
use super::palette;
use super::palette::TerminalColors;
#[cfg(windows)]
use super::paste_burst::CharDecision;
#[cfg(windows)]
use super::paste_burst::FlushResult;
#[cfg(windows)]
use super::paste_burst::PasteBurst;
use super::pending_input::PendingInput;
use super::reasoning_status::ReasoningStatus;
use super::resume_picker::ResumePicker;
use super::resume_picker::ResumePickerAction;
use super::skill_popup::SkillPopup;
use super::skills_view::SkillsView;
use super::skills_view::SkillsViewAction;
use super::startup_art;
use super::terminal_hyperlinks;
use super::terminal_hyperlinks::HyperlinkLine;
use crate::agent::CompactionOutcome;
use crate::agent::SubmitOutcome;
use crate::ansi_escape::ansi_escape_line;
use crate::assistant_message::AssistantMessage;
use crate::context::ContextSnapshot;
use crate::events::AgentEvent;
use crate::events::SteerId;
use crate::input::UserPrompt;
use crate::model::ModelSelection;
use crate::protocol::MessagePhase;
use crate::protocol::ParsedCommand;
use crate::rollout::SessionTranscriptItem;
use crate::rollout::SessionTranscriptTool;
use crate::rollout::SessionTranscriptToolOutput;
use crate::service_tier::ServiceTier;
use crate::shell_command::parse_command::parse_command;
use crate::skills::Skill;
use crate::skills::SkillSelection;
use crate::skills::SkillUpdate;
use crate::tools::BackgroundProcess;
use crate::tui::render::line_utils::line_to_static;
use crate::tui::wrapping::word_wrap_line;
use crate::update::AvailableUpdate;
use crossterm::event::Event;
use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyEventKind;
use crossterm::event::KeyModifiers;
use ratatui::Frame;
use ratatui::layout::Position;
use ratatui::layout::Rect;
use ratatui::style::Color;
use ratatui::style::Modifier;
use ratatui::style::Style;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::text::Span;
use ratatui::widgets::Block;
use ratatui::widgets::Clear;
use ratatui::widgets::Paragraph;
use ratatui::widgets::Wrap;
use serde_json::Value;
use std::borrow::Cow;
use std::collections::HashMap;
use std::io::Read;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::sync::OnceLock;
use std::time::Duration;
use std::time::Instant;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;
use uuid::Uuid;

const MUTED: Color = Color::Indexed(245);
const RULE: Color = Color::Indexed(8);
const LIVE_PREFIX_COLS: u16 = 2;
const TOOL_OUTPUT_MAX_ROWS: usize = 5;
const COMMAND_CONTINUATION_MAX_ROWS: usize = 2;
const MAX_PATCH_PREVIEW_SOURCE_BYTES: usize = 2 * 1024 * 1024;
const MAX_PATCH_PREVIEW_ROWS: usize = 1_000;
const MAX_PATCH_PREVIEW_ROW_BYTES: usize = 2 * 1024;
const ACTIVITY_COMPOSER_GAP: u16 = 1;
const COMPOSER_FOOTER_GAP: u16 = 0;
const STATUS_LINE_HEIGHT: u16 = 1;
const STATUS_DETAIL_PREFIX: &str = "  └ ";
const SLASH_COMMANDS: &[SlashCommand] = &[
    SlashCommand {
        name: "review",
        aliases: &[],
        description: "thoroughly review and refactor a specified target",
    },
    SlashCommand {
        name: "clear",
        aliases: &[],
        description: "start a fresh session",
    },
    SlashCommand {
        name: "copy",
        aliases: &[],
        description: "copy latest final response as raw Markdown",
    },
    SlashCommand {
        name: "fork",
        aliases: &[],
        description: "branch this conversation into a new saved session",
    },
    SlashCommand {
        name: "diff",
        aliases: &[],
        description: "show styled Git diff including untracked files",
    },
    SlashCommand {
        name: "context",
        aliases: &[],
        description: "visualize current context usage",
    },
    SlashCommand {
        name: "compact",
        aliases: &[],
        description: "summarize conversation to prevent hitting the context limit",
    },
    SlashCommand {
        name: "model",
        aliases: &[],
        description: "choose what model and reasoning effort to use",
    },
    SlashCommand {
        name: "fast",
        aliases: &[],
        description: "toggle faster inference with increased plan usage",
    },
    SlashCommand {
        name: "changelog",
        aliases: &[],
        description: "show released patch notes",
    },
    SlashCommand {
        name: "resume",
        aliases: &[],
        description: "resume a saved session",
    },
    SlashCommand {
        name: "help",
        aliases: &[],
        description: "show keyboard shortcuts",
    },
    SlashCommand {
        name: "ps",
        aliases: &[],
        description: "list background terminals",
    },
    SlashCommand {
        name: "skills",
        aliases: &[],
        description: "manage installed skills and invocation policy",
    },
    #[cfg(unix)]
    SlashCommand {
        name: "tmux",
        aliases: &[],
        description: "move this live session into tmux",
    },
    SlashCommand {
        name: "stop",
        aliases: &[],
        description: "stop all background terminals",
    },
    SlashCommand {
        name: "logout",
        aliases: &[],
        description: "log out of bettercodex",
    },
    SlashCommand {
        name: "quit",
        aliases: &["exit"],
        description: "leave bettercodex",
    },
];

#[derive(Debug, PartialEq, Eq)]
pub(super) enum Action {
    None,
    LoadPromptHistory,
    Submit(ComposerSubmission),
    Queue(ComposerSubmission),
    Cancel,
    Compact,
    ToggleFast,
    SelectModel(ModelSelection),
    Copy(String),
    Clear(ComposerSubmission),
    Fork(ComposerSubmission),
    ListBackgroundProcesses,
    OpenResumePicker(ComposerSubmission),
    ResumeSession {
        id: Uuid,
        submission: Option<ComposerSubmission>,
    },
    RunShellCommand {
        command: String,
        history_text: String,
    },
    ShowContext,
    ShowDiff,
    EnterTmux,
    Logout,
    StopBackgroundProcesses,
    UpdateSkill {
        path: PathBuf,
        update: SkillUpdate,
    },
    Quit,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct ComposerSubmission {
    prompt: UserPrompt,
    draft: editor::EditorSnapshot,
}

impl ComposerSubmission {
    fn take(editor: &mut Editor) -> Self {
        let draft = editor.snapshot();
        editor.remember_snapshot(&draft);
        let prompt = editor.take_prompt();
        Self { prompt, draft }
    }

    pub(super) fn prompt(&self) -> &UserPrompt {
        &self.prompt
    }

    pub(super) fn into_prompt(self) -> UserPrompt {
        self.prompt
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum InterruptIntent {
    StopTurn,
    SubmitSteering,
}

pub(super) struct View {
    cwd: PathBuf,
    repository: Repository,
    entries: Vec<TranscriptEntry>,
    committed_entries: usize,
    welcome_pending: bool,
    history_emitted: bool,
    clear_requested: bool,
    resize_reflow_requested: bool,
    editor: Editor,
    #[cfg(windows)]
    paste_burst: PasteBurst,
    file_search: FileSearchPopup,
    skill_popup: SkillPopup,
    skills: Vec<Skill>,
    context_tokens: Option<u64>,
    model_selection: ModelSelection,
    service_tier: ServiceTier,
    background_processes: Vec<BackgroundProcess>,
    busy: bool,
    action_required: bool,
    interrupting: Option<InterruptIntent>,
    working_since: Option<Instant>,
    turn_had_work: bool,
    reasoning_status: ReasoningStatus,
    status_detail: Option<String>,
    pending_input: PendingInput,
    terminal_assistant_received_this_turn: bool,
    active_message_phase: Option<MessagePhase>,
    composer_text_width: u16,
    overlay: Option<Overlay>,
    slash_selection: usize,
    dismissed_slash: Option<String>,
    user_message_style: Style,
    process_commands: HashMap<i64, String>,
    deferred_interactions: HashMap<String, ToolEntry>,
    unified_exec_wait_streak: Option<UnifiedExecWaitStreak>,
}

pub(super) struct PreparedView {
    width: u16,
    height: u16,
    active_height: u16,
    active_lines: Vec<HyperlinkLine>,
    history_lines: Vec<HyperlinkLine>,
}

impl PreparedView {
    pub(super) fn height(&self) -> u16 {
        self.height
    }

    pub(super) fn take_history_lines(&mut self) -> Vec<HyperlinkLine> {
        std::mem::take(&mut self.history_lines)
    }
}

#[derive(Debug)]
enum TranscriptEntry {
    User(DisplayedUserPrompt),
    Assistant {
        content: MarkdownRenderCache,
        phase: Option<MessagePhase>,
        streaming: bool,
        history: StreamedAssistantHistory,
    },
    Tool(ToolEntry),
    Exploration {
        tools: Vec<ToolEntry>,
        sealed: bool,
    },
    Notice(String),
    PatchNotes {
        content: MarkdownRenderCache,
    },
    UpdateAvailable(AvailableUpdate),
    Error(String),
    Diff(String),
    Processes(Vec<BackgroundProcess>),
    FinalMessageSeparator {
        elapsed_seconds: Option<u64>,
    },
}

#[derive(Debug)]
struct DisplayedUserPrompt {
    text: String,
    model_text: String,
    skill_mentions: Vec<crate::skills::SkillMention>,
    image_ranges: Vec<std::ops::Range<usize>>,
    image_count: usize,
}

impl DisplayedUserPrompt {
    fn from_prompt(prompt: &UserPrompt) -> Self {
        Self {
            text: prompt.as_str().to_string(),
            model_text: prompt.text_without_image_placeholders(),
            skill_mentions: prompt.skill_mentions().to_vec(),
            image_ranges: prompt
                .image_attachments()
                .iter()
                .map(|attachment| attachment.range().clone())
                .collect(),
            image_count: prompt.image_count(),
        }
    }

    fn replayed(mut text: String, image_count: usize) -> Self {
        let model_text = text.clone();
        let mut image_ranges = Vec::with_capacity(image_count);
        for index in 0..image_count {
            if !text.trim().is_empty() || index > 0 {
                text.push_str("\n\n");
            }
            let start = text.len();
            text.push_str(&format!("[Image {}]", index + 1));
            image_ranges.push(start..text.len());
        }
        Self {
            text,
            model_text,
            skill_mentions: Vec::new(),
            image_ranges,
            image_count,
        }
    }

    fn as_str(&self) -> &str {
        &self.text
    }

    fn skill_mentions(&self) -> &[crate::skills::SkillMention] {
        &self.skill_mentions
    }

    fn image_ranges(&self) -> &[std::ops::Range<usize>] {
        &self.image_ranges
    }
}

/// Rendered lines from an in-flight assistant cell that have moved into terminal scrollback.
///
/// The source text remains on the transcript entry so resize replay and finalization can rebuild
/// the canonical Markdown rendering. Only the tail after `lines` remains mutable on screen.
#[derive(Debug, Default)]
struct StreamedAssistantHistory {
    width: Option<u16>,
    started: bool,
    lines: Vec<HyperlinkLine>,
}

#[derive(Debug)]
struct ToolEntry {
    call_id: String,
    name: String,
    input: Option<Value>,
    display: ToolDisplay,
    outcome: Option<ToolOutcome>,
    started_at: Instant,
}

#[derive(Debug)]
struct UnifiedExecWaitStreak {
    session_id: i64,
    tool: ToolEntry,
}

#[derive(Debug)]
enum ToolDisplay {
    CodeMode(String),
    Command {
        command: String,
        parsed: Vec<ParsedCommand>,
    },
    Interaction {
        session_id: i64,
        command: String,
        input: String,
    },
    Patch(PatchDisplay),
    Papercut,
    Plan(PlanDisplay),
    ViewImage(String),
    WebSearch(crate::web_search::WebSearchAction),
    OpenAiDocs(OpenAiDocsActivity),
    Other,
}

#[derive(Debug)]
struct OpenAiDocsActivity {
    title: &'static str,
    detail: String,
    active_label: &'static str,
}

#[derive(Debug)]
struct ToolOutcome {
    output: Result<Value, String>,
}

#[derive(Debug, Default)]
struct PatchDisplay {
    files: Vec<PatchFile>,
}

#[derive(Debug)]
struct PatchFile {
    path: String,
    move_to: Option<String>,
    kind: PatchKind,
    rows: Vec<PatchRow>,
    added: usize,
    removed: Option<usize>,
    omission: Option<PatchPreviewOmission>,
    source_omission_bytes: Option<u64>,
}

#[derive(Clone, Copy, Debug)]
enum PatchPreviewOmission {
    Rows(usize),
    FileBytes(u64),
}

#[derive(Clone, Copy, Debug)]
enum PatchKind {
    Add,
    Delete,
    Update,
}

#[derive(Debug)]
struct PatchRow {
    number: usize,
    kind: PatchRowKind,
    text: String,
}

#[derive(Clone, Copy, Debug)]
enum PatchRowKind {
    Context,
    Add,
    Delete,
}

#[derive(Debug, Default)]
struct PlanDisplay {
    explanation: Option<String>,
    steps: Vec<PlanStep>,
}

#[derive(Debug)]
struct PlanStep {
    text: String,
    status: String,
}

struct Repository {
    name: String,
    branch: Option<String>,
}

struct SlashCommand {
    name: &'static str,
    aliases: &'static [&'static str],
    description: &'static str,
}

impl SlashCommand {
    fn names(&self) -> impl Iterator<Item = &'static str> + '_ {
        std::iter::once(self.name).chain(self.aliases.iter().copied())
    }

    fn matches(&self, query: &str) -> bool {
        self.names().any(|name| name.starts_with(query))
    }

    fn completion_name(&self, query: &str) -> &'static str {
        self.names()
            .find(|name| name.starts_with(query))
            .unwrap_or(self.name)
    }

    fn display_width(&self) -> usize {
        self.names().map(|name| name.len() + 1).sum::<usize>()
            + self.aliases.len().saturating_mul(2)
    }
}

enum Overlay {
    Shortcuts,
    Context(ContextWindowView),
    Model(Box<ModelPicker>),
    Resume(ResumePicker),
    Skills(SkillsView),
}

static PROCESS_START: OnceLock<Instant> = OnceLock::new();

impl View {
    pub(super) fn new(cwd: &Path) -> Self {
        Self::with_state(cwd, Vec::new())
    }

    fn with_state(cwd: &Path, skills: Vec<Skill>) -> Self {
        Self {
            cwd: cwd.to_path_buf(),
            repository: Repository::discover(cwd),
            entries: Vec::new(),
            committed_entries: 0,
            welcome_pending: true,
            history_emitted: false,
            clear_requested: false,
            resize_reflow_requested: false,
            editor: Editor::default(),
            #[cfg(windows)]
            paste_burst: PasteBurst::default(),
            file_search: FileSearchPopup::default(),
            skill_popup: SkillPopup::default(),
            skills,
            context_tokens: None,
            model_selection: ModelSelection::default(),
            service_tier: ServiceTier::default(),
            background_processes: Vec::new(),
            busy: false,
            action_required: false,
            interrupting: None,
            working_since: None,
            turn_had_work: false,
            reasoning_status: ReasoningStatus::default(),
            status_detail: None,
            pending_input: PendingInput::default(),
            terminal_assistant_received_this_turn: false,
            active_message_phase: None,
            composer_text_width: 1,
            overlay: None,
            slash_selection: 0,
            dismissed_slash: None,
            user_message_style: user_message_style_for(Some((31, 31, 31))),
            process_commands: HashMap::new(),
            deferred_interactions: HashMap::new(),
            unified_exec_wait_streak: None,
        }
    }

    pub(super) fn set_terminal_colors(
        &mut self,
        foreground: Option<(u8, u8, u8)>,
        background: Option<(u8, u8, u8)>,
    ) {
        self.user_message_style = user_message_style_for(background);
        palette::set_terminal_colors(foreground, background);
    }

    pub(super) fn seed_prompt_history(
        &mut self,
        history: impl IntoIterator<Item = String>,
        has_persistent_history: bool,
    ) {
        self.editor.seed_history(history);
        self.editor
            .set_persistent_history_available(has_persistent_history);
    }

    pub(super) fn prompt_history_loaded(
        &mut self,
        newest_first: impl IntoIterator<Item = String>,
        has_more: bool,
    ) -> bool {
        self.editor
            .persistent_history_loaded(newest_first, has_more);
        self.editor.begin_history_load()
    }

    pub(super) fn prompt_history_failed(&mut self) {
        self.editor.persistent_history_failed();
    }

    pub(super) fn set_skills(&mut self, skills: Vec<Skill>) {
        self.skills = skills;
        self.skill_popup.hide();
        if let Some(Overlay::Skills(skills)) = self.overlay.as_mut() {
            skills.clear_error();
        }
    }

    pub(super) fn skill_update_failed(&mut self, error: impl Into<String>) {
        let error = error.into();
        if let Some(Overlay::Skills(skills)) = self.overlay.as_mut() {
            skills.set_error(error);
        } else {
            self.entries
                .push(TranscriptEntry::Error(markdown::sanitize(&error)));
        }
    }

    pub(super) fn tmux_handoff_succeeded(&mut self, session_name: &str) {
        self.entries.push(TranscriptEntry::Notice(format!(
            "This live session is now in tmux session {session_name}. Reattach with `tmux attach -t {session_name}`."
        )));
    }

    pub(super) fn tmux_handoff_failed(&mut self, error: impl AsRef<str>) {
        self.entries
            .push(TranscriptEntry::Error(markdown::sanitize(error.as_ref())));
    }

    pub(super) fn request_terminal_reflow(&mut self) {
        self.resize_reflow_requested = true;
    }

    pub(super) fn add_notice(&mut self, notice: impl Into<String>) {
        self.entries.push(TranscriptEntry::Notice(notice.into()));
    }

    pub(super) fn add_patch_notes(&mut self, markdown: impl Into<String>) {
        self.entries.push(TranscriptEntry::PatchNotes {
            content: MarkdownRenderCache::new(markdown.into()),
        });
    }

    pub(super) fn add_error(&mut self, error: impl AsRef<str>) {
        self.entries
            .push(TranscriptEntry::Error(markdown::sanitize(error.as_ref())));
    }

    pub(super) fn add_update_available(&mut self, update: AvailableUpdate) {
        let insertion = self.entries[self.committed_entries..]
            .iter()
            .position(|entry| !entry.is_finalized())
            .map_or(self.entries.len(), |offset| self.committed_entries + offset);
        self.entries
            .insert(insertion, TranscriptEntry::UpdateAvailable(update));
    }

    pub(super) fn take_clear_request(&mut self) -> bool {
        std::mem::take(&mut self.clear_requested)
    }

    pub(super) fn take_resize_reflow_request(&mut self) -> bool {
        std::mem::take(&mut self.resize_reflow_requested)
    }

    pub(super) fn is_busy(&self) -> bool {
        self.busy
    }

    pub(super) fn action_required(&self) -> bool {
        self.action_required
    }

    pub(super) fn start_turn(&mut self, prompt: impl Into<UserPrompt>) {
        let prompt = prompt.into();
        self.flush_unified_exec_wait_streak();
        self.deferred_interactions.clear();
        self.seal_exploration();
        self.entries
            .push(TranscriptEntry::User(DisplayedUserPrompt::from_prompt(
                &prompt,
            )));
        self.busy = true;
        self.action_required = false;
        self.interrupting = None;
        self.working_since = Some(Instant::now());
        self.turn_had_work = false;
        self.reasoning_status.reset();
        self.status_detail = None;
        self.terminal_assistant_received_this_turn = false;
        self.active_message_phase = None;
    }

    pub(super) fn start_compaction(&mut self) {
        self.seal_exploration();
        self.context_tokens = None;
        self.busy = true;
        self.action_required = false;
        self.interrupting = None;
        self.working_since = Some(Instant::now());
        self.turn_had_work = false;
        self.reasoning_status.reset();
        self.status_detail = Some("Compacting conversation".to_string());
        self.terminal_assistant_received_this_turn = false;
        self.active_message_phase = None;
    }

    pub(super) fn add_user_message(&mut self, prompt: &UserPrompt) {
        self.flush_unified_exec_wait_streak();
        self.close_streaming_entries();
        self.seal_exploration();
        self.entries
            .push(TranscriptEntry::User(DisplayedUserPrompt::from_prompt(
                prompt,
            )));
        self.reasoning_status.reset();
        self.status_detail = None;
    }

    pub(super) fn add_pending_steer(&mut self, id: SteerId, prompt: UserPrompt) {
        self.pending_input.add_steer(id, prompt);
    }

    pub(super) fn queue_follow_up(&mut self, prompt: UserPrompt) {
        self.pending_input.queue_follow_up(prompt);
    }

    pub(super) fn pop_next_queued_follow_up(&mut self) -> Option<UserPrompt> {
        self.pending_input.pop_next_follow_up()
    }

    pub(super) fn has_pending_steers(&self) -> bool {
        self.pending_input.has_steers()
    }

    pub(super) fn restore_pending_input_to_composer(&mut self) {
        let prompts = self.pending_input.take_all();
        self.restore_prompts_to_composer(prompts);
    }

    pub(super) fn reject_composer_submission(
        &mut self,
        submission: ComposerSubmission,
        error: impl AsRef<str>,
    ) {
        self.restore_composer_submission(submission);
        self.add_error(error);
    }

    pub(super) fn defer_composer_action(
        &mut self,
        submission: ComposerSubmission,
        notice: impl Into<String>,
    ) {
        self.restore_composer_submission(submission);
        self.add_notice(notice);
    }

    pub(super) fn reject_prompt(&mut self, prompt: UserPrompt, error: impl AsRef<str>) {
        self.restore_prompts_to_composer(vec![prompt]);
        self.add_error(error);
    }

    fn restore_composer_submission(&mut self, submission: ComposerSubmission) {
        self.editor.restore_snapshot(submission.draft);
        self.dismiss_composer_completions();
    }

    fn restore_prompts_to_composer(&mut self, prompts: Vec<UserPrompt>) {
        if prompts.is_empty() {
            return;
        }
        let restored = UserPrompt::joined(prompts);
        if self.editor.is_empty() {
            self.editor.set_user_prompt(&restored);
        } else {
            self.editor.prepend_user_prompt(&restored);
        }
        self.dismiss_composer_completions();
    }

    fn dismiss_composer_completions(&mut self) {
        self.file_search.dismiss();
        self.skill_popup.hide();
        self.dismissed_slash = None;
        self.slash_selection = 0;
    }

    pub(super) fn finish_turn(
        &mut self,
        result: anyhow::Result<SubmitOutcome>,
    ) -> Option<UserPrompt> {
        self.flush_unified_exec_wait_streak();
        self.deferred_interactions.clear();
        self.close_streaming_entries();
        self.seal_exploration();
        self.finish_incomplete_tools();
        let elapsed_seconds = self
            .working_since
            .take()
            .map(|started| started.elapsed().as_secs());
        let turn_had_work = std::mem::take(&mut self.turn_had_work);
        self.busy = false;
        let interrupt_intent = self.interrupting.take();
        self.reasoning_status.reset();
        self.status_detail = None;
        self.action_required = result.is_err();
        match result {
            Ok(SubmitOutcome::Completed(answer)) => {
                if !self.terminal_assistant_received_this_turn && !answer.trim().is_empty() {
                    self.entries.push(TranscriptEntry::Assistant {
                        content: MarkdownRenderCache::new(answer),
                        phase: Some(MessagePhase::FinalAnswer),
                        streaming: false,
                        history: StreamedAssistantHistory::default(),
                    });
                }
                if turn_had_work {
                    self.entries
                        .push(TranscriptEntry::FinalMessageSeparator { elapsed_seconds });
                }
                None
            }
            Ok(SubmitOutcome::Cancelled) => {
                let steers = if interrupt_intent == Some(InterruptIntent::SubmitSteering) {
                    self.pending_input.take_steers()
                } else {
                    Vec::new()
                };
                if steers.is_empty() {
                    self.entries
                        .push(TranscriptEntry::Notice("Turn interrupted".to_string()));
                    None
                } else {
                    self.entries.push(TranscriptEntry::Notice(
                        "Model interrupted to submit steering input".to_string(),
                    ));
                    Some(UserPrompt::joined(steers))
                }
            }
            Err(error) => {
                self.entries
                    .push(TranscriptEntry::Error(markdown::sanitize(&format!(
                        "{error:#}"
                    ))));
                None
            }
        }
    }

    pub(super) fn finish_compaction(&mut self, result: anyhow::Result<CompactionOutcome>) {
        self.flush_unified_exec_wait_streak();
        self.deferred_interactions.clear();
        self.working_since = None;
        self.busy = false;
        self.interrupting = None;
        self.reasoning_status.reset();
        self.status_detail = None;
        self.action_required = result.is_err();
        match result {
            Ok(CompactionOutcome::Completed) => {
                self.entries
                    .push(TranscriptEntry::Notice("Context compacted".to_string()));
            }
            Ok(CompactionOutcome::Cancelled) => self.entries.push(TranscriptEntry::Notice(
                "Compaction interrupted".to_string(),
            )),
            Err(error) => self
                .entries
                .push(TranscriptEntry::Error(markdown::sanitize(&format!(
                    "{error:#}"
                )))),
        }
    }

    pub(super) fn set_interrupting(&mut self, intent: InterruptIntent) {
        if self.busy {
            self.interrupting = Some(intent);
            self.status_detail = None;
        }
    }

    pub(super) fn set_context_tokens(&mut self, tokens: Option<u64>) {
        self.context_tokens = tokens;
    }

    pub(super) fn set_model_selection(&mut self, selection: ModelSelection) {
        self.model_selection = selection;
        self.refresh_open_model_picker();
    }

    fn refresh_open_model_picker(&mut self) {
        if matches!(self.overlay, Some(Overlay::Model(_))) {
            self.overlay = Some(Overlay::Model(Box::new(ModelPicker::new(
                self.model_selection.clone(),
            ))));
        }
    }

    pub(super) fn set_service_tier(&mut self, service_tier: ServiceTier) {
        self.service_tier = service_tier;
    }

    pub(super) fn set_background_processes(&mut self, processes: Vec<BackgroundProcess>) -> bool {
        if self.background_processes == processes {
            return false;
        }
        self.background_processes = processes;
        true
    }

    pub(super) fn add_background_process_list(&mut self, processes: Vec<BackgroundProcess>) {
        self.entries.push(TranscriptEntry::Processes(processes));
    }

    pub(super) fn add_git_diff_result(&mut self, result: Result<String, String>) {
        match result {
            Ok(diff) => self.entries.push(TranscriptEntry::Diff(diff)),
            Err(error) => self
                .entries
                .push(TranscriptEntry::Error(markdown::sanitize(&format!(
                    "Could not compute Git diff: {error}"
                )))),
        }
    }

    pub(super) fn start_operator_command(&mut self, call_id: String, command: &str) {
        self.flush_unified_exec_wait_streak();
        self.seal_exploration();
        self.entries.push(TranscriptEntry::Tool(ToolEntry::new(
            call_id,
            "exec_command".to_string(),
            Some(serde_json::json!({"cmd": command})),
            &self.cwd,
            &self.process_commands,
        )));
    }

    pub(super) fn finish_operator_command(&mut self, call_id: &str, output: Result<Value, String>) {
        if let Some(tool) = self.find_tool_mut(call_id) {
            tool.outcome = Some(ToolOutcome { output });
        }
        self.remember_process_command(call_id);
        self.repository = Repository::discover(&self.cwd);
    }

    pub(super) fn session_transcript(&self) -> Vec<SessionTranscriptItem> {
        self.session_transcript_since(None).1
    }

    pub(super) fn session_transcript_since(
        &self,
        checkpoint: Option<usize>,
    ) -> (usize, Vec<SessionTranscriptItem>) {
        let checkpoint = checkpoint.unwrap_or_default();
        let mut item_count = 0_usize;
        let mut items = Vec::new();
        for entry in &self.entries {
            let included = match entry {
                TranscriptEntry::User(_) | TranscriptEntry::Tool(_) => true,
                TranscriptEntry::Assistant {
                    content,
                    streaming: false,
                    ..
                } => !content.source().trim().is_empty(),
                TranscriptEntry::Exploration { tools, .. } => !tools.is_empty(),
                _ => false,
            };
            if !included {
                continue;
            }
            if item_count >= checkpoint {
                let item = match entry {
                    TranscriptEntry::User(prompt) => SessionTranscriptItem::User {
                        text: prompt.model_text.clone(),
                        image_count: prompt.image_count,
                    },
                    TranscriptEntry::Assistant { content, phase, .. } => {
                        SessionTranscriptItem::Assistant {
                            text: content.source().to_string(),
                            phase: phase.clone(),
                        }
                    }
                    TranscriptEntry::Tool(tool) => SessionTranscriptItem::Tool {
                        tool: tool.session_transcript_tool(/*retain_success_output*/ true),
                    },
                    TranscriptEntry::Exploration { tools, .. } => {
                        SessionTranscriptItem::Exploration {
                            tools: tools
                                .iter()
                                .map(|tool| {
                                    tool.session_transcript_tool(
                                        /*retain_success_output*/ false,
                                    )
                                })
                                .collect(),
                        }
                    }
                    _ => unreachable!("transcript entry inclusion was checked above"),
                };
                items.push(item);
            }
            item_count = item_count.saturating_add(1);
        }
        (item_count, items)
    }

    pub(super) fn show_context(&mut self, snapshot: ContextSnapshot) {
        self.overlay = Some(Overlay::Context(ContextWindowView::new(
            snapshot,
            self.model_selection.model.clone(),
        )));
    }

    pub(super) fn show_resume_picker(&mut self) {
        self.overlay = Some(Overlay::Resume(ResumePicker::loading(&self.cwd)));
    }

    pub(super) fn show_resume_progress(&mut self, target: Uuid) {
        if let Some(Overlay::Resume(picker)) = self.overlay.as_mut() {
            picker.begin_resume(target);
        } else {
            self.overlay = Some(Overlay::Resume(ResumePicker::resuming(&self.cwd, target)));
        }
    }

    pub(super) fn set_resume_sessions(&mut self, sessions: Vec<crate::rollout::SessionSummary>) {
        if let Some(Overlay::Resume(picker)) = self.overlay.as_mut() {
            picker.set_sessions(sessions);
        }
    }

    pub(super) fn resume_failed(&mut self, error: impl Into<String>) {
        let error = error.into();
        if let Some(Overlay::Resume(picker)) = self.overlay.as_mut() {
            picker.set_error(error);
        } else {
            self.entries
                .push(TranscriptEntry::Error(markdown::sanitize(&error)));
        }
    }

    pub(super) fn resume_listing_failed(&mut self, error: impl Into<String>) {
        if let Some(Overlay::Resume(picker)) = self.overlay.as_mut() {
            picker.set_listing_error(error);
        }
    }

    pub(super) fn close_resume_picker(&mut self) {
        if matches!(self.overlay.as_ref(), Some(Overlay::Resume(_))) {
            self.overlay = None;
        }
    }

    pub(super) fn replay_transcript(
        &mut self,
        transcript: impl IntoIterator<Item = SessionTranscriptItem>,
    ) {
        let mut replaying_legacy_exploration = false;
        for item in transcript {
            match item {
                SessionTranscriptItem::User { text, image_count } => {
                    replaying_legacy_exploration = false;
                    self.entries
                        .push(TranscriptEntry::User(DisplayedUserPrompt::replayed(
                            text,
                            image_count,
                        )));
                }
                SessionTranscriptItem::Assistant { text, phase } => {
                    replaying_legacy_exploration = false;
                    self.entries.push(TranscriptEntry::Assistant {
                        content: MarkdownRenderCache::new(text),
                        phase,
                        streaming: false,
                        history: StreamedAssistantHistory::default(),
                    });
                }
                SessionTranscriptItem::Tool { tool } => {
                    replaying_legacy_exploration =
                        self.replay_tool(tool, replaying_legacy_exploration);
                }
                SessionTranscriptItem::Exploration { tools } => {
                    replaying_legacy_exploration = false;
                    let mut replayed = Vec::with_capacity(tools.len());
                    let mut call_ids = Vec::with_capacity(tools.len());
                    for tool in tools {
                        call_ids.push(tool.call_id.clone());
                        replayed.push(ToolEntry::from_session_transcript(
                            tool,
                            &self.cwd,
                            &self.process_commands,
                        ));
                    }
                    if !replayed.is_empty() {
                        self.entries.push(TranscriptEntry::Exploration {
                            tools: replayed,
                            sealed: true,
                        });
                        for call_id in call_ids {
                            self.remember_process_command(&call_id);
                        }
                    }
                }
            }
        }
    }

    fn replay_tool(&mut self, tool: SessionTranscriptTool, join_legacy_exploration: bool) -> bool {
        let call_id = tool.call_id.clone();
        let tool = ToolEntry::from_session_transcript(tool, &self.cwd, &self.process_commands);
        let is_exploration = tool.is_exploration();
        if is_exploration {
            match (join_legacy_exploration, self.entries.last_mut()) {
                (true, Some(TranscriptEntry::Exploration { tools, .. })) => tools.push(tool),
                _ => self.entries.push(TranscriptEntry::Exploration {
                    tools: vec![tool],
                    sealed: true,
                }),
            }
        } else {
            self.entries.push(TranscriptEntry::Tool(tool));
        }
        self.remember_process_command(&call_id);
        is_exploration
    }

    pub(super) fn switch_session(
        &mut self,
        cwd: &Path,
        context_tokens: Option<u64>,
        transcript: impl IntoIterator<Item = SessionTranscriptItem>,
        prompt_history: impl IntoIterator<Item = String>,
        has_persistent_history: bool,
        skills: Vec<Skill>,
    ) {
        let user_message_style = self.user_message_style;
        *self = Self::with_state(cwd, skills);
        self.user_message_style = user_message_style;
        self.context_tokens = context_tokens;
        self.replay_transcript(transcript);
        self.editor.seed_history(prompt_history);
        self.editor
            .set_persistent_history_available(has_persistent_history);
        self.clear_requested = true;
    }

    pub(super) fn file_search_query(&self) -> &str {
        self.file_search.query()
    }

    pub(super) fn handle_file_search_update(&mut self, update: FileSearchUpdate) {
        self.file_search.apply_update(update);
    }

    pub(super) fn clear(&mut self) {
        self.entries.clear();
        self.committed_entries = 0;
        self.welcome_pending = true;
        self.history_emitted = false;
        self.clear_requested = true;
        self.resize_reflow_requested = false;
        self.context_tokens = None;
        self.background_processes.clear();
        self.action_required = false;
        self.reasoning_status.reset();
        self.pending_input.clear();
        self.overlay = None;
        self.file_search = FileSearchPopup::default();
        self.skill_popup = SkillPopup::default();
        self.slash_selection = 0;
        self.dismissed_slash = None;
        self.process_commands.clear();
        self.deferred_interactions.clear();
        self.unified_exec_wait_streak = None;
        self.terminal_assistant_received_this_turn = false;
        self.active_message_phase = None;
    }

    pub(super) fn handle_agent_event(&mut self, event: AgentEvent) {
        match event {
            AgentEvent::ModelMessageStarted(message) => {
                self.flush_unified_exec_wait_streak();
                self.seal_exploration();
                self.close_streaming_entries();
                self.active_message_phase = message.phase;
                if !message.text.is_empty() {
                    self.entries.push(TranscriptEntry::Assistant {
                        content: MarkdownRenderCache::new(message.text),
                        phase: self.active_message_phase.clone(),
                        streaming: true,
                        history: StreamedAssistantHistory::default(),
                    });
                }
                self.status_detail = None;
            }
            AgentEvent::ModelMessageDelta(delta) => {
                self.flush_unified_exec_wait_streak();
                self.seal_exploration();
                match self.entries.last_mut() {
                    Some(TranscriptEntry::Assistant {
                        content, streaming, ..
                    }) if *streaming => {
                        content.append(&delta);
                    }
                    _ => self.entries.push(TranscriptEntry::Assistant {
                        content: MarkdownRenderCache::new(delta),
                        phase: self.active_message_phase.clone(),
                        streaming: true,
                        history: StreamedAssistantHistory::default(),
                    }),
                }
                self.status_detail = None;
            }
            AgentEvent::ModelMessageCompleted(message) => {
                self.complete_assistant_message(message);
            }
            // Reasoning summaries only drive the transient activity heading. They are not a
            // transcript boundary, so the active exploration cell must remain open across model
            // samples for later read/list/search calls to join it, matching Codex's ExecCell.
            AgentEvent::ReasoningSummarySectionStarted => {
                self.reasoning_status.reset();
            }
            AgentEvent::ReasoningSummaryDelta(delta) => {
                self.reasoning_status.push_delta(&delta);
            }
            AgentEvent::ModelResponseCompleted => {
                self.close_streaming_entries();
            }
            AgentEvent::ToolStarted {
                call_id,
                name,
                input,
            } => {
                self.close_streaming_entries();
                let entry = ToolEntry::new(call_id, name, input, &self.cwd, &self.process_commands);
                self.status_detail = Some(entry.activity_label());
                if entry.is_interaction() {
                    if entry.has_interaction_input() {
                        self.flush_unified_exec_wait_streak();
                    }
                    self.deferred_interactions
                        .insert(entry.call_id.clone(), entry);
                } else if entry.is_exploration() {
                    self.flush_unified_exec_wait_streak();
                    let last_is_uncommitted = self.entries.len() > self.committed_entries;
                    match self.entries.last_mut() {
                        Some(TranscriptEntry::Exploration {
                            tools,
                            sealed: false,
                        }) if last_is_uncommitted => {
                            tools.push(entry);
                        }
                        _ => self.entries.push(TranscriptEntry::Exploration {
                            tools: vec![entry],
                            sealed: false,
                        }),
                    }
                } else {
                    self.flush_unified_exec_wait_streak();
                    self.seal_exploration();
                    self.entries.push(TranscriptEntry::Tool(entry));
                }
            }
            AgentEvent::ToolCompleted {
                call_id,
                output,
                duration: _,
            } => {
                if let Some(tool) = self.deferred_interactions.remove(&call_id) {
                    self.complete_deferred_interaction(tool, output);
                } else {
                    let completed_work = self.find_tool_mut(&call_id).is_some_and(|tool| {
                        let completed_work = matches!(
                            &tool.display,
                            ToolDisplay::CodeMode(_)
                                | ToolDisplay::Command { .. }
                                | ToolDisplay::Patch(_)
                                | ToolDisplay::Papercut
                                | ToolDisplay::WebSearch(_)
                                | ToolDisplay::OpenAiDocs(_)
                                | ToolDisplay::Other
                        );
                        let output = if matches!(&tool.display, ToolDisplay::OpenAiDocs(_)) {
                            match output {
                                Ok(_) => Ok(Value::Null),
                                Err(error) => Err(first_display_line(&markdown::sanitize(&error))),
                            }
                        } else {
                            output
                        };
                        tool.outcome = Some(ToolOutcome { output });
                        completed_work
                    });
                    self.turn_had_work |= completed_work;
                    self.remember_process_command(&call_id);
                }
                self.repository = Repository::discover(&self.cwd);
                self.status_detail = self.latest_tool_activity();
            }
            AgentEvent::ContextUpdated(snapshot) => {
                self.context_tokens = snapshot.measured.then_some(snapshot.used_tokens);
                if let Some(Overlay::Context(context)) = self.overlay.as_mut() {
                    context.update(snapshot);
                }
            }
            AgentEvent::Warning(message) => self.add_notice(format!("Warning: {message}")),
            AgentEvent::SteeringCommitted(id) => {
                if let Some(prompt) = self.pending_input.commit_steer(id) {
                    self.add_user_message(&prompt);
                }
            }
            AgentEvent::CompactionStarted => {
                self.context_tokens = None;
                self.reasoning_status.reset();
                self.status_detail = Some("Compacting conversation".to_string());
            }
            AgentEvent::CompactionCompleted => self.status_detail = self.latest_tool_activity(),
        }
    }

    pub(super) fn handle_terminal_event(&mut self, event: Event) -> Action {
        if matches!(event, Event::Key(_) | Event::Paste(_)) {
            self.action_required = false;
        }
        if let Event::Paste(text) = &event
            && self.editor.history_search_active()
        {
            self.editor.history_search_insert(text);
            return self.prompt_history_action(Action::None);
        }
        let action = match event {
            Event::Key(key) if matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) => {
                if self.capture_paste_burst_key(key) {
                    Action::None
                } else {
                    self.handle_key(key)
                }
            }
            Event::Paste(text) if matches!(self.overlay.as_ref(), Some(Overlay::Resume(_))) => {
                if let Some(Overlay::Resume(picker)) = self.overlay.as_mut() {
                    picker.handle_paste(&text);
                }
                Action::None
            }
            Event::Paste(_) if self.overlay.is_some() => Action::None,
            Event::Paste(text) => {
                #[cfg(windows)]
                self.paste_burst.clear_after_explicit_paste();
                self.apply_pasted_text(text);
                Action::None
            }
            Event::Resize(_, _) => {
                self.resize_reflow_requested = true;
                Action::None
            }
            _ => Action::None,
        };
        self.sync_composer_popups();
        self.prompt_history_action(action)
    }

    fn prompt_history_action(&mut self, action: Action) -> Action {
        if matches!(&action, Action::None) && self.editor.begin_history_load() {
            Action::LoadPromptHistory
        } else {
            action
        }
    }

    fn sync_composer_popups(&mut self) {
        if self.editor.is_browsing_history() || self.editor.history_search_active() {
            self.file_search.hide();
            self.skill_popup.hide();
        } else {
            self.file_search
                .sync(self.editor.text(), self.editor.cursor());
            self.skill_popup.sync(
                self.editor.text(),
                self.editor.cursor(),
                &self.editor.skill_ranges(),
                &self.skills,
            );
        }
    }

    fn apply_pasted_text(&mut self, text: String) {
        let text = text.replace("\r\n", "\n").replace('\r', "\n");
        if let Some(image) = clipboard_paste::image_from_pasted_path(&text) {
            self.attach_image(image);
        } else {
            self.editor.insert_paste(text);
        }
        self.dismissed_slash = None;
        self.slash_selection = 0;
    }

    pub(super) fn paste_burst_active(&self) -> bool {
        #[cfg(windows)]
        {
            self.paste_burst.is_active()
        }
        #[cfg(not(windows))]
        {
            false
        }
    }

    pub(super) fn flush_paste_burst(&mut self) -> bool {
        #[cfg(windows)]
        {
            let changed = self.handle_paste_burst_flush(Instant::now());
            if changed {
                self.sync_composer_popups();
            }
            changed
        }
        #[cfg(not(windows))]
        {
            false
        }
    }

    #[cfg(windows)]
    fn handle_paste_burst_flush(&mut self, now: Instant) -> bool {
        match self.paste_burst.flush_if_due(now) {
            FlushResult::Paste(pasted) => {
                // Preserve the detector's short Enter-suppression window. An
                // explicit bracketed paste clears it in handle_terminal_event.
                self.apply_pasted_text(pasted);
                true
            }
            FlushResult::Typed(character) => {
                let mut encoded = [0; 4];
                self.editor.insert(character.encode_utf8(&mut encoded));
                true
            }
            FlushResult::None => false,
        }
    }

    fn capture_paste_burst_key(&mut self, key: KeyEvent) -> bool {
        #[cfg(not(windows))]
        {
            let _ = key;
            false
        }

        #[cfg(windows)]
        {
            if self.overlay.is_some() || self.editor.history_search_active() {
                if let Some(pasted) = self.paste_burst.flush_before_modified_input() {
                    self.apply_pasted_text(pasted);
                }
                self.paste_burst.clear_window_after_non_char();
                return false;
            }

            let now = Instant::now();
            self.handle_paste_burst_flush(now);

            if key.code == KeyCode::Enter {
                if self.paste_burst.append_newline_if_active(now) {
                    return true;
                }
                if self.paste_burst.direct_insert_newline_should_insert(now) {
                    self.editor.insert_newline();
                    self.paste_burst.extend_window(now);
                    return true;
                }
            }

            if let KeyCode::Char(character) = key.code
                && !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
                && !character.is_control()
            {
                if !character.is_ascii() {
                    if self.paste_burst.try_append_char_if_active(character, now) {
                        return true;
                    }
                    if let Some(pasted) = self.paste_burst.flush_before_modified_input() {
                        self.apply_pasted_text(pasted);
                    }
                    if let Some(decision) = self.paste_burst.on_plain_char_no_hold(now) {
                        match decision {
                            CharDecision::BufferAppend => {
                                self.paste_burst.append_char_to_buffer(character, now);
                                return true;
                            }
                            CharDecision::BeginBuffer { retro_chars } => {
                                let cursor = self.editor.cursor();
                                let before = &self.editor.text()[..cursor];
                                if let Some(grab) = self.paste_burst.decide_begin_buffer(
                                    now,
                                    before,
                                    usize::from(retro_chars),
                                ) {
                                    if !grab.grabbed.is_empty() {
                                        self.editor.replace_range(grab.start_byte..cursor, "");
                                    }
                                    self.paste_burst.append_char_to_buffer(character, now);
                                    return true;
                                }
                            }
                            CharDecision::RetainFirstChar
                            | CharDecision::BeginBufferFromPending => {
                                unreachable!("non-ASCII paste detection returned an ASCII decision")
                            }
                        }
                    }
                    return false;
                }

                match self.paste_burst.on_plain_char(character, now) {
                    CharDecision::BufferAppend => {
                        self.paste_burst.append_char_to_buffer(character, now);
                        return true;
                    }
                    CharDecision::BeginBuffer { retro_chars } => {
                        let cursor = self.editor.cursor();
                        let before = &self.editor.text()[..cursor];
                        if let Some(grab) = self.paste_burst.decide_begin_buffer(
                            now,
                            before,
                            usize::from(retro_chars),
                        ) {
                            if !grab.grabbed.is_empty() {
                                self.editor.replace_range(grab.start_byte..cursor, "");
                            }
                            self.paste_burst.append_char_to_buffer(character, now);
                            return true;
                        }
                    }
                    CharDecision::BeginBufferFromPending => {
                        self.paste_burst.append_char_to_buffer(character, now);
                        return true;
                    }
                    CharDecision::RetainFirstChar => return true,
                }
                return false;
            }

            if let Some(pasted) = self.paste_burst.flush_before_modified_input() {
                self.apply_pasted_text(pasted);
            }
            if !matches!(key.code, KeyCode::Char(_) | KeyCode::Enter) {
                self.paste_burst.clear_window_after_non_char();
            }
            false
        }
    }

    fn handle_key(&mut self, key: KeyEvent) -> Action {
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);

        if let Some(Overlay::Resume(picker)) = self.overlay.as_mut() {
            return match picker.handle_key(key) {
                ResumePickerAction::None => Action::None,
                ResumePickerAction::Close => {
                    self.overlay = None;
                    Action::None
                }
                ResumePickerAction::Resume(id) => Action::ResumeSession {
                    id,
                    submission: None,
                },
            };
        }
        if let Some(Overlay::Model(picker)) = self.overlay.as_mut() {
            return match picker.handle_key(key) {
                ModelPickerAction::None => Action::None,
                ModelPickerAction::Close => {
                    self.overlay = None;
                    Action::None
                }
                ModelPickerAction::Select(selection) => {
                    self.overlay = None;
                    Action::SelectModel(selection)
                }
            };
        }
        if control && key.code == KeyCode::Char('o') {
            return self.copy_latest_final_action();
        }
        if self.editor.history_search_active() {
            return self.handle_history_search_key(key);
        }
        if control && key.code == KeyCode::Char('c') {
            if self.overlay.is_some() {
                self.overlay = None;
                return Action::None;
            }
            if !self.editor.is_empty() {
                self.editor.clear_for_ctrl_c();
                self.file_search.hide();
                self.skill_popup.hide();
                return Action::None;
            }
            return if self.busy {
                Action::Cancel
            } else {
                Action::Quit
            };
        }
        if let Some(Overlay::Skills(skills)) = self.overlay.as_mut() {
            return match skills.handle_key(key, &self.skills) {
                SkillsViewAction::None => Action::None,
                SkillsViewAction::Close => {
                    self.overlay = None;
                    Action::None
                }
                SkillsViewAction::Update { path, update } => Action::UpdateSkill { path, update },
            };
        }
        if let Some(overlay) = self.overlay.as_mut() {
            let close = match overlay {
                Overlay::Shortcuts => true,
                Overlay::Context(context) => context.handle_key(key.code) == ContextAction::Close,
                Overlay::Model(_) => unreachable!("model picker keys are handled above"),
                Overlay::Resume(_) => unreachable!("resume picker keys are handled above"),
                Overlay::Skills(_) => unreachable!("skills keys are handled above"),
            };
            if close {
                self.overlay = None;
            }
            return Action::None;
        }
        if (control && key.code == KeyCode::Char('r'))
            || (key.modifiers.is_empty() && key.code == KeyCode::Char('\u{12}'))
        {
            self.file_search.hide();
            self.skill_popup.hide();
            self.editor.begin_history_search();
            return Action::None;
        }
        if key.code == KeyCode::Esc {
            if self.skill_popup.is_active() {
                self.skill_popup.dismiss();
                return Action::None;
            }
            if self.file_search.is_active() {
                self.file_search.dismiss();
                return Action::None;
            }
            if !self.slash_matches().is_empty() {
                self.dismissed_slash = Some(self.editor.text().to_string());
                return Action::None;
            }
            return if self.busy {
                Action::Cancel
            } else {
                Action::None
            };
        }
        if key.code == KeyCode::Char('?') && self.editor.is_empty() {
            self.overlay = Some(Overlay::Shortcuts);
            return Action::None;
        }
        let edit_queued = (key.code == KeyCode::Up && alt && !control && !shift)
            || (key.code == KeyCode::Left && shift && !control && !alt);
        if edit_queued
            && self.pending_input.has_follow_ups()
            && !self.skill_popup.is_active()
            && !self.file_search.is_active()
            && self.slash_matches().is_empty()
        {
            if let Some(prompt) = self.pending_input.pop_latest_follow_up() {
                self.restore_prompts_to_composer(vec![prompt]);
            }
            return Action::None;
        }

        if self.skill_popup.is_active() {
            if key.code == KeyCode::Up
                || (key.code == KeyCode::Char('p') && control && !alt && !shift)
            {
                self.skill_popup.move_up();
                return Action::None;
            }
            if key.code == KeyCode::Down
                || (key.code == KeyCode::Char('n') && control && !alt && !shift)
            {
                self.skill_popup.move_down();
                return Action::None;
            }
            if matches!(key.code, KeyCode::Tab)
                || (key.code == KeyCode::Enter && !shift && !alt && !control)
            {
                if let Some((range, skill)) = self.skill_popup.selected_skill(&self.skills) {
                    let inserted = format!("${}", skill.name());
                    let start = range.start;
                    self.editor.replace_range(range, &inserted);
                    self.editor.bind_skill(
                        start..start.saturating_add(inserted.len()),
                        SkillSelection::new(skill.name(), skill.path()),
                    );
                    self.advance_past_completion_separator();
                }
                self.skill_popup.hide();
                return Action::None;
            }
        }

        if self.file_search.is_active() {
            if key.code == KeyCode::Up
                || (key.code == KeyCode::Char('p') && control && !alt && !shift)
            {
                self.file_search.move_up();
                return Action::None;
            }
            if key.code == KeyCode::Down
                || (key.code == KeyCode::Char('n') && control && !alt && !shift)
            {
                self.file_search.move_down();
                return Action::None;
            }
            match key.code {
                KeyCode::Tab => {
                    if !self.insert_selected_file() {
                        self.file_search.dismiss();
                    }
                    return Action::None;
                }
                KeyCode::Enter if !shift && !alt && !control && self.insert_selected_file() => {
                    return Action::None;
                }
                _ => {}
            }
        }

        let slash_matches = self.slash_matches();
        if !slash_matches.is_empty() {
            let plain = key.modifiers == KeyModifiers::NONE;
            let control_only = key.modifiers == KeyModifiers::CONTROL;
            let selected = self.slash_selection.min(slash_matches.len() - 1);
            if (plain && key.code == KeyCode::Up)
                || (control_only && key.code == KeyCode::Char('p'))
            {
                self.slash_selection = if selected == 0 {
                    slash_matches.len() - 1
                } else {
                    selected - 1
                };
                return Action::None;
            }
            if (plain && key.code == KeyCode::Down)
                || (control_only && key.code == KeyCode::Char('n'))
            {
                self.slash_selection = (selected + 1) % slash_matches.len();
                return Action::None;
            }
            match key.code {
                KeyCode::Enter if !shift && !alt && !control => {
                    let command = slash_matches[selected];
                    self.complete_slash_command(command, selected);
                    return self.submit_action();
                }
                KeyCode::Tab => {
                    let command = slash_matches[selected];
                    self.complete_slash_command(command, selected);
                    if command.name == "skills" {
                        return self.submit_action();
                    }
                    self.editor.insert(" ");
                    return Action::None;
                }
                _ => {}
            }
        }

        if key.code == KeyCode::Tab && !shift && !alt && !control {
            return if self.busy {
                self.queue_action()
            } else {
                self.submit_action()
            };
        }

        let previous_text = self.editor.text().to_string();
        match key.code {
            KeyCode::Enter if shift || alt || control => self.editor.insert_newline(),
            KeyCode::Enter => return self.submit_action(),
            KeyCode::Char('j') if control => self.editor.insert_newline(),
            KeyCode::Char('d') if control && self.editor.is_empty() => return Action::Quit,
            KeyCode::Char('d') if control => self.editor.delete(),
            KeyCode::Char('a') if control => self.editor.move_home(),
            KeyCode::Char('e') if control => self.editor.move_end(),
            KeyCode::Char('u') if control => self.editor.kill_to_line_start(),
            KeyCode::Char('k') if control => self.editor.kill_to_line_end(),
            KeyCode::Char('w') if control => self.editor.delete_previous_word(),
            // Terminals whose Backspace byte is Ctrl+H encode Option+Backspace as Esc, Ctrl+H.
            KeyCode::Char('h') if control && alt && !shift => {
                self.editor.delete_previous_word();
            }
            KeyCode::Char('b') if alt && !control => self.editor.move_word_left(),
            KeyCode::Char('f') if alt && !control => self.editor.move_word_right(),
            KeyCode::Char(character)
                if ((!control && !alt) || (control && alt)) && !character.is_control() =>
            {
                let mut bytes = [0; 4];
                self.editor.insert(character.encode_utf8(&mut bytes));
            }
            KeyCode::Backspace if control || alt => self.editor.delete_previous_word(),
            KeyCode::Backspace => self.editor.backspace(),
            KeyCode::Delete => self.editor.delete(),
            KeyCode::Left if control || alt => self.editor.move_word_left(),
            KeyCode::Right if control || alt => self.editor.move_word_right(),
            KeyCode::Left => self.editor.move_left(),
            KeyCode::Right => self.editor.move_right(),
            KeyCode::Home => self.editor.move_home(),
            KeyCode::End => self.editor.move_end(),
            KeyCode::Up if self.editor.can_recall_older() => {
                self.editor.history_previous();
            }
            KeyCode::Down if self.editor.can_recall_newer() => {
                self.editor.history_next();
            }
            KeyCode::Up => self.editor.move_vertical(-1, self.composer_text_width),
            KeyCode::Down => self.editor.move_vertical(1, self.composer_text_width),
            _ => {}
        }
        if self.editor.text() != previous_text {
            self.dismissed_slash = None;
            self.slash_selection = 0;
        }
        Action::None
    }

    fn handle_history_search_key(&mut self, key: KeyEvent) -> Action {
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        match key.code {
            KeyCode::Char('o') if control => return self.copy_latest_final_action(),
            KeyCode::Char('r') if control => self.editor.history_search_older(),
            KeyCode::Char('\u{12}') if key.modifiers.is_empty() => {
                self.editor.history_search_older();
            }
            KeyCode::Char('s') if control => self.editor.history_search_newer(),
            KeyCode::Up => self.editor.history_search_older(),
            KeyCode::Down => self.editor.history_search_newer(),
            KeyCode::Esc => self.editor.cancel_history_search(),
            KeyCode::Char('c') if control => self.editor.cancel_history_search(),
            KeyCode::Char('\u{3}') if key.modifiers.is_empty() => {
                self.editor.cancel_history_search();
            }
            KeyCode::Enter => self.editor.accept_history_search(),
            KeyCode::Backspace => self.editor.history_search_backspace(),
            KeyCode::Char('h') if control => self.editor.history_search_backspace(),
            KeyCode::Char('u') if control => self.editor.history_search_clear(),
            KeyCode::Char(character) if !control && !alt && !character.is_control() => {
                let mut bytes = [0; 4];
                self.editor
                    .history_search_insert(character.encode_utf8(&mut bytes));
            }
            _ => {}
        }
        Action::None
    }

    fn complete_slash_command(&mut self, command: &SlashCommand, selection: usize) {
        let query = self.editor.text().strip_prefix('/').unwrap_or_default();
        let name = command.completion_name(query);
        self.editor.set_text(format!("/{name}"));
        self.dismissed_slash = None;
        self.slash_selection = selection;
    }

    fn insert_selected_file(&mut self) -> bool {
        let Some((token_range, path)) = self.file_search.selected_path() else {
            return false;
        };
        let inserted = if path.chars().any(char::is_whitespace) && !path.contains('"') {
            format!("\"{path}\"")
        } else {
            path
        };
        self.editor.replace_range(token_range, &inserted);
        self.advance_past_completion_separator();
        self.file_search.dismiss();
        true
    }

    fn advance_past_completion_separator(&mut self) {
        let cursor = self.editor.cursor();
        let separator = self.editor.text()[cursor..]
            .chars()
            .next()
            .filter(|character| is_horizontal_whitespace(*character));
        let Some(separator) = separator else {
            self.editor.insert(" ");
            return;
        };
        let after_separator = cursor + separator.len_utf8();
        if self.editor.text()[after_separator..]
            .chars()
            .next()
            .is_some_and(|character| !character.is_whitespace())
        {
            self.editor.insert(" ");
        } else {
            self.editor.move_right();
        }
    }

    fn attach_image(&mut self, image: Result<crate::input::PromptImage, String>) {
        let result = image.and_then(|image| self.editor.attach_image(image));
        if let Err(error) = result {
            self.entries.push(TranscriptEntry::Error(format!(
                "Failed to attach image: {error}"
            )));
        }
        self.file_search.dismiss();
        self.skill_popup.hide();
        self.dismissed_slash = None;
        self.slash_selection = 0;
    }

    fn submit_action(&mut self) -> Action {
        if self.editor.text().trim().is_empty() {
            return Action::None;
        }
        let submission = ComposerSubmission::take(&mut self.editor);
        let history_text = submission.prompt().text_without_image_placeholders();
        let command = history_text.trim();
        let local_command = submission.prompt().image_count() == 0;
        if local_command && let Some(shell_command) = command.strip_prefix('!') {
            let shell_command = shell_command.trim();
            if shell_command.is_empty() {
                return self.reject_local_submission(
                    submission,
                    TranscriptEntry::Notice(
                        "Run an operator shell command with !command".to_string(),
                    ),
                );
            }
            return Action::RunShellCommand {
                command: shell_command.to_string(),
                history_text: command.to_string(),
            };
        }
        if local_command
            && let Some(arguments) = command.strip_prefix("/resume")
            && (arguments.is_empty() || arguments.starts_with(char::is_whitespace))
        {
            if self.busy {
                return self.reject_local_submission(
                    submission,
                    TranscriptEntry::Notice(
                        "Interrupt the active turn before resuming another session".to_string(),
                    ),
                );
            }
            let arguments = arguments.trim();
            if arguments.is_empty() {
                return Action::OpenResumePicker(submission);
            }
            return match Uuid::parse_str(arguments) {
                Ok(id) => Action::ResumeSession {
                    id,
                    submission: Some(submission),
                },
                Err(_) => self.reject_local_submission(
                    submission,
                    TranscriptEntry::Error(
                        "`/resume` expects one bettercodex session UUID".to_string(),
                    ),
                ),
            };
        }
        #[cfg(unix)]
        if local_command
            && let Some(arguments) = command.strip_prefix("/tmux")
            && (arguments.is_empty() || arguments.starts_with(char::is_whitespace))
        {
            return match arguments.trim() {
                "" => Action::EnterTmux,
                _ => self.reject_local_submission(
                    submission,
                    TranscriptEntry::Error("`/tmux` does not accept arguments".to_string()),
                ),
            };
        }
        match command {
            _ if !local_command => Action::Submit(submission),
            "/q" | "/quit" | "/exit" => Action::Quit,
            "/compact" if self.busy => self.reject_local_submission(
                submission,
                TranscriptEntry::Error(
                    "'/compact' is disabled while a task is in progress.".to_string(),
                ),
            ),
            "/compact" => Action::Compact,
            "/model" => {
                self.overlay = Some(Overlay::Model(Box::new(ModelPicker::new(
                    self.model_selection.clone(),
                ))));
                Action::None
            }
            "/fast" => Action::ToggleFast,
            "/changelog" => {
                match crate::patch_notes::released() {
                    Ok(Some(markdown)) => self.add_patch_notes(markdown),
                    Ok(None) => {
                        self.add_notice("No released patch notes are available for this build")
                    }
                    Err(error) => self.add_error(format!("Could not load patch notes: {error:#}")),
                }
                Action::None
            }
            "/copy" => self.copy_latest_final_action(),
            "/diff" => Action::ShowDiff,
            "/fork" if self.busy => self.reject_local_submission(
                submission,
                TranscriptEntry::Notice(
                    "Interrupt the active turn before forking this session".to_string(),
                ),
            ),
            "/fork" => Action::Fork(submission),
            "/clear" if self.busy => self.reject_local_submission(
                submission,
                TranscriptEntry::Notice(
                    "Interrupt the active turn before starting a fresh session".to_string(),
                ),
            ),
            "/clear" => Action::Clear(submission),
            "/context" => Action::ShowContext,
            "/help" => {
                self.overlay = Some(Overlay::Shortcuts);
                Action::None
            }
            "/ps" => Action::ListBackgroundProcesses,
            "/skills" if self.busy => self.reject_local_submission(
                submission,
                TranscriptEntry::Error(
                    "'/skills' is disabled while a task is in progress.".to_string(),
                ),
            ),
            "/skills" => {
                self.overlay = Some(Overlay::Skills(SkillsView::new()));
                Action::None
            }
            "/stop" => Action::StopBackgroundProcesses,
            "/logout" if self.busy => self.reject_local_submission(
                submission,
                TranscriptEntry::Notice("Interrupt the active turn before logging out".to_string()),
            ),
            "/logout" => Action::Logout,
            _ => Action::Submit(submission),
        }
    }

    fn reject_local_submission(
        &mut self,
        submission: ComposerSubmission,
        entry: TranscriptEntry,
    ) -> Action {
        self.restore_composer_submission(submission);
        self.entries.push(entry);
        Action::None
    }

    fn queue_action(&mut self) -> Action {
        if self.editor.text().trim().is_empty() {
            return Action::None;
        }
        if self.editor.image_count() == 0 && is_local_command(self.editor.text().trim()) {
            self.entries.push(TranscriptEntry::Notice(
                "Slash commands cannot be queued; use Enter or wait for the active turn"
                    .to_string(),
            ));
            return Action::None;
        }
        let submission = ComposerSubmission::take(&mut self.editor);
        Action::Queue(submission)
    }

    fn copy_latest_final_action(&mut self) -> Action {
        let markdown = self.entries.iter().rev().find_map(|entry| match entry {
            TranscriptEntry::Assistant {
                phase: Some(MessagePhase::Commentary),
                ..
            } => None,
            TranscriptEntry::Assistant {
                content,
                streaming: false,
                ..
            } if !content.source().trim().is_empty() => Some(content.source().to_string()),
            _ => None,
        });
        match markdown {
            Some(markdown) => Action::Copy(markdown),
            None => {
                self.entries.push(TranscriptEntry::Notice(
                    "No completed final response is available to copy".to_string(),
                ));
                Action::None
            }
        }
    }

    fn complete_assistant_message(&mut self, message: AssistantMessage) {
        self.flush_unified_exec_wait_streak();
        self.seal_exploration();
        self.terminal_assistant_received_this_turn |=
            message.is_terminal() && !message.text.trim().is_empty();
        match self.entries.last_mut() {
            Some(TranscriptEntry::Assistant {
                content,
                phase,
                streaming,
                ..
            }) if *streaming => {
                content.replace(message.text);
                *phase = message.phase;
                *streaming = false;
            }
            _ if !message.text.trim().is_empty() => {
                self.entries.push(TranscriptEntry::Assistant {
                    content: MarkdownRenderCache::new(message.text),
                    phase: message.phase,
                    streaming: false,
                    history: StreamedAssistantHistory::default(),
                });
            }
            _ => {}
        }
        self.active_message_phase = None;
        self.status_detail = None;
    }

    fn close_streaming_entries(&mut self) {
        for entry in self.entries.iter_mut().rev() {
            match entry {
                TranscriptEntry::Assistant { streaming, .. } => *streaming = false,
                TranscriptEntry::User(_) => break,
                TranscriptEntry::Tool(_)
                | TranscriptEntry::Exploration { .. }
                | TranscriptEntry::Notice(_)
                | TranscriptEntry::PatchNotes { .. }
                | TranscriptEntry::UpdateAvailable(_)
                | TranscriptEntry::Error(_)
                | TranscriptEntry::Diff(_)
                | TranscriptEntry::Processes(_)
                | TranscriptEntry::FinalMessageSeparator { .. } => {}
            }
        }
        self.active_message_phase = None;
    }

    fn seal_exploration(&mut self) {
        if let Some(TranscriptEntry::Exploration { sealed, .. }) = self.entries.last_mut() {
            *sealed = true;
        }
    }

    fn finish_incomplete_tools(&mut self) {
        for entry in &mut self.entries[self.committed_entries..] {
            match entry {
                TranscriptEntry::Tool(tool) => tool.finish_if_incomplete(),
                TranscriptEntry::Exploration { tools, .. } => {
                    for tool in tools {
                        tool.finish_if_incomplete();
                    }
                }
                _ => {}
            }
        }
    }

    fn find_tool_mut(&mut self, call_id: &str) -> Option<&mut ToolEntry> {
        self.entries.iter_mut().rev().find_map(|entry| match entry {
            TranscriptEntry::Tool(tool) if tool.call_id == call_id => Some(tool),
            TranscriptEntry::Exploration { tools, .. } => {
                tools.iter_mut().rev().find(|tool| tool.call_id == call_id)
            }
            _ => None,
        })
    }

    fn complete_deferred_interaction(
        &mut self,
        mut tool: ToolEntry,
        output: Result<Value, String>,
    ) {
        let ToolDisplay::Interaction {
            session_id, input, ..
        } = &tool.display
        else {
            return;
        };
        let session_id = *session_id;
        let waited_only = input.is_empty();
        let process_is_still_running = output.as_ref().ok().is_some_and(|output| {
            output.get("session_id").and_then(Value::as_i64) == Some(session_id)
        });
        let process_finished = output
            .as_ref()
            .ok()
            .is_some_and(|output| output.get("session_id").is_none());
        let unknown_process = output
            .as_ref()
            .err()
            .is_some_and(|error| error.contains(&format!("Unknown process id {session_id}")));

        tool.outcome = Some(ToolOutcome { output });
        if process_finished || unknown_process {
            self.process_commands.remove(&session_id);
        }

        if waited_only {
            // Match current Codex unified-exec events: an empty poll is transcript-worthy only
            // when it returns while the background process is still alive. Finished and stale
            // polls remain model-visible but do not create misleading terminal history cells.
            if process_is_still_running {
                self.record_unified_exec_wait(session_id, tool);
            }
            return;
        }

        // Codex emits terminal-interaction history only after a successful write. Transport
        // failures are returned to the model without adding a second user-facing error surface.
        if tool
            .outcome
            .as_ref()
            .is_some_and(|outcome| outcome.output.is_ok())
        {
            self.turn_had_work = true;
            self.entries.push(TranscriptEntry::Tool(tool));
        }
    }

    fn record_unified_exec_wait(&mut self, session_id: i64, tool: ToolEntry) {
        match self.unified_exec_wait_streak.take() {
            Some(wait) if wait.session_id == session_id => {
                self.unified_exec_wait_streak = Some(wait);
            }
            Some(wait) => {
                self.turn_had_work = true;
                self.entries.push(TranscriptEntry::Tool(wait.tool));
                self.unified_exec_wait_streak = Some(UnifiedExecWaitStreak { session_id, tool });
            }
            None => {
                self.unified_exec_wait_streak = Some(UnifiedExecWaitStreak { session_id, tool });
            }
        }
    }

    fn flush_unified_exec_wait_streak(&mut self) {
        if let Some(wait) = self.unified_exec_wait_streak.take() {
            self.turn_had_work = true;
            self.entries.push(TranscriptEntry::Tool(wait.tool));
        }
    }

    fn remember_process_command(&mut self, call_id: &str) {
        let remembered = self.find_tool_mut(call_id).and_then(|tool| {
            let ToolDisplay::Command { command, .. } = &tool.display else {
                return None;
            };
            let output = tool.outcome.as_ref()?.output.as_ref().ok()?;
            let session_id = output.get("session_id")?.as_i64()?;
            Some((session_id, command.clone()))
        });
        if let Some((session_id, command)) = remembered {
            self.process_commands.insert(session_id, command);
        }
    }

    fn latest_tool_activity(&self) -> Option<String> {
        let transcript_tool = self.entries[self.committed_entries..]
            .iter()
            .rev()
            .find_map(|entry| match entry {
                TranscriptEntry::Tool(tool) if tool.outcome.is_none() => {
                    Some(tool.activity_label())
                }
                TranscriptEntry::Exploration { tools, .. } => tools
                    .iter()
                    .rev()
                    .find(|tool| tool.outcome.is_none())
                    .map(ToolEntry::activity_label),
                _ => None,
            });
        let deferred_tool = self
            .deferred_interactions
            .values()
            .max_by_key(|tool| tool.started_at)
            .map(ToolEntry::activity_label);
        deferred_tool
            .or_else(|| {
                self.unified_exec_wait_streak
                    .as_ref()
                    .map(|wait| wait.tool.activity_label())
            })
            .or(transcript_tool)
    }

    pub(super) fn take_pending_history_lines(
        &mut self,
        width: u16,
        screen_height: u16,
    ) -> Vec<HyperlinkLine> {
        let width = width.max(1);
        let mut lines = Vec::new();
        if self.welcome_pending {
            append_history_cell(
                terminal_hyperlinks::plain_hyperlink_lines(welcome_lines(
                    &self.cwd,
                    width,
                    screen_height,
                    &self.model_selection,
                    self.service_tier,
                )),
                &mut lines,
                &mut self.history_emitted,
            );
            self.welcome_pending = false;
        }
        while self
            .entries
            .get(self.committed_entries)
            .is_some_and(TranscriptEntry::is_finalized)
        {
            let entry = &mut self.entries[self.committed_entries];
            let (cell, continuation) = entry.display_lines_after_streamed_history(
                width,
                self.user_message_style,
                &self.cwd,
            );
            if continuation {
                lines.extend(cell);
            } else {
                append_history_cell(cell, &mut lines, &mut self.history_emitted);
            }
            entry.reset_streamed_history();
            self.committed_entries += 1;
        }
        lines
    }

    /// Re-render the finalized transcript from its source after a terminal resize.
    ///
    /// A terminal can reflow the mutable composer into scrollback before crossterm delivers the
    /// resize event. Codex repairs that state by clearing its terminal surface and replaying the
    /// retained transcript at the new width instead of trusting terminal-wrapped rows.
    pub(super) fn history_lines_for_resize_reflow(
        &mut self,
        width: u16,
        screen_height: u16,
    ) -> Vec<HyperlinkLine> {
        let width = width.max(1);
        for entry in &mut self.entries {
            entry.reset_streamed_history();
        }
        while self
            .entries
            .get(self.committed_entries)
            .is_some_and(TranscriptEntry::is_finalized)
        {
            self.committed_entries += 1;
        }

        let mut lines = Vec::new();
        let mut emitted = false;
        append_history_cell(
            terminal_hyperlinks::plain_hyperlink_lines(welcome_lines(
                &self.cwd,
                width,
                screen_height,
                &self.model_selection,
                self.service_tier,
            )),
            &mut lines,
            &mut emitted,
        );
        for entry in &mut self.entries[..self.committed_entries] {
            append_history_cell(
                entry.display_lines(width, self.user_message_style, &self.cwd),
                &mut lines,
                &mut emitted,
            );
        }
        self.welcome_pending = false;
        self.history_emitted = emitted;
        lines
    }

    /// Whether source-backed replay is needed before adding more terminal history.
    ///
    /// Width changes invalidate physical row boundaries. A completed stream can also reveal that
    /// later Markdown changed a row which was already emitted; replay repairs that uncommon case
    /// from the retained source before the finalized suffix is inserted.
    pub(super) fn streamed_history_needs_reflow(&mut self, width: u16) -> bool {
        let width = width.max(1);
        let cwd = &self.cwd;
        self.entries[self.committed_entries..]
            .iter_mut()
            .any(|entry| entry.streamed_history_needs_reflow(width, cwd))
    }

    /// Move complete rendered lines from a growing assistant response into terminal history.
    ///
    /// Capacity is measured in physical terminal rows, including soft wraps, but at least one
    /// logical line stays live so unterminated code and URLs retain their copy/paste semantics while
    /// still changing. In normal terminals the composer/status layout leaves a larger mutable tail.
    fn spill_streaming_history(&mut self, width: u16, live_capacity: usize) -> Vec<HyperlinkLine> {
        let history_was_emitted = self.history_emitted;
        let Some(TranscriptEntry::Assistant {
            content,
            streaming: true,
            history,
            ..
        }) = self.entries.get_mut(self.committed_entries)
        else {
            return Vec::new();
        };

        if history.started && history.width != Some(width) {
            return Vec::new();
        }
        let start = history.lines.len();
        let (rendered_len, remaining) =
            assistant_lines_after(content, width, &self.cwd, true, start);
        if start > rendered_len {
            return Vec::new();
        }

        let spill_separator = !history.started && history_was_emitted;
        let remaining_rows = remaining
            .iter()
            .map(|line| super::terminal::transcript_line_height(line, width))
            .fold(usize::from(spill_separator), usize::saturating_add);
        let overflow_rows = remaining_rows.saturating_sub(live_capacity);
        if overflow_rows == 0 {
            return Vec::new();
        }

        // Preserve one complete logical line as the mutable tail. Overwide code and URLs are kept
        // intact for copy/paste, so spill whole preceding lines until their physical terminal rows
        // cover the viewport overflow.
        let mut covered_rows = usize::from(spill_separator);
        let mut lines_to_spill = 0_usize;
        for line in remaining.iter().take(remaining.len().saturating_sub(1)) {
            if covered_rows >= overflow_rows {
                break;
            }
            covered_rows =
                covered_rows.saturating_add(super::terminal::transcript_line_height(line, width));
            lines_to_spill += 1;
        }
        if !spill_separator && lines_to_spill == 0 {
            return Vec::new();
        }

        let mut output = Vec::with_capacity(usize::from(spill_separator) + lines_to_spill);
        if !history.started {
            history.started = true;
            history.width = Some(width);
            self.history_emitted = true;
            if spill_separator {
                output.push(HyperlinkLine::default());
            }
        }

        let end = lines_to_spill.min(remaining.len());
        let mut newly_emitted = remaining;
        newly_emitted.truncate(end);
        output.extend(newly_emitted.iter().cloned());
        history.lines.extend(newly_emitted);
        output
    }

    #[cfg(test)]
    pub(super) fn desired_height(&mut self, width: u16, screen_height: u16) -> u16 {
        let width = width.max(1);
        let active_lines = self.active_lines(width);
        let active_height = rendered_line_count(&active_lines, width);
        self.desired_height_with_active_history(width, screen_height, active_height)
    }

    pub(super) fn prepare(&mut self, width: u16, screen_height: u16) -> PreparedView {
        let width = width.max(1);
        let (transcript_chrome_height, overlay_height) =
            self.height_requirements(width, screen_height);
        let live_capacity = usize::from(
            screen_height
                .max(1)
                .saturating_sub(transcript_chrome_height)
                .max(1),
        );
        let history_lines = self.spill_streaming_history(width, live_capacity);
        let active_lines = self.active_lines(width);
        let active_height = rendered_line_count(&active_lines, width);
        let height = active_height
            .saturating_add(transcript_chrome_height)
            .max(overlay_height)
            .clamp(1, screen_height.max(1));
        PreparedView {
            width,
            height,
            active_height,
            active_lines,
            history_lines,
        }
    }

    #[cfg(test)]
    fn desired_height_with_active_history(
        &mut self,
        width: u16,
        screen_height: u16,
        active_height: u16,
    ) -> u16 {
        let (transcript_chrome_height, overlay_height) =
            self.height_requirements(width, screen_height);
        active_height
            .saturating_add(transcript_chrome_height)
            .max(overlay_height)
            .clamp(1, screen_height.max(1))
    }

    fn height_requirements(&mut self, width: u16, screen_height: u16) -> (u16, u16) {
        self.composer_text_width = width.saturating_sub(3).max(1);
        let composer_height = self
            .editor
            .desired_height(self.composer_text_width)
            .max(1)
            .saturating_add(2);
        let pending_height = u16::try_from(self.pending_input.lines().len()).unwrap_or(u16::MAX);
        let activity_height = self.activity_height(width);
        let activity_gap = if activity_height > 0 {
            ACTIVITY_COMPOSER_GAP
        } else {
            0
        };
        let bottom_spacing: u16 = 1;
        let popup_height = self.completion_popup_height(width);
        // Match Codex's bottom-pane layout: an active completion list replaces the footer and
        // extends downward from the composer instead of becoming an overlay above it.
        let trailing_height = if popup_height > 0 {
            popup_height
        } else {
            COMPOSER_FOOTER_GAP.saturating_add(STATUS_LINE_HEIGHT)
        };
        let overlay_height = match self.overlay.as_ref() {
            Some(Overlay::Shortcuts) => shortcuts_height(width),
            Some(Overlay::Context(context)) => context.preferred_height(width),
            Some(Overlay::Model(picker)) => picker.preferred_height(width),
            Some(Overlay::Resume(_)) => screen_height,
            Some(Overlay::Skills(skills)) => skills.preferred_height(&self.skills, width),
            None => 0,
        };
        let transcript_chrome_height = bottom_spacing
            .saturating_add(pending_height)
            .saturating_add(activity_height)
            .saturating_add(activity_gap)
            .saturating_add(composer_height)
            .saturating_add(trailing_height);
        (transcript_chrome_height, overlay_height)
    }

    #[cfg(test)]
    pub(super) fn render(&mut self, frame: &mut Frame<'_>) {
        self.render_frame(frame, None);
    }

    pub(super) fn render_prepared(&mut self, frame: &mut Frame<'_>, prepared: PreparedView) {
        let prepared = (frame.area().width.max(1) == prepared.width).then_some(prepared);
        self.render_frame(frame, prepared);
    }

    fn render_frame(&mut self, frame: &mut Frame<'_>, prepared: Option<PreparedView>) {
        let area = frame.area();
        if area.is_empty() {
            return;
        }
        self.composer_text_width = area.width.saturating_sub(3).max(1);
        let popup_height = if self.overlay.is_some() {
            0
        } else {
            self.completion_popup_height(area.width)
        };
        let requested_trailing_height = if popup_height > 0 {
            popup_height
        } else {
            COMPOSER_FOOTER_GAP.saturating_add(STATUS_LINE_HEIGHT)
        };
        // Like Codex, let the composer consume the available terminal height before its textarea
        // scrolls. The footer/completion area and active-work status retain their own rows.
        let minimum_composer_height = area.height.min(3);
        let trailing_height =
            requested_trailing_height.min(area.height.saturating_sub(minimum_composer_height));
        let height_above_trailing = area.height.saturating_sub(trailing_height);
        let requested_activity_height = self.activity_height(area.width);
        let requested_activity_gap = if requested_activity_height > 0 {
            ACTIVITY_COMPOSER_GAP
        } else {
            0
        };
        let requested_activity_block_height =
            requested_activity_height.saturating_add(requested_activity_gap);
        let activity_block_height = requested_activity_block_height
            .min(height_above_trailing.saturating_sub(minimum_composer_height));
        let pending_lines = self.pending_input.lines();
        let requested_pending_height = u16::try_from(pending_lines.len()).unwrap_or(u16::MAX);
        let pending_height = requested_pending_height.min(
            height_above_trailing
                .saturating_sub(activity_block_height)
                .saturating_sub(minimum_composer_height),
        );
        let composer_height_limit = height_above_trailing
            .saturating_sub(activity_block_height)
            .saturating_sub(pending_height);
        let editor_height_limit = composer_height_limit.saturating_sub(2).max(1);
        let editor_layout = self
            .editor
            .layout(self.composer_text_width, editor_height_limit);
        let editor_rows = (editor_layout.lines.len() as u16).max(1);
        let composer_height = editor_rows.saturating_add(2).min(composer_height_limit);
        let composer_y = area
            .bottom()
            .saturating_sub(composer_height.saturating_add(trailing_height));
        let composer_area = Rect::new(area.x, composer_y, area.width, composer_height);
        let trailing_area = Rect::new(area.x, composer_area.bottom(), area.width, trailing_height);
        let footer_area = if popup_height == 0 {
            trailing_area
        } else {
            Rect::default()
        };
        let popup_area = if popup_height > 0 {
            trailing_area
        } else {
            Rect::default()
        };
        let activity_top = composer_y.saturating_sub(activity_block_height);
        let activity_height = requested_activity_height.min(activity_block_height);
        let activity_area = Rect::new(area.x, activity_top, area.width, activity_height);
        let pending_bottom = if activity_block_height > 0 {
            activity_top
        } else {
            composer_area.y
        };
        let pending_area = Rect::new(
            area.x,
            pending_bottom.saturating_sub(pending_height),
            area.width,
            pending_height,
        );
        let content_bottom = if pending_height > 0 {
            pending_area.y
        } else if activity_block_height > 0 {
            activity_top
        } else {
            composer_area.y
        };
        let history_bottom = content_bottom.saturating_sub(1).max(area.y);
        let history_area = Rect::new(
            area.x,
            area.y,
            area.width,
            history_bottom.saturating_sub(area.y),
        );

        self.render_active_history(frame, history_area, prepared);
        if pending_height > 0 {
            frame.render_widget(
                Paragraph::new(
                    pending_lines
                        .into_iter()
                        .take(usize::from(pending_height))
                        .map(|line| truncate_line(line, usize::from(pending_area.width)))
                        .collect::<Vec<_>>(),
                ),
                pending_area,
            );
        }
        if activity_height > 0 {
            frame.render_widget(
                Paragraph::new(self.activity_lines(activity_area.width)),
                activity_area,
            );
        }
        self.render_composer(frame, composer_area, footer_area, editor_layout);
        if self.overlay.is_none() {
            self.render_completion_popup(frame, popup_area);
        }
        match self.overlay.as_mut() {
            Some(Overlay::Shortcuts) => self.render_shortcuts(frame, area),
            Some(Overlay::Context(context)) => context.render(frame, area, self.user_message_style),
            Some(Overlay::Model(picker)) => picker.render(frame, area, self.user_message_style),
            Some(Overlay::Resume(picker)) => picker.render(frame, area),
            Some(Overlay::Skills(skills)) => {
                skills.render(frame, area, &self.skills, self.user_message_style)
            }
            None => {}
        }
    }

    fn render_active_history(
        &mut self,
        frame: &mut Frame<'_>,
        area: Rect,
        prepared: Option<PreparedView>,
    ) {
        if area.is_empty() {
            return;
        }
        let (lines, active_height) = prepared.map_or_else(
            || {
                let lines = self.active_lines(area.width);
                let active_height = rendered_line_count(&lines, area.width);
                (lines, active_height)
            },
            |prepared| (prepared.active_lines, prepared.active_height),
        );
        if lines.is_empty() {
            return;
        }
        let overflow = usize::from(active_height).saturating_sub(usize::from(area.height));
        super::terminal::render_transcript_lines(&lines, area, overflow, frame.buffer_mut());
    }

    fn render_composer(
        &self,
        frame: &mut Frame<'_>,
        area: Rect,
        footer_area: Rect,
        layout: super::editor::EditorLayout,
    ) {
        if area.is_empty() {
            return;
        }
        frame.render_widget(Block::default().style(self.user_message_style), area);
        let text_y = area.y.saturating_add(1);
        let text_x = area.x.saturating_add(LIVE_PREFIX_COLS);
        let text_width = area.right().saturating_sub(text_x).saturating_sub(1).max(1);
        let super::editor::EditorLayout {
            lines,
            paste_ranges,
            skill_ranges,
            image_ranges,
            history_search_ranges,
            cursor_row,
            cursor_column,
            ..
        } = layout;
        let lines = if lines.is_empty() {
            vec![String::new()]
        } else {
            lines
        };
        for (index, text) in lines.iter().enumerate() {
            let Ok(index) = u16::try_from(index) else {
                break;
            };
            let y = text_y.saturating_add(index);
            if y >= area.bottom().saturating_sub(1) {
                break;
            }
            frame.render_widget(
                Paragraph::new(editor_line(
                    text,
                    paste_ranges
                        .get(usize::from(index))
                        .map(Vec::as_slice)
                        .unwrap_or_default(),
                    skill_ranges
                        .get(usize::from(index))
                        .map(Vec::as_slice)
                        .unwrap_or_default(),
                    image_ranges
                        .get(usize::from(index))
                        .map(Vec::as_slice)
                        .unwrap_or_default(),
                    history_search_ranges
                        .get(usize::from(index))
                        .map(Vec::as_slice)
                        .unwrap_or_default(),
                )),
                Rect::new(text_x, y, text_width, 1),
            );
        }
        frame.render_widget(
            Paragraph::new(Line::from(Span::from("›").bold())),
            Rect::new(area.x, text_y, 1, 1),
        );

        if footer_area.height > COMPOSER_FOOTER_GAP {
            frame.render_widget(
                Paragraph::new(self.status_line(footer_area.width)),
                Rect::new(
                    footer_area.x,
                    footer_area.y.saturating_add(COMPOSER_FOOTER_GAP),
                    footer_area.width,
                    STATUS_LINE_HEIGHT,
                ),
            );
        }

        if self.overlay.is_none() && self.editor.history_search_active() && !footer_area.is_empty()
        {
            let prefix_width = UnicodeWidthStr::width("reverse-i-search: ");
            let query_width = self
                .editor
                .history_search_query()
                .map_or(0, UnicodeWidthStr::width);
            let cursor_x = footer_area
                .x
                .saturating_add(u16::try_from(prefix_width + query_width).unwrap_or(u16::MAX))
                .min(footer_area.right().saturating_sub(1));
            frame.set_cursor_position(Position::new(cursor_x, footer_area.y));
        } else if self.overlay.is_none() {
            let cursor_x = text_x
                .saturating_add(cursor_column)
                .min(area.right().saturating_sub(1));
            let cursor_y = text_y
                .saturating_add(cursor_row)
                .min(area.bottom().saturating_sub(2));
            frame.set_cursor_position(Position::new(cursor_x, cursor_y));
        }
    }

    fn render_shortcuts(&self, frame: &mut Frame<'_>, area: Rect) {
        if area.is_empty() {
            return;
        }
        frame.render_widget(Clear, area);
        let footer_height = 1.min(area.height);
        let content_area = Rect::new(
            area.x,
            area.y,
            area.width,
            area.height.saturating_sub(footer_height),
        );
        let footer_area = Rect::new(area.x, content_area.bottom(), area.width, footer_height);
        let inner = render_menu_surface(content_area, frame.buffer_mut(), self.user_message_style);
        if !inner.is_empty() {
            frame.render_widget(
                Paragraph::new(shortcut_reference_lines()).wrap(Wrap { trim: false }),
                inner,
            );
        }
        if !footer_area.is_empty() {
            let hint_area = Rect::new(
                footer_area.x.saturating_add(2),
                footer_area.y,
                footer_area.width.saturating_sub(2),
                footer_area.height,
            );
            frame.render_widget(Paragraph::new("Press any key to go back").dim(), hint_area);
        }
    }

    fn render_completion_popup(&self, frame: &mut Frame<'_>, area: Rect) {
        if self.skill_popup.is_active() {
            if area.is_empty() {
                return;
            }
            let lines = self
                .skill_popup
                .lines(&self.skills)
                .into_iter()
                .map(|line| truncate_line(line, usize::from(area.width)))
                .collect::<Vec<_>>();
            frame.render_widget(Paragraph::new(lines), area);
        } else if self.file_search.is_active() {
            if area.is_empty() {
                return;
            }
            let lines = self
                .file_search
                .lines(area.width)
                .into_iter()
                .map(|line| truncate_line(line, usize::from(area.width)))
                .collect::<Vec<_>>();
            frame.render_widget(Paragraph::new(lines), area);
        } else {
            self.render_slash_popup(frame, area);
        }
    }

    fn render_slash_popup(&self, frame: &mut Frame<'_>, area: Rect) {
        let matches = self.slash_matches();
        if matches.is_empty() || area.is_empty() || area.width <= LIVE_PREFIX_COLS {
            return;
        }
        let popup = Rect::new(
            area.x.saturating_add(LIVE_PREFIX_COLS),
            area.y,
            area.width.saturating_sub(LIVE_PREFIX_COLS),
            area.height,
        );
        let selected = self.slash_selection.min(matches.len() - 1);
        let visible = MAX_POPUP_ROWS.min(matches.len());
        let start = selected
            .saturating_add(1)
            .saturating_sub(visible)
            .min(matches.len().saturating_sub(visible));
        let query = self.editor.text().strip_prefix('/').unwrap_or_default();
        let name_width = matches
            .iter()
            .map(|command| command.display_width())
            .max()
            .unwrap_or(1);
        let lines = matches
            .iter()
            .enumerate()
            .skip(start)
            .take(visible)
            .map(|(index, command)| {
                let mut spans = Vec::with_capacity(command.aliases.len().saturating_mul(4) + 5);
                for (name_index, name) in command.names().enumerate() {
                    if name_index > 0 {
                        spans.push(Span::from(", "));
                    }
                    let matched = if name.starts_with(query) {
                        query.len()
                    } else {
                        0
                    };
                    spans.push(Span::from("/"));
                    spans.push(Span::from(name[..matched].to_string()).bold());
                    spans.push(Span::from(name[matched..].to_string()));
                }
                spans.push(Span::from(
                    " ".repeat(
                        name_width
                            .saturating_sub(command.display_width())
                            .saturating_add(2),
                    ),
                ));
                spans.push(Span::from(command.description).dim());
                let mut line = Line::from(spans);
                if index == selected {
                    let selected_style = palette::accent_style();
                    for span in &mut line.spans {
                        span.style = selected_style;
                    }
                }
                truncate_line(line, usize::from(popup.width))
            })
            .collect::<Vec<_>>();
        frame.render_widget(Paragraph::new(lines), popup);
    }

    fn slash_popup_height(&self, _width: u16) -> u16 {
        u16::try_from(MAX_POPUP_ROWS.min(self.slash_matches().len())).unwrap_or(u16::MAX)
    }

    fn completion_popup_height(&self, width: u16) -> u16 {
        if self.skill_popup.is_active() {
            self.skill_popup.height()
        } else if self.file_search.is_active() {
            self.file_search.height()
        } else {
            self.slash_popup_height(width)
        }
    }

    fn slash_matches(&self) -> Vec<&'static SlashCommand> {
        if self.editor.is_browsing_history() || self.editor.history_search_active() {
            return Vec::new();
        }
        let text = self.editor.text();
        if self.dismissed_slash.as_deref() == Some(text) {
            return Vec::new();
        }
        let Some(query) = text.strip_prefix('/') else {
            return Vec::new();
        };
        if query.chars().any(char::is_whitespace) {
            return Vec::new();
        }
        SLASH_COMMANDS
            .iter()
            .filter(|command| command.matches(query))
            .collect()
    }

    fn working_line(&self) -> Line<'static> {
        let elapsed = self
            .working_since
            .map(|started| started.elapsed())
            .unwrap_or_default();
        let waiting_for_background_terminal = self.waiting_for_background_terminal();
        let heading = if self.interrupting.is_some() {
            "Interrupting"
        } else if self.status_detail.as_deref() == Some("Compacting conversation") {
            "Compacting"
        } else if waiting_for_background_terminal {
            "Waiting for background terminal"
        } else {
            self.reasoning_status.heading().unwrap_or("Working")
        };
        let mut spans = vec![activity_marker(self.working_since), " ".into()];
        spans.extend(shimmer_spans(heading));
        spans.push(
            format!(
                " ({} • esc to interrupt)",
                format_elapsed(elapsed.as_secs())
            )
            .dim(),
        );
        let pending_steers = self.pending_input.steer_count();
        let queued_follow_ups = self.pending_input.follow_up_count();
        if pending_steers > 0 {
            spans.push(Span::from(" · ").dim());
            spans.push(Span::from(format!("{pending_steers} steering")).dim());
        }
        if queued_follow_ups > 0 {
            spans.push(Span::from(" · ").dim());
            spans.push(Span::from(format!("{queued_follow_ups} queued")).dim());
        }
        Line::from(spans)
    }

    fn waiting_for_background_terminal(&self) -> bool {
        self.unified_exec_wait_streak.is_some()
            || self
                .deferred_interactions
                .values()
                .any(ToolEntry::is_empty_interaction)
    }
}

impl View {
    fn activity_height(&self, width: u16) -> u16 {
        u16::try_from(self.activity_lines(width).len()).unwrap_or(u16::MAX)
    }

    fn activity_lines(&self, width: u16) -> Vec<Line<'static>> {
        if !self.busy {
            return self.background_process_line(width).into_iter().collect();
        }

        let mut lines = vec![truncate_line(self.working_line(), usize::from(width))];
        lines.extend(self.status_detail_line(width));
        // bettercodex deliberately keeps this footer on its own row while busy. Codex folds it
        // into the status header, which hides both surfaces behind the same truncation boundary.
        lines.extend(self.background_process_line(width));
        lines
    }

    fn status_detail_line(&self, width: u16) -> Option<Line<'static>> {
        let detail = self
            .status_detail
            .as_deref()
            .filter(|detail| *detail != "Compacting conversation")?
            .trim();
        if detail.is_empty() {
            return None;
        }
        let detail = first_display_line(&markdown::sanitize(detail));
        if detail.is_empty() {
            return None;
        }
        // Match Codex's status-details surface: live tool activity belongs below the header, where
        // it cannot displace the elapsed time or interrupt affordance.
        Some(truncate_line(
            Line::from(vec![STATUS_DETAIL_PREFIX.dim(), detail.dim()]),
            usize::from(width),
        ))
    }

    fn background_process_summary(&self) -> Option<String> {
        let count = self.background_processes.len();
        if count == 0 {
            return None;
        }
        let plural = if count == 1 { "" } else { "s" };
        Some(format!(
            "{count} background terminal{plural} running · /ps to view · /stop to close"
        ))
    }

    fn background_process_line(&self, width: u16) -> Option<Line<'static>> {
        if width < 4 {
            return None;
        }
        let summary = self.background_process_summary()?;
        Some(truncate_line(
            Line::from(vec![Span::from("  ").dim(), Span::from(summary).dim()]),
            usize::from(width),
        ))
    }

    fn status_line(&self, width: u16) -> Line<'static> {
        if let (Some(query), Some(status)) = (
            self.editor.history_search_query(),
            self.editor.history_search_status(),
        ) {
            let mut spans = vec!["reverse-i-search: ".dim(), query.to_string().cyan()];
            match status {
                editor::HistorySearchStatus::Idle => {}
                editor::HistorySearchStatus::Match => spans.extend([
                    "  ".into(),
                    "Enter".cyan().bold(),
                    " accept · ".dim(),
                    "Esc".cyan().bold(),
                    " cancel".dim(),
                ]),
                editor::HistorySearchStatus::Searching => {
                    spans.push("  searching older prompts…".dim());
                }
                editor::HistorySearchStatus::NoMatch => spans.push("  no match".red()),
            }
            return truncate_line(Line::from(spans), usize::from(width));
        }
        let mut spans = vec![
            Span::from(self.model_selection.model.clone()),
            Span::styled(
                format!(" {}", self.model_selection.reasoning_effort),
                Style::default().fg(MUTED),
            ),
        ];
        if self.service_tier.is_fast() {
            spans.push(Span::styled(" fast", Style::default().fg(Color::Magenta)));
        }
        spans.extend([
            Span::styled(" │ ", Style::default().fg(MUTED)),
            Span::styled(
                self.repository.name.clone(),
                Style::default().fg(Color::Cyan),
            ),
        ]);
        if let Some(branch) = &self.repository.branch {
            spans.push(Span::styled(
                format!(" / {branch}"),
                Style::default().fg(MUTED),
            ));
        }
        spans.push(Span::styled(" │ ", Style::default().fg(MUTED)));
        spans.push(Span::styled(
            format_context_usage(
                self.context_tokens,
                self.model_selection.effective_context_window(),
            ),
            Style::default().fg(MUTED),
        ));
        truncate_line(Line::from(spans), usize::from(width))
    }

    fn active_lines(&mut self, width: u16) -> Vec<HyperlinkLine> {
        let mut lines = Vec::new();
        let mut emitted = self.history_emitted;
        for entry in &mut self.entries[self.committed_entries..] {
            let (cell, continuation) = entry.display_lines_after_streamed_history(
                width,
                self.user_message_style,
                &self.cwd,
            );
            if continuation {
                lines.extend(cell);
            } else {
                append_history_cell(cell, &mut lines, &mut emitted);
            }
        }
        lines
    }
}

fn is_local_command(command: &str) -> bool {
    if command.starts_with('!') {
        return true;
    }
    if let Some(arguments) = command.strip_prefix("/resume")
        && (arguments.is_empty() || arguments.starts_with(char::is_whitespace))
    {
        return true;
    }
    #[cfg(unix)]
    {
        if let Some(arguments) = command.strip_prefix("/tmux")
            && (arguments.is_empty() || arguments.starts_with(char::is_whitespace))
        {
            return true;
        }
    }
    matches!(
        command,
        "/q" | "/quit"
            | "/exit"
            | "/compact"
            | "/model"
            | "/fast"
            | "/changelog"
            | "/copy"
            | "/diff"
            | "/clear"
            | "/fork"
            | "/context"
            | "/help"
            | "/ps"
            | "/skills"
            | "/stop"
            | "/logout"
    )
}

impl TranscriptEntry {
    fn is_finalized(&self) -> bool {
        match self {
            Self::User(_)
            | Self::Notice(_)
            | Self::PatchNotes { .. }
            | Self::UpdateAvailable(_)
            | Self::Error(_)
            | Self::Diff(_)
            | Self::Processes(_)
            | Self::FinalMessageSeparator { .. } => true,
            Self::Assistant { streaming, .. } => !streaming,
            Self::Tool(tool) => tool.outcome.is_some(),
            Self::Exploration { tools, sealed } => {
                *sealed && tools.iter().all(|tool| tool.outcome.is_some())
            }
        }
    }

    fn display_lines_after_streamed_history(
        &mut self,
        width: u16,
        user_style: Style,
        cwd: &Path,
    ) -> (Vec<HyperlinkLine>, bool) {
        match self {
            Self::Assistant {
                content,
                streaming,
                history,
                ..
            } if history.started && history.width == Some(width) => {
                let (_, output) =
                    assistant_lines_after(content, width, cwd, *streaming, history.lines.len());
                (output, true)
            }
            _ => (self.display_lines(width, user_style, cwd), false),
        }
    }

    fn streamed_history_needs_reflow(&mut self, width: u16, cwd: &Path) -> bool {
        let Self::Assistant {
            content,
            streaming,
            history,
            ..
        } = self
        else {
            return false;
        };
        if !history.started {
            return false;
        }
        if history.width != Some(width) {
            return true;
        }
        // Appending source normally changes only the retained live tail. Compare the canonical
        // prefix once the item closes, when Markdown constructs such as reference links can no
        // longer rewrite it again.
        if *streaming {
            return false;
        }
        !assistant_lines(content, width, cwd, false).starts_with(&history.lines)
    }

    fn reset_streamed_history(&mut self) {
        if let Self::Assistant { history, .. } = self {
            *history = StreamedAssistantHistory::default();
        }
    }

    fn display_lines(&mut self, width: u16, user_style: Style, cwd: &Path) -> Vec<HyperlinkLine> {
        let plain_lines = match self {
            Self::User(message) => user_message_lines(message, width, user_style),
            Self::Assistant {
                content, streaming, ..
            } => return assistant_lines(content, width, cwd, *streaming),
            Self::Tool(tool) => tool.display_lines(width, user_style),
            Self::Exploration { tools, .. } => exploration_lines(tools, width),
            Self::Notice(message) => vec![Line::from(vec![
                Span::from("• ").dim(),
                Span::from(message.clone()).dim(),
            ])],
            Self::PatchNotes { content } => {
                return patch_notes_lines(content, width, cwd);
            }
            Self::UpdateAvailable(update) => update_available_lines(update, width),
            Self::Error(message) => vec![Line::from(vec![
                Span::styled("■ ", Style::default().fg(Color::Red)),
                Span::styled(message.clone(), Style::default().fg(Color::Red)),
            ])],
            Self::Diff(diff) => git_diff_lines(diff, width),
            Self::Processes(processes) => background_process_lines(processes, width),
            Self::FinalMessageSeparator { elapsed_seconds } => {
                final_message_separator_lines(*elapsed_seconds, width)
            }
        };
        terminal_hyperlinks::plain_hyperlink_lines(plain_lines)
    }
}

impl ToolEntry {
    fn new(
        call_id: String,
        name: String,
        input: Option<Value>,
        cwd: &Path,
        process_commands: &HashMap<i64, String>,
    ) -> Self {
        let display = match name.as_str() {
            "exec" => ToolDisplay::CodeMode(
                input
                    .as_ref()
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
            ),
            "wait" => ToolDisplay::CodeMode(format!(
                "wait({})",
                input.as_ref().map(Value::to_string).unwrap_or_default()
            )),
            "exec_command" => {
                let command = input
                    .as_ref()
                    .and_then(|input| input.get("cmd"))
                    .and_then(Value::as_str)
                    .unwrap_or("exec_command")
                    .to_string();
                let command_argv = input
                    .as_ref()
                    .map(crate::tools::command_argv_for_display)
                    .unwrap_or_default();
                ToolDisplay::Command {
                    command,
                    parsed: parse_command(&command_argv),
                }
            }
            "write_stdin" => {
                let session_id = input
                    .as_ref()
                    .and_then(|input| input.get("session_id"))
                    .and_then(Value::as_i64)
                    .unwrap_or_default();
                let interaction = input
                    .as_ref()
                    .and_then(|input| input.get("chars"))
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                ToolDisplay::Interaction {
                    session_id,
                    command: process_commands
                        .get(&session_id)
                        .cloned()
                        .unwrap_or_else(|| format!("process {session_id}")),
                    input: interaction,
                }
            }
            "apply_patch" => ToolDisplay::Patch(PatchDisplay::parse(
                input.as_ref().and_then(Value::as_str).unwrap_or_default(),
                cwd,
            )),
            "log_papercut" => ToolDisplay::Papercut,
            "update_plan" => ToolDisplay::Plan(PlanDisplay::parse(input.as_ref())),
            "view_image" => ToolDisplay::ViewImage(
                input
                    .as_ref()
                    .and_then(|input| input.get("path"))
                    .and_then(Value::as_str)
                    .map(|path| display_tool_path(Path::new(path), cwd))
                    .unwrap_or_else(|| "image".to_string()),
            ),
            "web.run" => {
                ToolDisplay::WebSearch(crate::web_search::action_for_display(input.clone()))
            }
            _ => OpenAiDocsActivity::from_call(&name, input.as_ref())
                .map(ToolDisplay::OpenAiDocs)
                .unwrap_or(ToolDisplay::Other),
        };
        Self {
            call_id,
            name,
            input,
            display,
            outcome: None,
            started_at: Instant::now(),
        }
    }

    fn from_session_transcript(
        tool: SessionTranscriptTool,
        cwd: &Path,
        process_commands: &HashMap<i64, String>,
    ) -> Self {
        let SessionTranscriptTool {
            call_id,
            name,
            input,
            output,
        } = tool;
        let mut tool = Self::new(call_id, name, input, cwd, process_commands);
        tool.outcome = output.map(|output| ToolOutcome {
            output: match output {
                SessionTranscriptToolOutput::Success(output) => Ok(output),
                SessionTranscriptToolOutput::Error(error) => Err(error),
            },
        });
        tool.finish_if_incomplete();
        tool
    }

    fn session_transcript_tool(&self, retain_success_output: bool) -> SessionTranscriptTool {
        let retain_success_output = retain_success_output
            && matches!(
                &self.display,
                ToolDisplay::CodeMode(_)
                    | ToolDisplay::Command { .. }
                    | ToolDisplay::Interaction { .. }
                    | ToolDisplay::Papercut
                    | ToolDisplay::Other
            );
        let output = self.outcome.as_ref().map(|outcome| match &outcome.output {
            Ok(output) => SessionTranscriptToolOutput::Success(if retain_success_output {
                output.clone()
            } else {
                Value::Null
            }),
            Err(error) => SessionTranscriptToolOutput::Error(error.clone()),
        });
        SessionTranscriptTool {
            call_id: self.call_id.clone(),
            name: self.name.clone(),
            input: self.input.clone(),
            output,
        }
    }

    fn is_exploration(&self) -> bool {
        match &self.display {
            ToolDisplay::Command { parsed, .. } => {
                !parsed.is_empty()
                    && parsed
                        .iter()
                        .all(|command| !matches!(command, ParsedCommand::Unknown { .. }))
            }
            ToolDisplay::OpenAiDocs(_) => true,
            _ => false,
        }
    }

    fn is_interaction(&self) -> bool {
        matches!(self.display, ToolDisplay::Interaction { .. })
    }

    fn is_empty_interaction(&self) -> bool {
        matches!(
            &self.display,
            ToolDisplay::Interaction { input, .. } if input.is_empty()
        )
    }

    fn has_interaction_input(&self) -> bool {
        matches!(
            &self.display,
            ToolDisplay::Interaction { input, .. } if !input.is_empty()
        )
    }

    fn activity_label(&self) -> String {
        match &self.display {
            ToolDisplay::CodeMode(_) => "Running code".to_string(),
            ToolDisplay::Command { command, .. } => first_display_line(command),
            ToolDisplay::Interaction { command, .. } => command.clone(),
            ToolDisplay::Patch(_) => "Applying patch".to_string(),
            ToolDisplay::Papercut => "Logging papercut".to_string(),
            ToolDisplay::Plan(_) => "Updating plan".to_string(),
            ToolDisplay::ViewImage(path) => format!("Viewing {path}"),
            ToolDisplay::WebSearch(_) => "Searching the web".to_string(),
            ToolDisplay::OpenAiDocs(activity) => activity.active_label.to_string(),
            ToolDisplay::Other => self.name.clone(),
        }
    }

    fn finish_if_incomplete(&mut self) {
        if self.outcome.is_none() {
            self.outcome = Some(ToolOutcome {
                output: Err("tool stopped before returning a result".to_string()),
            });
        }
    }

    fn display_lines(&self, width: u16, user_style: Style) -> Vec<Line<'static>> {
        match &self.display {
            ToolDisplay::CodeMode(source) => code_mode_lines(self, source, width),
            ToolDisplay::Command { command, .. } => command_lines(self, command, width),
            ToolDisplay::Interaction { command, input, .. } => {
                interaction_lines(self, command, input, width)
            }
            ToolDisplay::Patch(patch) => {
                patch.display_lines(self.outcome.as_ref(), width, user_style)
            }
            ToolDisplay::Papercut => papercut_lines(self, width),
            ToolDisplay::Plan(plan) => plan.display_lines(self.outcome.as_ref(), width),
            ToolDisplay::ViewImage(path) => view_image_lines(self.outcome.as_ref(), path, width),
            ToolDisplay::WebSearch(action) => web_search_lines(self, action, width),
            ToolDisplay::OpenAiDocs(_) => exploration_lines(std::slice::from_ref(self), width),
            ToolDisplay::Other => generic_tool_lines(self, width),
        }
    }

    fn succeeded(&self) -> Option<bool> {
        let outcome = self.outcome.as_ref()?;
        Some(match &outcome.output {
            Err(_) => false,
            Ok(output) => output
                .get("exit_code")
                .and_then(Value::as_i64)
                .is_none_or(|code| code == 0),
        })
    }
}

impl OpenAiDocsActivity {
    fn from_call(name: &str, input: Option<&Value>) -> Option<Self> {
        let tool = name
            .strip_prefix(crate::openai_docs::NAMESPACE)?
            .strip_prefix('.')?;
        let argument = |key| {
            input
                .and_then(|input| input.get(key))
                .and_then(Value::as_str)
                .map(markdown::sanitize)
                .unwrap_or_default()
        };
        let activity = match tool {
            crate::openai_docs::SEARCH_OPENAI_DOCS => Self {
                title: "Search OpenAI docs",
                detail: argument("query"),
                active_label: "Searching OpenAI docs",
            },
            crate::openai_docs::FETCH_OPENAI_DOC => {
                let mut detail = argument("url");
                let anchor = argument("anchor");
                let anchor = anchor.trim_start_matches('#');
                if !detail.is_empty() && !anchor.is_empty() && !detail.contains('#') {
                    detail.push('#');
                    detail.push_str(anchor);
                }
                Self {
                    title: "Fetch OpenAI doc",
                    detail,
                    active_label: "Fetching OpenAI doc",
                }
            }
            crate::openai_docs::GET_OPENAPI_SPEC => Self {
                title: "Read OpenAPI spec",
                detail: argument("url"),
                active_label: "Reading OpenAPI spec",
            },
            crate::openai_docs::LIST_API_ENDPOINTS => Self {
                title: "List API endpoints",
                detail: String::new(),
                active_label: "Listing API endpoints",
            },
            crate::openai_docs::LIST_OPENAI_DOCS => Self {
                title: "List OpenAI docs",
                detail: String::new(),
                active_label: "Listing OpenAI docs",
            },
            _ => return None,
        };
        Some(activity)
    }
}

impl PatchDisplay {
    fn parse(source: &str, cwd: &Path) -> Self {
        let lines = if source.contains("\r\n") {
            Cow::Owned(source.replace("\r\n", "\n"))
        } else {
            Cow::Borrowed(source)
        };
        let lines = lines.lines().collect::<Vec<_>>();
        let mut files = Vec::new();
        let mut remaining_preview_rows = MAX_PATCH_PREVIEW_ROWS;
        let mut remaining_source_preview_bytes = MAX_PATCH_PREVIEW_SOURCE_BYTES;
        let mut index = 0;
        while index < lines.len() {
            let line = lines[index];
            index += 1;
            let (kind, path) = if let Some(path) = line.strip_prefix("*** Add File: ") {
                (PatchKind::Add, path)
            } else if let Some(path) = line.strip_prefix("*** Delete File: ") {
                (PatchKind::Delete, path)
            } else if let Some(path) = line.strip_prefix("*** Update File: ") {
                (PatchKind::Update, path)
            } else {
                continue;
            };
            let mut move_to = None;
            if let Some(destination) = lines
                .get(index)
                .and_then(|line| line.strip_prefix("*** Move to: "))
            {
                move_to = Some(destination.to_string());
                index += 1;
            }
            let mut rows = Vec::new();
            let mut added = 0_usize;
            let mut removed = 0_usize;
            let mut omitted_rows = 0_usize;
            let mut old_line = 1_usize;
            let mut new_line = 1_usize;
            let (original, source_omission_bytes) = if matches!(kind, PatchKind::Update) {
                match read_patch_preview_source(
                    &cwd.join(path),
                    &mut remaining_source_preview_bytes,
                ) {
                    PatchPreviewSource::Text(text) => (text, None),
                    PatchPreviewSource::Omitted(bytes) => (String::new(), Some(bytes)),
                    PatchPreviewSource::Unavailable => (String::new(), None),
                }
            } else {
                (String::new(), None)
            };
            let original_lines = original.lines().collect::<Vec<_>>();
            let mut search_start = 0_usize;
            let mut line_delta = 0_isize;
            let mut hunk_located = false;
            while index < lines.len() && !lines[index].starts_with("*** ") {
                let line = lines[index];
                index += 1;
                if line == "*** End of File" {
                    continue;
                }
                if line == "@@" || line.starts_with("@@ ") {
                    let located = line
                        .strip_prefix("@@ ")
                        .and_then(|anchor| {
                            find_patch_sequence(&original_lines, &[anchor], search_start)
                                .map(|position| position.saturating_add(1))
                        })
                        .or_else(|| {
                            patch_hunk_old_lines(&lines, index).and_then(|pattern| {
                                find_patch_sequence(&original_lines, &pattern, search_start)
                            })
                        });
                    if let Some(position) = located {
                        search_start = position;
                        old_line = position.saturating_add(1);
                        new_line = shifted_line_number(old_line, line_delta);
                    }
                    hunk_located = true;
                    continue;
                }
                if !hunk_located {
                    if let Some(pattern) = patch_hunk_old_lines(&lines, index.saturating_sub(1))
                        && let Some(position) =
                            find_patch_sequence(&original_lines, &pattern, search_start)
                    {
                        old_line = position.saturating_add(1);
                        new_line = shifted_line_number(old_line, line_delta);
                    }
                    hunk_located = true;
                }
                if let Some(text) = line.strip_prefix('+') {
                    added = added.saturating_add(1);
                    push_patch_preview_row(
                        &mut rows,
                        &mut remaining_preview_rows,
                        &mut omitted_rows,
                        PatchRow {
                            number: new_line,
                            kind: PatchRowKind::Add,
                            text: bounded_patch_row(text),
                        },
                    );
                    new_line += 1;
                    line_delta += 1;
                } else if let Some(text) = line.strip_prefix('-') {
                    removed = removed.saturating_add(1);
                    push_patch_preview_row(
                        &mut rows,
                        &mut remaining_preview_rows,
                        &mut omitted_rows,
                        PatchRow {
                            number: old_line,
                            kind: PatchRowKind::Delete,
                            text: bounded_patch_row(text),
                        },
                    );
                    old_line += 1;
                    line_delta -= 1;
                } else if let Some(text) = line.strip_prefix(' ') {
                    push_patch_preview_row(
                        &mut rows,
                        &mut remaining_preview_rows,
                        &mut omitted_rows,
                        PatchRow {
                            number: new_line,
                            kind: PatchRowKind::Context,
                            text: bounded_patch_row(text),
                        },
                    );
                    old_line += 1;
                    new_line += 1;
                } else if line.is_empty() {
                    push_patch_preview_row(
                        &mut rows,
                        &mut remaining_preview_rows,
                        &mut omitted_rows,
                        PatchRow {
                            number: new_line,
                            kind: PatchRowKind::Context,
                            text: String::new(),
                        },
                    );
                    old_line += 1;
                    new_line += 1;
                }
                search_start = old_line.saturating_sub(1);
            }
            let mut removed = Some(removed);
            let mut omission =
                (omitted_rows > 0).then_some(PatchPreviewOmission::Rows(omitted_rows));
            if matches!(kind, PatchKind::Delete) && rows.is_empty() && omitted_rows == 0 {
                let preview = deleted_file_preview(
                    &cwd.join(path),
                    &mut remaining_preview_rows,
                    &mut remaining_source_preview_bytes,
                );
                rows = preview.rows;
                removed = preview.removed;
                omission = preview.omission;
            }
            files.push(PatchFile {
                path: path.to_string(),
                move_to,
                kind,
                rows,
                added,
                removed,
                omission,
                source_omission_bytes,
            });
        }
        Self { files }
    }

    fn display_lines(
        &self,
        outcome: Option<&ToolOutcome>,
        width: u16,
        user_style: Style,
    ) -> Vec<Line<'static>> {
        let failure = outcome.and_then(|outcome| outcome.output.as_ref().err());
        if self.files.is_empty() {
            return match failure {
                Some(error) => failed_tool_lines("Failed to apply patch", error, width),
                None if outcome.is_none() => vec![Line::from(vec![
                    activity_marker(None),
                    " ".into(),
                    "Applying patch".bold(),
                ])],
                None => vec![Line::from(vec!["• ".dim(), "Applied patch".bold()])],
            };
        }
        let total_added = self.files.iter().map(PatchFile::added).sum::<usize>();
        let total_removed = self.files.iter().try_fold(0_usize, |total, file| {
            file.removed().map(|removed| total.saturating_add(removed))
        });
        let mut lines = Vec::new();
        let mut header = vec!["• ".dim()];
        if let [file] = self.files.as_slice() {
            header.push(match file.kind {
                PatchKind::Add => "Added".bold(),
                PatchKind::Delete => "Deleted".bold(),
                PatchKind::Update => "Edited".bold(),
            });
            header.push(" ".into());
            header.push(file.display_path().into());
            header.push(" ".into());
            header.extend(line_count_spans(file.added(), file.removed()));
        } else {
            header.push("Edited".bold());
            header.push(format!(" {} files ", self.files.len(),).into());
            header.extend(line_count_spans(total_added, total_removed));
        }
        lines.push(truncate_line(Line::from(header), usize::from(width)));
        for (file_index, file) in self.files.iter().enumerate() {
            if file_index > 0 {
                lines.push(Line::default());
            }
            if self.files.len() > 1 {
                let mut file_header = vec!["  └ ".dim(), file.display_path().into(), " ".into()];
                file_header.extend(line_count_spans(file.added(), file.removed()));
                lines.push(truncate_line(Line::from(file_header), usize::from(width)));
            }
            let number_width = file
                .rows
                .iter()
                .map(|row| row.number)
                .max()
                .unwrap_or(1)
                .to_string()
                .len();
            for row in &file.rows {
                lines.extend(patch_row_lines(row, width, number_width, user_style));
            }
            if let Some(omission) = file.omission {
                let notice = match omission {
                    PatchPreviewOmission::Rows(rows) => {
                        format!("    … {rows} diff rows omitted …")
                    }
                    PatchPreviewOmission::FileBytes(bytes) => {
                        format!("    … deleted-file preview omitted ({bytes} bytes) …")
                    }
                };
                lines.push(truncate_line(Line::from(notice).dim(), usize::from(width)));
            }
            if let Some(bytes) = file.source_omission_bytes {
                let notice = format!(
                    "    … source preview omitted ({bytes} bytes); line numbers are patch-relative …"
                );
                lines.push(truncate_line(Line::from(notice).dim(), usize::from(width)));
            }
        }
        if let Some(error) = failure {
            lines.push(Line::default());
            lines.push(Line::from("✘ Failed to apply patch").magenta().bold());
            append_bounded_output(error, width, &mut lines);
        }
        lines
    }
}

struct DeletedFilePreview {
    rows: Vec<PatchRow>,
    removed: Option<usize>,
    omission: Option<PatchPreviewOmission>,
}

enum PatchPreviewSource {
    Text(String),
    Omitted(u64),
    Unavailable,
}

fn read_patch_preview_source(path: &Path, remaining_bytes: &mut usize) -> PatchPreviewSource {
    let Ok(metadata) = path.metadata() else {
        return PatchPreviewSource::Unavailable;
    };
    let reported_bytes = metadata.len();
    let Ok(metadata_bytes) = usize::try_from(reported_bytes) else {
        return PatchPreviewSource::Omitted(reported_bytes);
    };
    if !metadata.is_file() || metadata_bytes > *remaining_bytes {
        return PatchPreviewSource::Omitted(reported_bytes);
    }
    let Ok(file) = std::fs::File::open(path) else {
        return PatchPreviewSource::Omitted(reported_bytes);
    };
    let mut bytes = Vec::with_capacity(metadata_bytes);
    let read_limit = remaining_bytes.saturating_add(1);
    if file
        .take(u64::try_from(read_limit).unwrap_or(u64::MAX))
        .read_to_end(&mut bytes)
        .is_err()
    {
        return PatchPreviewSource::Omitted(reported_bytes);
    }
    if bytes.len() > *remaining_bytes {
        return PatchPreviewSource::Omitted(
            reported_bytes.max(u64::try_from(bytes.len()).unwrap_or(u64::MAX)),
        );
    }
    let read_bytes = bytes.len();
    let Ok(text) = String::from_utf8(bytes) else {
        return PatchPreviewSource::Omitted(
            reported_bytes.max(u64::try_from(read_bytes).unwrap_or(u64::MAX)),
        );
    };
    *remaining_bytes -= read_bytes;
    PatchPreviewSource::Text(text)
}

fn deleted_file_preview(
    path: &Path,
    remaining_rows: &mut usize,
    remaining_bytes: &mut usize,
) -> DeletedFilePreview {
    let content = match read_patch_preview_source(path, remaining_bytes) {
        PatchPreviewSource::Text(content) => content,
        PatchPreviewSource::Omitted(bytes) => {
            return DeletedFilePreview {
                rows: Vec::new(),
                removed: None,
                omission: Some(PatchPreviewOmission::FileBytes(bytes)),
            };
        }
        PatchPreviewSource::Unavailable => {
            return DeletedFilePreview {
                rows: Vec::new(),
                removed: None,
                omission: None,
            };
        }
    };

    let mut rows = Vec::new();
    let mut removed = 0_usize;
    let mut omitted = 0_usize;
    for (index, text) in content.lines().enumerate() {
        removed = removed.saturating_add(1);
        push_patch_preview_row(
            &mut rows,
            remaining_rows,
            &mut omitted,
            PatchRow {
                number: index.saturating_add(1),
                kind: PatchRowKind::Delete,
                text: bounded_patch_row(text),
            },
        );
    }
    DeletedFilePreview {
        rows,
        removed: Some(removed),
        omission: (omitted > 0).then_some(PatchPreviewOmission::Rows(omitted)),
    }
}

fn push_patch_preview_row(
    rows: &mut Vec<PatchRow>,
    remaining_rows: &mut usize,
    omitted_rows: &mut usize,
    row: PatchRow,
) {
    if *remaining_rows == 0 {
        *omitted_rows = omitted_rows.saturating_add(1);
    } else {
        rows.push(row);
        *remaining_rows -= 1;
    }
}

fn bounded_patch_row(text: &str) -> String {
    if text.len() <= MAX_PATCH_PREVIEW_ROW_BYTES {
        return text.to_string();
    }
    let mut end = MAX_PATCH_PREVIEW_ROW_BYTES;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    let mut bounded = text[..end].to_string();
    bounded.push('…');
    bounded
}

fn patch_hunk_old_lines<'a>(lines: &'a [&'a str], start: usize) -> Option<Vec<&'a str>> {
    let pattern = lines[start..]
        .iter()
        .take_while(|line| {
            !line.starts_with("@@")
                && !line.starts_with("*** Add File: ")
                && !line.starts_with("*** Delete File: ")
                && !line.starts_with("*** Update File: ")
                && line.trim() != "*** End Patch"
        })
        .filter_map(|line| {
            if line.is_empty() {
                Some("")
            } else {
                line.strip_prefix(' ').or_else(|| line.strip_prefix('-'))
            }
        })
        .collect::<Vec<_>>();
    (!pattern.is_empty()).then_some(pattern)
}

fn find_patch_sequence(haystack: &[&str], needle: &[&str], start: usize) -> Option<usize> {
    if needle.is_empty() || needle.len() > haystack.len() {
        return None;
    }
    let last = haystack.len().saturating_sub(needle.len());
    let start = start.min(last);
    [
        |left: &str, right: &str| left == right,
        |left: &str, right: &str| left.trim_end() == right.trim_end(),
        |left: &str, right: &str| left.trim() == right.trim(),
    ]
    .into_iter()
    .find_map(|matches| {
        (start..=last).find(|&position| {
            haystack[position..position + needle.len()]
                .iter()
                .zip(needle)
                .all(|(actual, expected)| matches(actual, expected))
        })
    })
}

fn shifted_line_number(old_line: usize, delta: isize) -> usize {
    old_line.saturating_add_signed(delta).max(1)
}

fn patch_row_lines(
    row: &PatchRow,
    width: u16,
    number_width: usize,
    user_style: Style,
) -> Vec<Line<'static>> {
    let light_background = matches!(user_style.bg, Some(Color::Rgb(red, green, blue))
        if 0.299 * red as f32 + 0.587 * green as f32 + 0.114 * blue as f32 > 128.0);
    let (marker, line_style, gutter_style, marker_style, content_style) = match row.kind {
        PatchRowKind::Context => (
            ' ',
            Style::default(),
            Style::default().dim(),
            Style::default(),
            Style::default(),
        ),
        PatchRowKind::Add if light_background => (
            '+',
            Style::default().bg(Color::Rgb(218, 251, 225)),
            Style::default()
                .fg(Color::Rgb(31, 35, 40))
                .bg(Color::Rgb(172, 238, 187)),
            Style::default().fg(Color::Green),
            Style::default(),
        ),
        PatchRowKind::Delete if light_background => (
            '-',
            Style::default().bg(Color::Rgb(255, 235, 233)),
            Style::default()
                .fg(Color::Rgb(31, 35, 40))
                .bg(Color::Rgb(255, 206, 203)),
            Style::default().fg(Color::Red),
            Style::default(),
        ),
        PatchRowKind::Add => {
            let background = Color::Rgb(33, 58, 43);
            (
                '+',
                Style::default().bg(background),
                Style::default().dim(),
                Style::default().fg(Color::Green).bg(background),
                Style::default().fg(Color::Green).bg(background),
            )
        }
        PatchRowKind::Delete => {
            let background = Color::Rgb(74, 34, 29);
            (
                '-',
                Style::default().bg(background),
                Style::default().dim(),
                Style::default().fg(Color::Red).bg(background),
                Style::default().fg(Color::Red).bg(background),
            )
        }
    };
    let prefix_width = 4_usize.saturating_add(number_width).saturating_add(2);
    let content_width = usize::from(width)
        .saturating_sub(prefix_width)
        .max(1)
        .try_into()
        .unwrap_or(u16::MAX);
    let content = row.text.replace('\t', "    ");
    let wrapped = wrap_styled_line(
        &Line::from(Span::styled(content, content_style)),
        content_width,
    );
    wrapped
        .into_iter()
        .enumerate()
        .map(|(index, mut content)| {
            let mut spans = if index == 0 {
                vec![
                    Span::styled(format!("    {:>number_width$} ", row.number), gutter_style),
                    Span::styled(marker.to_string(), marker_style),
                ]
            } else {
                vec![Span::styled(
                    format!("    {:number_width$}  ", ""),
                    gutter_style,
                )]
            };
            spans.append(&mut content.spans);
            Line::from(spans).style(line_style)
        })
        .collect()
}

impl PatchFile {
    fn added(&self) -> usize {
        self.added
    }

    fn removed(&self) -> Option<usize> {
        self.removed
    }

    fn display_path(&self) -> String {
        self.move_to.as_ref().map_or_else(
            || self.path.clone(),
            |move_to| format!("{} → {move_to}", self.path),
        )
    }
}

impl PlanDisplay {
    fn parse(input: Option<&Value>) -> Self {
        let explanation = input
            .and_then(|input| input.get("explanation"))
            .and_then(Value::as_str)
            .map(str::to_string);
        let steps = input
            .and_then(|input| input.get("plan"))
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .map(|step| PlanStep {
                text: step
                    .get("step")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                status: step
                    .get("status")
                    .and_then(Value::as_str)
                    .unwrap_or("pending")
                    .to_string(),
            })
            .collect();
        Self { explanation, steps }
    }

    fn display_lines(&self, outcome: Option<&ToolOutcome>, width: u16) -> Vec<Line<'static>> {
        match outcome.map(|outcome| &outcome.output) {
            None => {
                return vec![Line::from(vec![
                    activity_marker(None),
                    " ".into(),
                    "Updating Plan".bold(),
                ])];
            }
            Some(Err(error)) => return failed_tool_lines("Failed to update plan", error, width),
            Some(Ok(_)) => {}
        }
        let mut lines = vec![Line::from(vec!["• ".dim(), "Updated Plan".bold()])];
        let mut details = Vec::new();
        if let Some(explanation) = self
            .explanation
            .as_deref()
            .map(str::trim)
            .filter(|explanation| !explanation.is_empty())
        {
            for line in editor::wrap_text(explanation, width.saturating_sub(4).max(1)) {
                details.push(Line::from(line).dim().italic());
            }
        }
        if self.steps.is_empty() {
            details.push(Line::from("(no steps provided)").dim().italic());
        } else {
            for step in &self.steps {
                let (marker, style) = match step.status.as_str() {
                    "completed" => (
                        "✔ ",
                        Style::default().add_modifier(Modifier::CROSSED_OUT | Modifier::DIM),
                    ),
                    "in_progress" => ("□ ", Style::default().cyan().bold()),
                    _ => ("□ ", Style::default().dim()),
                };
                let wrapped = editor::wrap_text(&step.text, width.saturating_sub(6).max(1));
                for (index, line) in wrapped.into_iter().enumerate() {
                    details.push(Line::from(vec![
                        Span::from(if index == 0 { marker } else { "  " }),
                        Span::styled(line, style),
                    ]));
                }
            }
        }
        prefix_lines(&mut lines, details, "  └ ", "    ");
        lines
    }
}

impl Repository {
    fn discover(cwd: &Path) -> Self {
        let root = command_output(cwd, &["rev-parse", "--show-toplevel"])
            .map(PathBuf::from)
            .unwrap_or_else(|| cwd.to_path_buf());
        let name = markdown::sanitize(
            root.file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("workspace"),
        );
        let branch = command_output(cwd, &["symbolic-ref", "--quiet", "--short", "HEAD"])
            .map(|branch| markdown::sanitize(&branch));
        Self { name, branch }
    }
}

fn command_output(cwd: &Path, arguments: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(arguments)
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn welcome_lines(
    cwd: &Path,
    available_width: u16,
    available_height: u16,
    model_selection: &ModelSelection,
    service_tier: ServiceTier,
) -> Vec<Line<'static>> {
    let card = welcome_card_lines(cwd, available_width, model_selection, service_tier);
    let mut artwork = startup_art::lines(available_width, available_height);
    if artwork.is_empty() || card.is_empty() {
        return card;
    }
    artwork.push(Line::default());
    artwork.extend(card);
    artwork
}

fn welcome_card_lines(
    cwd: &Path,
    available_width: u16,
    model_selection: &ModelSelection,
    service_tier: ServiceTier,
) -> Vec<Line<'static>> {
    if available_width < 4 {
        return Vec::new();
    }
    let maximum_content_width = usize::from(available_width.saturating_sub(4)).min(56);
    let mut content = vec![Line::from(vec![
        Span::from(">_ ").dim(),
        Span::from("bettercodex").bold(),
        Span::from(format!(" (v{})", env!("CARGO_PKG_VERSION"))).dim(),
    ])];
    content.push(Line::default());
    let mut model_spans = vec![
        Span::from("model:       ").dim(),
        Span::from(model_selection.model.clone()),
        Span::from(format!(" {}", model_selection.reasoning_effort)).dim(),
    ];
    if service_tier.is_fast() {
        model_spans.push(Span::from(" fast").magenta());
    }
    content.push(Line::from(model_spans));
    content.push(Line::from(vec![
        Span::from("directory:   ").dim(),
        Span::from(display_directory(cwd)),
    ]));
    content.push(Line::from(vec![
        Span::from("permissions: ").dim(),
        Span::from("current user"),
    ]));
    let content = content
        .into_iter()
        .map(|line| truncate_line(line, maximum_content_width))
        .collect::<Vec<_>>();
    with_card_border(content)
}

fn patch_notes_lines(
    content: &mut MarkdownRenderCache,
    available_width: u16,
    cwd: &Path,
) -> Vec<HyperlinkLine> {
    let available_width = available_width.max(1);
    let inset = usize::from(available_width > 2);
    let content_width = usize::from(available_width)
        .saturating_sub(inset.saturating_mul(2))
        .max(1);
    let border = || {
        HyperlinkLine::new(Line::from(Span::styled(
            "─".repeat(usize::from(available_width)),
            Style::default().fg(RULE),
        )))
    };
    let with_inset = |lines| {
        terminal_hyperlinks::prefix_hyperlink_lines(
            lines,
            Span::from(" ".repeat(inset)),
            Span::from(" ".repeat(inset)),
        )
    };

    let heading = Line::from(Span::styled(
        "What's New",
        Style::default().fg(Color::Cyan).bold(),
    ));
    let heading = word_wrap_line(&heading, content_width)
        .iter()
        .map(line_to_static)
        .collect::<Vec<_>>();
    let markdown = content.render_finalized(content_width, cwd).to_vec();

    let mut lines = Vec::with_capacity(heading.len().saturating_add(markdown.len() + 4));
    lines.push(border());
    lines.extend(with_inset(terminal_hyperlinks::plain_hyperlink_lines(
        heading,
    )));
    lines.push(HyperlinkLine::default());
    lines.extend(with_inset(markdown));
    lines.push(HyperlinkLine::default());
    lines.push(border());
    lines
}

fn update_available_lines(update: &AvailableUpdate, available_width: u16) -> Vec<Line<'static>> {
    if available_width < 5 {
        return Vec::new();
    }
    let content_width = usize::from(available_width.saturating_sub(4)).min(60);
    let warning = Style::default().fg(Color::Yellow);
    let source = [
        Line::from(Span::styled(
            "Update available",
            warning.add_modifier(Modifier::BOLD),
        )),
        Line::from(vec![
            Span::from("Run ").dim(),
            Span::styled("bcodex update", Style::default().fg(Color::Cyan)),
            Span::from(" in another terminal.").dim(),
        ]),
        Line::from(vec![
            Span::from("main: ").dim(),
            Span::from(update.current_short_revision().to_string()).dim(),
            Span::from(" → ").dim(),
            Span::from(update.latest_short_revision().to_string()).dim(),
        ]),
    ];
    let mut content = Vec::new();
    for line in &source {
        content.extend(
            word_wrap_line(line, content_width.max(1))
                .iter()
                .map(line_to_static),
        );
    }
    with_card_border_style(content, warning)
}

fn with_card_border(lines: Vec<Line<'static>>) -> Vec<Line<'static>> {
    with_card_border_style(lines, Style::default().fg(RULE))
}

fn with_card_border_style(lines: Vec<Line<'static>>, border_style: Style) -> Vec<Line<'static>> {
    let content_width = lines.iter().map(line_width).max().unwrap_or_default();
    let mut output = Vec::with_capacity(lines.len().saturating_add(2));
    output.push(Line::from(Span::styled(
        format!("╭{}╮", "─".repeat(content_width.saturating_add(2))),
        border_style,
    )));
    for mut line in lines {
        let used = line_width(&line);
        let mut spans = vec![Span::styled("│ ", border_style)];
        spans.append(&mut line.spans);
        spans.push(Span::styled(
            " ".repeat(content_width.saturating_sub(used)),
            border_style,
        ));
        spans.push(Span::styled(" │", border_style));
        output.push(Line::from(spans));
    }
    output.push(Line::from(Span::styled(
        format!("╰{}╯", "─".repeat(content_width.saturating_add(2))),
        border_style,
    )));
    output
}

fn display_directory(cwd: &Path) -> String {
    let Some(home) = crate::paths::home_dir() else {
        return markdown::sanitize(&cwd.display().to_string());
    };
    cwd.strip_prefix(&home).map_or_else(
        |_| markdown::sanitize(&cwd.display().to_string()),
        |relative| {
            if relative.as_os_str().is_empty() {
                "~".to_string()
            } else {
                markdown::sanitize(&format!("~/{}", relative.display()))
            }
        },
    )
}

fn display_tool_path(path: &Path, cwd: &Path) -> String {
    if path.is_relative() {
        return markdown::sanitize(&path.display().to_string());
    }
    if let Ok(relative) = path.strip_prefix(cwd) {
        return markdown::sanitize(&relative.display().to_string());
    }
    let Some(home) = crate::paths::home_dir() else {
        return markdown::sanitize(&path.display().to_string());
    };
    path.strip_prefix(home).map_or_else(
        |_| markdown::sanitize(&path.display().to_string()),
        |relative| markdown::sanitize(&format!("~/{}", relative.display())),
    )
}

fn user_message_lines(
    message: &DisplayedUserPrompt,
    width: u16,
    style: Style,
) -> Vec<Line<'static>> {
    let (message_text, skill_ranges) = sanitized_prompt(message);
    let message_text = message_text.trim_end_matches(['\r', '\n']);
    let wrap_width = width.saturating_sub(LIVE_PREFIX_COLS + 1).max(1);
    let wrapped = editor::visual_ranges(message_text, usize::from(wrap_width))
        .into_iter()
        .map(|range| {
            // Match the composer: tabs occupy one visible cell. Replacing them here preserves
            // byte offsets, so exact mention ranges remain valid after wrapping.
            let content = message_text[range.clone()].replace('\t', " ");
            styled_skill_mentions(&content, range.start, &skill_ranges, style)
        })
        .collect::<Vec<_>>();
    let mut lines = vec![Line::default().style(style)];
    for (index, mut content) in wrapped.into_iter().enumerate() {
        let mut spans = vec![Span::styled(
            if index == 0 { "› " } else { "  " },
            style.bold().dim(),
        )];
        spans.append(&mut content);
        lines.push(Line::from(spans).style(style));
    }
    lines.push(Line::default().style(style));
    lines
}

fn sanitized_prompt(message: &DisplayedUserPrompt) -> (String, Vec<std::ops::Range<usize>>) {
    let mut text = String::with_capacity(message.as_str().len());
    let mut source_ranges = message
        .skill_mentions()
        .iter()
        .filter_map(|mention| {
            let range = mention.range();
            (range.end <= message.as_str().len()
                && message.as_str().get(range.clone())
                    == Some(format!("${}", mention.selection().name()).as_str()))
            .then_some(range.clone())
        })
        .chain(message.image_ranges().iter().filter_map(|range| {
            message
                .as_str()
                .get(range.clone())
                .is_some_and(|label| label.starts_with("[Image ") && label.ends_with(']'))
                .then_some(range.clone())
        }))
        .collect::<Vec<_>>();
    source_ranges.sort_by_key(|range| range.start);
    let mut ranges = Vec::with_capacity(source_ranges.len());
    let mut cursor = 0_usize;
    for range in source_ranges {
        if range.start < cursor {
            continue;
        }
        text.push_str(&markdown::sanitize(&message.as_str()[cursor..range.start]));
        let start = text.len();
        text.push_str(&markdown::sanitize(&message.as_str()[range.clone()]));
        ranges.push(start..text.len());
        cursor = range.end;
    }
    text.push_str(&markdown::sanitize(&message.as_str()[cursor..]));
    (text, ranges)
}

fn styled_skill_mentions(
    text: &str,
    offset: usize,
    skill_ranges: &[std::ops::Range<usize>],
    style: Style,
) -> Vec<Span<'static>> {
    let end = offset.saturating_add(text.len());
    let ranges = skill_ranges.iter().filter_map(|range| {
        let start = range.start.max(offset);
        let end = range.end.min(end);
        (start < end).then_some(start - offset..end - offset)
    });
    let mut spans = Vec::with_capacity(skill_ranges.len().saturating_mul(2).saturating_add(1));
    let mut cursor = 0_usize;
    for range in ranges {
        if cursor < range.start {
            spans.push(Span::styled(text[cursor..range.start].to_string(), style));
        }
        spans.push(Span::styled(
            text[range.clone()].to_string(),
            style.fg(Color::Cyan),
        ));
        cursor = range.end;
    }
    if cursor < text.len() || spans.is_empty() {
        spans.push(Span::styled(text[cursor..].to_string(), style));
    }
    spans
}

fn assistant_lines(
    content: &mut MarkdownRenderCache,
    width: u16,
    cwd: &Path,
    streaming: bool,
) -> Vec<HyperlinkLine> {
    assistant_lines_after(content, width, cwd, streaming, 0).1
}

/// Clone and decorate only the rendered suffix that is still live in the viewport.
///
/// The cache retains the complete source-backed rendering for resize and finalization. During a
/// long stream, however, the prefix before `start` is already in terminal scrollback and must not
/// be cloned on every repaint.
fn assistant_lines_after(
    content: &mut MarkdownRenderCache,
    width: u16,
    cwd: &Path,
    streaming: bool,
    start: usize,
) -> (usize, Vec<HyperlinkLine>) {
    let content_width = usize::from(width.saturating_sub(2).max(1));
    let rendered = if streaming {
        content.render_streaming(content_width, cwd)
    } else {
        content.render_finalized(content_width, cwd)
    };
    let rendered_len = rendered.len();
    let start = start.min(rendered_len);
    let initial_prefix = if start == 0 {
        Span::from("• ").dim()
    } else {
        Span::from("  ")
    };
    let lines = terminal_hyperlinks::prefix_hyperlink_lines(
        rendered[start..].to_vec(),
        initial_prefix,
        Span::from("  "),
    );
    (rendered_len, lines)
}

fn background_process_lines(processes: &[BackgroundProcess], width: u16) -> Vec<Line<'static>> {
    if processes.is_empty() {
        return vec![Line::from(vec![
            "• ".dim(),
            "No background terminals running".bold(),
        ])];
    }
    let count = processes.len();
    let plural = if count == 1 { "" } else { "s" };
    let mut lines = vec![Line::from(vec![
        "• ".dim(),
        format!("{count} background terminal{plural} running").bold(),
    ])];
    for process in processes {
        let prefix = format!(
            "  {} · {} · ",
            process.session_id,
            format_elapsed(process.running_for.as_secs())
        );
        let prefix_width = UnicodeWidthStr::width(prefix.as_str());
        let command_width = width
            .saturating_sub(u16::try_from(prefix_width).unwrap_or(u16::MAX))
            .max(1);
        let command = markdown::sanitize(&process.command);
        let mut command_lines = editor::wrap_text(&command, command_width);
        if command_lines.is_empty() {
            command_lines.push(String::new());
        }
        for (index, command) in command_lines.into_iter().enumerate() {
            lines.push(if index == 0 {
                Line::from(vec![prefix.clone().dim(), command.into()])
            } else {
                Line::from(vec![" ".repeat(prefix_width).into(), command.into()])
            });
        }
    }
    lines
}

fn git_diff_lines(diff: &str, width: u16) -> Vec<Line<'static>> {
    let mut lines = vec![Line::from(vec!["• ".dim(), "Git diff".bold()])];
    for source in diff.lines() {
        let line = ansi_escape_line(source);
        for mut wrapped in wrap_styled_line(&line, width.saturating_sub(2).max(1)) {
            let mut spans = vec!["  ".into()];
            spans.append(&mut wrapped.spans);
            lines.push(Line::from(spans));
        }
    }
    if lines.len() == 1 {
        lines.push(Line::from("  (no changes)").dim());
    }
    lines
}

fn final_message_separator_lines(elapsed_seconds: Option<u64>, width: u16) -> Vec<Line<'static>> {
    let Some(elapsed) = elapsed_seconds
        .filter(|seconds| *seconds > 60)
        .map(format_elapsed)
    else {
        return vec![Line::from("─".repeat(usize::from(width))).dim()];
    };

    let label = format!("─ Worked for {elapsed} ─")
        .chars()
        .take(usize::from(width))
        .collect::<String>();
    let label_width = UnicodeWidthStr::width(label.as_str());
    vec![
        Line::from_iter([
            label,
            "─".repeat(usize::from(width).saturating_sub(label_width)),
        ])
        .dim(),
    ]
}

fn code_mode_lines(tool: &ToolEntry, source: &str, width: u16) -> Vec<Line<'static>> {
    let (bullet, title) = match tool.succeeded() {
        None => (activity_marker(Some(tool.started_at)), "Running code"),
        Some(true) => ("•".green().bold(), "Ran code"),
        Some(false) => ("•".red().bold(), "Ran code"),
    };
    let calls = code_mode_tool_names(source);
    let mut header = vec![bullet, " ".into(), title.bold()];
    if !calls.is_empty() {
        header.push(" · ".dim());
        header.push(calls.join(", ").cyan());
    }
    let mut lines = wrap_styled_line(&Line::from(header), width.max(1));
    append_bounded_output(source, width, &mut lines);
    if let Some(outcome) = &tool.outcome {
        match &outcome.output {
            Ok(output) => {
                let output = transcript_output_text(output);
                if !output.is_empty() {
                    append_bounded_output(&output, width, &mut lines);
                }
            }
            Err(error) => append_bounded_output(error, width, &mut lines),
        }
    }
    lines
}

fn code_mode_tool_names(source: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut remaining = source;
    while let Some(index) = remaining.find("tools.") {
        remaining = &remaining[index + "tools.".len()..];
        let length = remaining
            .bytes()
            .take_while(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
            .count();
        if length == 0 {
            continue;
        }
        let name = remaining[..length].replace("__", ".");
        if !names.contains(&name) {
            names.push(name);
        }
        remaining = &remaining[length..];
    }
    names
}

fn transcript_output_text(output: &Value) -> String {
    match output {
        Value::String(text) => text.clone(),
        Value::Array(items) => items
            .iter()
            .filter_map(|item| item.get("text").and_then(Value::as_str))
            .collect(),
        Value::Object(object) => object
            .get("output")
            .map(transcript_output_text)
            .unwrap_or_else(|| output.to_string()),
        Value::Null => String::new(),
        Value::Bool(_) | Value::Number(_) => output.to_string(),
    }
}

fn command_lines(tool: &ToolEntry, command: &str, width: u16) -> Vec<Line<'static>> {
    let (bullet, title) = match tool.succeeded() {
        None => (activity_marker(Some(tool.started_at)), "Running"),
        Some(true) => ("•".green().bold(), "Ran"),
        Some(false) => ("•".red().bold(), "Ran"),
    };
    let prefix_width = line_width(&Line::from(vec![
        bullet.clone(),
        " ".into(),
        title.bold(),
        " ".into(),
    ]));
    let mut command_rows = editor::wrap_text(
        markdown::sanitize(command).trim_end_matches(['\r', '\n']),
        width.saturating_sub(prefix_width as u16).max(1),
    );
    if command_rows.is_empty() {
        command_rows.push(String::new());
    }
    let first = command_rows.remove(0);
    let mut lines = vec![Line::from(vec![
        bullet,
        " ".into(),
        title.bold(),
        " ".into(),
        Span::from(first).cyan(),
    ])];
    let omitted = command_rows
        .len()
        .saturating_sub(COMMAND_CONTINUATION_MAX_ROWS);
    for row in command_rows.into_iter().take(COMMAND_CONTINUATION_MAX_ROWS) {
        lines.push(Line::from(vec!["  │ ".dim(), Span::from(row).cyan()]));
    }
    if omitted > 0 {
        lines.push(Line::from(vec![
            "  │ ".dim(),
            format!("… +{omitted} lines").dim(),
        ]));
    }
    if let Some(outcome) = &tool.outcome {
        match &outcome.output {
            Ok(output) => {
                let text = output
                    .get("output")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if text.is_empty() {
                    lines.push(Line::from(vec!["  └ ".dim(), "(no output)".dim()]));
                } else {
                    append_bounded_output(text, width, &mut lines);
                }
            }
            Err(error) => append_bounded_output(error, width, &mut lines),
        }
    }
    lines
}

fn interaction_lines(
    _tool: &ToolEntry,
    command: &str,
    input: &str,
    width: u16,
) -> Vec<Line<'static>> {
    let waited_only = input.is_empty();
    let command = first_display_line(&markdown::sanitize(command));
    let mut header_spans = if waited_only {
        vec!["• Waited for background terminal".bold()]
    } else {
        vec!["↳ ".dim(), "Interacted with background terminal".bold()]
    };
    if !command.is_empty() {
        header_spans.push(" · ".dim());
        header_spans.push(command.dim());
    }
    let mut lines = wrap_styled_line(&Line::from(header_spans), width.max(1));
    if waited_only {
        return lines;
    }

    let input = markdown::sanitize(input);
    let content_width = width.saturating_sub(4).max(1);
    let mut first_row = true;
    for source in input.lines() {
        let mut rows = editor::wrap_text(source, content_width);
        if rows.is_empty() {
            rows.push(String::new());
        }
        for row in rows {
            lines.push(Line::from(vec![
                if std::mem::replace(&mut first_row, false) {
                    "  └ ".dim()
                } else {
                    "    ".into()
                },
                Span::from(row),
            ]));
        }
    }
    lines
}

fn exploration_lines(tools: &[ToolEntry], width: u16) -> Vec<Line<'static>> {
    let active = tools.iter().any(|tool| tool.outcome.is_none());
    let failed = tools.iter().any(|tool| tool.succeeded() == Some(false));
    let started = tools
        .iter()
        .find(|tool| tool.outcome.is_none())
        .map(|tool| tool.started_at);
    let mut lines = vec![Line::from(vec![
        if active {
            activity_marker(started)
        } else if failed {
            "•".red().bold()
        } else {
            "•".dim()
        },
        " ".into(),
        if active {
            "Exploring".bold()
        } else {
            "Explored".bold()
        },
    ])];
    let detail_style = Style::default().fg(Color::Cyan);
    let mut details: Vec<(&str, Style, Vec<Span<'static>>)> = Vec::new();
    let mut read_names = Vec::<String>::new();
    let flush_reads = |details: &mut Vec<(&str, Style, Vec<Span<'static>>)>,
                       names: &mut Vec<String>| {
        if names.is_empty() {
            return;
        }
        let mut spans = Vec::new();
        for (index, name) in std::mem::take(names).into_iter().enumerate() {
            if index > 0 {
                spans.push(", ".dim());
            }
            spans.push(name.into());
        }
        details.push(("Read", detail_style, spans));
    };
    for tool in tools {
        match &tool.display {
            ToolDisplay::Command { parsed, .. } => {
                for parsed in parsed {
                    match parsed {
                        ParsedCommand::Read { name, .. } => {
                            if !read_names.contains(name) {
                                read_names.push(name.clone());
                            }
                        }
                        ParsedCommand::ListFiles { cmd, path } => {
                            flush_reads(&mut details, &mut read_names);
                            details.push((
                                "List",
                                detail_style,
                                vec![path.clone().unwrap_or_else(|| cmd.clone()).into()],
                            ));
                        }
                        ParsedCommand::Search { cmd, query, path } => {
                            flush_reads(&mut details, &mut read_names);
                            let spans = match (query, path) {
                                (Some(query), Some(path)) => {
                                    vec![query.clone().into(), " in ".dim(), path.clone().into()]
                                }
                                (Some(query), None) => vec![query.clone().into()],
                                _ => vec![cmd.clone().into()],
                            };
                            details.push(("Search", detail_style, spans));
                        }
                        ParsedCommand::Unknown { .. } => {}
                    }
                }
            }
            ToolDisplay::OpenAiDocs(activity) => {
                flush_reads(&mut details, &mut read_names);
                let spans = if activity.detail.is_empty() {
                    Vec::new()
                } else {
                    vec![Span::from(activity.detail.clone())]
                };
                details.push((activity.title, detail_style, spans));
            }
            _ => {}
        }
        if matches!(&tool.display, ToolDisplay::OpenAiDocs(_))
            && let Some(ToolOutcome { output: Err(error) }) = &tool.outcome
        {
            details.push((
                "Error",
                Style::default().fg(Color::Red),
                vec![Span::from(first_display_line(error)).red()],
            ));
        }
    }
    flush_reads(&mut details, &mut read_names);
    let detail_width = width.saturating_sub(4).max(1);
    let mut wrapped_details = Vec::new();
    for (title, title_style, spans) in details {
        let title = if spans.is_empty() {
            title.to_string()
        } else {
            format!("{title} ")
        };
        let title_width = UnicodeWidthStr::width(title.as_str()) as u16;
        let wrapped = wrap_styled_line(
            &Line::from(spans),
            detail_width.saturating_sub(title_width).max(1),
        );
        for (index, mut row) in wrapped.into_iter().enumerate() {
            let mut spans = vec![if index == 0 {
                Span::styled(title.clone(), title_style)
            } else {
                Span::from(" ".repeat(usize::from(title_width)))
            }];
            spans.append(&mut row.spans);
            wrapped_details.push(Line::from(spans));
        }
    }
    for (index, mut detail) in wrapped_details.into_iter().enumerate() {
        let mut spans = vec![if index == 0 {
            "  └ ".dim()
        } else {
            "    ".into()
        }];
        spans.append(&mut detail.spans);
        lines.push(Line::from(spans));
    }
    lines
}

fn view_image_lines(outcome: Option<&ToolOutcome>, path: &str, width: u16) -> Vec<Line<'static>> {
    match outcome.map(|outcome| &outcome.output) {
        None => vec![Line::from(vec![
            activity_marker(None),
            " ".into(),
            "Viewing Image".bold(),
        ])],
        Some(Ok(_)) => vec![
            Line::from(vec!["• ".dim(), "Viewed Image".bold()]),
            truncate_line(
                Line::from(vec!["  └ ".dim(), Span::from(path.to_string()).dim()]),
                usize::from(width),
            ),
        ],
        Some(Err(error)) => failed_tool_lines("Failed to view image", error, width),
    }
}

fn web_search_lines(
    tool: &ToolEntry,
    action: &crate::web_search::WebSearchAction,
    width: u16,
) -> Vec<Line<'static>> {
    if let Some(ToolOutcome { output: Err(error) }) = &tool.outcome {
        return failed_tool_lines("Failed to search the web", error, width);
    }

    let completed = tool.outcome.is_some();
    let detail = if completed {
        web_search_action_detail(action)
    } else {
        String::new()
    };
    let mut content = vec![
        Span::from(if completed {
            "Searched the web"
        } else {
            "Searching the web"
        })
        .bold(),
    ];
    if !detail.is_empty() {
        content.push(" for ".into());
        content.push(Span::from(detail));
    }
    let wrapped = wrap_styled_line(&Line::from(content), width.saturating_sub(2).max(1));
    let bullet = if completed {
        "•".dim()
    } else {
        activity_marker(Some(tool.started_at))
    };
    wrapped
        .into_iter()
        .enumerate()
        .map(|(index, mut line)| {
            let mut spans = if index == 0 {
                vec![bullet.clone(), " ".into()]
            } else {
                vec!["  ".into()]
            };
            spans.append(&mut line.spans);
            Line::from(spans)
        })
        .collect()
}

fn web_search_action_detail(action: &crate::web_search::WebSearchAction) -> String {
    use crate::web_search::WebSearchAction;

    match action {
        WebSearchAction::Search { query, queries } => {
            query.clone().filter(|q| !q.is_empty()).unwrap_or_else(|| {
                let items = queries.as_ref();
                let first = items
                    .and_then(|queries| queries.first())
                    .cloned()
                    .unwrap_or_default();
                if items.is_some_and(|queries| queries.len() > 1) && !first.is_empty() {
                    format!("{first} ...")
                } else {
                    first
                }
            })
        }
        WebSearchAction::OpenPage { url } => url.clone().unwrap_or_default(),
        WebSearchAction::FindInPage { url, pattern } => match (pattern, url) {
            (Some(pattern), Some(url)) => format!("'{pattern}' in {url}"),
            (Some(pattern), None) => format!("'{pattern}'"),
            (None, Some(url)) => url.clone(),
            (None, None) => String::new(),
        },
        WebSearchAction::Other => String::new(),
    }
}

fn generic_tool_lines(tool: &ToolEntry, width: u16) -> Vec<Line<'static>> {
    match tool.outcome.as_ref().map(|outcome| &outcome.output) {
        None => vec![Line::from(vec![
            activity_marker(Some(tool.started_at)),
            " ".into(),
            "Running".bold(),
            " ".into(),
            Span::from(tool.name.clone()).cyan(),
        ])],
        Some(Ok(output)) => {
            let mut lines = vec![Line::from(vec![
                "• ".dim(),
                "Called".bold(),
                " ".into(),
                Span::from(tool.name.clone()).cyan(),
            ])];
            append_bounded_output(&output.to_string(), width, &mut lines);
            lines
        }
        Some(Err(error)) => failed_tool_lines(&format!("Failed {}", tool.name), error, width),
    }
}

fn papercut_lines(tool: &ToolEntry, width: u16) -> Vec<Line<'static>> {
    let line = match tool.outcome.as_ref().map(|outcome| &outcome.output) {
        None => Line::from(vec![
            activity_marker(Some(tool.started_at)),
            " ".into(),
            "Logging papercut".bold(),
        ]),
        Some(Ok(output)) => {
            let path = output
                .get("path")
                .and_then(Value::as_str)
                .map(markdown::sanitize)
                .unwrap_or_else(|| "PAPERCUTS.md".to_string());
            Line::from(vec![
                "• ".dim(),
                "Logged papercut".bold(),
                " to ".into(),
                path.cyan(),
            ])
        }
        Some(Err(error)) => return failed_tool_lines("Failed to log papercut", error, width),
    };
    wrap_styled_line(&line, width.max(1))
}

fn failed_tool_lines(title: &str, error: &str, width: u16) -> Vec<Line<'static>> {
    let mut lines = vec![Line::from(vec![
        "✗ ".red().bold(),
        Span::from(title.to_string()).bold(),
    ])];
    append_bounded_output(error, width, &mut lines);
    lines
}

fn append_bounded_output(output: &str, width: u16, lines: &mut Vec<Line<'static>>) {
    let mut rendered = Vec::new();
    for raw in output.lines() {
        let ansi = ansi_escape_line(raw);
        rendered.extend(wrap_styled_line(&ansi, width.saturating_sub(4).max(1)));
    }
    if rendered.is_empty() {
        rendered.push(Line::from("(no output)").dim());
    }
    let rendered = middle_truncate_lines(rendered, TOOL_OUTPUT_MAX_ROWS);
    for (index, mut line) in rendered.into_iter().enumerate() {
        for span in &mut line.spans {
            span.style = span.style.add_modifier(Modifier::DIM);
        }
        let mut spans = vec![if index == 0 {
            "  └ ".dim()
        } else {
            "    ".into()
        }];
        spans.extend(line.spans);
        lines.push(Line::from(spans));
    }
}

fn middle_truncate_lines(lines: Vec<Line<'static>>, maximum: usize) -> Vec<Line<'static>> {
    if lines.len() <= maximum {
        return lines;
    }
    let head = maximum.saturating_sub(1) / 2;
    let tail = maximum.saturating_sub(head + 1);
    let omitted = lines.len().saturating_sub(head + tail);
    let mut kept = lines[..head].to_vec();
    kept.push(Line::from(format!(
        "… +{omitted} lines (ctrl + t to view transcript)"
    )));
    kept.extend(lines[lines.len() - tail..].iter().cloned());
    kept
}

fn wrap_styled_line(line: &Line<'static>, width: u16) -> Vec<Line<'static>> {
    let width = usize::from(width.max(1));
    let mut rows = Vec::new();
    let mut spans = Vec::new();
    let mut used = 0;
    for span in &line.spans {
        let mut content = String::new();
        for grapheme in span.content.graphemes(true) {
            let grapheme_width = UnicodeWidthStr::width(grapheme).max(1);
            if used + grapheme_width > width && used > 0 {
                if !content.is_empty() {
                    spans.push(Span::styled(std::mem::take(&mut content), span.style));
                }
                rows.push(Line::from(std::mem::take(&mut spans)).style(line.style));
                used = 0;
            }
            content.push_str(grapheme);
            used += grapheme_width;
        }
        if !content.is_empty() {
            spans.push(Span::styled(content, span.style));
        }
    }
    if !spans.is_empty() || rows.is_empty() {
        rows.push(Line::from(spans).style(line.style));
    }
    rows
}

fn prefix_lines(
    output: &mut Vec<Line<'static>>,
    lines: Vec<Line<'static>>,
    initial: &'static str,
    subsequent: &'static str,
) {
    for (index, mut line) in lines.into_iter().enumerate() {
        let mut spans = vec![if index == 0 {
            Span::from(initial).dim()
        } else {
            Span::from(subsequent)
        }];
        spans.append(&mut line.spans);
        output.push(Line::from(spans));
    }
}

fn append_history_cell(
    mut cell: Vec<HyperlinkLine>,
    output: &mut Vec<HyperlinkLine>,
    emitted: &mut bool,
) {
    if cell.is_empty() {
        return;
    }
    if *emitted {
        output.push(HyperlinkLine::default());
    } else {
        *emitted = true;
    }
    output.append(&mut cell);
}

fn line_count_spans(added: usize, removed: Option<usize>) -> Vec<Span<'static>> {
    vec![
        "(".into(),
        format!("+{added}").green(),
        " ".into(),
        format!(
            "-{}",
            removed.map_or_else(|| "?".to_string(), |count| count.to_string())
        )
        .red(),
        ")".into(),
    ]
}

fn rendered_line_count(lines: &[HyperlinkLine], width: u16) -> u16 {
    lines
        .iter()
        .map(|line| super::terminal::transcript_line_height(line, width))
        .fold(0_usize, usize::saturating_add)
        .try_into()
        .unwrap_or(u16::MAX)
}

fn user_message_style_for(background: Option<(u8, u8, u8)>) -> Style {
    // Codex normally gets this value from the bounded OSC 11 startup probe. Multiplexers can
    // swallow the response even when the underlying terminal is a dark true-color terminal; use
    // bettercodex's accepted dark canvas in that case so the composer does not silently lose the
    // Codex backfill that distinguishes user-authored text.
    let background = background.unwrap_or((31, 31, 31));
    let light =
        0.299 * background.0 as f32 + 0.587 * background.1 as f32 + 0.114 * background.2 as f32
            > 128.0;
    let (overlay, alpha) = if light {
        ((0, 0, 0), 0.04)
    } else {
        ((255, 255, 255), 0.12)
    };
    let (red, green, blue) = blend(overlay, background, alpha);
    Style::default().bg(Color::Rgb(red, green, blue))
}

fn activity_marker(started_at: Option<Instant>) -> Span<'static> {
    let elapsed = started_at
        .map(|started| started.elapsed())
        .unwrap_or_default();
    if supports_true_color() {
        shimmer_spans("•")
            .into_iter()
            .next()
            .unwrap_or_else(|| "•".into())
    } else if (elapsed.as_millis() / 600).is_multiple_of(2) {
        "•".into()
    } else {
        "◦".dim()
    }
}

fn shimmer_spans(text: &str) -> Vec<Span<'static>> {
    let started = PROCESS_START.get_or_init(Instant::now);
    shimmer_spans_at(
        text,
        started.elapsed(),
        palette::terminal_colors(),
        supports_true_color(),
    )
}

fn shimmer_spans_at(
    text: &str,
    elapsed: Duration,
    terminal_colors: Option<TerminalColors>,
    true_color: bool,
) -> Vec<Span<'static>> {
    let characters = text.chars().collect::<Vec<_>>();
    if characters.is_empty() {
        return Vec::new();
    }

    let padding = 10_usize;
    let period = characters.len() + padding * 2;
    let position = ((elapsed.as_secs_f32() % 2.0) / 2.0 * period as f32) as isize;
    let band_half_width = 5.0_f32;
    let base_color = terminal_colors
        .map(|colors| colors.foreground)
        .unwrap_or((128, 128, 128));
    let highlight_color = terminal_colors
        .map(|colors| colors.background)
        .unwrap_or((255, 255, 255));

    characters
        .into_iter()
        .enumerate()
        .map(|(index, character)| {
            let distance = ((index + padding) as isize - position).abs() as f32;
            let intensity = if distance <= band_half_width {
                let phase = std::f32::consts::PI * (distance / band_half_width);
                0.5 * (1.0 + phase.cos())
            } else {
                0.0
            };
            let style = if true_color {
                let color = blend(highlight_color, base_color, intensity.clamp(0.0, 1.0) * 0.9);
                Style::default()
                    .fg(Color::Rgb(color.0, color.1, color.2))
                    .add_modifier(Modifier::BOLD)
            } else if intensity < 0.2 {
                Style::default().add_modifier(Modifier::DIM)
            } else if intensity < 0.6 {
                Style::default()
            } else {
                Style::default().add_modifier(Modifier::BOLD)
            };
            Span::styled(character.to_string(), style)
        })
        .collect()
}

fn supports_true_color() -> bool {
    crate::terminal_color::stdout_supports_truecolor()
}

fn blend(foreground: (u8, u8, u8), background: (u8, u8, u8), alpha: f32) -> (u8, u8, u8) {
    (
        (foreground.0 as f32 * alpha + background.0 as f32 * (1.0 - alpha)) as u8,
        (foreground.1 as f32 * alpha + background.1 as f32 * (1.0 - alpha)) as u8,
        (foreground.2 as f32 * alpha + background.2 as f32 * (1.0 - alpha)) as u8,
    )
}

fn first_display_line(source: &str) -> String {
    let first = source.lines().next().unwrap_or(source).trim();
    let mut characters = first.chars();
    let shortened = characters.by_ref().take(100).collect::<String>();
    if characters.next().is_some() || source.lines().nth(1).is_some() {
        format!("{shortened} …")
    } else {
        shortened
    }
}

fn editor_line(
    text: &str,
    paste_ranges: &[std::ops::Range<usize>],
    skill_ranges: &[std::ops::Range<usize>],
    image_ranges: &[std::ops::Range<usize>],
    history_search_ranges: &[std::ops::Range<usize>],
) -> Line<'static> {
    let mut highlights = paste_ranges
        .iter()
        .chain(skill_ranges)
        .chain(image_ranges)
        .chain(history_search_ranges)
        .cloned()
        .collect::<Vec<_>>();
    highlights.sort_by_key(|range| range.start);
    let mut merged: Vec<std::ops::Range<usize>> = Vec::with_capacity(highlights.len());
    for range in highlights {
        if let Some(previous) = merged.last_mut()
            && range.start <= previous.end
        {
            previous.end = previous.end.max(range.end);
        } else {
            merged.push(range);
        }
    }
    let mut spans = Vec::with_capacity(merged.len().saturating_mul(2).saturating_add(1));
    let mut cursor = 0;
    for range in &merged {
        debug_assert!(range.start >= cursor && range.end <= text.len());
        if cursor < range.start {
            spans.push(Span::raw(text[cursor..range.start].to_string()));
        }
        spans.push(Span::styled(
            text[range.clone()].to_string(),
            Style::default().fg(Color::Cyan),
        ));
        cursor = range.end;
    }
    if cursor < text.len() || spans.is_empty() {
        spans.push(Span::raw(text[cursor..].to_string()));
    }
    Line::from(spans)
}

fn format_context_usage(tokens: Option<u64>, context_window: u64) -> String {
    let context_window_k = context_window / 1_000;
    let Some(tokens) = tokens else {
        return format!("? of {context_window_k}K");
    };
    let percent = (tokens as f64 / context_window as f64 * 100.0).clamp(0.0, 100.0);
    if percent > 0.0 && percent < 1.0 {
        format!("{percent:.1}% of {context_window_k}K")
    } else {
        format!("{percent:.0}% of {context_window_k}K")
    }
}

fn format_elapsed(seconds: u64) -> String {
    match seconds {
        0..=59 => format!("{seconds}s"),
        60..=3599 => format!("{}m {:02}s", seconds / 60, seconds % 60),
        _ => format!(
            "{}h {:02}m {:02}s",
            seconds / 3600,
            (seconds % 3600) / 60,
            seconds % 60
        ),
    }
}

fn shortcut_reference_lines() -> Vec<Line<'static>> {
    let mut lines = vec![
        Line::from("Keyboard shortcuts").bold(),
        Line::default(),
        shortcut_line("Enter", "submit prompt"),
        shortcut_line("Enter while working", "steer after current model step"),
        shortcut_line("Tab while working", "queue a follow-up turn"),
        shortcut_line("Alt+Up / Shift+Left", "edit last queued follow-up"),
        shortcut_line("Shift+Enter / Ctrl+J", "insert newline"),
        shortcut_line("@", "find and insert a file path"),
        shortcut_line("$", "mention an installed skill"),
        shortcut_line("Esc", "interrupt active turn"),
        shortcut_line("Up / Down", "restore prompt history"),
        shortcut_line(
            "Ctrl+R / Ctrl+S",
            "search prompt history backward / forward",
        ),
        shortcut_line("Ctrl+O", "copy latest final response as Markdown"),
        shortcut_line("Ctrl+C", "clear draft, interrupt work, or exit when idle"),
    ];
    #[cfg(windows)]
    lines.extend([
        shortcut_line("Ctrl+Left / Right", "jump by word (Alt works too)"),
        shortcut_line("Ctrl+Backspace", "delete previous word (Ctrl+W too)"),
        shortcut_line("Ctrl+V / Shift+Insert", "terminal-owned clipboard paste"),
        shortcut_line("Ctrl+C with selection", "terminal-owned selection copy"),
    ]);
    #[cfg(not(windows))]
    lines.extend([
        shortcut_line("Option+Left / Right", "jump by word"),
        shortcut_line("Option+Backspace", "delete previous word (Ctrl+W too)"),
    ]);
    lines
}

fn shortcuts_height(width: u16) -> u16 {
    measure_text_height(&shortcut_reference_lines(), width.saturating_sub(4))
        .saturating_add(menu_surface_padding_height())
        .saturating_add(1)
}

fn shortcut_line(key: &'static str, description: &'static str) -> Line<'static> {
    Line::from(vec![
        Span::from(format!("{key:<22}")),
        Span::from(description).dim(),
    ])
}

fn truncate_line(line: Line<'static>, width: usize) -> Line<'static> {
    if line_width(&line) <= width {
        return line;
    }
    if width == 0 {
        return Line::default();
    }
    let target = width.saturating_sub(1);
    let mut used = 0;
    let mut spans = Vec::new();
    'spans: for span in line.spans {
        let mut content = String::new();
        for grapheme in span.content.graphemes(true) {
            let grapheme_width = UnicodeWidthStr::width(grapheme);
            if used + grapheme_width > target {
                if !content.is_empty() {
                    spans.push(Span::styled(content, span.style));
                }
                break 'spans;
            }
            content.push_str(grapheme);
            used += grapheme_width;
        }
        if !content.is_empty() {
            spans.push(Span::styled(content, span.style));
        }
    }
    spans.push(Span::styled("…", Style::default().fg(MUTED)));
    Line::from(spans).style(line.style)
}

fn line_width(line: &Line<'_>) -> usize {
    line.spans
        .iter()
        .map(|span| UnicodeWidthStr::width(span.content.as_ref()))
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use serde_json::json;
    use std::time::Duration;

    fn prompt(text: impl Into<String>) -> UserPrompt {
        UserPrompt::text(text)
    }

    #[test]
    fn resumed_transcript_renders_user_tool_and_assistant_history_in_order() {
        let source =
            "const result = await tools.exec_command({cmd:\"cargo test\"}); text(result.output);";
        let transcript = vec![
            SessionTranscriptItem::User {
                text: "run the checks".to_string(),
                image_count: 0,
            },
            SessionTranscriptItem::Tool {
                tool: SessionTranscriptTool {
                    call_id: "call-1".to_string(),
                    name: "exec".to_string(),
                    input: Some(Value::String(source.to_string())),
                    output: Some(SessionTranscriptToolOutput::Success(Value::String(
                        "Script completed\nWall time 0.1 seconds\nOutput:\ntest result: ok"
                            .to_string(),
                    ))),
                },
            },
            SessionTranscriptItem::Assistant {
                text: "All checks pass.".to_string(),
                phase: Some(MessagePhase::FinalAnswer),
            },
        ];
        let mut view = View::new(Path::new("/tmp/bettercodex"));
        view.welcome_pending = false;

        view.replay_transcript(transcript.clone());

        assert_eq!(view.session_transcript(), transcript);
        let rendered = view
            .take_pending_history_lines(80, 24)
            .iter()
            .map(plain)
            .collect::<Vec<_>>()
            .join("\n");
        let user = rendered.find("run the checks").expect("replayed user");
        let tool = rendered.find("Ran code").expect("replayed Code Mode call");
        assert!(rendered.contains("exec_command"), "{rendered}");
        let output = rendered.find("test result: ok").expect("replayed output");
        let assistant = rendered
            .find("All checks pass.")
            .expect("replayed assistant");
        assert!(
            user < tool && tool < output && output < assistant,
            "{rendered}"
        );
    }

    #[test]
    fn assistant_live_suffix_matches_the_full_render() {
        let mut content = MarkdownRenderCache::new("first paragraph\n\n".to_string());
        content.append("second paragraph\n\n");
        content.append("third paragraph");
        let full = assistant_lines(&mut content, 80, Path::new("/tmp"), true);
        assert!(full.len() >= 3);

        let start = 2;
        let (rendered_len, suffix) =
            assistant_lines_after(&mut content, 80, Path::new("/tmp"), true, start);

        assert_eq!(rendered_len, full.len());
        assert_eq!(suffix, full[start..]);
    }

    #[test]
    fn streamed_assistant_filters_control_sequences_split_across_deltas() {
        let chunks = [
            "before ",
            "\x1b[",
            "31mred \x1b]0;secret",
            " continuation\x1b\\after",
        ];
        let source = chunks.concat();
        let mut view = View::new(Path::new("/tmp/bettercodex"));
        view.welcome_pending = false;
        view.start_turn("test");
        let _ = view.take_pending_history_lines(80, 24);
        view.handle_agent_event(AgentEvent::ModelMessageDelta(chunks[0].to_string()));
        let before_control = view
            .prepare(80, 24)
            .active_lines
            .iter()
            .map(plain)
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(before_control, "\n• before");

        for chunk in &chunks[1..] {
            view.handle_agent_event(AgentEvent::ModelMessageDelta(chunk.to_string()));
        }

        let streamed = view
            .prepare(80, 24)
            .active_lines
            .iter()
            .map(plain)
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(streamed, "\n• before red after");

        view.handle_agent_event(completed_message(source));
        let finalized = view
            .prepare(80, 24)
            .active_lines
            .iter()
            .map(plain)
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(finalized, streamed);
    }

    #[test]
    fn patch_notes_render_as_a_bordered_markdown_history_block() {
        const WIDTH: u16 = 40;
        let mut view = View::new(Path::new("/tmp/bettercodex"));
        view.welcome_pending = false;
        view.add_patch_notes("## [0.1.4]\n\n- Added `/model` selector");

        let lines = view.take_pending_history_lines(WIDTH, 24);
        let rows = lines.iter().map(plain).collect::<Vec<_>>();

        assert_eq!(rows.first().unwrap(), &"─".repeat(usize::from(WIDTH)));
        assert_eq!(rows.last(), rows.first());
        assert!(rows.iter().any(|row| row.trim() == "What's New"));
        assert!(rows.iter().any(|row| row.contains("[0.1.4]")));
        assert!(rows.iter().any(|row| row.contains("Added /model selector")));
        assert!(
            rows.iter()
                .all(|row| line_width(&Line::from(row.clone())) <= usize::from(WIDTH))
        );
    }

    #[test]
    fn review_completion_submits_the_explicit_review_command() {
        let mut view = View::new(Path::new("/tmp/bettercodex"));
        view.editor.set_text("/rev");

        let Action::Submit(submission) = view.handle_terminal_event(Event::Key(KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::NONE,
        ))) else {
            panic!("review completion should submit a turn");
        };
        assert_eq!(
            submission.prompt().text_without_image_placeholders(),
            "/review"
        );
    }

    #[test]
    fn rejected_local_commands_keep_the_draft_editable() {
        let mut rejected = vec![
            ("!", false),
            ("/resume not-a-session-id", false),
            ("/compact", true),
            ("/fork", true),
            ("/clear", true),
            ("/skills", true),
            ("/logout", true),
        ];
        #[cfg(unix)]
        rejected.push(("/tmux unexpected", false));
        for (draft, busy) in rejected {
            let mut view = View::new(Path::new("/tmp/bettercodex"));
            if busy {
                view.start_turn("active turn");
            }
            view.editor.set_text(draft);

            assert_eq!(
                view.handle_terminal_event(Event::Key(KeyEvent::new(
                    KeyCode::Enter,
                    KeyModifiers::NONE,
                ))),
                Action::None,
                "{draft}"
            );
            assert_eq!(view.editor.text(), draft, "{draft}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn rejected_local_command_preserves_a_compacted_large_paste() {
        let mut view = View::new(Path::new("/tmp/bettercodex"));
        let pasted = "x".repeat(1_001);
        view.editor.set_text("/tmux ");
        view.editor.insert_paste(pasted.clone());
        let compact_draft = view.editor.text().to_string();
        assert!(compact_draft.contains("[Pasted Content"), "{compact_draft}");

        assert_eq!(
            view.handle_terminal_event(Event::Key(KeyEvent::new(
                KeyCode::Enter,
                KeyModifiers::NONE,
            ))),
            Action::None
        );
        assert_eq!(view.editor.text(), compact_draft);
        assert_eq!(
            view.editor.take_prompt().text_without_image_placeholders(),
            format!("/tmux {pasted}")
        );
    }

    #[test]
    fn logout_cannot_be_queued_as_agent_input() {
        let mut view = View::new(Path::new("/tmp/bettercodex"));
        view.start_turn("active turn");
        view.editor.set_text("/logout ");

        assert_eq!(
            view.handle_terminal_event(Event::Key(
                KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE,)
            )),
            Action::None
        );
        assert_eq!(view.editor.text(), "/logout ");
        assert!(matches!(
            view.entries.last(),
            Some(TranscriptEntry::Notice(message))
                if message.starts_with("Slash commands cannot be queued")
        ));
    }

    #[test]
    fn runtime_rejection_restores_skill_and_image_bindings() {
        let mut view = View::new(Path::new("/tmp/bettercodex"));
        view.welcome_pending = false;
        let text = "$review inspect [Image #1]";
        let image_label = "[Image #1]";
        let image_start = text.find(image_label).unwrap();
        let expected = UserPrompt::with_attachments(
            text,
            vec![crate::skills::SkillMention::new(
                SkillSelection::new("review", "/tmp/review/SKILL.md"),
                0.."$review".len(),
            )],
            vec![crate::input::PromptImageAttachment::new(
                crate::input::PromptImage::from_bytes(
                    Path::new("fixture.png"),
                    b"\x89PNG\r\n\x1a\nfixture".to_vec(),
                    crate::input::ImageDetail::Original,
                )
                .unwrap(),
                image_start..image_start + image_label.len(),
            )],
        );
        view.editor.set_user_prompt(&expected);
        let submission = ComposerSubmission::take(&mut view.editor);

        view.reject_composer_submission(
            submission,
            "Could not start turn: the active agent is unavailable",
        );

        assert_eq!(view.editor.take_prompt(), expected);
        let rendered = view
            .take_pending_history_lines(80, 24)
            .iter()
            .flat_map(|line| line.line.spans.iter())
            .map(|span| span.content.as_ref())
            .collect::<String>();
        assert!(
            rendered.contains("■ Could not start turn: the active agent is unavailable"),
            "{rendered}"
        );
    }

    #[test]
    fn ctrl_c_cleared_rich_draft_is_recoverable_with_up() {
        let mut view = View::new(Path::new("/tmp/bettercodex"));
        let text = "$review inspect [Image #1]";
        let image_label = "[Image #1]";
        let image_start = text.find(image_label).unwrap();
        let prompt = UserPrompt::with_attachments(
            text,
            vec![crate::skills::SkillMention::new(
                SkillSelection::new("review", "/tmp/review/SKILL.md"),
                0.."$review".len(),
            )],
            vec![crate::input::PromptImageAttachment::new(
                crate::input::PromptImage::from_bytes(
                    Path::new("fixture.png"),
                    b"\x89PNG\r\n\x1a\nfixture".to_vec(),
                    crate::input::ImageDetail::Original,
                )
                .unwrap(),
                image_start..image_start + image_label.len(),
            )],
        );
        view.editor.set_user_prompt(&prompt);
        let pasted = "z".repeat(1_001);
        view.editor.insert_paste(pasted.clone());
        let compact_draft = view.editor.text().to_string();

        assert_eq!(
            view.handle_terminal_event(Event::Key(KeyEvent::new(
                KeyCode::Char('c'),
                KeyModifiers::CONTROL,
            ))),
            Action::None
        );
        assert!(view.editor.is_empty());
        assert_eq!(
            view.handle_terminal_event(Event::Key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE,))),
            Action::None
        );
        assert_eq!(view.editor.text(), compact_draft);

        let recalled = view.editor.take_prompt();
        assert_eq!(recalled.image_count(), 1);
        assert_eq!(recalled.skill_mentions(), prompt.skill_mentions());
        assert_eq!(
            recalled.text_without_image_placeholders(),
            format!("$review inspect {pasted}")
        );
    }

    #[test]
    fn first_persistent_history_recall_requests_a_lazy_batch() {
        let mut view = View::new(Path::new("/tmp/bettercodex"));
        view.seed_prompt_history(std::iter::empty(), true);

        assert_eq!(
            view.handle_terminal_event(Event::Key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE,))),
            Action::LoadPromptHistory
        );
        assert!(!view.prompt_history_loaded(["older prompt".to_string()], false));
        assert_eq!(view.editor.text(), "older prompt");
    }

    fn completed_message(text: impl Into<String>) -> AgentEvent {
        AgentEvent::ModelMessageCompleted(AssistantMessage {
            text: text.into(),
            phase: Some(MessagePhase::FinalAnswer),
        })
    }

    #[test]
    fn context_usage_matches_the_preserved_contract() {
        let window = ModelSelection::default().effective_context_window();
        assert_eq!(format_context_usage(None, window), "? of 258K");
        assert_eq!(format_context_usage(Some(1_000), window), "0.4% of 258K");
        assert_eq!(format_context_usage(Some(51_680), window), "20% of 258K");
        assert_eq!(format_context_usage(Some(u64::MAX), window), "100% of 258K");
    }

    #[test]
    fn status_line_matches_the_preserved_field_order() {
        let mut view = View::new(Path::new("/tmp/bettercodex"));
        view.repository = Repository {
            name: "pi".to_string(),
            branch: Some("main".to_string()),
        };
        view.context_tokens = Some(51_680);
        assert_eq!(
            plain(&view.status_line(80)),
            "gpt-5.6-sol xhigh │ pi / main │ 20% of 258K"
        );
    }

    #[test]
    fn fast_command_and_rendered_footer_show_fast_beside_reasoning() {
        let mut view = View::new(Path::new("/tmp/bettercodex"));
        view.editor.set_text("/fast");
        assert_eq!(
            view.handle_terminal_event(Event::Key(KeyEvent::new(
                KeyCode::Enter,
                KeyModifiers::NONE,
            ))),
            Action::ToggleFast
        );

        view.welcome_pending = false;
        view.repository = Repository {
            name: "bettercodex".to_string(),
            branch: Some("main".to_string()),
        };
        view.set_service_tier(ServiceTier::Fast);
        assert_eq!(
            plain(&view.status_line(80)),
            "gpt-5.6-sol xhigh fast │ bettercodex / main │ ? of 258K"
        );

        let height = view.desired_height(80, 24);
        let backend = TestBackend::new(80, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| view.render(frame)).unwrap();
        let rendered = render_buffer(terminal.backend().buffer());
        assert!(
            rendered.contains("gpt-5.6-sol xhigh fast │ bettercodex / main"),
            "{rendered}"
        );
    }

    #[test]
    fn model_command_opens_the_fixed_picker() {
        let mut view = View::new(Path::new("/tmp/bettercodex"));
        view.editor.set_text("/model");

        assert_eq!(
            view.handle_terminal_event(Event::Key(KeyEvent::new(
                KeyCode::Enter,
                KeyModifiers::NONE,
            ))),
            Action::None
        );
        assert!(matches!(&view.overlay, Some(Overlay::Model(_))));
        assert!(view.editor.is_empty());
    }

    #[test]
    fn busy_terminal_detail_and_background_summary_use_dedicated_rows() {
        const WIDTH: u16 = 96;

        let mut view = View::new(Path::new("/tmp/bettercodex"));
        view.welcome_pending = false;
        view.start_turn("test");
        let _ = view.take_pending_history_lines(WIDTH, 24);
        view.status_detail = Some("cargo nextest run".to_string());
        let height_without_background_terminal = view.desired_height(WIDTH, 24);

        assert!(view.set_background_processes(vec![BackgroundProcess {
            session_id: 42,
            command: "cargo nextest run".to_string(),
            cwd: PathBuf::from("/tmp/bettercodex"),
            running_for: Duration::from_secs(10),
        }]));

        let height = view.desired_height(WIDTH, 24);
        assert_eq!(height, height_without_background_terminal + 1);
        let backend = TestBackend::new(WIDTH, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| view.render(frame)).unwrap();
        let buffer = terminal.backend().buffer();
        let rendered = render_buffer(buffer);
        let rows = rendered.lines().collect::<Vec<_>>();
        let status_y = rows
            .iter()
            .position(|row| row.contains("Working ("))
            .expect("rendered activity status") as u16;
        let detail_y = rows
            .iter()
            .position(|row| row.contains("└ cargo nextest run"))
            .expect("rendered terminal detail row") as u16;
        let background_terminal_y = rows
            .iter()
            .position(|row| row.contains("1 background terminal running"))
            .expect("rendered background terminal row") as u16;
        let composer_background = view.user_message_style.bg.unwrap();
        let composer_y = (0..height)
            .find(|&y| buffer[(0, y)].bg == composer_background)
            .expect("rendered composer");

        assert!(
            !rows[usize::from(status_y)].contains("cargo nextest")
                && !rows[usize::from(status_y)].contains("background terminal"),
            "{rendered}"
        );
        assert_eq!(detail_y, status_y + 1, "{rendered}");
        assert_eq!(background_terminal_y, detail_y + 1, "{rendered}");
        assert_eq!(
            composer_y,
            background_terminal_y + 1 + ACTIVITY_COMPOSER_GAP,
            "{rendered}"
        );
        assert_eq!(
            rendered.matches("cargo nextest run").count(),
            1,
            "{rendered}"
        );
        assert_eq!(
            rendered.matches("1 background terminal running").count(),
            1,
            "{rendered}"
        );
    }

    #[test]
    fn background_wait_command_uses_a_detail_row_between_status_and_composer() {
        const WIDTH: u16 = 48;
        const COMMAND: &str = "cargo test -p bettercodex -- --exact some::very::long::test::name";

        let mut view = View::new(Path::new("/tmp/bettercodex"));
        view.welcome_pending = false;
        view.start_turn("test");
        let _ = view.take_pending_history_lines(WIDTH, 24);
        view.handle_agent_event(AgentEvent::ToolStarted {
            call_id: "cell:start".to_string(),
            name: "exec_command".to_string(),
            input: Some(json!({"cmd": COMMAND})),
        });
        view.handle_agent_event(AgentEvent::ToolCompleted {
            call_id: "cell:start".to_string(),
            output: Ok(json!({"session_id": 42, "output": ""})),
            duration: Duration::from_millis(10),
        });
        let _ = view.take_pending_history_lines(WIDTH, 24);
        let height_without_wait = view.desired_height(WIDTH, 24);

        view.handle_agent_event(AgentEvent::ToolStarted {
            call_id: "cell:wait".to_string(),
            name: "write_stdin".to_string(),
            input: Some(json!({"session_id": 42})),
        });

        let height = view.desired_height(WIDTH, 24);
        assert_eq!(height, height_without_wait + 1);
        let backend = TestBackend::new(WIDTH, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| view.render(frame)).unwrap();
        let buffer = terminal.backend().buffer();
        let rendered = render_buffer(buffer);
        let rows = rendered.lines().collect::<Vec<_>>();
        let status_y = rows
            .iter()
            .position(|row| row.contains("Waiting for background terminal"))
            .expect("rendered wait status") as u16;
        let detail_y = rows
            .iter()
            .position(|row| row.contains("└ cargo test -p bettercodex"))
            .expect("rendered command detail") as u16;
        let composer_background = view.user_message_style.bg.unwrap();
        let composer_y = (0..height)
            .find(|&y| buffer[(0, y)].bg == composer_background)
            .expect("rendered composer");
        let composer_bottom = (0..height)
            .rfind(|&y| buffer[(0, y)].bg == composer_background)
            .expect("rendered composer")
            .saturating_add(1);
        let footer_y = rows
            .iter()
            .position(|row| row.contains(view.model_selection.model.as_str()))
            .expect("rendered footer") as u16;

        assert!(
            !rows[usize::from(status_y)].contains("cargo test"),
            "{rendered}"
        );
        assert_eq!(detail_y, status_y + 1, "{rendered}");
        assert_eq!(
            composer_y,
            detail_y + 1 + ACTIVITY_COMPOSER_GAP,
            "{rendered}"
        );
        assert_eq!(footer_y, composer_bottom, "{rendered}");
        assert_eq!(rendered.matches("cargo test").count(), 1, "{rendered}");
        assert!(rows[usize::from(detail_y)].contains('…'), "{rendered}");
    }

    #[test]
    fn initial_viewport_contains_only_the_codex_composer() {
        let mut view = View::new(Path::new("/tmp/bettercodex"));
        assert_eq!(view.desired_height(88, 24), 5);
        let history = view.take_pending_history_lines(88, 24);
        assert!(
            history
                .iter()
                .any(|line| plain(line).contains("bettercodex"))
        );
        assert_eq!(view.desired_height(88, 24), 5);

        let backend = TestBackend::new(88, 5);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| view.render(frame)).unwrap();
        let rendered = render_buffer(terminal.backend().buffer());
        assert!(!rendered.contains(">_ bettercodex"));
        assert!(!rendered.contains("Ask bettercodex to do anything"));
        assert!(rendered.lines().any(|line| line.trim() == "›"));
        assert!(rendered.contains("gpt-5.6-sol xhigh"));
    }

    #[test]
    fn update_available_card_renders_the_update_command() {
        let mut view = View::new(Path::new("/tmp/bettercodex"));
        view.welcome_pending = false;
        view.add_update_available(AvailableUpdate::test_fixture());
        let height = view.desired_height(60, 16);
        let backend = TestBackend::new(60, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| view.render(frame)).unwrap();
        let rendered = render_buffer(terminal.backend().buffer());

        assert!(rendered.contains("Update available"), "{rendered}");
        assert!(rendered.contains("bcodex update"), "{rendered}");
        assert!(rendered.contains("in another terminal"), "{rendered}");
        assert!(rendered.contains("111111111111"), "{rendered}");
        assert!(rendered.contains("222222222222"), "{rendered}");
    }

    #[test]
    fn shimmer_uses_the_terminal_palette_for_its_sweep() {
        let colors = TerminalColors {
            foreground: (220, 220, 220),
            background: (20, 20, 20),
        };
        let spans = shimmer_spans_at("Working", Duration::from_millis(741), Some(colors), true);

        assert_eq!(spans.len(), "Working".chars().count());
        assert_eq!(spans[0].content.as_ref(), "W");
        assert_ne!(spans[0].style.fg, spans[6].style.fg);
        assert!(
            spans
                .iter()
                .all(|span| span.style.add_modifier.contains(Modifier::BOLD))
        );
    }

    #[test]
    fn welcome_card_uses_codex_content_width_instead_of_terminal_width() {
        let lines = welcome_card_lines(
            Path::new("/tmp/bettercodex"),
            100,
            &ModelSelection::default(),
            ServiceTier::default(),
        );
        let widest_content = lines[1..lines.len() - 1]
            .iter()
            .map(line_width)
            .max()
            .unwrap();
        assert_eq!(line_width(&lines[0]), widest_content);
        assert!(line_width(&lines[0]) < 60);
    }

    #[test]
    fn welcome_card_shows_fast_beside_reasoning_when_resuming_fast_session() {
        let lines = welcome_card_lines(
            Path::new("/tmp/bettercodex"),
            80,
            &ModelSelection::default(),
            ServiceTier::Fast,
        );
        assert!(
            lines
                .iter()
                .any(|line| plain(line).contains("gpt-5.6-sol xhigh fast"))
        );
    }

    #[test]
    fn roomy_welcome_includes_art_and_small_layouts_keep_the_card() {
        let roomy = welcome_lines(
            Path::new("/tmp/bettercodex"),
            80,
            42,
            &ModelSelection::default(),
            ServiceTier::default(),
        );
        let short = welcome_lines(
            Path::new("/tmp/bettercodex"),
            80,
            27,
            &ModelSelection::default(),
            ServiceTier::default(),
        );
        let narrow = welcome_lines(
            Path::new("/tmp/bettercodex"),
            29,
            42,
            &ModelSelection::default(),
            ServiceTier::default(),
        );

        assert!(
            roomy
                .iter()
                .take(usize::from(startup_art::ART_HEIGHT))
                .flat_map(|line| line.spans.iter())
                .flat_map(|span| span.content.chars())
                .any(|character| ('\u{2800}'..='\u{28ff}').contains(&character))
        );
        assert_eq!(
            short.len(),
            welcome_card_lines(
                Path::new("/tmp/bettercodex"),
                80,
                &ModelSelection::default(),
                ServiceTier::default(),
            )
            .len()
        );
        assert_eq!(
            narrow.len(),
            welcome_card_lines(
                Path::new("/tmp/bettercodex"),
                29,
                &ModelSelection::default(),
                ServiceTier::default(),
            )
            .len()
        );
        assert!(short.iter().any(|line| plain(line).contains("bettercodex")));
        assert!(
            narrow
                .iter()
                .any(|line| plain(line).contains("bettercodex"))
        );
    }

    #[test]
    fn resize_reflow_rebuilds_finalized_history_from_source() {
        let mut view = View::new(Path::new("/tmp/bettercodex"));
        assert!(
            view.take_pending_history_lines(80, 24)
                .iter()
                .any(|line| plain(line).contains("bettercodex"))
        );
        view.start_turn("a user message that wraps at the narrower width");
        let _ = view.take_pending_history_lines(80, 24);
        view.handle_agent_event(AgentEvent::ModelMessageDelta("assistant reply".to_string()));
        view.handle_agent_event(completed_message("assistant reply"));
        let _ = view.take_pending_history_lines(80, 24);

        assert_eq!(
            view.handle_terminal_event(Event::Resize(32, 18)),
            Action::None
        );
        assert!(view.take_resize_reflow_request());
        let replay = view
            .history_lines_for_resize_reflow(32, 18)
            .iter()
            .map(plain)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(replay.contains("bettercodex"), "{replay}");
        assert!(replay.contains("› a user message"), "{replay}");
        assert!(replay.contains("• assistant reply"), "{replay}");
        assert!(!view.welcome_pending);
        assert_eq!(view.committed_entries, 2);
    }

    #[test]
    fn streamed_tail_keeps_continuation_style_without_completion_reflow() {
        const WIDTH: u16 = 48;
        const HEIGHT: u16 = 12;
        let source = (0..80)
            .map(|index| {
                format!("paragraph {index} with [documentation](https://example.com/{index})")
            })
            .collect::<Vec<_>>()
            .join("\n\n");

        let mut view = View::new(Path::new("/tmp/bettercodex"));
        view.welcome_pending = false;
        view.start_turn("test");
        let _ = view.take_pending_history_lines(WIDTH, HEIGHT);
        view.handle_agent_event(AgentEvent::ModelMessageDelta(source.clone()));

        let prepared = view.prepare(WIDTH, HEIGHT);
        assert!(!prepared.history_lines.is_empty());
        let prefix = &prepared.active_lines[0].line.spans[0];
        assert_eq!(prefix.content.as_ref(), "  ");
        assert!(!prefix.style.add_modifier.contains(Modifier::DIM));

        view.handle_agent_event(completed_message(source));
        assert!(!view.streamed_history_needs_reflow(WIDTH));
    }

    #[test]
    fn user_message_and_composer_share_codex_background() {
        let style = user_message_style_for(Some((31, 31, 31)));
        let prompt = prompt("test");
        let lines = user_message_lines(&DisplayedUserPrompt::from_prompt(&prompt), 40, style);
        assert_eq!(lines.len(), 3);
        assert_eq!(plain(&lines[1]), "› test");
        assert_eq!(lines[0].style.bg, style.bg);
        assert_eq!(lines[1].style.bg, style.bg);
        assert_eq!(lines[2].style.bg, style.bg);

        let mut view = View::new(Path::new("/tmp/bettercodex"));
        view.welcome_pending = false;
        view.user_message_style = style;
        let backend = TestBackend::new(40, 5);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| view.render(frame)).unwrap();
        assert_eq!(terminal.backend().buffer()[(0, 1)].bg, style.bg.unwrap());
    }

    #[test]
    fn submitted_user_message_keeps_its_background_in_scrollback() {
        let mut view = View::new(Path::new("/tmp/bettercodex"));
        view.welcome_pending = false;
        view.start_turn("test");

        let lines = view.take_pending_history_lines(40, 24);
        let buffer = crate::tui::terminal::render_history_lines(&lines, 40);
        let background = view.user_message_style.bg.unwrap();

        assert_eq!(lines.len(), 3);
        for y in 0..3 {
            for x in 0..40 {
                assert_eq!(buffer[(x, y)].bg, background);
            }
        }
    }

    #[test]
    fn nested_exec_uses_codex_command_cell_shape() {
        let mut view = View::new(Path::new("/tmp/bettercodex"));
        view.welcome_pending = false;
        view.handle_agent_event(AgentEvent::ToolStarted {
            call_id: "cell:cmd".to_string(),
            name: "exec_command".to_string(),
            input: Some(json!({"cmd": "cargo test"})),
        });
        view.handle_agent_event(AgentEvent::ToolCompleted {
            call_id: "cell:cmd".to_string(),
            output: Ok(json!({"exit_code": 0, "output": "21 passed\n"})),
            duration: Duration::from_millis(50),
        });
        let rendered = view
            .take_pending_history_lines(80, 24)
            .iter()
            .map(plain)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(rendered.contains("• Ran cargo test"));
        assert!(rendered.contains("  └ 21 passed"));
        assert!(!rendered.contains("exec ·"));
    }

    #[test]
    fn write_stdin_uses_current_codex_interaction_cell_shape() {
        let mut view = View::new(Path::new("/tmp/bettercodex"));
        view.welcome_pending = false;
        view.handle_agent_event(AgentEvent::ToolStarted {
            call_id: "cell:start".to_string(),
            name: "exec_command".to_string(),
            input: Some(json!({"cmd": "bash"})),
        });
        view.handle_agent_event(AgentEvent::ToolCompleted {
            call_id: "cell:start".to_string(),
            output: Ok(json!({"session_id": 42, "output": ""})),
            duration: Duration::from_millis(10),
        });
        view.handle_agent_event(AgentEvent::ToolStarted {
            call_id: "cell:input".to_string(),
            name: "write_stdin".to_string(),
            input: Some(json!({"session_id": 42, "chars": "echo ready\n"})),
        });
        view.handle_agent_event(AgentEvent::ToolCompleted {
            call_id: "cell:input".to_string(),
            output: Ok(json!({"exit_code": 0, "output": "ready\n"})),
            duration: Duration::from_millis(10),
        });
        let rendered = view
            .take_pending_history_lines(80, 24)
            .iter()
            .map(plain)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            rendered.contains("↳ Interacted with background terminal · bash"),
            "{rendered}"
        );
        assert!(rendered.contains("  └ echo ready"), "{rendered}");
        assert!(!rendered.contains("  └ ready"), "{rendered}");
    }

    #[test]
    fn stale_empty_write_stdin_poll_is_not_rendered() {
        let mut view = View::new(Path::new("/tmp/bettercodex"));
        view.welcome_pending = false;
        view.handle_agent_event(AgentEvent::ToolStarted {
            call_id: "cell:start".to_string(),
            name: "exec_command".to_string(),
            input: Some(json!({"cmd": "set -eu\nprintf done"})),
        });
        view.handle_agent_event(AgentEvent::ToolCompleted {
            call_id: "cell:start".to_string(),
            output: Ok(json!({"session_id": 42, "output": ""})),
            duration: Duration::from_millis(10),
        });
        let _ = view.take_pending_history_lines(80, 24);

        view.handle_agent_event(AgentEvent::ToolStarted {
            call_id: "cell:stale-wait".to_string(),
            name: "write_stdin".to_string(),
            input: Some(json!({"session_id": 42})),
        });
        view.handle_agent_event(AgentEvent::ToolCompleted {
            call_id: "cell:stale-wait".to_string(),
            output: Err("Unknown process id 42".to_string()),
            duration: Duration::from_millis(10),
        });

        assert!(view.take_pending_history_lines(80, 24).is_empty());
        assert!(view.active_lines(80).is_empty());
        assert!(!view.process_commands.contains_key(&42));
    }

    #[test]
    fn completed_empty_write_stdin_poll_is_not_rendered() {
        let mut view = View::new(Path::new("/tmp/bettercodex"));
        view.welcome_pending = false;
        view.process_commands.insert(42, "cargo test".to_string());
        view.handle_agent_event(AgentEvent::ToolStarted {
            call_id: "cell:final-wait".to_string(),
            name: "write_stdin".to_string(),
            input: Some(json!({"session_id": 42})),
        });
        view.handle_agent_event(AgentEvent::ToolCompleted {
            call_id: "cell:final-wait".to_string(),
            output: Ok(json!({"exit_code": 0, "output": "done\n"})),
            duration: Duration::from_millis(10),
        });

        assert!(view.take_pending_history_lines(80, 24).is_empty());
        assert!(view.active_lines(80).is_empty());
        assert!(!view.process_commands.contains_key(&42));
    }

    #[test]
    fn repeated_live_write_stdin_polls_coalesce_like_codex() {
        let mut view = View::new(Path::new("/tmp/bettercodex"));
        view.welcome_pending = false;
        view.process_commands.insert(42, "just fix".to_string());
        for call_id in ["cell:wait-1", "cell:wait-2"] {
            view.handle_agent_event(AgentEvent::ToolStarted {
                call_id: call_id.to_string(),
                name: "write_stdin".to_string(),
                input: Some(json!({"session_id": 42})),
            });
            view.handle_agent_event(AgentEvent::ToolCompleted {
                call_id: call_id.to_string(),
                output: Ok(json!({"session_id": 42, "output": ""})),
                duration: Duration::from_millis(10),
            });
        }
        view.handle_agent_event(completed_message("Finished."));

        let rendered = view
            .take_pending_history_lines(80, 24)
            .iter()
            .map(plain)
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(
            rendered
                .matches("• Waited for background terminal · just fix")
                .count(),
            1,
            "{rendered}"
        );
        assert!(!rendered.contains("Unknown process id"), "{rendered}");
    }

    #[test]
    fn read_commands_use_codex_explored_cell() {
        let mut view = View::new(Path::new("/tmp/bettercodex"));
        view.welcome_pending = false;
        view.handle_agent_event(AgentEvent::ToolStarted {
            call_id: "cell:read".to_string(),
            name: "exec_command".to_string(),
            input: Some(json!({"cmd": "sed -n '1,20p' src/main.rs"})),
        });
        view.handle_agent_event(AgentEvent::ToolCompleted {
            call_id: "cell:read".to_string(),
            output: Ok(json!({"exit_code": 0, "output": "fn main() {}\n"})),
            duration: Duration::from_millis(10),
        });
        view.seal_exploration();
        let rendered = view
            .take_pending_history_lines(80, 24)
            .iter()
            .map(plain)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(rendered.contains("• Explored"), "{rendered}");
        assert!(rendered.contains("Read main.rs"), "{rendered}");
        assert!(!rendered.contains("fn main"), "{rendered}");
    }

    #[test]
    fn web_search_uses_codex_activity_cell_and_hides_internal_reference_ids() {
        let mut view = View::new(Path::new("/tmp/bettercodex"));
        view.welcome_pending = false;
        view.handle_agent_event(AgentEvent::ToolStarted {
            call_id: "cell:web".to_string(),
            name: "web.run".to_string(),
            input: Some(json!({
                "open": [
                    {"ref_id": "turn0search1"},
                    {"ref_id": "turn0search3"},
                    {"ref_id": "turn0search4"},
                    {"ref_id": "turn0search0"},
                ],
            })),
        });

        assert_eq!(
            view.active_lines(80).iter().map(plain).collect::<Vec<_>>(),
            ["• Searching the web"]
        );

        view.handle_agent_event(AgentEvent::ToolCompleted {
            call_id: "cell:web".to_string(),
            output: Ok(json!(
                "opaque search result that must remain model-facing only"
            )),
            duration: Duration::from_millis(50),
        });
        assert_eq!(
            view.take_pending_history_lines(80, 24)
                .iter()
                .map(plain)
                .collect::<Vec<_>>(),
            ["• Searched the web"]
        );
    }

    #[test]
    fn openai_docs_calls_collapse_into_one_explored_tree() {
        let mut view = View::new(Path::new("/tmp/bettercodex"));
        view.welcome_pending = false;
        let calls = [
            (
                "search",
                "openaiDeveloperDocs.search_openai_docs",
                json!({"query": "Responses API compaction"}),
            ),
            (
                "fetch",
                "openaiDeveloperDocs.fetch_openai_doc",
                json!({
                    "url": "https://developers.openai.com/api/docs/guides/compaction",
                    "anchor": "server-side-compaction"
                }),
            ),
            (
                "spec",
                "openaiDeveloperDocs.get_openapi_spec",
                json!({"url": "https://api.openai.com/v1/responses"}),
            ),
            (
                "endpoints",
                "openaiDeveloperDocs.list_api_endpoints",
                json!({}),
            ),
            (
                "list",
                "openaiDeveloperDocs.list_openai_docs",
                json!({"limit": 10}),
            ),
        ];

        for (call_id, name, input) in calls {
            view.handle_agent_event(AgentEvent::ToolStarted {
                call_id: call_id.to_string(),
                name: name.to_string(),
                input: Some(input),
            });
            view.handle_agent_event(AgentEvent::ToolCompleted {
                call_id: call_id.to_string(),
                output: Ok(json!("FULL DOCUMENT BODY MUST STAY COLLAPSED")),
                duration: Duration::from_millis(10),
            });
        }
        view.seal_exploration();

        let retained_output = &view
            .find_tool_mut("search")
            .expect("search tool")
            .outcome
            .as_ref()
            .expect("search outcome")
            .output;
        assert_eq!(retained_output, &Ok(Value::Null));

        let rendered = view
            .take_pending_history_lines(120, 24)
            .iter()
            .map(plain)
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(
            rendered,
            "• Explored\n  └ Search OpenAI docs Responses API compaction\n    Fetch OpenAI doc https://developers.openai.com/api/docs/guides/compaction#server-side-compaction\n    Read OpenAPI spec https://api.openai.com/v1/responses\n    List API endpoints\n    List OpenAI docs"
        );
        assert!(!rendered.contains("openaiDeveloperDocs"), "{rendered}");
        assert!(!rendered.contains("FULL DOCUMENT BODY"), "{rendered}");
    }

    #[test]
    fn openai_docs_tree_shows_live_status_and_failures() {
        let mut view = View::new(Path::new("/tmp/bettercodex"));
        view.welcome_pending = false;
        view.handle_agent_event(AgentEvent::ToolStarted {
            call_id: "fetch".to_string(),
            name: "openaiDeveloperDocs.fetch_openai_doc".to_string(),
            input: Some(json!({"url": "https://developers.openai.com/codex/cli"})),
        });

        let live = view
            .active_lines(80)
            .iter()
            .map(|line| plain(&line.line))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(live.contains("Exploring"), "{live}");
        assert!(live.contains("Fetch OpenAI doc"), "{live}");
        assert_eq!(view.status_detail.as_deref(), Some("Fetching OpenAI doc"));

        view.handle_agent_event(AgentEvent::ToolCompleted {
            call_id: "fetch".to_string(),
            output: Err("documentation request timed out\nretry exhausted".to_string()),
            duration: Duration::from_millis(10),
        });
        view.seal_exploration();
        let rendered = view
            .take_pending_history_lines(80, 24)
            .iter()
            .map(plain)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(rendered.contains("• Explored"), "{rendered}");
        assert!(
            rendered.contains("Error documentation request timed out"),
            "{rendered}"
        );
        assert!(!rendered.contains("retry exhausted"), "{rendered}");
    }

    #[test]
    fn plan_and_image_cells_match_codex_labels() {
        let plan = PlanDisplay::parse(Some(&json!({
            "plan": [
                {"step": "Port viewport", "status": "completed"},
                {"step": "Port tools", "status": "in_progress"}
            ]
        })));
        let outcome = ToolOutcome {
            output: Ok(json!("Plan updated")),
        };
        let rendered = plan
            .display_lines(Some(&outcome), 80)
            .iter()
            .map(plain)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(rendered.contains("• Updated Plan"));
        assert!(rendered.contains("✔ Port viewport"));
        assert!(rendered.contains("□ Port tools"));

        let image = view_image_lines(Some(&outcome), "screen.png", 80);
        assert_eq!(plain(&image[0]), "• Viewed Image");
        assert_eq!(plain(&image[1]), "  └ screen.png");
    }

    #[test]
    fn patch_summary_matches_codex_structure() {
        let patch = PatchDisplay::parse(
            "*** Begin Patch\n*** Add File: new.txt\n+alpha\n+beta\n*** End Patch",
            Path::new("/tmp"),
        );
        let outcome = ToolOutcome {
            output: Ok(json!("Done!")),
        };
        let lines = patch.display_lines(
            Some(&outcome),
            80,
            user_message_style_for(Some((31, 31, 31))),
        );
        let rendered = lines.iter().map(plain).collect::<Vec<_>>().join("\n");
        assert!(rendered.contains("• Added new.txt (+2 -0)"));
        assert!(rendered.contains("1 +alpha"));
        assert!(rendered.contains("2 +beta"));
        assert_eq!(
            lines
                .iter()
                .find(|line| plain(line).contains("+alpha"))
                .and_then(|line| line.style.bg),
            Some(Color::Rgb(33, 58, 43))
        );
    }

    #[test]
    fn active_patch_rows_fill_the_viewport_width() {
        const WIDTH: u16 = 40;
        let mut view = View::new(Path::new("/tmp/bettercodex"));
        view.welcome_pending = false;
        view.handle_agent_event(AgentEvent::ToolStarted {
            call_id: "cell:patch".to_string(),
            name: "apply_patch".to_string(),
            input: Some(json!(
                "*** Begin Patch\n*** Add File: new.txt\n+alpha\n*** End Patch"
            )),
        });

        let height = view.desired_height(WIDTH, 24);
        let backend = TestBackend::new(WIDTH, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| view.render(frame)).unwrap();
        let buffer = terminal.backend().buffer();
        let rendered = render_buffer(buffer);
        let patch_row = rendered
            .lines()
            .position(|row| row.contains("+alpha"))
            .expect("active patch row") as u16;

        for column in 0..WIDTH {
            assert_eq!(
                buffer[(column, patch_row)].bg,
                Color::Rgb(33, 58, 43),
                "column {column} did not retain the patch row background\n{rendered}"
            );
        }
    }

    #[test]
    fn patch_hunk_rows_use_source_line_numbers() {
        let root = std::env::temp_dir().join(format!("bcodex-tui-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("sample.txt");
        std::fs::write(
            &path,
            (1..=9)
                .map(|number| format!("line {number}"))
                .collect::<Vec<_>>()
                .join("\n"),
        )
        .unwrap();
        let patch = PatchDisplay::parse(
            "*** Begin Patch\n*** Update File: sample.txt\n@@ line 5\n-line 6\n+six\n line 7\n*** End Patch",
            &root,
        );
        let rendered = patch
            .display_lines(
                Some(&ToolOutcome {
                    output: Ok(json!("Done!")),
                }),
                80,
                user_message_style_for(Some((31, 31, 31))),
            )
            .iter()
            .map(plain)
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::remove_dir_all(&root).unwrap();

        assert!(rendered.contains("6 -line 6"), "{rendered}");
        assert!(rendered.contains("6 +six"), "{rendered}");
        assert!(rendered.contains("7  line 7"), "{rendered}");
    }

    #[test]
    fn composer_supports_multiline_editing_and_submission() {
        let mut view = View::new(Path::new("/tmp/bettercodex"));
        for character in "first".chars() {
            assert_eq!(
                view.handle_terminal_event(Event::Key(KeyEvent::new(
                    KeyCode::Char(character),
                    KeyModifiers::NONE,
                ))),
                Action::None
            );
        }
        view.handle_terminal_event(Event::Key(KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::SHIFT,
        )));
        for character in "second".chars() {
            view.handle_terminal_event(Event::Key(KeyEvent::new(
                KeyCode::Char(character),
                KeyModifiers::NONE,
            )));
        }
        let Action::Submit(submission) = view.handle_terminal_event(Event::Key(KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::NONE,
        ))) else {
            panic!("multiline prompt should submit");
        };
        assert_eq!(submission.prompt(), &prompt("first\nsecond"));
    }

    #[test]
    fn ctrl_backspace_deletes_the_previous_word() {
        let mut view = View::new(Path::new("/tmp/bettercodex"));
        view.editor.insert("first second");

        assert_eq!(
            view.handle_terminal_event(Event::Key(KeyEvent::new(
                KeyCode::Backspace,
                KeyModifiers::CONTROL,
            ))),
            Action::None
        );

        assert_eq!(view.editor.text(), "first ");
    }

    #[cfg(windows)]
    #[test]
    fn windows_shortcut_reference_uses_windows_chords() {
        let rendered = shortcut_reference_lines()
            .iter()
            .map(plain)
            .collect::<Vec<_>>()
            .join("\n");

        assert!(rendered.contains("Ctrl+Left / Right"), "{rendered}");
        assert!(rendered.contains("Ctrl+Backspace"), "{rendered}");
        assert!(rendered.contains("Shift+Enter / Ctrl+J"), "{rendered}");
        assert!(
            rendered.contains("terminal-owned clipboard paste"),
            "{rendered}"
        );
        assert!(!rendered.contains("Option+"), "{rendered}");
    }

    #[test]
    fn rendering_survives_tiny_terminal_sizes() {
        for (width, height) in [(1, 1), (2, 3), (4, 4), (7, 6), (20, 8)] {
            let mut view = View::new(Path::new("/tmp/bettercodex"));
            view.welcome_pending = false;
            let backend = TestBackend::new(width, height);
            let mut terminal = Terminal::new(backend).unwrap();
            terminal.draw(|frame| view.render(frame)).unwrap();
        }
    }

    trait PlainLine {
        fn line(&self) -> &Line<'_>;
    }

    impl PlainLine for Line<'_> {
        fn line(&self) -> &Line<'_> {
            self
        }
    }

    impl PlainLine for HyperlinkLine {
        fn line(&self) -> &Line<'_> {
            &self.line
        }
    }

    impl<T: PlainLine + ?Sized> PlainLine for &T {
        fn line(&self) -> &Line<'_> {
            (*self).line()
        }
    }

    fn plain<T: PlainLine>(value: &T) -> String {
        value
            .line()
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }

    fn render_buffer(buffer: &ratatui::buffer::Buffer) -> String {
        let area = buffer.area;
        (area.y..area.bottom())
            .map(|y| {
                (area.x..area.right())
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}
