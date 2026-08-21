use super::agent_switcher::AgentSwitcher;
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
use crate::events::SteerId;
use crate::input::UserPrompt;
use crate::input::file_attachment_text;
use crate::model::ModelSelection;
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
const ACTIVITY_SWITCHER_GAP: u16 = 1;
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
    specialist_coordination_enabled: bool,
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
    agent_switcher: AgentSwitcher,
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
            specialist_coordination_enabled: false,
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
            agent_switcher: AgentSwitcher::default(),
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

    pub(super) fn set_agent_switcher(&mut self, switcher: AgentSwitcher) {
        self.agent_switcher = switcher;
    }

    pub(super) fn session_switcher_blocked(&self) -> bool {
        self.overlay.is_some()
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

    pub(super) fn sync_context_snapshot(&mut self, snapshot: &ContextSnapshot) {
        self.context_tokens = Some(snapshot.used_tokens);
        self.ask_user_question_enabled = snapshot.ask_user_question_enabled;
        self.specialist_coordination_enabled = snapshot.specialist_coordination_enabled;
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
        self.specialist_coordination_enabled = snapshot.specialist_coordination_enabled;
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

    // Shard 2 activates this prepared view swap when session navigation becomes visible.
    #[allow(dead_code)]
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
                self.sync_context_snapshot(&snapshot);
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
                        view: ToolsView::under_context(
                            context.ask_user_question_enabled(),
                            context.specialist_coordination_enabled(),
                        ),
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
                    view: ToolsView::standalone(
                        self.ask_user_question_enabled,
                        self.specialist_coordination_enabled,
                    ),
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
        let switcher_height = self.agent_switcher.preferred_height();
        let activity_switcher_gap = if activity_height > 0 && switcher_height > 0 {
            ACTIVITY_SWITCHER_GAP
        } else {
            0
        };
        let activity_composer_gap = if activity_height > 0 || switcher_height > 0 {
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
            .saturating_add(activity_switcher_gap)
            .saturating_add(switcher_height)
            .saturating_add(activity_composer_gap)
            .saturating_add(composer_height)
            .saturating_add(trailing_height);
        (transcript_chrome_height, overlay_height)
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
        let requested_switcher_height = self.agent_switcher.preferred_height();
        let requested_activity_switcher_gap =
            if requested_activity_height > 0 && requested_switcher_height > 0 {
                ACTIVITY_SWITCHER_GAP
            } else {
                0
            };
        let requested_activity_composer_gap =
            if requested_activity_height > 0 || requested_switcher_height > 0 {
                ACTIVITY_COMPOSER_GAP
            } else {
                0
            };
        let available_cluster_height =
            height_above_trailing.saturating_sub(minimum_composer_height);
        let requested_content_height =
            requested_activity_height.saturating_add(requested_switcher_height);
        let available_gap_rows = available_cluster_height.saturating_sub(requested_content_height);
        let activity_switcher_gap_height = requested_activity_switcher_gap.min(available_gap_rows);
        let activity_composer_gap_height = requested_activity_composer_gap
            .min(available_gap_rows.saturating_sub(activity_switcher_gap_height));
        let available_rows = available_cluster_height
            .saturating_sub(activity_switcher_gap_height)
            .saturating_sub(activity_composer_gap_height);
        // Session navigation is the only route back to a live child or Main, so preserve its rows
        // before the informational activity line when the terminal is extremely short.
        let switcher_height = requested_switcher_height.min(available_rows);
        let activity_height =
            requested_activity_height.min(available_rows.saturating_sub(switcher_height));
        let activity_block_height = activity_height
            .saturating_add(activity_switcher_gap_height)
            .saturating_add(switcher_height)
            .saturating_add(activity_composer_gap_height);
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
        let switcher_bottom = composer_y.saturating_sub(activity_composer_gap_height);
        let switcher_top = switcher_bottom.saturating_sub(switcher_height);
        let activity_bottom = switcher_top.saturating_sub(activity_switcher_gap_height);
        let activity_top = activity_bottom.saturating_sub(activity_height);
        let activity_area = Rect::new(area.x, activity_top, area.width, activity_height);
        let switcher_area = Rect::new(area.x, switcher_top, area.width, switcher_height);
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
        if switcher_height > 0 {
            self.agent_switcher.render(frame, switcher_area);
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

        if self.overlay.is_none()
            && !self.agent_switcher.is_selecting()
            && self.editor.history_search_active()
            && !footer_area.is_empty()
        {
            let prefix_width = display_width("reverse-i-search: ");
            let query_width = self.editor.history_search_query().map_or(0, display_width);
            let cursor_x = footer_area
                .x
                .saturating_add(u16::try_from(prefix_width + query_width).unwrap_or(u16::MAX))
                .min(footer_area.right().saturating_sub(1));
            frame.set_cursor_position(Position::new(cursor_x, footer_area.y));
        } else if self.overlay.is_none() && !self.agent_switcher.is_selecting() {
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
    let Some(elapsed) = elapsed_seconds.map(format_elapsed) else {
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
    let bullet = match search.state {
        WebSearchState::Active => activity_marker(Some(search.started_at)),
        WebSearchState::Completed => "•".dim(),
        WebSearchState::Failed => "•".red().bold(),
        WebSearchState::Interrupted => "•".dim(),
    };
    let (header, tail) = web_search_label(search);
    let mut content = vec![Span::from(header).bold()];
    let tail = markdown::sanitize_inline(&tail);
    if !tail.is_empty() {
        content.push(Span::from(tail));
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

fn web_search_label(search: &WebSearchEntry) -> (&'static str, String) {
    use crate::web_search::WebSearchAction;

    match search.search.action.as_ref() {
        Some(WebSearchAction::Search { query, queries }) => {
            let header = match search.state {
                WebSearchState::Active => "Searching the web",
                WebSearchState::Completed => "Searched the web",
                WebSearchState::Failed => "Web search failed",
                WebSearchState::Interrupted => "Web search interrupted",
            };
            let detail = web_search_query_detail(query.as_deref(), queries.as_deref());
            let tail = if detail.is_empty() {
                if search.state == WebSearchState::Completed {
                    " (search terms unavailable)".to_string()
                } else {
                    String::new()
                }
            } else {
                format!(" for {detail}")
            };
            (header, tail)
        }
        Some(WebSearchAction::OpenPage { url }) => {
            let header = match search.state {
                WebSearchState::Active => "Opening",
                WebSearchState::Completed => "Opened",
                WebSearchState::Failed => "Failed to open",
                WebSearchState::Interrupted => "Opening interrupted:",
            };
            let target = url
                .as_deref()
                .filter(|url| !url.trim().is_empty())
                .unwrap_or("a search result");
            (header, format!(" {target}"))
        }
        Some(WebSearchAction::FindInPage { url, pattern }) => {
            let header = match search.state {
                WebSearchState::Active => "Searching a web page",
                WebSearchState::Completed => "Searched a web page",
                WebSearchState::Failed => "Failed to search a web page",
                WebSearchState::Interrupted => "Web-page search interrupted",
            };
            let url = url.as_deref().filter(|url| !url.trim().is_empty());
            let pattern = pattern
                .as_deref()
                .filter(|pattern| !pattern.trim().is_empty());
            let tail = match (pattern, url) {
                (Some(pattern), Some(url)) => format!(" for '{pattern}' at {url}"),
                (Some(pattern), None) => format!(" for '{pattern}'"),
                (None, Some(url)) => format!(" at {url}"),
                (None, None) if search.state == WebSearchState::Completed => {
                    " (page details unavailable)".to_string()
                }
                (None, None) => String::new(),
            };
            (header, tail)
        }
        Some(WebSearchAction::Other) | None => {
            let header = match search.state {
                WebSearchState::Active => "Searching the web",
                WebSearchState::Completed => "Web search completed",
                WebSearchState::Failed => "Web search failed",
                WebSearchState::Interrupted => "Web search interrupted",
            };
            let tail = if search.state == WebSearchState::Completed {
                " (details unavailable)".to_string()
            } else {
                String::new()
            };
            (header, tail)
        }
    }
}

fn web_search_query_detail(query: Option<&str>, queries: Option<&[String]>) -> String {
    if let Some(query) = query.filter(|query| !query.trim().is_empty()) {
        return query.to_string();
    }
    let first = queries
        .and_then(|queries| queries.iter().find(|query| !query.trim().is_empty()))
        .cloned()
        .unwrap_or_default();
    if queries.is_some_and(|queries| queries.len() > 1) && !first.is_empty() {
        format!("{first} ...")
    } else {
        first
    }
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
    use crate::context::ContextSnapshot;
    use crate::web_search::WebSearchCall;
    use serde_json::json;

    fn plain_line(line: &Line<'_>) -> String {
        line.spans.iter().fold(String::new(), |mut text, span| {
            text.push_str(span.content.as_ref());
            text
        })
    }

    fn rendered_web_search(item: Value) -> Vec<String> {
        let Some(search) = WebSearchCall::from_response_item(&item) else {
            panic!("test item should be a web search call");
        };
        web_search_lines(&WebSearchEntry::finished(search), 120)
            .iter()
            .map(plain_line)
            .collect()
    }

    #[test]
    fn final_message_separator_includes_every_measured_duration() {
        for (seconds, expected) in [(0, "0s"), (12, "12s"), (60, "1m 00s"), (61, "1m 01s")] {
            let rendered = final_message_separator_lines(Some(seconds), 80);

            assert_eq!(rendered.len(), 1);
            assert!(
                plain_line(&rendered[0]).contains(&format!("Worked for {expected}")),
                "separator omitted duration for {seconds} seconds"
            );
        }
    }

    #[test]
    fn web_search_open_without_url_names_the_action() {
        assert_eq!(
            rendered_web_search(json!({
                "type": "web_search_call",
                "id": "ws_open",
                "status": "completed",
                "action": {"type": "open_page"},
            })),
            ["• Opened a search result"]
        );
    }

    #[test]
    fn completed_web_searches_use_action_specific_labels() {
        assert_eq!(
            rendered_web_search(json!({
                "type": "web_search_call",
                "id": "ws_url",
                "status": "completed",
                "action": {
                    "type": "open_page",
                    "url": "https://example.com/docs",
                },
            })),
            ["• Opened https://example.com/docs"]
        );
        assert_eq!(
            rendered_web_search(json!({
                "type": "web_search_call",
                "id": "ws_find",
                "status": "completed",
                "action": {
                    "type": "find_in_page",
                    "url": "https://example.com/docs",
                    "pattern": "installation",
                },
            })),
            ["• Searched a web page for 'installation' at https://example.com/docs"]
        );
        assert_eq!(
            rendered_web_search(json!({
                "type": "web_search_call",
                "id": "ws_search",
                "status": "completed",
                "action": {"type": "search"},
            })),
            ["• Searched the web (search terms unavailable)"]
        );
        assert_eq!(
            rendered_web_search(json!({
                "type": "web_search_call",
                "id": "ws_unknown",
                "status": "completed",
                "action": {"type": "future_action"},
            })),
            ["• Web search completed (details unavailable)"]
        );
    }

    #[test]
    fn unmeasured_context_snapshot_is_available_to_the_footer() {
        let mut view = View::new(Path::new("/tmp/bettercodex-context-startup-test"));
        let snapshot = ContextSnapshot {
            used_tokens: 12_900,
            context_window: 258_000,
            compact_at_tokens: 246_000,
            measured: false,
            ask_user_question_enabled: false,
            specialist_coordination_enabled: false,
            sections: Vec::new(),
            total_usage: Default::default(),
            rate_limits: Vec::new(),
        };

        view.sync_context_snapshot(&snapshot);

        assert_eq!(view.context_tokens, Some(12_900));
        assert_eq!(
            format_context_usage(view.context_tokens, snapshot.context_window),
            "5% of 258K"
        );
    }
}
