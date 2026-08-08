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
use super::palette;
use super::palette::TerminalColors;
use super::pending_input::PendingInput;
use super::reasoning_status::ReasoningStatus;
use super::resume_picker::ResumePicker;
use super::resume_picker::ResumePickerAction;
use super::skill_popup::SkillPopup;
use super::skills_view::SkillsView;
use super::skills_view::SkillsViewAction;
use super::terminal_hyperlinks;
use super::terminal_hyperlinks::HyperlinkLine;
use super::tool_catalogue::CatalogueAction;
use super::tool_catalogue::ToolCatalogueView;
use crate::MODEL;
use crate::agent::CompactionOutcome;
use crate::agent::SubmitOutcome;
use crate::ansi_escape::ansi_escape_line;
use crate::assistant_message::AssistantMessage;
use crate::context::ContextSnapshot;
use crate::context::EFFECTIVE_CONTEXT_WINDOW;
use crate::events::AgentEvent;
use crate::events::SteerId;
use crate::input::UserPrompt;
use crate::protocol::MessagePhase;
use crate::protocol::ParsedCommand;
use crate::quality_loop::LoopProgress;
use crate::rollout::SessionTranscriptItem;
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
use ratatui::text::Text;
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
const LOOP_LINE_HEIGHT: u16 = 1;
const LOOP_INDENT: &str = "  ";
const LOOP_SEPARATOR: &str = " · ";
const LOOP_NAME_COLOR: Color = Color::Indexed(245);
const LOOP_FIELD_COLOR: Color = Color::Indexed(243);
const LOOP_SEPARATOR_COLOR: Color = Color::Indexed(240);
const SLASH_COMMANDS: &[SlashCommand] = &[
    SlashCommand {
        name: "loop",
        aliases: &[],
        description: "run a task-specific evaluator and improvement loop",
    },
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
    SlashCommand {
        name: "tmux",
        aliases: &[],
        description: "move this live session into tmux",
    },
    SlashCommand {
        name: "tools",
        aliases: &[],
        description: "inspect the active tool catalogue",
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
    Submit(UserPrompt),
    Queue(UserPrompt),
    Cancel,
    Compact,
    Copy(String),
    Clear,
    Fork,
    ListBackgroundProcesses,
    OpenResumePicker,
    ResumeSession(Uuid),
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
    file_search: FileSearchPopup,
    skill_popup: SkillPopup,
    skills: Vec<Skill>,
    context_tokens: Option<u64>,
    background_processes: Vec<BackgroundProcess>,
    busy: bool,
    action_required: bool,
    interrupting: Option<InterruptIntent>,
    working_since: Option<Instant>,
    turn_had_work: bool,
    reasoning_status: ReasoningStatus,
    status_detail: Option<String>,
    loop_progress: Option<LoopProgress>,
    pending_input: PendingInput,
    terminal_assistant_received_this_turn: bool,
    active_message_phase: Option<MessagePhase>,
    composer_text_width: u16,
    overlay: Option<Overlay>,
    slash_selection: usize,
    dismissed_slash: Option<String>,
    user_message_style: Style,
    process_commands: HashMap<i64, String>,
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
        text: String,
        phase: Option<MessagePhase>,
        streaming: bool,
        rendered: MarkdownRenderCache,
        history: StreamedAssistantHistory,
    },
    Tool(ToolEntry),
    Exploration {
        tools: Vec<ToolEntry>,
        sealed: bool,
    },
    Notice(String),
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

/// Rows from an in-flight assistant cell that have already moved into terminal scrollback.
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
    display: ToolDisplay,
    outcome: Option<ToolOutcome>,
    started_at: Instant,
}

#[derive(Debug)]
enum ToolDisplay {
    Command {
        command: String,
        parsed: Vec<ParsedCommand>,
    },
    Interaction {
        command: String,
        input: String,
    },
    Patch(PatchDisplay),
    Papercut,
    Plan(PlanDisplay),
    ViewImage(String),
    WebSearch(Vec<crate::web_search::WebActivity>),
    Other,
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
    Resume(ResumePicker),
    Skills(SkillsView),
    Tools(ToolCatalogueView),
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
            file_search: FileSearchPopup::default(),
            skill_popup: SkillPopup::default(),
            skills,
            context_tokens: None,
            background_processes: Vec::new(),
            busy: false,
            action_required: false,
            interrupting: None,
            working_since: None,
            turn_had_work: false,
            reasoning_status: ReasoningStatus::default(),
            status_detail: None,
            loop_progress: None,
            pending_input: PendingInput::default(),
            terminal_assistant_received_this_turn: false,
            active_message_phase: None,
            composer_text_width: 1,
            overlay: None,
            slash_selection: 0,
            dismissed_slash: None,
            user_message_style: user_message_style_for(Some((31, 31, 31))),
            process_commands: HashMap::new(),
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

    pub(super) fn seed_prompt_history(&mut self, history: impl IntoIterator<Item = String>) {
        self.editor.seed_history(history);
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
        self.loop_progress = None;
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
        self.file_search.dismiss();
        self.skill_popup.hide();
        self.dismissed_slash = None;
        self.slash_selection = 0;
    }

    pub(super) fn finish_turn(
        &mut self,
        result: anyhow::Result<SubmitOutcome>,
    ) -> Option<UserPrompt> {
        self.close_streaming_entries();
        self.seal_exploration();
        self.finish_incomplete_tools();
        let elapsed_seconds = self
            .working_since
            .take()
            .map(|started| started.elapsed().as_secs());
        let turn_had_work = std::mem::take(&mut self.turn_had_work);
        self.busy = false;
        self.loop_progress = None;
        let interrupt_intent = self.interrupting.take();
        self.reasoning_status.reset();
        self.status_detail = None;
        self.action_required = result.is_err();
        match result {
            Ok(SubmitOutcome::Completed(answer)) => {
                if !self.terminal_assistant_received_this_turn && !answer.trim().is_empty() {
                    self.entries.push(TranscriptEntry::Assistant {
                        text: answer,
                        phase: Some(MessagePhase::FinalAnswer),
                        streaming: false,
                        rendered: MarkdownRenderCache::default(),
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
        self.entries
            .iter()
            .filter_map(|entry| match entry {
                TranscriptEntry::User(prompt) => Some(SessionTranscriptItem::User {
                    text: prompt.model_text.clone(),
                    image_count: prompt.image_count,
                }),
                TranscriptEntry::Assistant {
                    text,
                    phase,
                    streaming: false,
                    ..
                } if !text.trim().is_empty() => Some(SessionTranscriptItem::Assistant {
                    text: text.clone(),
                    phase: phase.clone(),
                }),
                _ => None,
            })
            .collect()
    }

    pub(super) fn show_context(&mut self, snapshot: ContextSnapshot) {
        self.overlay = Some(Overlay::Context(ContextWindowView::new(snapshot)));
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
        self.entries
            .extend(transcript.into_iter().map(|item| match item {
                SessionTranscriptItem::User { text, image_count } => {
                    TranscriptEntry::User(DisplayedUserPrompt::replayed(text, image_count))
                }
                SessionTranscriptItem::Assistant { text, phase } => TranscriptEntry::Assistant {
                    text,
                    phase,
                    streaming: false,
                    rendered: MarkdownRenderCache::default(),
                    history: StreamedAssistantHistory::default(),
                },
            }));
    }

    pub(super) fn switch_session(
        &mut self,
        cwd: &Path,
        context_tokens: Option<u64>,
        transcript: impl IntoIterator<Item = SessionTranscriptItem>,
        prompt_history: impl IntoIterator<Item = String>,
        skills: Vec<Skill>,
    ) {
        let user_message_style = self.user_message_style;
        *self = Self::with_state(cwd, skills);
        self.user_message_style = user_message_style;
        self.context_tokens = context_tokens;
        self.replay_transcript(transcript);
        self.editor.seed_history(prompt_history);
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
        self.terminal_assistant_received_this_turn = false;
        self.active_message_phase = None;
    }

    pub(super) fn handle_agent_event(&mut self, event: AgentEvent) {
        match event {
            AgentEvent::ModelMessageStarted(message) => {
                self.seal_exploration();
                self.close_streaming_entries();
                self.active_message_phase = message.phase;
                if !message.text.is_empty() {
                    self.entries.push(TranscriptEntry::Assistant {
                        text: message.text,
                        phase: self.active_message_phase.clone(),
                        streaming: true,
                        rendered: MarkdownRenderCache::default(),
                        history: StreamedAssistantHistory::default(),
                    });
                }
                self.status_detail = None;
            }
            AgentEvent::ModelMessageDelta(delta) => {
                self.seal_exploration();
                match self.entries.last_mut() {
                    Some(TranscriptEntry::Assistant {
                        text, streaming, ..
                    }) if *streaming => {
                        text.push_str(&delta);
                    }
                    _ => self.entries.push(TranscriptEntry::Assistant {
                        text: delta,
                        phase: self.active_message_phase.clone(),
                        streaming: true,
                        rendered: MarkdownRenderCache::default(),
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
                if entry.is_exploration() {
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
                    self.seal_exploration();
                    self.entries.push(TranscriptEntry::Tool(entry));
                }
            }
            AgentEvent::ToolCompleted {
                call_id,
                output,
                duration: _,
            } => {
                let completed_work = self.find_tool_mut(&call_id).is_some_and(|tool| {
                    let completed_work = matches!(
                        &tool.display,
                        ToolDisplay::Command { .. }
                            | ToolDisplay::Interaction { .. }
                            | ToolDisplay::Patch(_)
                            | ToolDisplay::Papercut
                            | ToolDisplay::WebSearch(_)
                            | ToolDisplay::Other
                    );
                    tool.outcome = Some(ToolOutcome { output });
                    completed_work
                });
                self.turn_had_work |= completed_work;
                self.remember_process_command(&call_id);
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
            AgentEvent::LoopProgress(progress) => self.loop_progress = Some(progress),
            AgentEvent::LoopProgressCleared => self.loop_progress = None,
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
            return Action::None;
        }
        let action = match event {
            Event::Key(key) if matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) => {
                self.handle_key(key)
            }
            Event::Paste(text) if matches!(self.overlay.as_ref(), Some(Overlay::Resume(_))) => {
                if let Some(Overlay::Resume(picker)) = self.overlay.as_mut() {
                    picker.handle_paste(&text);
                }
                Action::None
            }
            Event::Paste(_) if self.overlay.is_some() => Action::None,
            Event::Paste(text) => {
                let text = text.replace("\r\n", "\n").replace('\r', "\n");
                if let Some(image) = clipboard_paste::image_from_pasted_path(&text) {
                    self.attach_image(image);
                } else {
                    self.editor.insert_paste(text);
                }
                self.dismissed_slash = None;
                self.slash_selection = 0;
                Action::None
            }
            Event::Resize(_, _) => {
                self.resize_reflow_requested = true;
                Action::None
            }
            _ => Action::None,
        };
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
        action
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
                ResumePickerAction::Resume(id) => Action::ResumeSession(id),
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
                self.editor.set_text("");
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
        if let Some(overlay) = self.overlay.as_ref() {
            let close = match overlay {
                Overlay::Shortcuts => true,
                Overlay::Context(context) => context.handle_key(key.code) == ContextAction::Close,
                Overlay::Resume(_) => unreachable!("resume picker keys are handled above"),
                Overlay::Skills(_) => unreachable!("skills keys are handled above"),
                Overlay::Tools(catalogue) => {
                    catalogue.handle_key(key.code) == CatalogueAction::Close
                }
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
                    if command.name == "loop" {
                        self.editor.insert(" ");
                        return Action::None;
                    }
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
            KeyCode::Backspace if alt && !control => self.editor.delete_previous_word(),
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
        let Some((query, range)) = self.editor.slash_command_query() else {
            return;
        };
        let name = command.completion_name(&query);
        self.editor.replace_range(range, &format!("/{name}"));
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
        let prompt = self.editor.take_prompt();
        let history_text = prompt.text_without_image_placeholders();
        self.editor.remember(&history_text);
        let command = history_text.trim();
        let local_command = prompt.image_count() == 0;
        if local_command && let Some(shell_command) = command.strip_prefix('!') {
            let shell_command = shell_command.trim();
            if shell_command.is_empty() {
                self.entries.push(TranscriptEntry::Notice(
                    "Run an operator shell command with !command".to_string(),
                ));
                return Action::None;
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
                self.entries.push(TranscriptEntry::Notice(
                    "Interrupt the active turn before resuming another session".to_string(),
                ));
                return Action::None;
            }
            let arguments = arguments.trim();
            if arguments.is_empty() {
                return Action::OpenResumePicker;
            }
            return match Uuid::parse_str(arguments) {
                Ok(id) => Action::ResumeSession(id),
                Err(_) => {
                    self.entries.push(TranscriptEntry::Error(
                        "`/resume` expects one bettercodex session UUID".to_string(),
                    ));
                    Action::None
                }
            };
        }
        if local_command
            && let Some(arguments) = command.strip_prefix("/tmux")
            && (arguments.is_empty() || arguments.starts_with(char::is_whitespace))
        {
            return match arguments.trim() {
                "" => Action::EnterTmux,
                _ => {
                    self.entries.push(TranscriptEntry::Error(
                        "`/tmux` does not accept arguments".to_string(),
                    ));
                    Action::None
                }
            };
        }
        match command {
            _ if !local_command => Action::Submit(prompt),
            "/q" | "/quit" | "/exit" => Action::Quit,
            "/compact" if self.busy => {
                self.entries.push(TranscriptEntry::Error(
                    "'/compact' is disabled while a task is in progress.".to_string(),
                ));
                Action::None
            }
            "/compact" => Action::Compact,
            "/copy" => self.copy_latest_final_action(),
            "/diff" => Action::ShowDiff,
            "/fork" if self.busy => {
                self.entries.push(TranscriptEntry::Notice(
                    "Interrupt the active turn before forking this session".to_string(),
                ));
                Action::None
            }
            "/fork" => Action::Fork,
            "/clear" if self.busy => {
                self.entries.push(TranscriptEntry::Notice(
                    "Interrupt the active turn before starting a fresh session".to_string(),
                ));
                Action::None
            }
            "/clear" => Action::Clear,
            "/context" => Action::ShowContext,
            "/help" => {
                self.overlay = Some(Overlay::Shortcuts);
                Action::None
            }
            "/ps" => Action::ListBackgroundProcesses,
            "/skills" if self.busy => {
                self.entries.push(TranscriptEntry::Error(
                    "'/skills' is disabled while a task is in progress.".to_string(),
                ));
                Action::None
            }
            "/skills" => {
                self.overlay = Some(Overlay::Skills(SkillsView::new()));
                Action::None
            }
            "/tools" => {
                self.overlay = Some(Overlay::Tools(ToolCatalogueView::new()));
                Action::None
            }
            "/stop" => Action::StopBackgroundProcesses,
            "/logout" if self.busy => {
                self.entries.push(TranscriptEntry::Notice(
                    "Interrupt the active turn before logging out".to_string(),
                ));
                Action::None
            }
            "/logout" => Action::Logout,
            _ => Action::Submit(prompt),
        }
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
        let prompt = self.editor.take_prompt();
        self.editor
            .remember(&prompt.text_without_image_placeholders());
        Action::Queue(prompt)
    }

    fn copy_latest_final_action(&mut self) -> Action {
        let markdown = self.entries.iter().rev().find_map(|entry| match entry {
            TranscriptEntry::Assistant {
                phase: Some(MessagePhase::Commentary),
                ..
            } => None,
            TranscriptEntry::Assistant {
                text,
                streaming: false,
                ..
            } if !text.trim().is_empty() => Some(text.clone()),
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
        self.seal_exploration();
        self.terminal_assistant_received_this_turn |=
            message.is_terminal() && !message.text.trim().is_empty();
        match self.entries.last_mut() {
            Some(TranscriptEntry::Assistant {
                text,
                phase,
                streaming,
                rendered,
                ..
            }) if *streaming => {
                *text = message.text;
                *phase = message.phase;
                *streaming = false;
            }
            _ if !message.text.trim().is_empty() => {
                self.entries.push(TranscriptEntry::Assistant {
                    text: message.text,
                    phase: message.phase,
                    streaming: false,
                    rendered: MarkdownRenderCache::default(),
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
        self.entries[self.committed_entries..]
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
            })
    }

    pub(super) fn take_pending_history_lines(&mut self, width: u16) -> Vec<HyperlinkLine> {
        let width = width.max(1);
        let mut lines = Vec::new();
        if self.welcome_pending {
            append_history_cell(
                terminal_hyperlinks::plain_hyperlink_lines(welcome_lines(&self.cwd, width)),
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
    pub(super) fn history_lines_for_resize_reflow(&mut self, width: u16) -> Vec<HyperlinkLine> {
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
            terminal_hyperlinks::plain_hyperlink_lines(welcome_lines(&self.cwd, width)),
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

    /// Move the oldest rendered rows of a growing assistant response into real terminal history.
    ///
    /// Keeping at least one row live prevents an unterminated final line from being committed while
    /// it is still changing. In normal terminals the complete composer/status layout leaves a much
    /// larger mutable tail; only rows that would otherwise be clipped are emitted.
    fn spill_streaming_history(&mut self, width: u16, live_capacity: usize) -> Vec<HyperlinkLine> {
        let history_was_emitted = self.history_emitted;
        let Some(TranscriptEntry::Assistant {
            text,
            streaming: true,
            rendered,
            history,
            ..
        }) = self.entries.get_mut(self.committed_entries)
        else {
            return Vec::new();
        };

        if history.started && history.width != Some(width) {
            return Vec::new();
        }
        let rendered = assistant_lines(text, width, &self.cwd, true, rendered);
        if history.lines.len() > rendered.len() {
            return Vec::new();
        }

        let separator_rows = usize::from(!history.started && history_was_emitted);
        let remaining_rows = rendered.len().saturating_sub(history.lines.len());
        let mut rows_to_spill = separator_rows
            .saturating_add(remaining_rows)
            .saturating_sub(live_capacity);
        if rows_to_spill == 0 {
            return Vec::new();
        }

        let mut output = Vec::with_capacity(rows_to_spill);
        if !history.started {
            history.started = true;
            history.width = Some(width);
            self.history_emitted = true;
            if history_was_emitted {
                output.push(HyperlinkLine::default());
                rows_to_spill = rows_to_spill.saturating_sub(1);
            }
        }

        let start = history.lines.len();
        let end = start.saturating_add(rows_to_spill).min(rendered.len());
        let newly_emitted = rendered[start..end].to_vec();
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
        let activity_height = u16::from(self.has_activity_surface(width));
        let loop_height = LOOP_LINE_HEIGHT.saturating_mul(u16::from(self.loop_progress.is_some()));
        let activity_composer_height = if activity_height > 0 {
            ACTIVITY_COMPOSER_GAP.max(loop_height)
        } else {
            loop_height
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
            Some(Overlay::Resume(_)) => screen_height,
            Some(Overlay::Skills(skills)) => skills.preferred_height(&self.skills, width),
            Some(Overlay::Tools(catalogue)) => catalogue.preferred_height(),
            None => 0,
        };
        let transcript_chrome_height = bottom_spacing
            .saturating_add(pending_height)
            .saturating_add(activity_height)
            .saturating_add(activity_composer_height)
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
        let requested_loop_height =
            LOOP_LINE_HEIGHT.saturating_mul(u16::from(self.loop_progress.is_some()));
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
        let has_activity_surface = self.has_activity_surface(area.width);
        let requested_activity_height = u16::from(has_activity_surface);
        let requested_activity_composer_height = if has_activity_surface {
            ACTIVITY_COMPOSER_GAP.max(requested_loop_height)
        } else {
            requested_loop_height
        };
        let requested_pre_composer_height =
            requested_activity_height.saturating_add(requested_activity_composer_height);
        let pre_composer_height = requested_pre_composer_height
            .min(height_above_trailing.saturating_sub(minimum_composer_height));
        let pending_lines = self.pending_input.lines();
        let requested_pending_height = u16::try_from(pending_lines.len()).unwrap_or(u16::MAX);
        let pending_height = requested_pending_height.min(
            height_above_trailing
                .saturating_sub(pre_composer_height)
                .saturating_sub(minimum_composer_height),
        );
        let composer_height_limit = height_above_trailing
            .saturating_sub(pre_composer_height)
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
        // The loop replaces Codex's quiet activity-to-composer spacer. Keeping it in this
        // interstitial row makes the loop read as nested live activity instead of a second footer.
        let pre_composer_top = composer_y.saturating_sub(pre_composer_height);
        let activity_height = requested_activity_height.min(pre_composer_height);
        let activity_area = Rect::new(area.x, pre_composer_top, area.width, activity_height);
        let interstitial_height = pre_composer_height.saturating_sub(activity_height);
        let loop_height = requested_loop_height.min(interstitial_height);
        let loop_area = Rect::new(
            area.x,
            composer_y.saturating_sub(loop_height),
            area.width,
            loop_height,
        );
        let pending_bottom = if pre_composer_height > 0 {
            pre_composer_top
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
        } else if pre_composer_height > 0 {
            pre_composer_top
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
            let line = if self.busy {
                self.working_line()
            } else {
                self.standalone_background_process_line(area.width)
                    .expect("an idle activity surface requires background processes")
            };
            frame.render_widget(
                Paragraph::new(truncate_line(line, usize::from(activity_area.width))),
                activity_area,
            );
        }
        if let Some(progress) = &self.loop_progress
            && !loop_area.is_empty()
        {
            frame.render_widget(
                Paragraph::new(loop_status_line(progress, loop_area.width)),
                loop_area,
            );
        }
        self.render_composer(frame, composer_area, footer_area, editor_layout);
        if self.overlay.is_none() {
            self.render_completion_popup(frame, popup_area);
        }
        match self.overlay.as_ref() {
            Some(Overlay::Shortcuts) => self.render_shortcuts(frame, area),
            Some(Overlay::Context(context)) => context.render(frame, area, self.user_message_style),
            Some(Overlay::Resume(picker)) => picker.render(frame, area),
            Some(Overlay::Skills(skills)) => {
                skills.render(frame, area, &self.skills, self.user_message_style)
            }
            Some(Overlay::Tools(catalogue)) => {
                catalogue.render(frame, area, self.user_message_style)
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
        let paragraph = Paragraph::new(Text::from(terminal_hyperlinks::visible_lines_ref(&lines)))
            .wrap(Wrap { trim: false });
        let overflow = usize::from(active_height).saturating_sub(usize::from(area.height));
        frame.render_widget(
            paragraph.scroll((u16::try_from(overflow).unwrap_or(u16::MAX), 0)),
            area,
        );
        terminal_hyperlinks::mark_buffer_hyperlinks(frame.buffer_mut(), area, &lines, overflow);
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
        if self.dismissed_slash.as_deref() == Some(self.editor.text()) {
            return Vec::new();
        }
        let Some((query, _)) = self.editor.slash_command_query() else {
            return Vec::new();
        };
        SLASH_COMMANDS
            .iter()
            .filter(|command| command.matches(&query))
            .collect()
    }

    fn working_line(&self) -> Line<'static> {
        let elapsed = self
            .working_since
            .map(|started| started.elapsed())
            .unwrap_or_default();
        let heading = if self.interrupting.is_some() {
            "Interrupting"
        } else if self.status_detail.as_deref() == Some("Compacting conversation") {
            "Compacting"
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
        if let Some(detail) = self
            .status_detail
            .as_deref()
            .filter(|detail| *detail != "Compacting conversation")
        {
            spans.push(Span::from(" · ").dim());
            spans.push(Span::from(detail.to_string()).dim());
        }
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
        if let Some(summary) = self.background_process_summary() {
            spans.push(Span::from(" · ").dim());
            spans.push(Span::from(summary).dim());
        }
        Line::from(spans)
    }
}

fn loop_status_line(progress: &LoopProgress, width: u16) -> Line<'static> {
    let width = usize::from(width);
    if width == 0 {
        return Line::default();
    }
    let phase_width = UnicodeWidthStr::width(progress.phase.as_str());
    let preferred_indent_width = UnicodeWidthStr::width(LOOP_INDENT);
    let indent = if width >= phase_width.saturating_add(preferred_indent_width) {
        LOOP_INDENT
    } else {
        ""
    };
    let content_width = width.saturating_sub(UnicodeWidthStr::width(indent));
    let diff = progress
        .additions
        .zip(progress.deletions)
        .map(|(additions, deletions)| format!("+{additions} −{deletions}"));
    if let Some(diff) = diff.as_deref() {
        let fields = [
            progress.name.as_str(),
            progress.phase.as_str(),
            diff,
            progress.pulse.as_str(),
        ];
        if loop_fields_width(&fields) <= content_width {
            return styled_loop_fields(&fields, indent, true);
        }
    }
    let fields = [
        progress.name.as_str(),
        progress.phase.as_str(),
        progress.pulse.as_str(),
    ];
    if loop_fields_width(&fields) <= content_width {
        return styled_loop_fields(&fields, indent, true);
    }

    let fixed = loop_fields_width(&[progress.phase.as_str(), progress.pulse.as_str()])
        .saturating_add(UnicodeWidthStr::width(LOOP_SEPARATOR));
    if content_width > fixed {
        let name = crate::quality_loop::truncate_width(
            &progress.name,
            content_width.saturating_sub(fixed),
        );
        if !name.is_empty() {
            let fields = [
                name.as_str(),
                progress.phase.as_str(),
                progress.pulse.as_str(),
            ];
            if loop_fields_width(&fields) <= content_width {
                return styled_loop_fields(&fields, indent, true);
            }
        }
    }

    let separator_width = UnicodeWidthStr::width(LOOP_SEPARATOR);
    if content_width > phase_width.saturating_add(separator_width) {
        let pulse = crate::quality_loop::truncate_width(
            &progress.pulse,
            content_width
                .saturating_sub(phase_width)
                .saturating_sub(separator_width),
        );
        if !pulse.is_empty() {
            return styled_loop_fields(&[progress.phase.as_str(), pulse.as_str()], indent, false);
        }
    }
    let phase = crate::quality_loop::truncate_width(&progress.phase, content_width);
    styled_loop_fields(&[phase.as_str()], indent, false)
}

fn loop_fields_width(fields: &[&str]) -> usize {
    fields
        .iter()
        .map(|field| UnicodeWidthStr::width(*field))
        .sum::<usize>()
        .saturating_add(
            fields
                .len()
                .saturating_sub(1)
                .saturating_mul(UnicodeWidthStr::width(LOOP_SEPARATOR)),
        )
}

fn styled_loop_fields(fields: &[&str], indent: &str, first_is_name: bool) -> Line<'static> {
    let mut spans = Vec::with_capacity(fields.len().saturating_mul(2));
    if !indent.is_empty() {
        spans.push(Span::from(indent.to_string()));
    }
    for (index, field) in fields.iter().enumerate() {
        if index > 0 {
            spans.push(Span::styled(
                LOOP_SEPARATOR,
                Style::default().fg(LOOP_SEPARATOR_COLOR),
            ));
        }
        let color = if index == 0 && first_is_name {
            LOOP_NAME_COLOR
        } else {
            LOOP_FIELD_COLOR
        };
        spans.push(Span::styled(
            (*field).to_string(),
            Style::default().fg(color),
        ));
    }
    Line::from(spans)
}

impl View {
    fn has_activity_surface(&self, width: u16) -> bool {
        self.busy || (width >= 4 && !self.background_processes.is_empty())
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

    fn standalone_background_process_line(&self, width: u16) -> Option<Line<'static>> {
        if width < 4 {
            return None;
        }
        let summary = self.background_process_summary()?;
        Some(Line::from(vec![
            Span::from("  ").dim(),
            Span::from(summary).dim(),
        ]))
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
                editor::HistorySearchStatus::NoMatch => spans.push("  no match".red()),
            }
            return truncate_line(Line::from(spans), usize::from(width));
        }
        let mut spans = vec![
            Span::from(MODEL),
            Span::styled(" max", Style::default().fg(MUTED)),
            Span::styled(" │ ", Style::default().fg(MUTED)),
            Span::styled(
                self.repository.name.clone(),
                Style::default().fg(Color::Cyan),
            ),
        ];
        if let Some(branch) = &self.repository.branch {
            spans.push(Span::styled(
                format!(" / {branch}"),
                Style::default().fg(MUTED),
            ));
        }
        spans.push(Span::styled(" │ ", Style::default().fg(MUTED)));
        spans.push(Span::styled(
            format_context_usage(self.context_tokens),
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
    if let Some(arguments) = command.strip_prefix("/tmux")
        && (arguments.is_empty() || arguments.starts_with(char::is_whitespace))
    {
        return true;
    }
    matches!(
        command,
        "/q" | "/quit"
            | "/exit"
            | "/compact"
            | "/copy"
            | "/diff"
            | "/clear"
            | "/fork"
            | "/context"
            | "/help"
            | "/ps"
            | "/skills"
            | "/tools"
            | "/stop"
    )
}

impl TranscriptEntry {
    fn is_finalized(&self) -> bool {
        match self {
            Self::User(_)
            | Self::Notice(_)
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
                text,
                streaming,
                rendered,
                history,
                ..
            } if history.started && history.width == Some(width) => {
                let rendered_lines = assistant_lines(text, width, cwd, *streaming, rendered);
                let start = history.lines.len().min(rendered_lines.len());
                let output = rendered_lines[start..].to_vec();
                (output, true)
            }
            _ => (self.display_lines(width, user_style, cwd), false),
        }
    }

    fn streamed_history_needs_reflow(&mut self, width: u16, cwd: &Path) -> bool {
        let Self::Assistant {
            text,
            streaming,
            rendered,
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
        !assistant_lines(text, width, cwd, false, rendered).starts_with(&history.lines)
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
                text,
                streaming,
                rendered,
                ..
            } => return assistant_lines(text, width, cwd, *streaming, rendered),
            Self::Tool(tool) => tool.display_lines(width, user_style),
            Self::Exploration { tools, .. } => exploration_lines(tools, width),
            Self::Notice(message) => vec![Line::from(vec![
                Span::from("• ").dim(),
                Span::from(message.clone()).dim(),
            ])],
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
            "web.run" => ToolDisplay::WebSearch(crate::web_search::activities_for_display(input)),
            _ => ToolDisplay::Other,
        };
        Self {
            call_id,
            name,
            display,
            outcome: None,
            started_at: Instant::now(),
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
            ToolDisplay::WebSearch(_) => true,
            _ => false,
        }
    }

    fn activity_label(&self) -> String {
        match &self.display {
            ToolDisplay::Command { command, .. } => first_display_line(command),
            ToolDisplay::Interaction { command, .. } => command.clone(),
            ToolDisplay::Patch(_) => "Applying patch".to_string(),
            ToolDisplay::Papercut => "Logging papercut".to_string(),
            ToolDisplay::Plan(_) => "Updating plan".to_string(),
            ToolDisplay::ViewImage(path) => format!("Viewing {path}"),
            ToolDisplay::WebSearch(_) => "Searching the web".to_string(),
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
            ToolDisplay::Command { command, .. } => command_lines(self, command, width),
            ToolDisplay::Interaction { command, input } => {
                interaction_lines(self, command, input, width)
            }
            ToolDisplay::Patch(patch) => {
                patch.display_lines(self.outcome.as_ref(), width, user_style)
            }
            ToolDisplay::Papercut => papercut_lines(self, width),
            ToolDisplay::Plan(plan) => plan.display_lines(self.outcome.as_ref(), width),
            ToolDisplay::ViewImage(path) => view_image_lines(self.outcome.as_ref(), path, width),
            ToolDisplay::WebSearch(_) => exploration_lines(std::slice::from_ref(self), width),
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

fn welcome_lines(cwd: &Path, available_width: u16) -> Vec<Line<'static>> {
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
    content.push(Line::from(vec![
        Span::from("model:       ").dim(),
        Span::from(MODEL),
        Span::from(" max").dim(),
    ]));
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
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
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
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
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
    message: &str,
    width: u16,
    cwd: &Path,
    streaming: bool,
    cache: &mut MarkdownRenderCache,
) -> Vec<HyperlinkLine> {
    let content_width = usize::from(width.saturating_sub(2).max(1));
    terminal_hyperlinks::prefix_hyperlink_lines(
        cache.render(message, content_width, cwd, streaming),
        Span::from("• ").dim(),
        Span::from("  "),
    )
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
    tool: &ToolEntry,
    command: &str,
    input: &str,
    width: u16,
) -> Vec<Line<'static>> {
    let bullet = match tool.succeeded() {
        None => activity_marker(Some(tool.started_at)),
        Some(true) => "•".green().bold(),
        Some(false) => "•".red().bold(),
    };
    let detail = if input.is_empty() {
        format!("Waited for `{command}`")
    } else {
        format!(
            "Interacted with `{command}`, sent `{}`",
            summarize_interaction_input(input)
        )
    };
    let rows = editor::wrap_text(&detail, width.saturating_sub(4).max(1));
    let mut lines = rows
        .into_iter()
        .enumerate()
        .map(|(index, row)| {
            if index == 0 {
                Line::from(vec![bullet.clone(), " ".into(), Span::from(row)])
            } else {
                Line::from(vec!["  │ ".dim(), Span::from(row)])
            }
        })
        .collect::<Vec<_>>();
    if let Some(outcome) = &tool.outcome {
        match &outcome.output {
            Ok(output) => {
                if let Some(output) = output.get("output").and_then(Value::as_str)
                    && !output.is_empty()
                {
                    append_bounded_output(output, width, &mut lines);
                }
            }
            Err(error) => append_bounded_output(error, width, &mut lines),
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
            ToolDisplay::WebSearch(activities) => {
                flush_reads(&mut details, &mut read_names);
                for activity in activities {
                    let spans = if activity.detail.is_empty() {
                        Vec::new()
                    } else {
                        vec![Span::from(activity.detail.clone())]
                    };
                    details.push((activity.verb, detail_style, spans));
                }
                if let Some(ToolOutcome { output: Err(error) }) = &tool.outcome {
                    details.push((
                        "Error",
                        Style::default().fg(Color::Red),
                        vec![Span::from(first_display_line(error)).red()],
                    ));
                }
            }
            _ => {}
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
    if lines.is_empty() {
        return 0;
    }
    Paragraph::new(Text::from(terminal_hyperlinks::visible_lines_ref(lines)))
        .wrap(Wrap { trim: false })
        .line_count(width.max(1))
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
    supports_color::on_cached(supports_color::Stream::Stdout).is_some_and(|level| level.has_16m)
}

fn blend(foreground: (u8, u8, u8), background: (u8, u8, u8), alpha: f32) -> (u8, u8, u8) {
    (
        (foreground.0 as f32 * alpha + background.0 as f32 * (1.0 - alpha)) as u8,
        (foreground.1 as f32 * alpha + background.1 as f32 * (1.0 - alpha)) as u8,
        (foreground.2 as f32 * alpha + background.2 as f32 * (1.0 - alpha)) as u8,
    )
}

fn summarize_interaction_input(input: &str) -> String {
    let sanitized = input.replace('\n', "\\n").replace('`', "\\`");
    let mut characters = sanitized.chars();
    let preview = characters.by_ref().take(80).collect::<String>();
    if characters.next().is_some() {
        format!("{preview}...")
    } else {
        preview
    }
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

fn format_context_usage(tokens: Option<u64>) -> String {
    let context_window_k = EFFECTIVE_CONTEXT_WINDOW / 1_000;
    let Some(tokens) = tokens else {
        return format!("? of {context_window_k}K");
    };
    let percent = (tokens as f64 / EFFECTIVE_CONTEXT_WINDOW as f64 * 100.0).clamp(0.0, 100.0);
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
    vec![
        Line::from("Keyboard shortcuts").bold(),
        Line::default(),
        shortcut_line("Enter", "submit prompt"),
        shortcut_line("Enter while working", "steer after current model step"),
        shortcut_line("Tab while working", "queue a follow-up turn"),
        shortcut_line("Alt+Up / Shift+Left", "edit last queued follow-up"),
        shortcut_line("Option+Left / Right", "jump by word"),
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
        shortcut_line("Option+Backspace", "delete previous word (Ctrl+W too)"),
        shortcut_line("Ctrl+C", "clear draft, interrupt work, or exit when idle"),
    ]
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
    fn review_completion_submits_the_explicit_review_command() {
        let mut view = View::new(Path::new("/tmp/bettercodex"));
        view.editor.set_text("/rev");

        let Action::Submit(prompt) = view.handle_terminal_event(Event::Key(KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::NONE,
        ))) else {
            panic!("review completion should submit a turn");
        };
        assert_eq!(prompt.text_without_image_placeholders(), "/review");
    }

    fn completed_message(text: impl Into<String>) -> AgentEvent {
        AgentEvent::ModelMessageCompleted(AssistantMessage {
            text: text.into(),
            phase: Some(MessagePhase::FinalAnswer),
        })
    }

    #[test]
    fn context_usage_matches_the_preserved_contract() {
        assert_eq!(format_context_usage(None), "? of 258K");
        assert_eq!(format_context_usage(Some(1_000)), "0.4% of 258K");
        assert_eq!(format_context_usage(Some(51_680)), "20% of 258K");
        assert_eq!(format_context_usage(Some(u64::MAX)), "100% of 258K");
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
            "gpt-5.6-sol max │ pi / main │ 20% of 258K"
        );
    }

    #[test]
    fn initial_viewport_contains_only_the_codex_composer() {
        let mut view = View::new(Path::new("/tmp/bettercodex"));
        assert_eq!(view.desired_height(88, 24), 5);
        let history = view.take_pending_history_lines(88);
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
        assert!(rendered.contains("gpt-5.6-sol max"));
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
        let lines = welcome_lines(Path::new("/tmp/bettercodex"), 100);
        let widest_content = lines[1..lines.len() - 1]
            .iter()
            .map(line_width)
            .max()
            .unwrap();
        assert_eq!(line_width(&lines[0]), widest_content);
        assert!(line_width(&lines[0]) < 60);
    }

    #[test]
    fn resize_reflow_rebuilds_finalized_history_from_source() {
        let mut view = View::new(Path::new("/tmp/bettercodex"));
        assert!(
            view.take_pending_history_lines(80)
                .iter()
                .any(|line| plain(line).contains("bettercodex"))
        );
        view.start_turn("a user message that wraps at the narrower width");
        let _ = view.take_pending_history_lines(80);
        view.handle_agent_event(AgentEvent::ModelMessageDelta("assistant reply".to_string()));
        view.handle_agent_event(completed_message("assistant reply"));
        let _ = view.take_pending_history_lines(80);

        assert_eq!(
            view.handle_terminal_event(Event::Resize(32, 18)),
            Action::None
        );
        assert!(view.take_resize_reflow_request());
        let replay = view
            .history_lines_for_resize_reflow(32)
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

        let lines = view.take_pending_history_lines(40);
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
            .take_pending_history_lines(80)
            .iter()
            .map(plain)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(rendered.contains("• Ran cargo test"));
        assert!(rendered.contains("  └ 21 passed"));
        assert!(!rendered.contains("exec ·"));
    }

    #[test]
    fn write_stdin_uses_codex_interaction_cell_and_keeps_output() {
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
            .take_pending_history_lines(80)
            .iter()
            .map(plain)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            rendered.contains("• Interacted with `bash`, sent `echo ready\\n`"),
            "{rendered}"
        );
        assert!(rendered.contains("  └ ready"), "{rendered}");
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
            .take_pending_history_lines(80)
            .iter()
            .map(plain)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(rendered.contains("• Explored"), "{rendered}");
        assert!(rendered.contains("Read main.rs"), "{rendered}");
        assert!(!rendered.contains("fn main"), "{rendered}");
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
    fn patch_hunk_rows_use_source_line_numbers() {
        let root = std::env::temp_dir().join(format!("bcodex-tui-{}", crate::new_uuid()));
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
        assert_eq!(
            view.handle_terminal_event(Event::Key(KeyEvent::new(
                KeyCode::Enter,
                KeyModifiers::NONE,
            ))),
            Action::Submit(prompt("first\nsecond"))
        );
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
