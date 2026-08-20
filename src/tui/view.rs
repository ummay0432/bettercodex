use super::ask_user_question::AskUserQuestionCard;
use super::ask_user_question::AskUserQuestionCardAction;
use super::bottom_pane::selection_popup_common::MAX_POPUP_ROWS;
use super::bottom_pane::selection_popup_common::measure_text_height;
use super::bottom_pane::selection_popup_common::menu_surface_padding_height;
use super::bottom_pane::selection_popup_common::render_menu_surface;
use super::clipboard_paste;
use super::context_window::ContextAction;
use super::context_window::ContextWindowView;
use super::context_window::format_tokens;
use super::editor;
use super::editor::Editor;
use super::file_change;
use super::file_search::FileSearchPopup;
use super::file_search::FileSearchUpdate;
use super::file_search::is_horizontal_whitespace;
use super::markdown;
use super::markdown_cache::MarkdownRenderCache;
use super::model_picker::ModelPicker;
use super::model_picker::ModelPickerAction;
use super::palette;
use super::palette::TerminalColors;
use super::pending_input::PendingInput;
use super::presentation::AssistantPresentation;
#[cfg(test)]
use super::presentation::MIN_FRAME_INTERVAL;
use super::presentation::item_reveal_budget;
use super::resume_picker::ResumePicker;
use super::resume_picker::ResumePickerAction;
use super::skill_popup::SkillPopup;
use super::skills_view::SkillsView;
use super::skills_view::SkillsViewAction;
use super::startup_art;
use super::status::StatusSnapshot;
use super::terminal_hyperlinks;
use super::terminal_hyperlinks::HyperlinkLine;
use super::tools_view::ToolsAction;
use super::tools_view::ToolsView;
use crate::agent::CompactionOutcome;
use crate::agent::SubmitOutcome;
use crate::ansi_escape::ansi_escape_line;
use crate::ask_user_question::AskUserQuestionArgs;
use crate::ask_user_question::AskUserQuestionResponse;
use crate::assistant_message::AssistantMessage;
use crate::assistant_message::with_citation_sources;
use crate::context::ContextSnapshot;
use crate::events::AgentEvent;
#[cfg(test)]
use crate::events::ModelTextDelta;
use crate::events::SteerId;
use crate::input::UserPrompt;
use crate::input::file_attachment_text;
use crate::model::ModelSelection;
#[cfg(test)]
use crate::protocol::FileChange;
use crate::protocol::MessagePhase;
use crate::protocol::ParsedCommand;
use crate::protocol::ToolFileChange;
use crate::rollout::SessionTranscriptItem;
use crate::rollout::SessionTranscriptTool;
use crate::rollout::SessionTranscriptToolOrigin;
use crate::rollout::SessionTranscriptToolOutput;
use crate::service_tier::ServiceTier;
use crate::shell_command::is_only_plain_ripgrep_script;
use crate::shell_command::parse_command::parse_command;
use crate::skills::Skill;
use crate::skills::SkillSelection;
use crate::skills::SkillUpdate;
use crate::tui::render::highlight::highlight_bash_to_lines;
use crate::tui::render::line_utils::line_to_static;
use crate::tui::width::display_width;
use crate::tui::width::line_width;
use crate::tui::wrapping::RtOptions;
use crate::tui::wrapping::adaptive_wrap_line;
use crate::tui::wrapping::line_contains_url_like;
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
use std::collections::VecDeque;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::sync::OnceLock;
use std::time::Duration;
use std::time::Instant;
use unicode_segmentation::UnicodeSegmentation;
use uuid::Uuid;

const MUTED: Color = Color::Indexed(245);
const RULE: Color = Color::Indexed(8);
const LIVE_PREFIX_COLS: u16 = 2;
const TOOL_OUTPUT_MAX_ROWS: usize = 5;
const OPERATOR_OUTPUT_MAX_ROWS: usize = 50;
const COMMAND_CONTINUATION_MAX_ROWS: usize = 2;
const LIVE_TOOL_OUTPUT_MAX_BYTES: usize = 128 * 1024;
const PENDING_INPUT_GAP: u16 = 1;
const ACTIVITY_COMPOSER_GAP: u16 = 1;
const COMPOSER_FOOTER_GAP: u16 = 0;
const STATUS_LINE_HEIGHT: u16 = 1;
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
        name: "tools",
        aliases: &[],
        description: "inspect available tools and their context cost",
    },
    SlashCommand {
        name: "status",
        aliases: &[],
        description: "show current session configuration and token usage",
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
        name: "logout",
        aliases: &[],
        description: "log out of bettercodex",
    },
    SlashCommand {
        name: "quit",
        aliases: &["exit", "q"],
        description: "leave bettercodex",
    },
];

#[derive(Debug, PartialEq, Eq)]
pub(super) enum Action {
    None,
    LoadPromptHistory,
    Submit(ComposerSubmission),
    Cancel,
    Compact,
    ToggleFast,
    SelectModel(ModelSelection),
    Copy(String),
    Clear(ComposerSubmission),
    Fork(ComposerSubmission),
    OpenResumePicker(ComposerSubmission),
    CloseResumePicker,
    CancelResumeLoad,
    ResumeSessionFromPicker(Uuid),
    ResumeSessionFromComposer {
        id: Uuid,
        submission: ComposerSubmission,
    },
    RunShellCommand {
        command: String,
        history_text: String,
    },
    ShowContext,
    ShowStatus,
    ShowDiff,
    EnterTmux,
    Logout,
    UpdateSkill {
        path: PathBuf,
        update: SkillUpdate,
    },
    ResolveAskUserQuestion {
        call_id: String,
        response: AskUserQuestionResponse,
    },
    Quit,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct ComposerSubmission {
    prompt: UserPrompt,
    draft: editor::EditorSnapshot,
}

struct SubmittedPrompt {
    prompt: UserPrompt,
    entry: usize,
}

impl ComposerSubmission {
    fn take(editor: &mut Editor) -> Self {
        let draft = editor.snapshot();
        let prompt = editor.take_prompt();
        Self { prompt, draft }
    }

    fn remember(&self, editor: &mut Editor) {
        editor.remember_snapshot(&self.draft);
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
    EditPrompt,
    SubmitSteering,
}

pub(super) struct View {
    cwd: PathBuf,
    repository: Repository,
    repository_refresh_pending: bool,
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
    ask_user_question_enabled: bool,
    model_selection: ModelSelection,
    service_tier: ServiceTier,
    busy: bool,
    action_required: bool,
    interrupting: Option<InterruptIntent>,
    working_since: Option<Instant>,
    turn_had_work: bool,
    submitted_prompt: Option<SubmittedPrompt>,
    assistant_presentation: AssistantPresentation,
    deferred_agent_events: VecDeque<QueuedAgentEvent>,
    compacting: bool,
    pending_input: PendingInput,
    terminal_assistant_received_this_turn: bool,
    active_message_phase: Option<MessagePhase>,
    composer_text_width: u16,
    overlay: Option<Overlay>,
    slash_selection: Option<usize>,
    dismissed_slash: Option<String>,
    user_message_style: Style,
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
    WebSearch(WebSearchEntry),
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
    Status(Box<StatusSnapshot>),
    FinalMessageSeparator {
        elapsed_seconds: Option<u64>,
    },
}

#[derive(Debug)]
struct QueuedAgentEvent {
    event: AgentEvent,
    enqueued_at: Instant,
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
            model_text: prompt.text_without_image_placeholders().into_owned(),
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum WebSearchState {
    Active,
    Completed,
    Failed,
    Interrupted,
}

#[derive(Debug)]
struct WebSearchEntry {
    search: crate::web_search::WebSearchCall,
    state: WebSearchState,
    started_at: Instant,
}

#[derive(Debug)]
struct ToolEntry {
    call_id: String,
    origin: ToolOrigin,
    name: String,
    input: Option<Value>,
    display: ToolDisplay,
    outcome: Option<ToolOutcome>,
    empty_ripgrep_result: bool,
    recovery: Option<String>,
    file_change: Option<ToolFileChange>,
    live_output: String,
    command_output_cache: Option<CommandOutputRenderCache>,
    started_at: Instant,
}

#[derive(Debug)]
struct CommandOutputRenderCache {
    width: u16,
    lines: Vec<Line<'static>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ToolOrigin {
    Agent,
    Operator,
}

#[derive(Debug)]
enum ToolDisplay {
    Command {
        command: String,
        parsed: Vec<ParsedCommand>,
    },
    Read(String),
    FileChange {
        path: String,
        action: &'static str,
    },
    AskUserQuestion {
        questions: usize,
    },
    Other,
}

#[derive(Debug)]
struct ToolOutcome {
    output: Result<Value, String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ToolStatus {
    Active,
    Succeeded,
    Failed,
    Recovered,
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
    Tools {
        view: ToolsView,
        parent: Option<Box<ContextWindowView>>,
    },
    Model(Box<ModelPicker>),
    Resume(ResumePicker),
    Skills(SkillsView),
    AskUserQuestion(Box<AskUserQuestionCard>),
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
            repository_refresh_pending: false,
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
            ask_user_question_enabled: false,
            model_selection: ModelSelection::default(),
            service_tier: ServiceTier::default(),
            busy: false,
            action_required: false,
            interrupting: None,
            working_since: None,
            turn_had_work: false,
            submitted_prompt: None,
            assistant_presentation: AssistantPresentation::default(),
            deferred_agent_events: VecDeque::new(),
            compacting: false,
            pending_input: PendingInput::default(),
            terminal_assistant_received_this_turn: false,
            active_message_phase: None,
            composer_text_width: 1,
            overlay: None,
            slash_selection: None,
            dismissed_slash: None,
            user_message_style: user_message_style_for(Some((31, 31, 31))),
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

    pub(super) fn refresh_skills(&mut self, skills: &[Skill]) {
        if self.skills != skills {
            self.set_skills(skills.to_vec());
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

    pub(super) fn start_turn(&mut self, prompt: &UserPrompt) {
        self.seal_exploration();
        self.submitted_prompt = Some(SubmittedPrompt {
            prompt: prompt.clone(),
            entry: self.entries.len(),
        });
        self.entries
            .push(TranscriptEntry::User(DisplayedUserPrompt::from_prompt(
                prompt,
            )));
        self.busy = true;
        self.action_required = false;
        self.interrupting = None;
        self.working_since = Some(Instant::now());
        self.turn_had_work = false;
        self.assistant_presentation.clear();
        self.deferred_agent_events.clear();
        self.compacting = false;
        self.terminal_assistant_received_this_turn = false;
        self.active_message_phase = None;
    }

    pub(super) fn start_compaction(&mut self) {
        self.seal_exploration();
        self.submitted_prompt = None;
        self.context_tokens = None;
        self.busy = true;
        self.action_required = false;
        self.interrupting = None;
        self.working_since = Some(Instant::now());
        self.turn_had_work = false;
        self.assistant_presentation.clear();
        self.deferred_agent_events.clear();
        self.compacting = true;
        self.terminal_assistant_received_this_turn = false;
        self.active_message_phase = None;
    }

    pub(super) fn add_user_message(&mut self, prompt: &UserPrompt) {
        self.submitted_prompt = None;
        self.close_streaming_entries();
        self.seal_exploration();
        self.entries
            .push(TranscriptEntry::User(DisplayedUserPrompt::from_prompt(
                prompt,
            )));
        self.compacting = false;
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

    pub(super) fn can_edit_submitted_prompt(&self) -> bool {
        self.busy
            && self.submitted_prompt.is_some()
            && self.editor.is_empty()
            && !self.pending_input.has_steers()
            && !self.pending_input.has_follow_ups()
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

    pub(super) fn restore_composer_submission(&mut self, submission: ComposerSubmission) {
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
        self.slash_selection = None;
    }

    pub(super) fn finish_turn(
        &mut self,
        result: anyhow::Result<SubmitOutcome>,
    ) -> Option<Vec<(SteerId, UserPrompt)>> {
        self.flush_presentation();
        self.refresh_repository_if_pending();
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
        self.compacting = false;
        self.action_required = result.is_err();
        if matches!(&result, Ok(SubmitOutcome::CancelledBeforeProcessing))
            && let Some(submitted) = self.submitted_prompt.take()
        {
            let mut prompts = vec![submitted.prompt];
            prompts.extend(self.pending_input.take_all());
            if submitted.entry < self.entries.len() {
                self.entries.remove(submitted.entry);
                if submitted.entry < self.committed_entries {
                    self.committed_entries -= 1;
                }
            }
            self.restore_prompts_to_composer(prompts);
            self.resize_reflow_requested = true;
            return None;
        }
        self.submitted_prompt = None;
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
            Ok(SubmitOutcome::Cancelled | SubmitOutcome::CancelledBeforeProcessing) => {
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
                    Some(steers)
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
        self.submitted_prompt = None;
        self.assistant_presentation.clear();
        self.deferred_agent_events.clear();
        self.compacting = false;
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
        }
    }

    pub(super) fn set_context_tokens(&mut self, tokens: Option<u64>) {
        self.context_tokens = tokens;
    }

    pub(super) fn set_ask_user_question_enabled(&mut self, enabled: bool) {
        self.ask_user_question_enabled = enabled;
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
        self.submitted_prompt = None;
        // A terminal action is an explicit transcript boundary. Materialize agent events already
        // accepted by the view before inserting the local command so replay preserves their order.
        self.flush_presentation();
        self.close_streaming_entries();
        self.seal_exploration();
        self.entries.push(TranscriptEntry::Tool(ToolEntry::operator(
            call_id, command, &self.cwd,
        )));
    }

    pub(super) fn append_operator_command_output(&mut self, call_id: &str, chunk: &str) {
        if let Some(tool) = self.find_tool_mut(call_id)
            && tool.origin == ToolOrigin::Operator
        {
            tool.append_live_output(chunk);
        }
    }

    pub(super) fn finish_operator_command(&mut self, call_id: &str, output: Result<Value, String>) {
        if let Some(tool) = self.find_tool_mut(call_id)
            && tool.origin == ToolOrigin::Operator
        {
            tool.set_outcome(output);
        }
        self.repository = Repository::discover(&self.cwd);
        self.repository_refresh_pending = false;
    }

    fn has_active_task(&self) -> bool {
        self.busy || self.has_running_operator_command()
    }

    fn has_running_operator_command(&self) -> bool {
        self.entries.iter().any(|entry| {
            matches!(
                entry,
                TranscriptEntry::Tool(ToolEntry {
                    origin: ToolOrigin::Operator,
                    outcome: None,
                    ..
                })
            )
        })
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
                TranscriptEntry::User(_)
                | TranscriptEntry::WebSearch(_)
                | TranscriptEntry::Tool(_) => true,
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
                            citations: content.citations().to_vec(),
                        }
                    }
                    TranscriptEntry::WebSearch(search) => SessionTranscriptItem::WebSearch {
                        search: search.search.clone(),
                    },
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
        self.ask_user_question_enabled = snapshot.ask_user_question_enabled;
        self.overlay = Some(Overlay::Context(ContextWindowView::new(
            snapshot,
            self.model_selection.model.clone(),
        )));
    }

    pub(super) fn show_ask_user_question(
        &mut self,
        call_id: String,
        arguments: AskUserQuestionArgs,
    ) {
        self.overlay = Some(Overlay::AskUserQuestion(Box::new(
            AskUserQuestionCard::new(call_id, arguments),
        )));
        self.action_required = true;
    }

    pub(super) fn dismiss_ask_user_question(&mut self, call_id: &str) {
        if matches!(
            self.overlay.as_ref(),
            Some(Overlay::AskUserQuestion(card)) if card.call_id() == call_id
        ) {
            self.overlay = None;
        }
    }

    pub(super) fn add_status(&mut self, snapshot: StatusSnapshot) {
        self.seal_exploration();
        self.entries
            .push(TranscriptEntry::Status(Box::new(snapshot)));
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
                SessionTranscriptItem::Assistant {
                    text,
                    phase,
                    citations,
                } => {
                    replaying_legacy_exploration = false;
                    let mut content = MarkdownRenderCache::new(text);
                    content.set_citations(citations);
                    self.entries.push(TranscriptEntry::Assistant {
                        content,
                        phase,
                        streaming: false,
                        history: StreamedAssistantHistory::default(),
                    });
                }
                SessionTranscriptItem::WebSearch { search } => {
                    replaying_legacy_exploration = false;
                    self.entries
                        .push(TranscriptEntry::WebSearch(WebSearchEntry::finished(search)));
                }
                SessionTranscriptItem::Tool { tool } => {
                    replaying_legacy_exploration =
                        self.replay_tool(tool, replaying_legacy_exploration);
                }
                SessionTranscriptItem::Exploration { tools } => {
                    replaying_legacy_exploration = false;
                    let mut replayed = Vec::with_capacity(tools.len());
                    for tool in tools {
                        replayed.push(ToolEntry::from_session_transcript(tool, &self.cwd));
                    }
                    if !replayed.is_empty() {
                        self.entries.push(TranscriptEntry::Exploration {
                            tools: replayed,
                            sealed: true,
                        });
                    }
                }
            }
        }
    }

    fn replay_tool(&mut self, tool: SessionTranscriptTool, join_legacy_exploration: bool) -> bool {
        let tool = ToolEntry::from_session_transcript(tool, &self.cwd);
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
        self.action_required = false;
        self.assistant_presentation.clear();
        self.deferred_agent_events.clear();
        self.pending_input.clear();
        self.overlay = None;
        self.file_search = FileSearchPopup::default();
        self.skill_popup = SkillPopup::default();
        self.slash_selection = None;
        self.dismissed_slash = None;
        self.terminal_assistant_received_this_turn = false;
        self.active_message_phase = None;
        self.submitted_prompt = None;
    }

    pub(super) fn handle_agent_event(&mut self, event: AgentEvent) {
        if matches!(event, AgentEvent::UserInputAccepted) {
            self.submitted_prompt = None;
            return;
        }
        if !self.deferred_agent_events.is_empty() {
            self.defer_agent_event(event);
            return;
        }
        match event {
            AgentEvent::ModelMessageDelta(delta) => {
                self.assistant_presentation
                    .enqueue(delta.text, delta.received_at);
            }
            event if self.busy && is_presentation_step(&event) => {
                self.defer_agent_event(event);
            }
            event if self.assistant_presentation.is_pending() => {
                self.defer_agent_event(event);
            }
            event => self.apply_agent_event(event),
        }
    }

    fn defer_agent_event(&mut self, event: AgentEvent) {
        self.deferred_agent_events.push_back(QueuedAgentEvent {
            event,
            enqueued_at: Instant::now(),
        });
    }

    pub(super) fn has_pending_presentation(&self) -> bool {
        self.assistant_presentation.is_pending() || !self.deferred_agent_events.is_empty()
    }

    pub(super) fn advance_presentation(&mut self, now: Instant) -> bool {
        let mut changed = false;
        let item_budget = item_reveal_budget(
            now,
            self.deferred_agent_events.iter().filter_map(|queued| {
                is_presentation_step(&queued.event).then_some(queued.enqueued_at)
            }),
        );
        let mut revealed_items = 0_usize;
        loop {
            let revealed = self.assistant_presentation.reveal(now);
            if !revealed.is_empty() {
                self.append_model_message_delta(&revealed);
                // Keep the transition from model-authored text to a discrete transcript item on
                // its own frame, even when this reveal happened to empty the text queue.
                return true;
            }
            if self.assistant_presentation.is_pending() {
                return changed;
            }

            if revealed_items >= item_budget
                && self
                    .deferred_agent_events
                    .front()
                    .is_some_and(|queued| is_presentation_step(&queued.event))
            {
                return changed;
            }
            let Some(queued) = self.deferred_agent_events.pop_front() else {
                return changed;
            };
            match queued.event {
                AgentEvent::ModelMessageDelta(delta) => {
                    self.assistant_presentation
                        .enqueue(delta.text, delta.received_at);
                    if changed {
                        return true;
                    }
                }
                event => {
                    revealed_items += usize::from(is_presentation_step(&event));
                    self.apply_agent_event(event);
                    changed = true;
                }
            }
        }
    }

    pub(super) fn flush_presentation(&mut self) {
        loop {
            let revealed = self.assistant_presentation.take_all();
            if !revealed.is_empty() {
                self.append_model_message_delta(&revealed);
            }
            let Some(queued) = self.deferred_agent_events.pop_front() else {
                break;
            };
            match queued.event {
                AgentEvent::ModelMessageDelta(delta) => self
                    .assistant_presentation
                    .enqueue(delta.text, delta.received_at),
                event => self.apply_agent_event(event),
            }
        }
    }

    fn append_model_message_delta(&mut self, delta: &str) {
        self.seal_exploration();
        match self.entries.last_mut() {
            Some(TranscriptEntry::Assistant {
                content, streaming, ..
            }) if *streaming => {
                content.append(delta);
            }
            _ => self.entries.push(TranscriptEntry::Assistant {
                content: MarkdownRenderCache::new(delta.to_string()),
                phase: self.active_message_phase.clone(),
                streaming: true,
                history: StreamedAssistantHistory::default(),
            }),
        }
        self.compacting = false;
    }

    fn apply_agent_event(&mut self, event: AgentEvent) {
        match event {
            AgentEvent::ModelMessageStarted(message) => {
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
                self.compacting = false;
            }
            AgentEvent::ModelMessageDelta(delta) => self.append_model_message_delta(&delta.text),
            AgentEvent::ModelMessageCompleted(message) => {
                self.complete_assistant_message(message);
            }
            AgentEvent::ModelResponseCompleted => self.close_streaming_entries(),
            AgentEvent::UserInputAccepted => {}
            AgentEvent::FileContextInjected(injected) => {
                let unit = if injected.tokens == 1 {
                    "token"
                } else {
                    "tokens"
                };
                self.add_notice(format!(
                    "Injected {} into context · ~{} {unit}",
                    injected.path,
                    format_tokens(injected.tokens),
                ));
            }
            AgentEvent::WebSearchStarted(search) => {
                self.close_streaming_entries();
                self.seal_exploration();
                self.entries
                    .push(TranscriptEntry::WebSearch(WebSearchEntry::active(search)));
            }
            AgentEvent::WebSearchCompleted(search) => {
                self.close_streaming_entries();
                self.seal_exploration();
                if let Some(entry) = self.find_web_search_mut(&search.id) {
                    entry.finish(search);
                } else {
                    self.entries
                        .push(TranscriptEntry::WebSearch(WebSearchEntry::finished(search)));
                }
                self.turn_had_work = true;
            }
            AgentEvent::ToolStarted {
                call_id,
                name,
                input,
            } => {
                self.close_streaming_entries();
                let entry = ToolEntry::new(call_id, name, input, &self.cwd);
                let is_exploration = entry.is_exploration();
                if is_exploration {
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
            AgentEvent::ToolOutputDelta {
                call_id,
                stream: _,
                chunk,
            } => {
                if let Some(tool) = self.find_tool_mut(&call_id) {
                    tool.append_live_output(&chunk);
                }
            }
            AgentEvent::ToolCompleted {
                call_id,
                output,
                file_change,
                duration: _,
            } => {
                let authoritative_path = file_change
                    .as_ref()
                    .map(|change| display_tool_path(&change.path, &self.cwd));
                let may_change_repository = self.find_tool_mut(&call_id).map(|tool| {
                    let may_change_repository = tool.may_change_repository();
                    tool.set_outcome(output);
                    tool.set_file_change(file_change, authoritative_path);
                    may_change_repository
                });
                self.turn_had_work |= may_change_repository.is_some();
                self.repository_refresh_pending |= may_change_repository.unwrap_or(false);
            }
            AgentEvent::ContextUpdated(snapshot) => {
                self.refresh_repository_if_pending();
                self.context_tokens = snapshot.measured.then_some(snapshot.used_tokens);
                self.ask_user_question_enabled = snapshot.ask_user_question_enabled;
                match self.overlay.as_mut() {
                    Some(Overlay::Context(context)) => context.update(snapshot),
                    Some(Overlay::Tools {
                        parent: Some(context),
                        ..
                    }) => context.update(snapshot),
                    _ => {}
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
                self.compacting = true;
            }
            AgentEvent::CompactionCompleted => self.compacting = false,
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
                self.handle_key(key)
            }
            Event::Paste(text)
                if matches!(self.overlay.as_ref(), Some(Overlay::AskUserQuestion(_))) =>
            {
                if let Some(Overlay::AskUserQuestion(card)) = self.overlay.as_mut() {
                    card.handle_paste(&text);
                }
                Action::None
            }
            Event::Paste(text) if matches!(self.overlay.as_ref(), Some(Overlay::Resume(_))) => {
                if let Some(Overlay::Resume(picker)) = self.overlay.as_mut() {
                    picker.handle_paste(&text);
                }
                Action::None
            }
            Event::Paste(_) if self.overlay.is_some() => Action::None,
            Event::Paste(text) => {
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
        self.slash_selection = None;
    }

    fn handle_key(&mut self, key: KeyEvent) -> Action {
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);

        let ask_user_question_width = self.composer_text_width.saturating_sub(1).max(1);
        if let Some(Overlay::AskUserQuestion(card)) = self.overlay.as_mut() {
            return match card.handle_key(key, ask_user_question_width) {
                AskUserQuestionCardAction::None => Action::None,
                AskUserQuestionCardAction::Submit { call_id, response } => {
                    Action::ResolveAskUserQuestion { call_id, response }
                }
                AskUserQuestionCardAction::Cancel { call_id } => Action::ResolveAskUserQuestion {
                    call_id,
                    response: AskUserQuestionResponse::cancelled(),
                },
                AskUserQuestionCardAction::Interrupt => Action::Cancel,
            };
        }
        if let Some(Overlay::Resume(picker)) = self.overlay.as_mut() {
            return match picker.handle_key(key) {
                ResumePickerAction::None => Action::None,
                ResumePickerAction::Close => Action::CloseResumePicker,
                ResumePickerAction::CancelResume => Action::CancelResumeLoad,
                ResumePickerAction::Resume(id) => Action::ResumeSessionFromPicker(id),
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
            return if self.has_active_task() {
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
        if let Some(Overlay::Context(context)) = self.overlay.as_ref() {
            match context.handle_key(key.code) {
                ContextAction::StayOpen => {}
                ContextAction::OpenTools => {
                    let Some(Overlay::Context(context)) = self.overlay.take() else {
                        unreachable!("context overlay was matched above");
                    };
                    self.overlay = Some(Overlay::Tools {
                        view: ToolsView::under_context(context.ask_user_question_enabled()),
                        parent: Some(Box::new(context)),
                    });
                }
                ContextAction::Close => self.overlay = None,
            }
            return Action::None;
        }
        if let Some(Overlay::Tools { view, .. }) = self.overlay.as_ref() {
            if view.handle_key(key.code) == ToolsAction::Close {
                let Some(Overlay::Tools { parent, .. }) = self.overlay.take() else {
                    unreachable!("tools overlay was matched above");
                };
                self.overlay = parent.map(|context| Overlay::Context(*context));
            }
            return Action::None;
        }
        if self.overlay.is_some() {
            self.overlay = None;
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
            return if self.has_active_task() {
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
                self.editor.set_user_prompt(&prompt);
                self.dismiss_composer_completions();
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
            let selected = self
                .slash_selection
                .unwrap_or_default()
                .min(slash_matches.len() - 1);
            if (plain && key.code == KeyCode::Up)
                || (control_only && key.code == KeyCode::Char('p'))
            {
                self.slash_selection = Some(if selected == 0 {
                    slash_matches.len() - 1
                } else {
                    selected - 1
                });
                return Action::None;
            }
            if (plain && key.code == KeyCode::Down)
                || (control_only && key.code == KeyCode::Char('n'))
            {
                self.slash_selection = Some((selected + 1) % slash_matches.len());
                return Action::None;
            }
            match key.code {
                KeyCode::Enter if !shift && !alt && !control => {
                    // The first row is selected automatically; do not dispatch it until the user
                    // types a command prefix or explicitly moves the selection.
                    if self.editor.text() == "/" && self.slash_selection.is_none() {
                        return Action::None;
                    }
                    let command = slash_matches[selected];
                    self.complete_slash_command(command, selected);
                    return self.submit_action();
                }
                KeyCode::Tab => {
                    let command = slash_matches[selected];
                    self.complete_slash_command(command, selected);
                    self.editor.insert(" ");
                    return Action::None;
                }
                _ => {}
            }
        }

        let previous_text = (self.dismissed_slash.is_some() || self.slash_selection.is_some())
            .then(|| self.editor.text().to_string());
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
        if previous_text
            .as_deref()
            .is_some_and(|previous| self.editor.text() != previous)
        {
            self.dismissed_slash = None;
            self.slash_selection = None;
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
        self.slash_selection = Some(selection);
    }

    fn insert_selected_file(&mut self) -> bool {
        let Some((token_range, path, match_type)) = self.file_search.selected_path() else {
            return false;
        };
        let path = PathBuf::from(path);
        let inserted = file_attachment_text(&path);
        let start = token_range.start;
        self.editor.replace_range(token_range, &inserted);
        if match_type == crate::file_search::MatchType::File {
            self.editor
                .bind_file(start..start.saturating_add(inserted.len()), path);
        }
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
        self.slash_selection = None;
    }

    fn submit_action(&mut self) -> Action {
        if self.editor.text().trim().is_empty() {
            return Action::None;
        }
        let submission = ComposerSubmission::take(&mut self.editor);
        let history_text = submission
            .prompt()
            .text_without_image_placeholders()
            .into_owned();
        let command = history_text.trim();
        let local_command = submission.prompt().image_count() == 0;
        let slash_command =
            local_command && !history_text.starts_with(' ') && command.starts_with('/');
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
            submission.remember(&mut self.editor);
            return Action::RunShellCommand {
                command: shell_command.to_string(),
                history_text: command.to_string(),
            };
        }
        if slash_command {
            let name = command
                .strip_prefix('/')
                .unwrap_or_default()
                .split_whitespace()
                .next()
                .unwrap_or_default();
            let known = SLASH_COMMANDS
                .iter()
                .flat_map(SlashCommand::names)
                .any(|candidate| candidate == name);
            if !name.is_empty() && !name.contains('/') && !known {
                return self.reject_local_submission(
                    submission,
                    TranscriptEntry::Notice(format!(
                        "Unrecognized command '/{name}'. Type \"/\" for a list of supported commands."
                    )),
                );
            }
        }
        if !(slash_command && command == "/clear") {
            submission.remember(&mut self.editor);
        }
        if slash_command
            && let Some(arguments) = command.strip_prefix("/resume")
            && (arguments.is_empty() || arguments.starts_with(char::is_whitespace))
        {
            if self.has_active_task() {
                return self.reject_local_submission(
                    submission,
                    TranscriptEntry::Notice(
                        "Interrupt the active task before resuming another session".to_string(),
                    ),
                );
            }
            let arguments = arguments.trim();
            if arguments.is_empty() {
                return Action::OpenResumePicker(submission);
            }
            return match Uuid::parse_str(arguments) {
                Ok(id) => Action::ResumeSessionFromComposer { id, submission },
                Err(_) => self.reject_local_submission(
                    submission,
                    TranscriptEntry::Error(
                        "`/resume` expects one bettercodex session UUID".to_string(),
                    ),
                ),
            };
        }
        if slash_command
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
            _ if !slash_command => Action::Submit(submission),
            "/q" | "/quit" | "/exit" => Action::Quit,
            "/compact" if self.has_active_task() => self.reject_local_submission(
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
            "/fork" if self.has_active_task() => self.reject_local_submission(
                submission,
                TranscriptEntry::Notice(
                    "Interrupt the active task before forking this session".to_string(),
                ),
            ),
            "/fork" => Action::Fork(submission),
            "/clear" if self.has_active_task() => self.reject_local_submission(
                submission,
                TranscriptEntry::Notice(
                    "Interrupt the active task before starting a fresh session".to_string(),
                ),
            ),
            "/clear" => Action::Clear(submission),
            "/context" => Action::ShowContext,
            "/tools" => {
                self.overlay = Some(Overlay::Tools {
                    view: ToolsView::standalone(self.ask_user_question_enabled),
                    parent: None,
                });
                Action::None
            }
            "/status" => Action::ShowStatus,
            "/help" => {
                self.overlay = Some(Overlay::Shortcuts);
                Action::None
            }
            "/skills" if self.has_active_task() => self.reject_local_submission(
                submission,
                TranscriptEntry::Error(
                    "'/skills' is disabled while a task is in progress.".to_string(),
                ),
            ),
            "/skills" => {
                self.overlay = Some(Overlay::Skills(SkillsView::new()));
                Action::None
            }
            "/logout" if self.has_active_task() => self.reject_local_submission(
                submission,
                TranscriptEntry::Notice("Interrupt the active task before logging out".to_string()),
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
            } if !content.source().trim().is_empty() => {
                Some(with_citation_sources(content.source(), content.citations()))
            }
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
                content,
                phase,
                streaming,
                ..
            }) if *streaming => {
                content.replace(message.text);
                content.set_citations(message.citations);
                *phase = message.phase;
                *streaming = false;
            }
            _ if !message.text.trim().is_empty() => {
                let mut content = MarkdownRenderCache::new(message.text);
                content.set_citations(message.citations);
                self.entries.push(TranscriptEntry::Assistant {
                    content,
                    phase: message.phase,
                    streaming: false,
                    history: StreamedAssistantHistory::default(),
                });
            }
            _ => {}
        }
        self.active_message_phase = None;
        self.compacting = false;
    }

    fn close_streaming_entries(&mut self) {
        for entry in self.entries.iter_mut().rev() {
            match entry {
                TranscriptEntry::Assistant { streaming, .. } => *streaming = false,
                TranscriptEntry::User(_) => break,
                TranscriptEntry::WebSearch(_)
                | TranscriptEntry::Tool(_)
                | TranscriptEntry::Exploration { .. }
                | TranscriptEntry::Notice(_)
                | TranscriptEntry::PatchNotes { .. }
                | TranscriptEntry::UpdateAvailable(_)
                | TranscriptEntry::Error(_)
                | TranscriptEntry::Diff(_)
                | TranscriptEntry::Status(_)
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
                TranscriptEntry::WebSearch(search) => search.interrupt(),
                TranscriptEntry::Tool(tool) if tool.origin == ToolOrigin::Agent => {
                    tool.finish_if_incomplete();
                }
                TranscriptEntry::Exploration { tools, .. } => {
                    for tool in tools {
                        tool.finish_if_incomplete();
                    }
                }
                _ => {}
            }
        }
    }

    fn find_web_search_mut(&mut self, call_id: &str) -> Option<&mut WebSearchEntry> {
        self.entries.iter_mut().rev().find_map(|entry| match entry {
            TranscriptEntry::WebSearch(search) if search.search.id == call_id => Some(search),
            _ => None,
        })
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

    fn refresh_repository_if_pending(&mut self) {
        if std::mem::take(&mut self.repository_refresh_pending) {
            self.repository = Repository::discover(&self.cwd);
        }
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
        let pending_height =
            u16::try_from(self.pending_input.lines(width).len()).unwrap_or(u16::MAX);
        let pending_gap = if pending_height > 0 {
            PENDING_INPUT_GAP
        } else {
            0
        };
        let activity_height = self.activity_height();
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
            Some(Overlay::Tools { view, .. }) => view.preferred_height(width),
            Some(Overlay::Model(picker)) => picker.preferred_height(width),
            Some(Overlay::Resume(_)) => screen_height,
            Some(Overlay::Skills(skills)) => skills.preferred_height(&self.skills, width),
            Some(Overlay::AskUserQuestion(card)) => card.preferred_height(width, &self.cwd),
            None => 0,
        };
        let transcript_chrome_height = bottom_spacing
            .saturating_add(pending_height)
            .saturating_add(pending_gap)
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
        let requested_activity_height = self.activity_height();
        let requested_activity_gap = if requested_activity_height > 0 {
            ACTIVITY_COMPOSER_GAP
        } else {
            0
        };
        let requested_activity_block_height =
            requested_activity_height.saturating_add(requested_activity_gap);
        let activity_block_height = requested_activity_block_height
            .min(height_above_trailing.saturating_sub(minimum_composer_height));
        let pending_lines = self.pending_input.lines(area.width);
        let requested_pending_height = u16::try_from(pending_lines.len()).unwrap_or(u16::MAX);
        let requested_pending_gap = if requested_pending_height > 0 {
            PENDING_INPUT_GAP
        } else {
            0
        };
        let requested_pending_block_height =
            requested_pending_height.saturating_add(requested_pending_gap);
        let pending_block_height = requested_pending_block_height.min(
            height_above_trailing
                .saturating_sub(activity_block_height)
                .saturating_sub(minimum_composer_height),
        );
        let composer_height_limit = height_above_trailing
            .saturating_sub(activity_block_height)
            .saturating_sub(pending_block_height);
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
        let pending_block_bottom = if activity_block_height > 0 {
            activity_top
        } else {
            composer_area.y
        };
        let pending_block_top = pending_block_bottom.saturating_sub(pending_block_height);
        let pending_height = requested_pending_height.min(pending_block_height);
        let pending_area = Rect::new(area.x, pending_block_top, area.width, pending_height);
        let content_bottom = if pending_block_height > 0 {
            pending_block_top
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
            Some(Overlay::Tools { view, .. }) => view.render(frame, area, self.user_message_style),
            Some(Overlay::Model(picker)) => picker.render(frame, area, self.user_message_style),
            Some(Overlay::Resume(picker)) => picker.render(frame, area),
            Some(Overlay::Skills(skills)) => {
                skills.render(frame, area, &self.skills, self.user_message_style)
            }
            Some(Overlay::AskUserQuestion(card)) => {
                card.render(frame, area, self.user_message_style, &self.cwd)
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
            let prefix_width = display_width("reverse-i-search: ");
            let query_width = self.editor.history_search_query().map_or(0, display_width);
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
        let selected = self
            .slash_selection
            .unwrap_or_default()
            .min(matches.len() - 1);
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
        let heading = if self.interrupting.is_some() {
            "Interrupting"
        } else if self.compacting {
            "Compacting"
        } else {
            "Working"
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
}

fn is_presentation_step(event: &AgentEvent) -> bool {
    matches!(
        event,
        AgentEvent::FileContextInjected(_)
            | AgentEvent::WebSearchStarted(_)
            | AgentEvent::WebSearchCompleted(_)
            | AgentEvent::ToolStarted { .. }
            | AgentEvent::Warning(_)
            | AgentEvent::SteeringCommitted(_)
    )
}

impl View {
    fn activity_height(&self) -> u16 {
        u16::from(self.busy)
    }

    fn activity_lines(&self, width: u16) -> Vec<Line<'static>> {
        self.busy
            .then(|| truncate_line(self.working_line(), usize::from(width)))
            .into_iter()
            .collect()
    }

    fn status_line(&self, width: u16) -> Line<'static> {
        if let (Some(query), Some(status)) = (
            self.editor.history_search_query(),
            self.editor.history_search_status(),
        ) {
            let mut spans = vec![
                "reverse-i-search: ".dim(),
                Span::styled(query.to_string(), palette::accent_text_style()),
            ];
            match status {
                editor::HistorySearchStatus::Idle => {}
                editor::HistorySearchStatus::Match => spans.extend([
                    "  ".into(),
                    Span::styled("Enter", palette::accent_style()),
                    " accept · ".dim(),
                    Span::styled("Esc", palette::accent_style()),
                    " cancel".dim(),
                ]),
                editor::HistorySearchStatus::Searching => {
                    spans.push("  searching older prompts…".dim());
                }
                editor::HistorySearchStatus::NoMatch => spans.push("  no match".dim()),
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
            spans.push(Span::styled(" fast", palette::soft_accent_style()));
        }
        spans.extend([
            Span::styled(" │ ", Style::default().fg(MUTED)),
            Span::styled(self.repository.name.clone(), palette::accent_text_style()),
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

impl TranscriptEntry {
    fn is_finalized(&self) -> bool {
        match self {
            Self::User(_)
            | Self::Notice(_)
            | Self::PatchNotes { .. }
            | Self::UpdateAvailable(_)
            | Self::Error(_)
            | Self::Diff(_)
            | Self::Status(_)
            | Self::FinalMessageSeparator { .. } => true,
            Self::Assistant { streaming, .. } => !streaming,
            Self::WebSearch(search) => search.state != WebSearchState::Active,
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
            Self::WebSearch(search) => web_search_lines(search, width),
            Self::Tool(tool) => tool.display_lines(width, user_style),
            Self::Exploration { tools, .. } => exploration_lines(tools, width),
            Self::Notice(message) => vec![Line::from(vec![
                Span::from("• ").dim(),
                Span::from(markdown::sanitize_inline(message)).dim(),
            ])],
            Self::PatchNotes { content } => {
                return patch_notes_lines(content, width, cwd);
            }
            Self::UpdateAvailable(update) => update_available_lines(update, width),
            Self::Error(message) => vec![Line::from(vec![
                Span::styled("• ", Style::default().fg(Color::Red)),
                Span::styled(
                    markdown::sanitize_inline(message),
                    Style::default().fg(Color::Red),
                ),
            ])],
            Self::Diff(diff) => git_diff_lines(diff, width),
            Self::Status(snapshot) => return snapshot.display_lines(width),
            Self::FinalMessageSeparator { elapsed_seconds } => {
                final_message_separator_lines(*elapsed_seconds, width)
            }
        };
        terminal_hyperlinks::plain_hyperlink_lines(plain_lines)
    }
}

impl ToolEntry {
    fn new(call_id: String, name: String, input: Option<Value>, cwd: &Path) -> Self {
        let display = match name.as_str() {
            "bash" => {
                let command = input
                    .as_ref()
                    .and_then(|input| input.get("command"))
                    .and_then(Value::as_str)
                    .unwrap_or("bash")
                    .to_string();
                let argv = vec!["bash".to_string(), "-c".to_string(), command.clone()];
                ToolDisplay::Command {
                    command,
                    parsed: parse_command(&argv),
                }
            }
            "read" => ToolDisplay::Read(
                input
                    .as_ref()
                    .and_then(|input| input.get("path"))
                    .and_then(Value::as_str)
                    .map(|path| display_tool_path(Path::new(path), cwd))
                    .unwrap_or_else(|| "file".to_string()),
            ),
            "write" => ToolDisplay::FileChange {
                path: input
                    .as_ref()
                    .and_then(|input| input.get("path"))
                    .and_then(Value::as_str)
                    .map(|path| display_tool_path(Path::new(path), cwd))
                    .unwrap_or_else(|| "file".to_string()),
                action: "Writing",
            },
            "edit" => ToolDisplay::FileChange {
                path: input
                    .as_ref()
                    .and_then(|input| input.get("path"))
                    .and_then(Value::as_str)
                    .map(|path| display_tool_path(Path::new(path), cwd))
                    .unwrap_or_else(|| "file".to_string()),
                action: "Editing",
            },
            crate::ask_user_question::TOOL_NAME => ToolDisplay::AskUserQuestion {
                questions: input
                    .as_ref()
                    .and_then(|input| input.get("questions"))
                    .and_then(|questions| {
                        questions
                            .as_array()
                            .map(Vec::len)
                            .or_else(|| questions.as_u64().and_then(|count| count.try_into().ok()))
                    })
                    .unwrap_or(1),
            },
            _ => ToolDisplay::Other,
        };
        let input = retain_tool_input(&name, input);
        Self {
            call_id,
            origin: ToolOrigin::Agent,
            name,
            input,
            display,
            outcome: None,
            empty_ripgrep_result: false,
            recovery: None,
            file_change: None,
            live_output: String::new(),
            command_output_cache: None,
            started_at: Instant::now(),
        }
    }

    fn operator(call_id: String, command: &str, cwd: &Path) -> Self {
        let mut tool = Self::new(
            call_id,
            "bash".to_string(),
            Some(serde_json::json!({"command": command})),
            cwd,
        );
        tool.origin = ToolOrigin::Operator;
        tool
    }

    fn from_session_transcript(tool: SessionTranscriptTool, cwd: &Path) -> Self {
        let SessionTranscriptTool {
            call_id,
            origin,
            name,
            input,
            output,
            file_change,
        } = tool;
        let mut tool = Self::new(call_id, name, input, cwd);
        tool.origin = match origin {
            SessionTranscriptToolOrigin::Agent => ToolOrigin::Agent,
            SessionTranscriptToolOrigin::Operator => ToolOrigin::Operator,
        };
        let authoritative_path = file_change
            .as_ref()
            .map(|change| display_tool_path(&change.path, cwd));
        tool.set_file_change(file_change, authoritative_path);
        if let Some(output) = output {
            if matches!(tool.name.as_str(), "write" | "edit")
                && let Some(message) = output.recovered_file_state_message()
            {
                tool.set_recovery(message.to_string());
            } else {
                tool.set_outcome(match output {
                    SessionTranscriptToolOutput::Success(output) => Ok(output),
                    SessionTranscriptToolOutput::Error(error) => Err(error),
                });
            }
        }
        tool.finish_if_incomplete();
        tool
    }

    fn session_transcript_tool(&self, retain_success_output: bool) -> SessionTranscriptTool {
        let retain_success_output = match &self.display {
            ToolDisplay::Command { .. } => {
                retain_success_output || self.status() == ToolStatus::Failed
            }
            ToolDisplay::AskUserQuestion { .. } => true,
            _ => false,
        };
        let output = if let Some(recovery) = &self.recovery {
            Some(SessionTranscriptToolOutput::recovered_file_state(
                recovery.clone(),
            ))
        } else {
            self.outcome.as_ref().map(|outcome| match &outcome.output {
                Ok(output) => SessionTranscriptToolOutput::Success(if retain_success_output {
                    output.clone()
                } else {
                    Value::Null
                }),
                Err(error) => SessionTranscriptToolOutput::Error(error.clone()),
            })
        };
        SessionTranscriptTool {
            call_id: self.call_id.clone(),
            origin: match self.origin {
                ToolOrigin::Agent => SessionTranscriptToolOrigin::Agent,
                ToolOrigin::Operator => SessionTranscriptToolOrigin::Operator,
            },
            name: self.name.clone(),
            input: self.input.clone(),
            output,
            file_change: self.file_change.clone(),
        }
    }

    fn is_exploration(&self) -> bool {
        if self.origin != ToolOrigin::Agent {
            return false;
        }
        match &self.display {
            ToolDisplay::Read(_) => true,
            ToolDisplay::Command { parsed, .. } => {
                !parsed.is_empty()
                    && parsed
                        .iter()
                        .all(|command| !matches!(command, ParsedCommand::Unknown { .. }))
            }
            _ => false,
        }
    }

    fn may_change_repository(&self) -> bool {
        matches!(self.name.as_str(), "bash" | "write" | "edit")
    }

    fn set_file_change(&mut self, file_change: Option<ToolFileChange>, path: Option<String>) {
        if let (
            ToolDisplay::FileChange {
                path: displayed, ..
            },
            Some(path),
        ) = (&mut self.display, path)
        {
            *displayed = path;
        }
        self.file_change = file_change;
    }

    fn output_max_rows(&self) -> usize {
        match self.origin {
            ToolOrigin::Agent => TOOL_OUTPUT_MAX_ROWS,
            ToolOrigin::Operator => OPERATOR_OUTPUT_MAX_ROWS,
        }
    }

    fn append_live_output(&mut self, chunk: &str) {
        const OMITTED_MARKER: &str = "… earlier live output omitted …\n";
        self.command_output_cache = None;
        self.live_output.push_str(chunk);
        let retained_bytes = LIVE_TOOL_OUTPUT_MAX_BYTES.saturating_sub(OMITTED_MARKER.len());
        if self.live_output.len() <= LIVE_TOOL_OUTPUT_MAX_BYTES {
            return;
        }
        let mut start = self.live_output.len().saturating_sub(retained_bytes);
        while !self.live_output.is_char_boundary(start) {
            start = start.saturating_add(1);
        }
        self.live_output.drain(..start);
        self.live_output.insert_str(0, OMITTED_MARKER);
    }

    fn set_outcome(&mut self, output: Result<Value, String>) {
        self.recovery = None;
        self.empty_ripgrep_result = self.is_empty_ripgrep_result(&output);
        // Successful non-command payloads are model-facing data that these cells never render.
        // Retain errors and command output, but do not duplicate large reads or web results in the
        // long-lived TUI transcript.
        let output = match (&self.display, output) {
            (ToolDisplay::Read(_) | ToolDisplay::FileChange { .. }, Ok(_)) => Ok(Value::Null),
            (_, output) => output,
        };
        self.outcome = Some(ToolOutcome { output });
        self.live_output = String::new();
        self.command_output_cache = None;
    }

    fn set_recovery(&mut self, message: String) {
        self.outcome = Some(ToolOutcome {
            output: Ok(Value::Null),
        });
        self.empty_ripgrep_result = false;
        self.recovery = Some(message);
        self.live_output = String::new();
        self.command_output_cache = None;
    }

    fn finish_if_incomplete(&mut self) {
        if self.outcome.is_none() {
            self.set_outcome(Err("tool stopped before returning a result".to_string()));
        }
    }

    fn display_lines(&mut self, width: u16, user_style: Style) -> Vec<Line<'static>> {
        if let ToolDisplay::Command { command, .. } = &self.display {
            let command = command.clone();
            return command_lines(self, &command, width);
        }
        match &self.display {
            ToolDisplay::Command { .. } => unreachable!("commands return before display dispatch"),
            ToolDisplay::Read(path) => file_change_lines(self, "Reading", path, width),
            ToolDisplay::FileChange { path, action } => {
                if self.recovery.is_some() {
                    file_change_lines(self, action, path, width)
                } else if self.status() == ToolStatus::Succeeded
                    && let Some(file_change) = &self.file_change
                {
                    file_change::lines(path, &file_change.change, width, user_style)
                } else {
                    file_change_lines(self, action, path, width)
                }
            }
            ToolDisplay::AskUserQuestion { questions } => {
                ask_user_question_tool_lines(self, *questions, width)
            }
            ToolDisplay::Other => generic_tool_lines(self, width),
        }
    }

    fn status(&self) -> ToolStatus {
        if self.recovery.is_some() {
            return ToolStatus::Recovered;
        }
        let Some(outcome) = &self.outcome else {
            return ToolStatus::Active;
        };
        match &outcome.output {
            Err(_) => ToolStatus::Failed,
            Ok(output)
                if output
                    .get("exit_code")
                    .and_then(Value::as_i64)
                    .is_some_and(|code| code != 0) =>
            {
                ToolStatus::Failed
            }
            Ok(_) => ToolStatus::Succeeded,
        }
    }

    fn failed_for_display(&self) -> bool {
        self.status() == ToolStatus::Failed && !self.empty_ripgrep_result
    }

    // Ripgrep reserves status 1 for a completed search with no matches. Preserve that status in
    // the tool result, but do not present an output-free agent script containing only plain `rg`
    // commands as a failure. Use the source script rather than lossy display metadata: the latter
    // intentionally drops helpers such as `cd`, `head`, and `true`.
    fn is_empty_ripgrep_result(&self, output: &Result<Value, String>) -> bool {
        if self.origin != ToolOrigin::Agent {
            return false;
        }
        let ToolDisplay::Command { command, .. } = &self.display else {
            return false;
        };
        let Ok(output) = output else {
            return false;
        };
        output.get("exit_code").and_then(Value::as_i64) == Some(1)
            && command_output_is_empty(output)
            && is_only_plain_ripgrep_script(command)
    }
}

fn retain_tool_input(name: &str, input: Option<Value>) -> Option<Value> {
    if name == crate::ask_user_question::TOOL_NAME {
        let questions = input
            .as_ref()
            .and_then(|input| input.get("questions"))
            .and_then(Value::as_array)
            .map(Vec::len)
            .unwrap_or_default();
        return Some(serde_json::json!({"questions": questions}));
    }
    let retained_field = match name {
        "bash" => "command",
        "read" | "write" | "edit" => "path",
        _ => return input,
    };
    let input = input?;
    let Some(value) = input
        .get(retained_field)
        .filter(|value| value.is_string())
        .cloned()
    else {
        return Some(input);
    };
    let mut retained = serde_json::Map::new();
    retained.insert(retained_field.to_string(), value);
    Some(Value::Object(retained))
}

impl Repository {
    fn discover(cwd: &Path) -> Self {
        let root = command_output(cwd, &["rev-parse", "--show-toplevel"])
            .map(PathBuf::from)
            .unwrap_or_else(|| cwd.to_path_buf());
        let name = markdown::sanitize_inline(
            root.file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("workspace"),
        );
        let branch = command_output(cwd, &["symbolic-ref", "--quiet", "--short", "HEAD"])
            .map(|branch| markdown::sanitize_inline(&branch));
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

    let heading = Line::from(Span::styled("What's New", palette::accent_style()));
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
    let warning = Style::default().fg(palette::warning_color());
    let source = [
        Line::from(Span::styled("Update available", warning.bold())),
        Line::from(vec![
            Span::from("Run ").dim(),
            Span::styled("bcodex update", palette::accent_text_style()),
            Span::from(" in another terminal.").dim(),
        ]),
        Line::from(vec![
            Span::from("version: ").dim(),
            Span::from(update.current_version().to_string()).dim(),
            Span::from(" → ").dim(),
            Span::from(update.latest_version().to_string()).dim(),
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
        return markdown::sanitize_inline(&cwd.display().to_string());
    };
    cwd.strip_prefix(&home).map_or_else(
        |_| markdown::sanitize_inline(&cwd.display().to_string()),
        |relative| {
            if relative.as_os_str().is_empty() {
                "~".to_string()
            } else {
                markdown::sanitize_inline(&format!("~/{}", relative.display()))
            }
        },
    )
}

fn display_tool_path(path: &Path, cwd: &Path) -> String {
    if path.is_relative() {
        return markdown::sanitize_inline(&path.display().to_string());
    }
    if let Ok(relative) = path.strip_prefix(cwd) {
        return markdown::sanitize_inline(&relative.display().to_string());
    }
    let Some(home) = crate::paths::home_dir() else {
        return markdown::sanitize_inline(&path.display().to_string());
    };
    path.strip_prefix(home).map_or_else(
        |_| markdown::sanitize_inline(&path.display().to_string()),
        |relative| markdown::sanitize_inline(&format!("~/{}", relative.display())),
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
            style.patch(palette::accent_text_style()),
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
    let label_width = display_width(&label);
    vec![
        Line::from_iter([
            label,
            "─".repeat(usize::from(width).saturating_sub(label_width)),
        ])
        .dim(),
    ]
}

fn command_lines(tool: &mut ToolEntry, command: &str, width: u16) -> Vec<Line<'static>> {
    let completed_title = match tool.origin {
        ToolOrigin::Agent => "Ran",
        ToolOrigin::Operator => "You ran",
    };
    let (bullet, title) = match tool.status() {
        ToolStatus::Active => (activity_marker(Some(tool.started_at)), "Running"),
        ToolStatus::Succeeded => ("•".green().bold(), completed_title),
        ToolStatus::Failed => ("•".red().bold(), completed_title),
        ToolStatus::Recovered => ("•".dim(), "Recovered"),
    };
    let mut header = Line::from(vec![bullet, " ".into(), title.bold(), " ".into()]);
    let header_width = line_width(&header);
    let available_width = usize::from(width);
    let continuation_prefix = if width >= 5 {
        "  │ "
    } else if width >= 3 {
        "│ "
    } else {
        ""
    };
    let continuation_width = available_width
        .saturating_sub(display_width(continuation_prefix))
        .max(1);
    let header_has_command_room = available_width > header_width;
    let first_width = if header_has_command_room {
        available_width.saturating_sub(header_width)
    } else {
        continuation_width
    }
    .max(1);
    let command = markdown::sanitize(command);
    let command = command.trim_end_matches(['\r', '\n']);
    let highlighted = highlight_bash_to_lines(command);
    let no_hyphenation =
        |width| RtOptions::new(width).word_splitter(textwrap::WordSplitter::NoHyphenation);
    let mut continuation_rows = Vec::new();
    if let Some((first, rest)) = highlighted.split_first() {
        let first_wrapped = adaptive_wrap_line(first, no_hyphenation(first_width));
        let mut first_rows = first_wrapped.iter().map(line_to_static);
        if header_has_command_room && let Some(mut first_row) = first_rows.next() {
            header.spans.append(&mut first_row.spans);
        }
        continuation_rows.extend(first_rows);
        for source_line in rest {
            continuation_rows.extend(
                adaptive_wrap_line(source_line, no_hyphenation(continuation_width))
                    .iter()
                    .map(line_to_static),
            );
        }
    }

    let omitted = continuation_rows
        .len()
        .saturating_sub(COMMAND_CONTINUATION_MAX_ROWS);
    let mut lines = vec![header];
    for mut row in continuation_rows
        .into_iter()
        .take(COMMAND_CONTINUATION_MAX_ROWS)
    {
        let mut spans = vec![Span::from(continuation_prefix).dim()];
        spans.append(&mut row.spans);
        lines.push(Line::from(spans));
    }
    if omitted > 0 {
        lines.push(Line::from(vec![
            Span::from(continuation_prefix).dim(),
            format!("… +{omitted} lines").dim(),
        ]));
    }
    lines.extend(command_output_lines(tool, width));
    lines
}

fn command_output_lines(tool: &mut ToolEntry, width: u16) -> Vec<Line<'static>> {
    if let Some(cache) = &tool.command_output_cache
        && cache.width == width
    {
        return cache.lines.clone();
    }

    let mut lines = Vec::new();
    match &tool.outcome {
        None if !tool.live_output.is_empty() => {
            append_bounded_output(&tool.live_output, width, tool.output_max_rows(), &mut lines);
        }
        Some(ToolOutcome { output: Ok(output) }) => {
            let text = command_output_text(output);
            append_bounded_output(&text, width, tool.output_max_rows(), &mut lines);
        }
        Some(ToolOutcome { output: Err(error) }) => {
            append_bounded_output(error, width, tool.output_max_rows(), &mut lines);
        }
        None => {}
    }
    tool.command_output_cache = Some(CommandOutputRenderCache {
        width,
        lines: lines.clone(),
    });
    lines
}

fn command_output_is_empty(output: &Value) -> bool {
    let Some(object) = output.as_object() else {
        return output.as_str().unwrap_or_default().is_empty();
    };
    object
        .get("stdout")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .is_empty()
        && object
            .get("stderr")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .is_empty()
}

fn command_output_text(output: &Value) -> String {
    let Some(object) = output.as_object() else {
        return output.as_str().unwrap_or_default().to_string();
    };
    let stdout = object
        .get("stdout")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let stderr = object
        .get("stderr")
        .and_then(Value::as_str)
        .unwrap_or_default();
    match (stdout.is_empty(), stderr.is_empty()) {
        (false, false) => format!("{stdout}\n{stderr}"),
        (false, true) => stdout.to_string(),
        (true, false) => stderr.to_string(),
        (true, true) => String::new(),
    }
}

fn exploration_lines(tools: &[ToolEntry], width: u16) -> Vec<Line<'static>> {
    let active = tools.iter().any(|tool| tool.outcome.is_none());
    let failed = tools.iter().any(ToolEntry::failed_for_display);
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

    let mut details = Vec::<(&str, Vec<Span<'static>>)>::new();
    let mut read_names = Vec::<String>::new();
    let flush_reads = |details: &mut Vec<(&str, Vec<Span<'static>>)>, names: &mut Vec<String>| {
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
        details.push(("Read", spans));
    };

    for tool in tools {
        match &tool.display {
            ToolDisplay::Read(path) if !read_names.contains(path) => {
                read_names.push(path.clone());
            }
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
                            details.push(("Search", spans));
                        }
                        ParsedCommand::Unknown { .. } => {}
                    }
                }
            }
            _ => {}
        }
    }
    flush_reads(&mut details, &mut read_names);

    let detail_width = width.saturating_sub(4).max(1);
    let mut first_detail = true;
    for (title, mut spans) in details {
        for span in &mut spans {
            span.content = markdown::sanitize_inline(span.content.as_ref()).into();
        }
        let title = format!("{title} ");
        let title_width = u16::try_from(display_width(&title)).unwrap_or(u16::MAX);
        for (index, mut row) in wrap_styled_line(
            &Line::from(spans),
            detail_width.saturating_sub(title_width).max(1),
        )
        .into_iter()
        .enumerate()
        {
            let mut row_spans = vec![if std::mem::replace(&mut first_detail, false) {
                "  └ ".dim()
            } else {
                "    ".into()
            }];
            row_spans.push(if index == 0 {
                Span::styled(title.clone(), palette::accent_text_style())
            } else {
                Span::from(" ".repeat(usize::from(title_width)))
            });
            row_spans.append(&mut row.spans);
            lines.push(Line::from(row_spans));
        }
    }
    for tool in tools {
        let Some((title, detail)) = exploration_failure(tool) else {
            continue;
        };
        lines.extend(failed_tool_lines(&title, &detail, width));
    }
    lines
}

fn exploration_failure(tool: &ToolEntry) -> Option<(String, String)> {
    if !tool.failed_for_display() {
        return None;
    }
    let title = match &tool.display {
        ToolDisplay::Read(path) => format!("Failed to read {path}"),
        ToolDisplay::Command { command, .. } => {
            format!("Failed {}", first_display_line(command))
        }
        _ => format!("Failed {}", tool.name),
    };
    let detail = match &tool.outcome {
        Some(ToolOutcome { output: Err(error) }) => error.clone(),
        Some(ToolOutcome { output: Ok(output) }) => {
            let text = command_output_text(output);
            if text.is_empty() {
                output.get("exit_code").and_then(Value::as_i64).map_or_else(
                    || "tool failed without output".to_string(),
                    |code| format!("process exited with status {code}"),
                )
            } else {
                text
            }
        }
        None => return None,
    };
    Some((title, detail))
}

fn file_change_lines(
    tool: &ToolEntry,
    active_action: &str,
    path: &str,
    width: u16,
) -> Vec<Line<'static>> {
    if let Some(recovery) = &tool.recovery {
        let action = match active_action {
            "Writing" => "write",
            "Editing" => "edit",
            _ => "use",
        };
        return recovered_tool_lines(
            &format!("Recovered {action} state for {path}"),
            recovery,
            width,
        );
    }
    if let Some(ToolOutcome { output: Err(error) }) = &tool.outcome {
        let action = match active_action {
            "Reading" => "read",
            "Writing" => "write",
            "Editing" => "edit",
            _ => "use",
        };
        return failed_tool_lines(&format!("Failed to {action} {path}"), error, width);
    }
    let completed_action = match active_action {
        "Reading" => "Read",
        "Writing" => "Wrote",
        "Editing" => "Edited",
        _ => "Used",
    };
    let (bullet, action) = match tool.status() {
        ToolStatus::Active => (
            activity_marker(Some(tool.started_at)),
            active_action.to_string(),
        ),
        ToolStatus::Succeeded => ("•".green().bold(), completed_action.to_string()),
        ToolStatus::Failed => ("•".red().bold(), format!("Failed {active_action}")),
        ToolStatus::Recovered => ("•".dim(), format!("Recovered {active_action}")),
    };
    wrap_styled_line(
        &Line::from(vec![
            bullet,
            " ".into(),
            Span::from(action).bold(),
            " ".into(),
            Span::styled(path.to_string(), palette::accent_text_style()),
        ]),
        width.max(1),
    )
}

impl WebSearchEntry {
    fn active(search: crate::web_search::WebSearchCall) -> Self {
        Self {
            search,
            state: WebSearchState::Active,
            started_at: Instant::now(),
        }
    }

    fn finished(search: crate::web_search::WebSearchCall) -> Self {
        let state = Self::finished_state(&search);
        Self {
            search,
            state,
            started_at: Instant::now(),
        }
    }

    fn finish(&mut self, search: crate::web_search::WebSearchCall) {
        self.state = Self::finished_state(&search);
        self.search = search;
    }

    fn interrupt(&mut self) {
        if self.state == WebSearchState::Active {
            self.state = WebSearchState::Interrupted;
        }
    }

    fn finished_state(search: &crate::web_search::WebSearchCall) -> WebSearchState {
        match search.status.as_deref() {
            Some("failed") => WebSearchState::Failed,
            Some("in_progress") | Some("searching") => WebSearchState::Interrupted,
            _ => WebSearchState::Completed,
        }
    }
}

fn web_search_lines(search: &WebSearchEntry, width: u16) -> Vec<Line<'static>> {
    let (bullet, header, detail_prefix) = match search.state {
        WebSearchState::Active => (
            activity_marker(Some(search.started_at)),
            "Searching the web",
            " ",
        ),
        WebSearchState::Completed => ("•".dim(), "Searched the web", " for "),
        WebSearchState::Failed => ("•".red().bold(), "Web search failed", " for "),
        WebSearchState::Interrupted => ("•".dim(), "Web search interrupted", " for "),
    };
    let detail = markdown::sanitize_inline(&search.search.detail());
    let mut content = vec![Span::from(header).bold()];
    if !detail.is_empty() {
        content.push(detail_prefix.into());
        content.push(Span::from(detail));
    }
    let mut lines = wrap_styled_line(&Line::from(content), width.saturating_sub(2).max(1));
    for (index, line) in lines.iter_mut().enumerate() {
        let mut spans = if index == 0 {
            vec![bullet.clone(), " ".into()]
        } else {
            vec!["  ".into()]
        };
        spans.append(&mut line.spans);
        *line = Line::from(spans);
    }
    lines
}

fn ask_user_question_tool_lines(
    tool: &ToolEntry,
    questions: usize,
    width: u16,
) -> Vec<Line<'static>> {
    let question_label = if questions == 1 {
        "question".to_string()
    } else {
        format!("{questions} questions")
    };
    let line = match tool.outcome.as_ref().map(|outcome| &outcome.output) {
        None => Line::from(vec![
            activity_marker(Some(tool.started_at)),
            " ".into(),
            "Waiting for your answer".bold(),
            " to ".into(),
            Span::styled(question_label, palette::accent_text_style()),
        ]),
        Some(Ok(output)) if output.get("cancelled").and_then(Value::as_bool) == Some(true) => {
            Line::from(vec!["• ".dim(), "Question cancelled".bold()])
        }
        Some(Ok(output)) => {
            let answered = output
                .get("answers")
                .and_then(Value::as_array)
                .map(Vec::len)
                .unwrap_or(questions);
            let answered_label = if answered == 1 {
                "1 question".to_string()
            } else {
                format!("{answered} questions")
            };
            Line::from(vec![
                "• ".green().bold(),
                "Answered".bold(),
                " ".into(),
                Span::styled(answered_label, palette::accent_text_style()),
            ])
        }
        Some(Err(error)) => {
            return failed_tool_lines("Failed to ask user", error, width);
        }
    };
    wrap_styled_line(&line, width.max(1))
}

fn generic_tool_lines(tool: &ToolEntry, width: u16) -> Vec<Line<'static>> {
    let name = markdown::sanitize_inline(&tool.name);
    match tool.outcome.as_ref().map(|outcome| &outcome.output) {
        None => vec![Line::from(vec![
            activity_marker(Some(tool.started_at)),
            " ".into(),
            "Running".bold(),
            " ".into(),
            Span::styled(name, palette::accent_text_style()),
        ])],
        Some(Ok(output)) => {
            let mut lines = vec![Line::from(vec![
                "• ".dim(),
                "Called".bold(),
                " ".into(),
                Span::styled(name, palette::accent_text_style()),
            ])];
            append_bounded_output(&output.to_string(), width, TOOL_OUTPUT_MAX_ROWS, &mut lines);
            lines
        }
        Some(Err(error)) => failed_tool_lines(&format!("Failed {}", tool.name), error, width),
    }
}

fn recovered_tool_lines(title: &str, detail: &str, width: u16) -> Vec<Line<'static>> {
    let header = Line::from(vec![
        "•".dim(),
        " ".into(),
        Span::from(markdown::sanitize_inline(title)).bold(),
    ]);
    let mut lines = vec![truncate_line(header, usize::from(width))];
    append_bounded_output(detail, width, TOOL_OUTPUT_MAX_ROWS, &mut lines);
    lines
}

fn failed_tool_lines(title: &str, error: &str, width: u16) -> Vec<Line<'static>> {
    let header = Line::from(vec![
        "•".red().bold(),
        " ".into(),
        Span::from(markdown::sanitize_inline(title)).bold(),
    ]);
    let mut lines = vec![truncate_line(header, usize::from(width))];
    append_bounded_output(error, width, TOOL_OUTPUT_MAX_ROWS, &mut lines);
    lines
}

fn append_bounded_output(
    output: &str,
    width: u16,
    maximum_rows: usize,
    lines: &mut Vec<Line<'static>>,
) {
    let (first_prefix, subsequent_prefix) = if width >= 5 {
        ("  └ ", "    ")
    } else if width >= 3 {
        ("└ ", "  ")
    } else {
        ("", "")
    };
    let prefix_width = display_width(first_prefix).max(display_width(subsequent_prefix));
    let content_width = usize::from(width).saturating_sub(prefix_width).max(1);
    let wrap_options =
        RtOptions::new(content_width).word_splitter(textwrap::WordSplitter::NoHyphenation);
    let mut rendered = BoundedOutputRows::new(maximum_rows);
    for raw in output.lines() {
        let ansi = ansi_escape_line(raw);
        let mut push = |line: Line<'static>| {
            let height = prefixed_output_line_height(&line, prefix_width, width);
            rendered.push(line, height);
        };
        if line_contains_url_like(&ansi) {
            for wrapped in adaptive_wrap_line(&ansi, wrap_options.clone()) {
                push(line_to_static(&wrapped));
            }
        } else {
            for_each_wrapped_styled_line(
                &ansi,
                u16::try_from(content_width).unwrap_or(u16::MAX),
                |line| {
                    push(line);
                    true
                },
            );
        }
    }
    if rendered.is_empty() {
        let line = truncate_line(Line::from("(no output)").dim(), content_width);
        let height = prefixed_output_line_height(&line, prefix_width, width);
        rendered.push(line, height);
    }
    for (index, mut line) in rendered.finish(content_width).into_iter().enumerate() {
        for span in &mut line.spans {
            span.style = span.style.add_modifier(Modifier::DIM);
        }
        let prefix = if index == 0 {
            Span::from(first_prefix).dim()
        } else {
            Span::from(subsequent_prefix)
        };
        let mut spans = vec![prefix];
        spans.extend(line.spans);
        lines.push(Line::from(spans));
    }
}

fn prefixed_output_line_height(line: &Line<'static>, prefix_width: usize, width: u16) -> usize {
    let width = width.max(1);
    if prefix_width.saturating_add(line_width(line)) <= usize::from(width) {
        return 1;
    }

    let mut spans = Vec::with_capacity(line.spans.len().saturating_add(1));
    spans.push(Span::from(" ".repeat(prefix_width)));
    spans.extend(line.spans.iter().cloned());
    super::terminal::transcript_line_height(
        &HyperlinkLine::new(Line::from(spans).style(line.style)),
        width,
    )
}

struct BoundedOutputRow {
    index: usize,
    line: Line<'static>,
    height: usize,
}

struct BoundedOutputRows {
    maximum_rows: usize,
    total_lines: usize,
    total_rows: usize,
    head_rows: usize,
    head_open: bool,
    head: Vec<BoundedOutputRow>,
    tail_rows: usize,
    tail: VecDeque<BoundedOutputRow>,
}

impl BoundedOutputRows {
    fn new(maximum_rows: usize) -> Self {
        Self {
            maximum_rows: maximum_rows.max(1),
            total_lines: 0,
            total_rows: 0,
            head_rows: 0,
            head_open: true,
            head: Vec::new(),
            tail_rows: 0,
            tail: VecDeque::new(),
        }
    }

    fn is_empty(&self) -> bool {
        self.total_lines == 0
    }

    fn push(&mut self, line: Line<'static>, height: usize) {
        let height = height.max(1);
        let row = BoundedOutputRow {
            index: self.total_lines,
            line,
            height,
        };
        self.total_lines = self.total_lines.saturating_add(1);
        self.total_rows = self.total_rows.saturating_add(height);

        if self.head_open {
            if self.head_rows.saturating_add(height) <= self.maximum_rows {
                self.head_rows += height;
                self.head.push(BoundedOutputRow {
                    index: row.index,
                    line: row.line.clone(),
                    height,
                });
            } else {
                self.head_open = false;
            }
        }

        self.tail_rows = self.tail_rows.saturating_add(height);
        self.tail.push_back(row);
        while self.tail_rows > self.maximum_rows {
            let Some(removed) = self.tail.pop_front() else {
                break;
            };
            self.tail_rows = self.tail_rows.saturating_sub(removed.height);
        }
    }

    fn finish(self, content_width: usize) -> Vec<Line<'static>> {
        if self.total_rows <= self.maximum_rows {
            return self.head.into_iter().map(|row| row.line).collect();
        }

        let available_rows = self.maximum_rows.saturating_sub(1);
        let head_budget = available_rows / 2;
        let tail_budget = available_rows.saturating_sub(head_budget);

        let mut selected_head = Vec::new();
        let mut selected_head_rows = 0usize;
        for row in self.head {
            if selected_head_rows.saturating_add(row.height) > head_budget {
                break;
            }
            selected_head_rows += row.height;
            selected_head.push(row);
        }
        let tail_after = selected_head
            .last()
            .map_or(0, |row| row.index.saturating_add(1));

        let mut selected_tail = Vec::new();
        let mut selected_tail_rows = 0usize;
        for row in self.tail.into_iter().rev() {
            if row.index < tail_after || selected_tail_rows.saturating_add(row.height) > tail_budget
            {
                break;
            }
            selected_tail_rows += row.height;
            selected_tail.push(row);
        }
        selected_tail.reverse();

        let omitted = self
            .total_lines
            .saturating_sub(selected_head.len().saturating_add(selected_tail.len()));
        let mut output = selected_head
            .into_iter()
            .map(|row| row.line)
            .collect::<Vec<_>>();
        output.push(truncate_line(
            Line::from(format!("… +{omitted} lines omitted")),
            content_width,
        ));
        output.extend(selected_tail.into_iter().map(|row| row.line));
        output
    }
}

pub(super) fn wrap_styled_line(line: &Line<'static>, width: u16) -> Vec<Line<'static>> {
    let mut rows = Vec::new();
    for_each_wrapped_styled_line(line, width, |row| {
        rows.push(row);
        true
    });
    rows
}

/// Hard-wrap a styled line while stopping before more than `maximum_rows` are retained.
///
/// The boolean reports whether additional source content was omitted.
pub(super) fn wrap_styled_line_bounded(
    line: &Line<'static>,
    width: u16,
    maximum_rows: usize,
) -> (Vec<Line<'static>>, bool) {
    let mut rows = Vec::with_capacity(maximum_rows);
    let completed = for_each_wrapped_styled_line(line, width, |row| {
        if rows.len() == maximum_rows {
            return false;
        }
        rows.push(row);
        true
    });
    (rows, !completed)
}

fn for_each_wrapped_styled_line(
    line: &Line<'static>,
    width: u16,
    mut emit: impl FnMut(Line<'static>) -> bool,
) -> bool {
    let width = usize::from(width.max(1));
    let mut spans = Vec::new();
    let mut used = 0;
    let mut emitted = false;
    for span in &line.spans {
        let mut content = String::new();
        for grapheme in span.content.graphemes(true) {
            let grapheme_width = display_width(grapheme);
            if used + grapheme_width > width && used > 0 {
                if !content.is_empty() {
                    spans.push(Span::styled(std::mem::take(&mut content), span.style));
                }
                if !emit(Line::from(std::mem::take(&mut spans)).style(line.style)) {
                    return false;
                }
                emitted = true;
                used = 0;
            }
            content.push_str(grapheme);
            used += grapheme_width;
        }
        if !content.is_empty() {
            spans.push(Span::styled(content, span.style));
        }
    }
    if (!spans.is_empty() || !emitted) && !emit(Line::from(spans).style(line.style)) {
        return false;
    }
    true
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
    let mut graphemes = first.graphemes(/*is_extended*/ true);
    let shortened = graphemes.by_ref().take(100).collect::<String>();
    if graphemes.next().is_some() || source.lines().nth(1).is_some() {
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
            palette::accent_text_style(),
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
        shortcut_line("Alt+Up / Shift+Left", "edit last queued follow-up"),
        shortcut_line("Shift+Enter / Ctrl+J", "insert newline"),
        shortcut_line("@", "find and attach a file to context"),
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

pub(super) fn truncate_line(line: Line<'static>, width: usize) -> Line<'static> {
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
            let grapheme_width = display_width(grapheme);
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
    fn accepted_input_releases_the_editable_prompt() {
        let mut view = View::new(Path::new("/tmp/bettercodex"));
        view.start_turn(&prompt(
            "large prompt retained only until input is accepted",
        ));
        assert!(view.submitted_prompt.is_some());

        view.handle_agent_event(AgentEvent::UserInputAccepted);

        assert!(view.submitted_prompt.is_none());
        assert!(!view.can_edit_submitted_prompt());
    }

    #[test]
    fn file_search_selection_binds_the_inserted_path_as_context_attachment() {
        let cwd = Path::new("/tmp/bettercodex");
        let mut view = View::new(cwd);
        view.editor.set_text("@SPEC");
        view.sync_composer_popups();
        view.handle_file_search_update(FileSearchUpdate::Matches {
            query: "SPEC".to_string(),
            matches: vec![crate::file_search::FileMatch {
                score: 100,
                path: PathBuf::from("file-inject-SPEC.md"),
                match_type: crate::file_search::MatchType::File,
                root: cwd.to_path_buf(),
                indices: None,
            }],
        });

        assert!(view.insert_selected_file());
        let prompt = view.editor.take_prompt();
        assert_eq!(prompt.as_str(), "file-inject-SPEC.md ");
        assert_eq!(prompt.file_attachments().len(), 1);
        assert_eq!(
            prompt.file_attachments()[0].path(),
            Path::new("file-inject-SPEC.md")
        );
        assert_eq!(prompt.file_attachments()[0].range(), &(0..19));
    }

    #[test]
    fn injected_file_context_renders_as_a_muted_token_sized_notice() {
        let mut view = View::new(Path::new("/tmp/bettercodex"));
        view.start_turn(&prompt("Use file-inject-SPEC.md"));
        view.handle_agent_event(AgentEvent::UserInputAccepted);
        view.handle_agent_event(AgentEvent::FileContextInjected(
            crate::file_context::InjectedFileContext {
                path: "file-inject-SPEC.md".to_string(),
                tokens: 1_240,
            },
        ));
        assert!(view.advance_presentation(Instant::now() + Duration::from_secs(1)));

        let lines = view.take_pending_history_lines(80, 24);
        let notice = lines
            .iter()
            .find(|line| plain(*line).contains("Injected file-inject-SPEC.md into context"))
            .expect("file injection notice");
        assert_eq!(
            plain(notice),
            "• Injected file-inject-SPEC.md into context · ~1.2K tokens"
        );
        assert!(
            notice
                .line
                .spans
                .iter()
                .all(|span| { span.style.add_modifier.contains(Modifier::DIM) })
        );
    }

    #[test]
    fn refreshed_skills_preserve_an_unchanged_completion_and_reset_a_stale_one() {
        let mut view = View::new(Path::new("/tmp/bettercodex"));
        let skills = crate::skills::SkillCatalog::load(Path::new("/tmp/bettercodex"))
            .skills()
            .to_vec();
        assert!(!skills.is_empty());
        view.set_skills(skills.clone());
        view.editor.set_text("$rev");
        view.sync_composer_popups();
        assert!(view.skill_popup.is_active());

        view.refresh_skills(&skills);
        assert!(view.skill_popup.is_active());

        view.refresh_skills(&[]);
        assert!(!view.skill_popup.is_active());
        assert!(view.skills.is_empty());
    }

    #[test]
    fn resumed_transcript_renders_user_tool_and_assistant_history_in_order() {
        let transcript = vec![
            SessionTranscriptItem::User {
                text: "run the checks".to_string(),
                image_count: 0,
            },
            SessionTranscriptItem::Tool {
                tool: SessionTranscriptTool {
                    call_id: "call-1".to_string(),
                    origin: SessionTranscriptToolOrigin::Agent,
                    name: "bash".to_string(),
                    input: Some(json!({"command": "cargo test"})),
                    output: Some(SessionTranscriptToolOutput::Success(json!({
                        "stdout": "test result: ok",
                        "stderr": "",
                        "exit_code": 0,
                    }))),
                    file_change: None,
                },
            },
            SessionTranscriptItem::Assistant {
                text: "All checks pass.".to_string(),
                phase: Some(MessagePhase::FinalAnswer),
                citations: Vec::new(),
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
        let tool = rendered.find("Ran cargo test").expect("replayed bash call");
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
    fn recovered_file_state_is_neutral_and_identical_after_transcript_replay() {
        for (name, action) in [("write", "write"), ("edit", "edit")] {
            let recovery = format!(
                "Recovery: `/tmp/bettercodex/tracked.txt` has reconciled state for the interrupted {name} call."
            );
            let transcript = vec![SessionTranscriptItem::Tool {
                tool: SessionTranscriptTool {
                    call_id: format!("call-recovered-{name}"),
                    origin: SessionTranscriptToolOrigin::Agent,
                    name: name.to_string(),
                    input: Some(json!({"path": "tracked.txt"})),
                    output: Some(SessionTranscriptToolOutput::recovered_file_state(recovery)),
                    file_change: None,
                },
            }];
            let mut recovered = View::new(Path::new("/tmp/bettercodex"));
            recovered.welcome_pending = false;
            recovered.replay_transcript(transcript.clone());

            assert_eq!(recovered.session_transcript(), transcript);
            let recovered_lines = recovered.take_pending_history_lines(100, 24);
            let rendered = recovered_lines
                .iter()
                .map(plain)
                .collect::<Vec<_>>()
                .join("\n");
            assert!(
                rendered.contains(&format!("Recovered {action} state for tracked.txt")),
                "{rendered}"
            );
            assert!(rendered.contains("has reconciled state"), "{rendered}");
            assert!(!rendered.contains("Wrote tracked.txt"), "{rendered}");
            assert!(!rendered.contains("Edited tracked.txt"), "{rendered}");
            assert!(!rendered.contains("Failed"), "{rendered}");
            let marker = &recovered_lines[0].line.spans[0];
            assert_eq!(marker.content.as_ref(), "•");
            assert_eq!(marker.style.fg, None);
            assert!(marker.style.add_modifier.contains(Modifier::DIM));

            let mut replay = View::new(Path::new("/tmp/bettercodex"));
            replay.welcome_pending = false;
            replay.replay_transcript(transcript);
            assert_eq!(replay.take_pending_history_lines(100, 24), recovered_lines);
        }
    }

    #[test]
    fn operator_command_survives_turn_completion_and_replays_with_its_source() {
        let mut view = View::new(Path::new("/tmp/bettercodex"));
        view.welcome_pending = false;
        view.start_turn(&UserPrompt::text("keep working"));
        view.handle_agent_event(AgentEvent::ModelMessageDelta(ModelTextDelta::now(
            "Accepted before the local command.",
        )));
        view.start_operator_command("operator:test".to_string(), "printf 'live marker\\n'");
        view.append_operator_command_output("operator:test", "live marker\n");

        let boundary = view.session_transcript();
        assert!(matches!(
            boundary.as_slice(),
            [
                SessionTranscriptItem::User { .. },
                SessionTranscriptItem::Assistant { text, .. },
                SessionTranscriptItem::Tool { tool },
            ] if text == "Accepted before the local command."
                && tool.origin == SessionTranscriptToolOrigin::Operator
        ));

        view.finish_turn(Ok(SubmitOutcome::Completed("Turn complete.".to_string())));

        let active = view
            .active_lines(80)
            .iter()
            .map(plain)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(active.contains("Running printf 'live marker"), "{active}");
        assert!(active.contains("live marker"), "{active}");
        assert!(
            !active.contains("tool stopped before returning"),
            "{active}"
        );
        assert_eq!(
            view.handle_terminal_event(Event::Key(
                KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE,)
            )),
            Action::Cancel
        );
        assert_eq!(
            view.handle_terminal_event(Event::Key(KeyEvent::new(
                KeyCode::Char('c'),
                KeyModifiers::CONTROL,
            ))),
            Action::Cancel
        );

        view.finish_operator_command(
            "operator:test",
            Ok(json!({
                "stdout": "live marker\nfinished marker\n",
                "stderr": "",
                "exit_code": 0,
            })),
        );
        let transcript = view.session_transcript();
        let operator_tool = transcript.iter().find_map(|item| match item {
            SessionTranscriptItem::Tool { tool } => Some(tool),
            _ => None,
        });
        assert_eq!(
            operator_tool.map(|tool| tool.origin),
            Some(SessionTranscriptToolOrigin::Operator)
        );

        let rendered = view
            .take_pending_history_lines(80, 24)
            .iter()
            .map(plain)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            rendered.contains("You ran printf 'live marker"),
            "{rendered}"
        );
        assert!(rendered.contains("finished marker"), "{rendered}");
        assert!(rendered.contains("Turn complete."), "{rendered}");

        let mut replay = View::new(Path::new("/tmp/bettercodex"));
        replay.welcome_pending = false;
        replay.replay_transcript(transcript);
        let replayed = replay
            .take_pending_history_lines(80, 24)
            .iter()
            .map(plain)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            replayed.contains("You ran printf 'live marker"),
            "{replayed}"
        );
        assert!(replayed.contains("finished marker"), "{replayed}");
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
    fn streamed_assistant_filters_hidden_markup_split_across_deltas() {
        let chunks = [
            "before ",
            "\x1b[",
            "31mred \x1b]0;secret",
            " continuation\x1b\\after ",
            "\u{e200}ci",
            "te\u{e202}turn0search2\u{e202}",
            "turn1news4\u{e201}done",
        ];
        let source = chunks.concat();
        let mut view = View::new(Path::new("/tmp/bettercodex"));
        view.welcome_pending = false;
        view.start_turn(&UserPrompt::text("test"));
        let _ = view.take_pending_history_lines(80, 24);
        view.handle_agent_event(AgentEvent::ModelMessageDelta(ModelTextDelta::now(
            chunks[0],
        )));
        view.flush_presentation();
        let before_control = view
            .prepare(80, 24)
            .active_lines
            .iter()
            .map(plain)
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(before_control, "\n• before");

        for chunk in &chunks[1..] {
            view.handle_agent_event(AgentEvent::ModelMessageDelta(ModelTextDelta::now(*chunk)));
        }
        view.flush_presentation();

        let streamed = view
            .prepare(80, 24)
            .active_lines
            .iter()
            .map(plain)
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(streamed, "\n• before red after done");

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
    fn non_markdown_transcript_fields_strip_terminal_controls() {
        let mut view = View::new(Path::new("/tmp/bettercodex"));
        view.welcome_pending = false;
        view.add_notice("safe\u{7}\nnext\t\x1b[31mred\x1b[0m");

        let notice = view
            .take_pending_history_lines(80, 24)
            .iter()
            .map(plain)
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(notice, "• safe next red");
        assert!(!notice.chars().any(char::is_control));

        let preview = file_change::lines(
            "bad\u{7}\nname.rs",
            &FileChange::Add {
                content: "safe\u{7}\tred".to_string(),
            },
            80,
            Style::default(),
        );
        let preview_lines = preview.iter().map(plain).collect::<Vec<_>>();
        let preview = preview_lines.join("\n");
        assert!(preview.contains("bad name.rs"), "{preview:?}");
        assert!(preview.contains("safe    red"), "{preview:?}");
        assert!(
            preview_lines
                .iter()
                .all(|line| !line.chars().any(char::is_control))
        );
    }

    #[test]
    fn large_delta_is_revealed_progressively_and_catches_up_within_latency_bound() {
        let mut view = View::new(Path::new("/tmp/bettercodex"));
        view.welcome_pending = false;
        view.start_turn(&UserPrompt::text("stream smoothly"));
        let _ = view.take_pending_history_lines(80, 24);
        let received_at = Instant::now();
        let source = "x".repeat(100);
        view.handle_agent_event(AgentEvent::ModelMessageDelta(ModelTextDelta::new(
            source.clone(),
            received_at,
        )));

        assert!(view.prepare(80, 24).active_lines.is_empty());
        assert!(view.advance_presentation(received_at));
        let first_frame = view
            .prepare(80, 24)
            .active_lines
            .iter()
            .map(plain)
            .collect::<String>();
        let first_frame_chars = first_frame.matches('x').count();
        assert!(first_frame_chars > 0 && first_frame_chars < source.len());

        for frame in 1..=24 {
            view.advance_presentation(received_at + MIN_FRAME_INTERVAL * frame);
        }
        let final_frame = view
            .prepare(80, 24)
            .active_lines
            .iter()
            .map(plain)
            .collect::<String>();
        assert_eq!(final_frame.matches('x').count(), source.len());
        assert!(!view.has_pending_presentation());
    }

    #[test]
    fn rapid_deltas_stay_ordered_and_graphemes_are_not_split() {
        let mut view = View::new(Path::new("/tmp/bettercodex"));
        view.welcome_pending = false;
        view.start_turn(&UserPrompt::text("preserve stream order"));
        let received_at = Instant::now();
        let chunks = ["A", "👨‍", "👩‍👧‍👦", "e", "\u{301}BCDEFGHIJKLMNOP"];
        let complete = chunks.concat();
        for text in chunks {
            view.handle_agent_event(AgentEvent::ModelMessageDelta(ModelTextDelta::new(
                text.to_string(),
                received_at,
            )));
        }

        view.advance_presentation(received_at);
        let Some(TranscriptEntry::Assistant { content, .. }) = view.entries.last() else {
            panic!("expected visible assistant text");
        };
        let visible = content.source();
        assert!(visible.len() < complete.len());
        assert!(complete.starts_with(visible));
        assert!(
            complete
                .grapheme_indices(/*is_extended*/ true)
                .any(|(start, _)| start == visible.len()),
            "presentation split an extended grapheme at byte {}",
            visible.len(),
        );
        view.flush_presentation();
        let Some(TranscriptEntry::Assistant { content, .. }) = view.entries.last() else {
            panic!("expected completed assistant text");
        };
        assert_eq!(content.source(), complete);
    }

    #[test]
    fn tool_boundary_waits_for_presented_assistant_text_without_losing_events() {
        let mut view = View::new(Path::new("/tmp/bettercodex"));
        view.welcome_pending = false;
        view.start_turn(&UserPrompt::text("keep transcript order"));
        view.handle_agent_event(AgentEvent::ModelMessageDelta(ModelTextDelta::now(
            "assistant text before the tool",
        )));
        view.handle_agent_event(AgentEvent::ToolStarted {
            call_id: "call-1".to_string(),
            name: "bash".to_string(),
            input: Some(json!({"command": "printf done"})),
        });
        view.handle_agent_event(AgentEvent::ToolCompleted {
            call_id: "call-1".to_string(),
            output: Ok(json!({"exit_code": 0, "stdout": "done", "stderr": ""})),
            file_change: None,
            duration: Duration::from_millis(1),
        });

        let mut frame_at = Instant::now();
        while view.assistant_presentation.is_pending() {
            assert!(view.advance_presentation(frame_at));
            frame_at += MIN_FRAME_INTERVAL;
        }
        assert!(matches!(
            view.entries.last(),
            Some(TranscriptEntry::Assistant { .. })
        ));
        assert!(view.has_pending_presentation());
        assert!(view.advance_presentation(frame_at));
        assert!(matches!(
            view.entries.last(),
            Some(TranscriptEntry::Tool(_))
        ));
        let transcript = view.session_transcript();
        assert!(matches!(
            transcript.first(),
            Some(SessionTranscriptItem::User { .. })
        ));
        assert!(matches!(
            transcript.get(1),
            Some(SessionTranscriptItem::Assistant { text, .. })
                if text == "assistant text before the tool"
        ));
        assert!(matches!(
            transcript.get(2),
            Some(SessionTranscriptItem::Tool { tool }) if tool.call_id == "call-1"
        ));
    }

    #[test]
    fn rapidly_arriving_exploration_tools_are_revealed_across_frames() {
        let mut view = View::new(Path::new("/tmp/bettercodex"));
        view.welcome_pending = false;
        view.start_turn(&UserPrompt::text("inspect several areas"));
        let _ = view.take_pending_history_lines(80, 24);
        for index in 0..30 {
            let call_id = format!("search-{index}");
            view.handle_agent_event(AgentEvent::ToolStarted {
                call_id: call_id.clone(),
                name: "bash".to_string(),
                input: Some(json!({"command": format!("rg needle-{index} src")})),
            });
            view.handle_agent_event(AgentEvent::ToolCompleted {
                call_id,
                output: Ok(json!({"exit_code": 0, "stdout": "", "stderr": ""})),
                file_change: None,
                duration: Duration::from_millis(1),
            });
        }

        let first_frame_at = Instant::now();
        assert!(view.advance_presentation(first_frame_at));
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| view.render(frame)).unwrap();
        let first_frame = render_buffer(terminal.backend().buffer());
        assert!(first_frame.contains("needle-0"), "{first_frame}");
        assert!(!first_frame.contains("needle-1"), "{first_frame}");
        assert!(view.has_pending_presentation());

        assert!(view.advance_presentation(first_frame_at + MIN_FRAME_INTERVAL));
        terminal.draw(|frame| view.render(frame)).unwrap();
        let second_frame = render_buffer(terminal.backend().buffer());
        assert!(second_frame.contains("needle-1"), "{second_frame}");
        assert!(!second_frame.contains("needle-2"), "{second_frame}");

        for frame in 2..=24 {
            view.advance_presentation(first_frame_at + MIN_FRAME_INTERVAL * frame);
        }
        assert!(!view.has_pending_presentation());
        let completed = view
            .active_lines(80)
            .iter()
            .map(plain)
            .collect::<Vec<_>>()
            .join("\n");
        for index in 0..30 {
            assert!(
                completed.contains(&format!("needle-{index}")),
                "{completed}"
            );
        }
    }

    #[test]
    fn rapidly_arriving_notices_are_revealed_across_frames() {
        let mut view = View::new(Path::new("/tmp/bettercodex"));
        view.welcome_pending = false;
        view.start_turn(&UserPrompt::text("show notices fluently"));
        let _ = view.take_pending_history_lines(80, 24);
        view.handle_agent_event(AgentEvent::Warning("first notice".to_string()));
        view.handle_agent_event(AgentEvent::Warning("second notice".to_string()));

        let first_frame_at = Instant::now();
        assert!(view.advance_presentation(first_frame_at));
        let first_frame = view
            .prepare(80, 24)
            .active_lines
            .iter()
            .map(plain)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(first_frame.contains("first notice"), "{first_frame}");
        assert!(!first_frame.contains("second notice"), "{first_frame}");
        assert!(view.has_pending_presentation());

        assert!(view.advance_presentation(first_frame_at + MIN_FRAME_INTERVAL));
        let second_frame = view
            .prepare(80, 24)
            .active_lines
            .iter()
            .map(plain)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(second_frame.contains("first notice"), "{second_frame}");
        assert!(second_frame.contains("second notice"), "{second_frame}");
        assert!(!view.has_pending_presentation());
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
    fn enter_on_a_bare_slash_waits_for_a_command() {
        let mut view = View::new(Path::new("/tmp/bettercodex"));
        view.editor.set_text("/");

        assert_eq!(
            view.handle_terminal_event(Event::Key(KeyEvent::new(
                KeyCode::Enter,
                KeyModifiers::NONE,
            ))),
            Action::None
        );
        assert_eq!(
            view.handle_terminal_event(Event::Key(KeyEvent::new(
                KeyCode::Char('q'),
                KeyModifiers::NONE,
            ))),
            Action::None
        );
        assert_eq!(
            view.handle_terminal_event(Event::Key(KeyEvent::new(
                KeyCode::Enter,
                KeyModifiers::NONE,
            ))),
            Action::Quit
        );
    }

    #[test]
    fn q_alias_remains_executable_after_dismissing_slash_completion() {
        let mut view = View::new(Path::new("/tmp/bettercodex"));
        view.editor.set_text("/q");

        assert_eq!(
            view.handle_terminal_event(Event::Key(
                KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE,)
            )),
            Action::None
        );
        assert_eq!(
            view.handle_terminal_event(Event::Key(KeyEvent::new(
                KeyCode::Enter,
                KeyModifiers::NONE,
            ))),
            Action::Quit
        );
    }

    #[test]
    fn enter_on_a_bare_slash_submits_an_explicit_keyboard_selection() {
        let mut view = View::new(Path::new("/tmp/bettercodex"));
        view.editor.set_text("/");

        assert_eq!(
            view.handle_terminal_event(Event::Key(KeyEvent::new(
                KeyCode::Down,
                KeyModifiers::NONE,
            ))),
            Action::None
        );
        let Action::Clear(submission) = view.handle_terminal_event(Event::Key(KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::NONE,
        ))) else {
            panic!("explicit selection should submit /clear");
        };
        assert_eq!(
            submission.prompt().text_without_image_placeholders(),
            "/clear"
        );

        let mut view = View::new(Path::new("/tmp/bettercodex"));
        view.editor.set_text("/");
        for key in [KeyCode::Down, KeyCode::Up] {
            assert_eq!(
                view.handle_terminal_event(Event::Key(KeyEvent::new(key, KeyModifiers::NONE,))),
                Action::None
            );
        }
        let Action::Submit(submission) = view.handle_terminal_event(Event::Key(KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::NONE,
        ))) else {
            panic!("selection cycled back to /review should still be explicit");
        };
        assert_eq!(
            submission.prompt().text_without_image_placeholders(),
            "/review"
        );
    }

    #[test]
    fn rejected_local_commands_keep_the_draft_editable() {
        let rejected = vec![
            ("!", false),
            ("/resume not-a-session-id", false),
            ("/compact", true),
            ("/fork", true),
            ("/clear", true),
            ("/skills", true),
            ("/logout", true),
            ("/tmux unexpected", false),
        ];
        for (draft, busy) in rejected {
            let mut view = View::new(Path::new("/tmp/bettercodex"));
            if busy {
                view.start_turn(&UserPrompt::text("active turn"));
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

    #[test]
    fn unknown_slash_commands_are_rejected_without_entering_history() {
        let mut view = View::new(Path::new("/tmp/bettercodex"));
        view.editor.set_text("/does-not-exist");

        assert_eq!(
            view.handle_terminal_event(Event::Key(KeyEvent::new(
                KeyCode::Enter,
                KeyModifiers::NONE,
            ))),
            Action::None
        );
        assert_eq!(view.editor.text(), "/does-not-exist");
        let rendered = view
            .take_pending_history_lines(80, 24)
            .iter()
            .flat_map(|line| line.line.spans.iter())
            .map(|span| span.content.as_ref())
            .collect::<String>();
        assert!(
            rendered.contains("Unrecognized command '/does-not-exist'"),
            "{rendered}"
        );

        view.editor.set_text("");
        assert_eq!(
            view.handle_terminal_event(Event::Key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE,))),
            Action::None
        );
        assert!(view.editor.is_empty());
    }

    #[test]
    fn slash_paths_and_leading_space_commands_submit_as_text() {
        for text in ["/Users/example/project", " /clear"] {
            let mut view = View::new(Path::new("/tmp/bettercodex"));
            view.editor.set_text(text);

            let Action::Submit(submission) = view.handle_terminal_event(Event::Key(KeyEvent::new(
                KeyCode::Enter,
                KeyModifiers::NONE,
            ))) else {
                panic!("{text:?} should submit as ordinary text");
            };
            assert_eq!(submission.prompt().text_without_image_placeholders(), text);
            drop(submission);
            assert_eq!(
                view.handle_terminal_event(Event::Key(KeyEvent::new(
                    KeyCode::Up,
                    KeyModifiers::NONE,
                ))),
                Action::None
            );
            assert_eq!(view.editor.text(), text);
        }
    }

    #[test]
    fn clear_is_excluded_from_composer_history() {
        let mut view = View::new(Path::new("/tmp/bettercodex"));
        view.editor.set_text("/clear");
        assert!(matches!(
            view.handle_terminal_event(Event::Key(KeyEvent::new(
                KeyCode::Enter,
                KeyModifiers::NONE,
            ))),
            Action::Clear(_)
        ));
        view.clear();

        assert_eq!(
            view.handle_terminal_event(Event::Key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE,))),
            Action::None
        );
        assert!(view.editor.is_empty());
    }

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
    fn tab_completes_a_slash_command_without_dispatching_it() {
        let mut view = View::new(Path::new("/tmp/bettercodex"));
        view.editor.set_text("/ski");

        assert_eq!(
            view.handle_terminal_event(Event::Key(
                KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE,)
            )),
            Action::None
        );
        assert_eq!(view.editor.text(), "/skills ");
        assert!(view.overlay.is_none());
    }

    #[test]
    fn queue_edit_shortcuts_restore_only_the_most_recent_follow_up() {
        for key in [
            KeyEvent::new(KeyCode::Up, KeyModifiers::ALT),
            KeyEvent::new(KeyCode::Left, KeyModifiers::SHIFT),
        ] {
            let mut view = View::new(Path::new("/tmp/bettercodex"));
            view.queue_follow_up(UserPrompt::text("first queued"));
            view.queue_follow_up(UserPrompt::text("second queued"));
            view.editor.set_text("unsubmitted draft");

            assert_eq!(view.handle_terminal_event(Event::Key(key)), Action::None);
            assert_eq!(view.editor.text(), "second queued");
            assert_eq!(
                view.pop_next_queued_follow_up(),
                Some(UserPrompt::text("first queued"))
            );
            assert_eq!(view.pop_next_queued_follow_up(), None);
        }
    }

    #[test]
    fn runtime_rejection_restores_skill_file_and_image_bindings() {
        let mut view = View::new(Path::new("/tmp/bettercodex"));
        view.welcome_pending = false;
        let text = "$review inspect file-inject-SPEC.md [Image #1]";
        let file_label = "file-inject-SPEC.md";
        let file_start = text.find(file_label).unwrap();
        let image_label = "[Image #1]";
        let image_start = text.find(image_label).unwrap();
        let expected = UserPrompt::with_all_attachments(
            text,
            vec![crate::skills::SkillMention::new(
                SkillSelection::new("review", "/tmp/review/SKILL.md"),
                0.."$review".len(),
            )],
            vec![crate::input::PromptFileAttachment::new(
                PathBuf::from(file_label),
                file_start..file_start + file_label.len(),
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
            rendered.contains("• Could not start turn: the active agent is unavailable"),
            "{rendered}"
        );
    }

    #[test]
    fn ctrl_c_cleared_rich_draft_is_recoverable_with_up() {
        let mut view = View::new(Path::new("/tmp/bettercodex"));
        let text = "$review inspect [Image #1]";
        let image_label = "[Image #1]";
        let image_start = text.find(image_label).unwrap();
        let prompt = UserPrompt::with_all_attachments(
            text,
            vec![crate::skills::SkillMention::new(
                SkillSelection::new("review", "/tmp/review/SKILL.md"),
                0.."$review".len(),
            )],
            Vec::new(),
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
            citations: Vec::new(),
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
        let status = view.status_line(80);
        assert_eq!(
            plain(&status),
            "gpt-5.6-sol xhigh │ pi / main │ 20% of 258K"
        );
        assert_eq!(
            status
                .spans
                .iter()
                .find(|span| span.content.as_ref() == "pi")
                .and_then(|span| span.style.fg),
            Some(palette::accent_color())
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
    fn live_tool_activity_is_rendered_once_above_the_working_status() {
        const WIDTH: u16 = 96;
        const COMMAND: &str = "cargo build --release --locked";
        let mut view = View::new(Path::new("/tmp/bettercodex"));
        view.welcome_pending = false;
        view.start_turn(&UserPrompt::text("build the project"));
        let _ = view.take_pending_history_lines(WIDTH, 24);
        view.handle_agent_event(AgentEvent::ToolStarted {
            call_id: "build".to_string(),
            name: "bash".to_string(),
            input: Some(json!({"command": COMMAND})),
        });
        assert!(view.advance_presentation(Instant::now()));

        let height = view.desired_height(WIDTH, 24);
        let backend = TestBackend::new(WIDTH, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| view.render(frame)).unwrap();
        let rendered = render_buffer(terminal.backend().buffer());
        let rows = rendered.lines().collect::<Vec<_>>();
        let tool_y = rows
            .iter()
            .position(|row| row.contains("Running cargo build --release --locked"))
            .expect("rendered live tool entry");
        let status_y = rows
            .iter()
            .position(|row| row.contains("Working ("))
            .expect("rendered activity status");

        assert!(tool_y < status_y, "{rendered}");
        assert_eq!(rendered.matches(COMMAND).count(), 1, "{rendered}");
    }

    #[test]
    fn pending_steering_is_separated_from_activity() {
        const WIDTH: u16 = 80;

        let mut view = View::new(Path::new("/tmp/bettercodex"));
        view.welcome_pending = false;
        view.start_turn(&UserPrompt::text("test"));
        let _ = view.take_pending_history_lines(WIDTH, 24);
        view.add_pending_steer(
            SteerId(0),
            UserPrompt::text("include my second prompt in the spec.md verbatim."),
        );

        let height = view.desired_height(WIDTH, 24);
        let backend = TestBackend::new(WIDTH, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| view.render(frame)).unwrap();
        let rendered = render_buffer(terminal.backend().buffer());
        let rows = rendered.lines().collect::<Vec<_>>();
        let steer_y = rows
            .iter()
            .position(|row| row.contains("↳ include my second prompt"))
            .expect("rendered pending steer");
        let activity_y = rows
            .iter()
            .position(|row| row.contains("Working ("))
            .expect("rendered activity status");

        assert!(rows[steer_y + 1].trim().is_empty(), "{rendered}");
        assert_eq!(activity_y, steer_y + 2, "{rendered}");
    }

    #[test]
    fn queued_input_preview_wraps_instead_of_clipping() {
        const WIDTH: u16 = 32;

        let mut view = View::new(Path::new("/tmp/bettercodex"));
        view.welcome_pending = false;
        view.start_turn(&UserPrompt::text("active turn"));
        let _ = view.take_pending_history_lines(WIDTH, 24);
        view.queue_follow_up(UserPrompt::text(
            "alpha beta gamma delta epsilon zeta eta theta omega",
        ));

        let height = view.desired_height(WIDTH, 24);
        let backend = TestBackend::new(WIDTH, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| view.render(frame)).unwrap();
        let rendered = render_buffer(terminal.backend().buffer());
        let rows = rendered.lines().collect::<Vec<_>>();

        assert!(rendered.contains("↳ alpha beta"), "{rendered}");
        let continuation = rows
            .iter()
            .find(|row| row.contains("theta omega"))
            .expect("wrapped queued-input continuation");
        assert!(continuation.starts_with("    "), "{rendered}");
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
    fn update_available_card_renders_the_stable_update_command() {
        let mut view = View::new(Path::new("/tmp/bettercodex"));
        view.welcome_pending = false;
        view.add_update_available(AvailableUpdate::test_fixture());
        let height = view.desired_height(60, 16);
        let backend = TestBackend::new(60, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| view.render(frame)).unwrap();
        let rendered = render_buffer(terminal.backend().buffer());

        assert!(rendered.contains("Update available"), "{rendered}");
        assert!(
            rendered.contains("Run bcodex update in another terminal."),
            "{rendered}"
        );
        assert!(rendered.contains("version: 1.2.3 → 1.3.0"), "{rendered}");
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
        view.start_turn(&UserPrompt::text(
            "a user message that wraps at the narrower width",
        ));
        let _ = view.take_pending_history_lines(80, 24);
        view.handle_agent_event(AgentEvent::ModelMessageDelta(ModelTextDelta::now(
            "assistant reply",
        )));
        view.flush_presentation();
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
    fn rich_markdown_links_survive_resize_and_session_replay() {
        const DESTINATION: &str = "https://example.com/docs";
        let source = concat!(
            "Intro 👩‍💻 e\u{301} [web](https://example.com/docs).\n\n",
            "[local](file:///tmp/bettercodex/src/main.rs#L2)\n\n",
            "```markdown\n",
            "| Name | Value |\n",
            "| --- | --- |\n",
            "| alpha | 界 |\n",
            "```\n\n",
            "```bash\n",
            "printf one\tthen-two\n",
            "```",
        );
        let mut view = View::new(Path::new("/tmp/bettercodex"));
        view.welcome_pending = false;
        view.start_turn(&UserPrompt::text("render rich markdown"));
        view.handle_agent_event(AgentEvent::ModelMessageCompleted(AssistantMessage {
            text: source.to_string(),
            phase: Some(MessagePhase::FinalAnswer),
            citations: vec![crate::web_search::UrlCitation {
                start_index: 0,
                end_index: 5,
                url: DESTINATION.to_string(),
                title: "Documentation".to_string(),
            }],
        }));
        let transcript = view.session_transcript();

        let resized = view.history_lines_for_resize_reflow(24, 18);
        let visible = resized.iter().map(plain).collect::<Vec<_>>().join("\n");
        assert!(visible.contains("👩‍💻 e\u{301}"), "{visible}");
        assert!(visible.contains("src/main.rs:2"), "{visible}");
        assert!(visible.contains("alpha"), "{visible}");
        assert!(visible.contains('界'), "{visible}");
        assert!(visible.contains("printf one    then-two"), "{visible}");
        assert!(!visible.contains('\t'));
        assert!(resized.iter().any(|line| {
            line.hyperlinks
                .iter()
                .any(|link| link.destination == DESTINATION)
        }));
        assert!(resized.iter().all(|line| {
            line.hyperlinks
                .iter()
                .all(|link| link.destination.starts_with("https://"))
        }));
        let buffer = crate::tui::terminal::render_history_lines(&resized, 24);
        assert!(buffer.area.positions().any(|position| {
            buffer[position]
                .symbol()
                .contains(&format!("\x1b]8;;{DESTINATION}\x07"))
        }));

        let mut replay = View::new(Path::new("/tmp/bettercodex"));
        replay.welcome_pending = false;
        replay.replay_transcript(transcript);
        let replayed = replay.history_lines_for_resize_reflow(24, 18);
        assert_eq!(replayed, resized);
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
        view.start_turn(&UserPrompt::text("test"));
        let _ = view.take_pending_history_lines(WIDTH, HEIGHT);
        view.handle_agent_event(AgentEvent::ModelMessageDelta(ModelTextDelta::now(
            source.clone(),
        )));
        view.flush_presentation();

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
        view.start_turn(&UserPrompt::text("test"));

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
    fn direct_bash_streams_and_uses_the_command_cell_shape() {
        let mut view = View::new(Path::new("/tmp/bettercodex"));
        view.welcome_pending = false;
        view.handle_agent_event(AgentEvent::ToolStarted {
            call_id: "cell:cmd".to_string(),
            name: "bash".to_string(),
            input: Some(json!({"command": "cargo test"})),
        });
        view.handle_agent_event(AgentEvent::ToolOutputDelta {
            call_id: "cell:cmd".to_string(),
            stream: crate::process_runtime::OutputStream::Stdout,
            chunk: "checking\n".to_string(),
        });
        assert!(
            view.active_lines(80)
                .iter()
                .map(plain)
                .any(|line| line.contains("checking"))
        );
        view.handle_agent_event(AgentEvent::ToolCompleted {
            call_id: "cell:cmd".to_string(),
            output: Ok(json!({"exit_code": 0, "stdout": "21 passed\n", "stderr": ""})),
            file_change: None,
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
    }

    #[test]
    fn highlighted_shell_commands_preserve_visible_text_styles_and_url_tokens() {
        let command = "printf '%s' one\tthen-two\necho \"界e\u{301}\"";
        let mut tool = ToolEntry::new(
            "cell:highlight".to_string(),
            "bash".to_string(),
            Some(json!({"command": command})),
            Path::new("/tmp/bettercodex"),
        );
        tool.set_outcome(Ok(json!({"exit_code": 0, "stdout": "", "stderr": ""})));

        let wide = command_lines(&mut tool, command, 80);
        assert_eq!(plain(&wide[0]), "• Ran printf '%s' one    then-two");
        assert_eq!(plain(&wide[1]), "  │ echo \"界e\u{301}\"");
        assert!(wide.iter().all(|line| !plain(line).contains('\t')));
        assert!(
            wide[..2]
                .iter()
                .flat_map(|line| &line.spans)
                .any(|span| span.style.fg.is_some()),
            "syntax highlighting should survive command layout: {wide:?}"
        );

        let long = concat!(
            "first_token_is_long_enough_to_wrap\n",
            "second_token_is_also_long_enough_to_wrap",
        );
        let mut narrow_tool = ToolEntry::new(
            "cell:narrow-highlight".to_string(),
            "bash".to_string(),
            Some(json!({"command": long})),
            Path::new("/tmp/bettercodex"),
        );
        narrow_tool.set_outcome(Ok(json!({"exit_code": 0, "stdout": "", "stderr": ""})));
        let narrow = command_lines(&mut narrow_tool, long, 18);
        let visible = narrow.iter().map(plain).collect::<Vec<_>>();

        assert!(visible.iter().any(|line| line.starts_with("  │ ")));
        assert!(visible.iter().any(|line| line.contains("… +")));
        assert!(
            narrow.iter().all(|line| line_width(line) <= 18),
            "{visible:?}"
        );

        let url_command =
            "curl https://example.com/a/single/unbroken/token/that/exceeds/the/terminal";
        let mut url_tool = ToolEntry::new(
            "cell:url-highlight".to_string(),
            "bash".to_string(),
            Some(json!({"command": url_command})),
            Path::new("/tmp/bettercodex"),
        );
        url_tool.set_outcome(Ok(json!({"exit_code": 0, "stdout": "", "stderr": ""})));
        let url_lines = command_lines(&mut url_tool, url_command, 18);
        let visible = url_lines.iter().map(plain).collect::<Vec<_>>();
        assert_eq!(
            visible
                .iter()
                .filter(|line| line.contains(
                    "https://example.com/a/single/unbroken/token/that/exceeds/the/terminal"
                ))
                .count(),
            1,
            "expected the full command URL on one semantic line: {visible:?}"
        );

        let output_url = "example.test/api/v1/projects/alpha/releases/2026/session_id=abc123def456";
        url_tool.set_outcome(Ok(json!({
            "exit_code": 0,
            "stdout": output_url,
            "stderr": "",
        })));
        let output_lines = command_output_lines(&mut url_tool, 36);
        let visible = output_lines.iter().map(plain).collect::<Vec<_>>();
        assert_eq!(
            visible
                .iter()
                .filter(|line| line.contains(output_url))
                .count(),
            1,
            "expected the full output URL on one semantic line: {visible:?}"
        );
        let output_rows = output_lines
            .iter()
            .map(|line| {
                crate::tui::terminal::transcript_line_height(
                    &HyperlinkLine::new(line.clone()),
                    /*width*/ 36,
                )
            })
            .sum::<usize>();
        assert!(output_rows <= TOOL_OUTPUT_MAX_ROWS, "{visible:?}");

        let huge_url = format!(
            "https://example.test/api/v1/{}",
            "very-long-segment-".repeat(120)
        );
        url_tool.set_outcome(Ok(json!({
            "exit_code": 0,
            "stdout": format!("{huge_url}\n{huge_url}\n"),
            "stderr": "",
        })));
        let bounded = command_output_lines(&mut url_tool, 20);
        let bounded_rows = bounded
            .iter()
            .map(|line| {
                crate::tui::terminal::transcript_line_height(
                    &HyperlinkLine::new(line.clone()),
                    /*width*/ 20,
                )
            })
            .sum::<usize>();
        let visible = bounded.iter().map(plain).collect::<Vec<_>>();
        assert!(bounded_rows <= TOOL_OUTPUT_MAX_ROWS, "{visible:?}");
        assert!(
            visible.iter().any(|line| line.contains("… +")),
            "{visible:?}"
        );

        let long_failure = format!("{}👩‍💻tail\nsecond line", "x".repeat(99));
        assert_eq!(
            first_display_line(&long_failure),
            format!("{}👩‍💻 …", "x".repeat(99))
        );
    }

    #[test]
    fn tool_output_wrapping_matches_terminal_width_for_zero_width_graphemes() {
        let mut lines = Vec::new();
        append_bounded_output(
            "\u{301}ab",
            /*width*/ 6,
            TOOL_OUTPUT_MAX_ROWS,
            &mut lines,
        );

        assert_eq!(lines.len(), 1, "{lines:?}");
        assert_eq!(plain(&lines[0]), "  └ \u{301}ab");
        assert_eq!(line_width(&lines[0]), 6);
        assert_eq!(
            crate::tui::terminal::transcript_line_height(
                &HyperlinkLine::new(lines[0].clone()),
                /*width*/ 6,
            ),
            1
        );
    }

    #[test]
    fn direct_exploration_uses_the_explored_cell_and_replays_failures() {
        let mut view = View::new(Path::new("/tmp/bettercodex"));
        view.welcome_pending = false;
        view.handle_agent_event(AgentEvent::ToolStarted {
            call_id: "cell:read".to_string(),
            name: "read".to_string(),
            input: Some(json!({"path": "src/main.rs", "offset": 1, "limit": 20})),
        });
        view.handle_agent_event(AgentEvent::ToolCompleted {
            call_id: "cell:read".to_string(),
            output: Ok(json!("fn main() {}\n")),
            file_change: None,
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
        assert!(rendered.contains("Read src/main.rs"), "{rendered}");
        assert!(!rendered.contains("fn main"), "{rendered}");

        let mut failed = View::new(Path::new("/tmp/bettercodex"));
        failed.welcome_pending = false;
        failed.handle_agent_event(AgentEvent::ToolStarted {
            call_id: "cell:failed-read".to_string(),
            name: "read".to_string(),
            input: Some(json!({"path": "missing.rs"})),
        });
        failed.handle_agent_event(AgentEvent::ToolCompleted {
            call_id: "cell:failed-read".to_string(),
            output: Err("unable to read `missing.rs`".to_string()),
            file_change: None,
            duration: Duration::from_millis(10),
        });
        failed.seal_exploration();
        let lines = failed.take_pending_history_lines(80, 24);
        let failure_row = lines
            .iter()
            .position(|line| plain(line).contains("Failed to read missing.rs"))
            .expect("rendered failed read header");
        let buffer = crate::tui::terminal::render_history_lines(&lines, 80);
        let marker = &buffer[(0, failure_row as u16)];
        assert_eq!(marker.symbol(), "•");
        assert_eq!(marker.fg, Color::Red);
        let rendered = lines.iter().map(plain).collect::<Vec<_>>().join("\n");
        assert!(rendered.contains("Failed to read missing.rs"), "{rendered}");
        assert!(
            rendered.contains("unable to read `missing.rs`"),
            "{rendered}"
        );

        let mut failed_bash = View::new(Path::new("/tmp/bettercodex"));
        failed_bash.welcome_pending = false;
        failed_bash.handle_agent_event(AgentEvent::ToolStarted {
            call_id: "cell:failed-search".to_string(),
            name: "bash".to_string(),
            input: Some(json!({"command": "rg needle src"})),
        });
        failed_bash.handle_agent_event(AgentEvent::ToolCompleted {
            call_id: "cell:failed-search".to_string(),
            output: Ok(json!({
                "exit_code": 2,
                "stdout": "",
                "stderr": "rg: invalid search pattern",
            })),
            file_change: None,
            duration: Duration::from_millis(10),
        });
        failed_bash.seal_exploration();
        let transcript = failed_bash.session_transcript();
        let rendered = failed_bash
            .take_pending_history_lines(80, 24)
            .iter()
            .map(plain)
            .collect::<Vec<_>>()
            .join("\n");

        let mut replay = View::new(Path::new("/tmp/bettercodex"));
        replay.welcome_pending = false;
        replay.replay_transcript(transcript);
        let replayed = replay
            .take_pending_history_lines(80, 24)
            .iter()
            .map(plain)
            .collect::<Vec<_>>()
            .join("\n");
        for output in [rendered, replayed] {
            assert!(output.contains("Failed rg needle src"), "{output}");
            assert!(output.contains("rg: invalid search pattern"), "{output}");
        }
    }

    #[test]
    fn empty_agent_ripgrep_result_is_neutral_live_and_after_replay() {
        let mut view = View::new(Path::new("/tmp/bettercodex"));
        view.welcome_pending = false;
        view.handle_agent_event(AgentEvent::ToolStarted {
            call_id: "cell:empty-search".to_string(),
            name: "bash".to_string(),
            input: Some(json!({"command": "rg needle src"})),
        });
        view.handle_agent_event(AgentEvent::ToolCompleted {
            call_id: "cell:empty-search".to_string(),
            output: Ok(json!({"exit_code": 1, "stdout": "", "stderr": ""})),
            file_change: None,
            duration: Duration::from_millis(10),
        });
        view.seal_exploration();

        let transcript = view.session_transcript();
        let [SessionTranscriptItem::Exploration { tools }] = transcript.as_slice() else {
            panic!("empty search should be saved as one exploration cell");
        };
        let [tool] = tools.as_slice() else {
            panic!("empty search exploration should retain one tool");
        };
        let Some(SessionTranscriptToolOutput::Success(output)) = &tool.output else {
            panic!("empty search should retain its structured output");
        };
        assert_eq!(output.get("exit_code").and_then(Value::as_i64), Some(1));

        let lines = view.take_pending_history_lines(80, 24);
        let explored_row = lines
            .iter()
            .position(|line| plain(line).contains("Explored"))
            .expect("rendered empty search header");
        let buffer = crate::tui::terminal::render_history_lines(&lines, 80);
        assert_ne!(buffer[(0, explored_row as u16)].fg, Color::Red);
        let rendered = lines.iter().map(plain).collect::<Vec<_>>().join("\n");

        let mut replay = View::new(Path::new("/tmp/bettercodex"));
        replay.welcome_pending = false;
        replay.replay_transcript(transcript);
        let replayed = replay
            .take_pending_history_lines(80, 24)
            .iter()
            .map(plain)
            .collect::<Vec<_>>()
            .join("\n");
        for output in [rendered.as_str(), replayed.as_str()] {
            assert!(output.contains("• Explored"), "{output}");
            assert!(output.contains("Search needle in src"), "{output}");
            assert!(!output.contains("Failed"), "{output}");
            assert!(!output.contains("process exited with status 1"), "{output}");
        }
    }

    #[test]
    fn compound_and_operator_ripgrep_status_one_remain_failures() {
        let mut compound = View::new(Path::new("/tmp/bettercodex"));
        compound.welcome_pending = false;
        compound.handle_agent_event(AgentEvent::ToolStarted {
            call_id: "cell:compound-search".to_string(),
            name: "bash".to_string(),
            input: Some(json!({"command": "true && rg needle src"})),
        });
        compound.handle_agent_event(AgentEvent::ToolCompleted {
            call_id: "cell:compound-search".to_string(),
            output: Ok(json!({"exit_code": 1, "stdout": "", "stderr": ""})),
            file_change: None,
            duration: Duration::from_millis(10),
        });
        compound.seal_exploration();
        let compound_transcript = compound.session_transcript();
        let compound_lines = compound.take_pending_history_lines(80, 24);

        let mut compound_replay = View::new(Path::new("/tmp/bettercodex"));
        compound_replay.welcome_pending = false;
        compound_replay.replay_transcript(compound_transcript);
        let compound_replay_lines = compound_replay.take_pending_history_lines(80, 24);
        for lines in [&compound_lines, &compound_replay_lines] {
            let failure_row = lines
                .iter()
                .position(|line| plain(line).contains("Failed true && rg needle src"))
                .expect("compound search failure row");
            let buffer = crate::tui::terminal::render_history_lines(lines, 80);
            assert_eq!(buffer[(0, failure_row as u16)].fg, Color::Red);
            let rendered = lines.iter().map(plain).collect::<Vec<_>>().join("\n");
            assert!(
                rendered.contains("process exited with status 1"),
                "{rendered}"
            );
        }

        let mut operator = View::new(Path::new("/tmp/bettercodex"));
        operator.welcome_pending = false;
        operator.start_operator_command("operator:empty-search".to_string(), "rg needle src");
        operator.finish_operator_command(
            "operator:empty-search",
            Ok(json!({"exit_code": 1, "stdout": "", "stderr": ""})),
        );
        let operator_transcript = operator.session_transcript();
        let [SessionTranscriptItem::Tool { tool }] = operator_transcript.as_slice() else {
            panic!("operator search should be saved as a command cell");
        };
        assert_eq!(tool.origin, SessionTranscriptToolOrigin::Operator);
        let Some(SessionTranscriptToolOutput::Success(output)) = &tool.output else {
            panic!("operator search should retain its structured output");
        };
        assert_eq!(output.get("exit_code").and_then(Value::as_i64), Some(1));

        let mut operator_replay = View::new(Path::new("/tmp/bettercodex"));
        operator_replay.welcome_pending = false;
        operator_replay.replay_transcript(operator_transcript);
        for view in [&mut operator, &mut operator_replay] {
            let lines = view.take_pending_history_lines(80, 24);
            let command_row = lines
                .iter()
                .position(|line| plain(line).contains("You ran rg needle src"))
                .expect("operator search command row");
            let buffer = crate::tui::terminal::render_history_lines(&lines, 80);
            assert_eq!(buffer[(0, command_row as u16)].fg, Color::Red);
            let rendered = lines.iter().map(plain).collect::<Vec<_>>().join("\n");
            assert!(!rendered.contains("Explored"), "{rendered}");
        }
    }

    #[test]
    fn direct_file_changes_render_and_replay_changed_rows_only() {
        let update = diffy::create_patch("alpha\nbeta\n", "alpha\ngamma\n").to_string();
        let multi_hunk_update = concat!(
            "--- original\n",
            "+++ modified\n",
            "@@ -1,2 +1,2 @@\n",
            "-one\n",
            "+ONE\n",
            " keep\n",
            "@@ -12,2 +12,2 @@\n",
            " x\n",
            "-last\n",
            "+LAST\n",
        );
        let cases = [
            (
                "write",
                json!({"path": "stale/new-from-input.rs", "content": "alpha\nbeta\n"}),
                ToolFileChange {
                    path: PathBuf::from("/tmp/bettercodex/new.rs"),
                    change: FileChange::Add {
                        content: "alpha\nbeta\n".to_string(),
                    },
                },
                vec!["• Added new.rs (+2 -0)", "1 +alpha", "2 +beta"],
            ),
            (
                "edit",
                json!({"path": "existing.rs", "edits": []}),
                ToolFileChange {
                    path: PathBuf::from("/tmp/bettercodex/existing.rs"),
                    change: FileChange::Update {
                        unified_diff: update,
                        move_path: None,
                    },
                },
                vec!["• Edited existing.rs (+1 -1)", "2 -beta", "2 +gamma"],
            ),
            (
                "edit",
                json!({"path": "separated.rs", "edits": []}),
                ToolFileChange {
                    path: PathBuf::from("/tmp/bettercodex/separated.rs"),
                    change: FileChange::Update {
                        unified_diff: multi_hunk_update.to_string(),
                        move_path: None,
                    },
                },
                vec![
                    "• Edited separated.rs (+2 -2)",
                    "1 -one",
                    "1 +ONE",
                    "⋮",
                    "13 -last",
                    "13 +LAST",
                ],
            ),
            (
                "edit",
                json!({"path": "obsolete.rs", "edits": []}),
                ToolFileChange {
                    path: PathBuf::from("/tmp/bettercodex/obsolete.rs"),
                    change: FileChange::Delete {
                        content: "old\ncontent\n".to_string(),
                    },
                },
                vec!["• Deleted obsolete.rs (+0 -2)", "1 -old", "2 -content"],
            ),
        ];

        for (name, input, file_change, expected) in cases {
            let mut view = View::new(Path::new("/tmp/bettercodex"));
            view.welcome_pending = false;
            view.handle_agent_event(AgentEvent::ToolStarted {
                call_id: "file-change".to_string(),
                name: name.to_string(),
                input: Some(input),
            });
            view.handle_agent_event(AgentEvent::ToolCompleted {
                call_id: "file-change".to_string(),
                output: Ok(json!("Done")),
                file_change: Some(file_change),
                duration: Duration::from_millis(1),
            });
            let transcript = view.session_transcript();
            let lines = view.take_pending_history_lines(80, 24);
            let buffer = crate::tui::terminal::render_history_lines(&lines, 80);
            let rendered = render_buffer(&buffer);
            let rendered_rows = rendered
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .collect::<Vec<_>>();
            assert_eq!(rendered_rows, expected, "{rendered}");

            let mut replay = View::new(Path::new("/tmp/bettercodex"));
            replay.welcome_pending = false;
            replay.replay_transcript(transcript);
            let replayed_rows = replay
                .take_pending_history_lines(80, 24)
                .iter()
                .map(|line| plain(line).trim().to_string())
                .filter(|line| !line.is_empty())
                .collect::<Vec<_>>();
            assert_eq!(replayed_rows, expected);
        }

        let narrow = file_change::lines(
            "wide.rs",
            &FileChange::Add {
                content: "x".repeat(2_049),
            },
            8,
            Style::default(),
        );
        assert!(narrow.len() < 100, "{} rendered rows", narrow.len());
        assert!(narrow.iter().all(|line| line_width(line) <= 8));
        assert!(plain(narrow.last().expect("truncated row")).contains('…'));
    }

    #[test]
    fn assistant_url_citations_render_as_clickable_sources() {
        let mut view = View::new(Path::new("/tmp/bettercodex"));
        view.welcome_pending = false;
        view.handle_agent_event(AgentEvent::ModelMessageCompleted(AssistantMessage {
            text: "A cited answer.\u{e200}cite\u{e202}turn0search2\u{e201}".to_string(),
            phase: Some(MessagePhase::FinalAnswer),
            citations: vec![crate::web_search::UrlCitation {
                start_index: 15,
                end_index: 18,
                url: "https://example.com/source".to_string(),
                title: "Example source".to_string(),
            }],
        }));

        let lines = view.take_pending_history_lines(80, 24);
        let rendered = lines.iter().map(plain).collect::<Vec<_>>().join("\n");
        assert!(!rendered.contains("turn0search2"), "{rendered}");
        assert!(!rendered.contains('\u{e200}'), "{rendered}");
        assert!(lines.iter().any(|line| plain(line).contains("Sources:")));
        let source = lines
            .iter()
            .find(|line| plain(line).contains("Example source"))
            .expect("citation source line");
        assert_eq!(source.hyperlinks.len(), 1);
        assert_eq!(
            source.hyperlinks[0].destination,
            "https://example.com/source"
        );
        assert_eq!(
            view.copy_latest_final_action(),
            Action::Copy(
                "A cited answer.\n\nSources:\n1. Example source: https://example.com/source"
                    .to_string()
            )
        );
    }

    #[test]
    fn fast_hosted_web_search_shows_an_active_frame_before_completion() {
        let mut view = View::new(Path::new("/tmp/bettercodex"));
        view.welcome_pending = false;
        view.start_turn(&UserPrompt::text("find current information"));
        let _ = view.take_pending_history_lines(80, 24);
        let active = crate::web_search::WebSearchCall {
            id: "ws_fast".to_string(),
            status: Some("in_progress".to_string()),
            action: Some(crate::web_search::WebSearchAction::Search {
                query: Some("current information".to_string()),
                queries: None,
            }),
        };
        let mut completed = active.clone();
        completed.status = Some("completed".to_string());

        view.handle_agent_event(AgentEvent::WebSearchStarted(active));
        view.handle_agent_event(AgentEvent::WebSearchCompleted(completed));

        let first_frame_at = Instant::now();
        assert!(view.advance_presentation(first_frame_at));
        let first_frame = view
            .active_lines(80)
            .iter()
            .map(plain)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            first_frame.contains("• Searching the web current information"),
            "{first_frame}"
        );
        assert!(!first_frame.contains("Searched the web"), "{first_frame}");
        assert!(view.has_pending_presentation());

        assert!(view.advance_presentation(first_frame_at + MIN_FRAME_INTERVAL));
        let second_frame = view
            .active_lines(80)
            .iter()
            .map(plain)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            second_frame.contains("• Searched the web for current information"),
            "{second_frame}"
        );
    }

    #[test]
    fn hosted_web_search_activity_replays_its_completed_action() {
        let mut view = View::new(Path::new("/tmp/bettercodex"));
        view.welcome_pending = false;
        let active = crate::web_search::WebSearchCall {
            id: "ws_1".to_string(),
            status: Some("in_progress".to_string()),
            action: Some(crate::web_search::WebSearchAction::Search {
                query: Some("ratatui activity".to_string()),
                queries: None,
            }),
        };
        view.handle_agent_event(AgentEvent::WebSearchStarted(active.clone()));
        assert_eq!(
            view.active_lines(80).iter().map(plain).collect::<Vec<_>>(),
            ["• Searching the web ratatui activity"]
        );

        let mut completed = active;
        completed.status = Some("completed".to_string());
        view.handle_agent_event(AgentEvent::WebSearchCompleted(completed));
        let transcript = view.session_transcript();
        let expected = ["• Searched the web for ratatui activity"];
        assert_eq!(
            view.take_pending_history_lines(80, 24)
                .iter()
                .map(plain)
                .collect::<Vec<_>>(),
            expected
        );

        let mut replay = View::new(Path::new("/tmp/bettercodex"));
        replay.welcome_pending = false;
        replay.replay_transcript(transcript);
        assert_eq!(
            replay
                .take_pending_history_lines(80, 24)
                .iter()
                .map(plain)
                .collect::<Vec<_>>(),
            expected
        );
    }

    #[test]
    fn hosted_web_search_terminal_statuses_are_not_rendered_as_success() {
        let failed = crate::web_search::WebSearchCall {
            id: "ws_failed".to_string(),
            status: Some("failed".to_string()),
            action: Some(crate::web_search::WebSearchAction::Search {
                query: None,
                queries: Some(vec!["unavailable source".to_string()]),
            }),
        };
        let mut failed_view = View::new(Path::new("/tmp/bettercodex"));
        failed_view.welcome_pending = false;
        failed_view.handle_agent_event(AgentEvent::WebSearchCompleted(failed));
        assert_eq!(
            failed_view
                .take_pending_history_lines(80, 24)
                .iter()
                .map(plain)
                .collect::<Vec<_>>(),
            ["• Web search failed for unavailable source"]
        );

        let interrupted = crate::web_search::WebSearchCall {
            id: "ws_interrupted".to_string(),
            status: Some("searching".to_string()),
            action: Some(crate::web_search::WebSearchAction::OpenPage {
                url: Some("https://example.com/interrupted".to_string()),
            }),
        };
        let mut interrupted_view = View::new(Path::new("/tmp/bettercodex"));
        interrupted_view.welcome_pending = false;
        interrupted_view.handle_agent_event(AgentEvent::WebSearchStarted(interrupted));
        interrupted_view.finish_incomplete_tools();
        let transcript = interrupted_view.session_transcript();
        let expected = ["• Web search interrupted for https://example.com/interrupted"];
        assert_eq!(
            interrupted_view
                .take_pending_history_lines(80, 24)
                .iter()
                .map(plain)
                .collect::<Vec<_>>(),
            expected
        );

        let mut replay = View::new(Path::new("/tmp/bettercodex"));
        replay.welcome_pending = false;
        replay.replay_transcript(transcript);
        assert_eq!(
            replay
                .take_pending_history_lines(80, 24)
                .iter()
                .map(plain)
                .collect::<Vec<_>>(),
            expected
        );
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

    #[test]
    fn rendering_survives_tiny_terminal_sizes() {
        for (width, height) in [(1, 1), (2, 3), (4, 4), (7, 6), (20, 8)] {
            let mut view = View::new(Path::new("/tmp/bettercodex"));
            view.welcome_pending = false;
            view.editor.set_text("👩‍💻e\u{301}\tdraft-with-a-long-token");
            view.handle_agent_event(AgentEvent::ToolStarted {
                call_id: "cell:tiny".to_string(),
                name: "bash".to_string(),
                input: Some(json!({
                    "command": "printf '👩‍💻é' https://example.com/a/long/unbroken/token"
                })),
            });
            view.handle_agent_event(AgentEvent::ToolOutputDelta {
                call_id: "cell:tiny".to_string(),
                stream: crate::process_runtime::OutputStream::Stdout,
                chunk: "界é\toutput-with-a-long-token\n".to_string(),
            });
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
