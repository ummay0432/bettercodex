mod agent;
mod api;
mod assistant_message;
mod auth;
mod compaction;
mod context;
mod events;
mod input;
mod login;
mod managed_session;
mod openai_docs;
mod paths;
mod prompt_history;
mod quality_loop;
mod repository;
mod rollout;
mod skill_settings;
mod skills;
mod state_file;
mod system_skills;
mod tools;
mod tui;
mod update;
mod usage;
mod web_search;

use agent::Agent;
use anyhow::Result;
use anyhow::anyhow;
use input::ImageDetail;
use input::UserInput;
use rollout::ResumeSelector;
use std::fmt;
use std::io;
use std::io::IsTerminal;
use std::io::Write;
use std::path::PathBuf;
use std::str::FromStr;
use tokio::io::AsyncBufReadExt;
use tokio::io::BufReader;
use tokio::sync::mpsc::unbounded_channel;
use uuid::Uuid;

const MODEL: &str = "gpt-5.6-sol";

fn main() {
    if let Err(error) = run() {
        if is_broken_pipe(&error) {
            return;
        }
        let _ = write_stderr_line(format_args!("error: {error:#}"));
        std::process::exit(1);
    }
}

fn is_broken_pipe(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause
            .downcast_ref::<io::Error>()
            .is_some_and(|error| error.kind() == io::ErrorKind::BrokenPipe)
    })
}

fn write_stdout(arguments: fmt::Arguments<'_>) -> io::Result<()> {
    let mut output = io::stdout().lock();
    output.write_fmt(arguments)?;
    output.flush()
}

fn write_stdout_line(arguments: fmt::Arguments<'_>) -> io::Result<()> {
    let mut output = io::stdout().lock();
    writeln!(output, "{arguments}")
}

fn write_stderr_line(arguments: fmt::Arguments<'_>) -> io::Result<()> {
    let mut output = io::stderr().lock();
    writeln!(output, "{arguments}")
}

fn run() -> Result<()> {
    let arguments = std::env::args().skip(1).collect::<Vec<_>>();
    if let Some(result) = managed_session::run_relay_command(&arguments) {
        return result;
    }
    let command = Command::parse(arguments.iter().cloned())?;
    match command {
        Command::Help => {
            write_help()?;
            Ok(())
        }
        Command::Version => {
            write_stdout_line(format_args!("bcodex {}", env!("CARGO_PKG_VERSION")))?;
            Ok(())
        }
        Command::ToolCatalogue => {
            write_stdout_line(format_args!("{}", tools::catalogue_text()))?;
            Ok(())
        }
        Command::ToolCatalogueStats => {
            write_stdout_line(format_args!("{}", tool_catalogue_stats()))?;
            Ok(())
        }
        Command::ToolContextJson => {
            let cwd = std::env::current_dir()?.canonicalize()?;
            write_stdout_line(format_args!(
                "{}",
                serde_json::json!({
                    "instructions": api::harness_instructions(),
                    "stable_prefix": api::stable_request_prefix(),
                    "world_state": context::initial_context_items(&cwd)?,
                })
            ))?;
            Ok(())
        }
        Command::InternalPackageSmoke => {
            tools::package_smoke_test().map_err(anyhow::Error::msg)?;
            write_stdout_line(format_args!(
                "bcodex {} package smoke passed",
                env!("CARGO_PKG_VERSION")
            ))?;
            Ok(())
        }
        Command::InternalSourceRevision => {
            let revision = update::source_revision()
                .ok_or_else(|| anyhow!("this build has no embedded source revision"))?;
            write_stdout_line(format_args!("{revision}"))?;
            Ok(())
        }
        Command::Login(command) => run_login_command(command),
        Command::Logout => run_logout_command(),
        Command::LogoutHelp => {
            write_logout_help()?;
            Ok(())
        }
        Command::Update => update::run_update(),
        Command::UpdateHelp => {
            write_update_help()?;
            Ok(())
        }
        Command::InternalLoopState { run_root, contract } => {
            let cwd = std::env::current_dir()?;
            write_stdout_line(format_args!(
                "{}",
                quality_loop::capture_state_identity(&cwd, &run_root, &contract)?
            ))?;
            Ok(())
        }
        Command::Run(options) => run_agent_command(&arguments, options, None),
        Command::Resume { selector, options } => {
            run_agent_command(&arguments, options, Some(selector))
        }
    }
}

fn run_login_command(command: LoginCommand) -> Result<()> {
    let mode = match command {
        LoginCommand::Browser => login::LoginMode::Browser,
        LoginCommand::DeviceCode => login::LoginMode::DeviceCode,
        LoginCommand::Status => {
            return match login::status()? {
                login::LoginStatus::ChatGpt => {
                    write_stderr_line(format_args!("Logged in using ChatGPT"))?;
                    Ok(())
                }
                login::LoginStatus::AccessToken => {
                    write_stderr_line(format_args!("Logged in using access token"))?;
                    Ok(())
                }
                login::LoginStatus::NotLoggedIn => Err(anyhow!("Not logged in")),
            };
        }
        LoginCommand::Help => {
            write_login_help()?;
            return Ok(());
        }
    };

    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(login::login(mode))?;
    write_stderr_line(format_args!("Successfully logged in"))?;
    Ok(())
}

fn run_logout_command() -> Result<()> {
    let removed = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(login::logout())?;
    let message = if removed {
        "Successfully logged out"
    } else {
        "Not logged in"
    };
    write_stderr_line(format_args!("{message}"))?;
    Ok(())
}

fn run_agent_command(
    arguments: &[String],
    options: RunOptions,
    resume: Option<ResumeSelector>,
) -> Result<()> {
    let interactive_terminal = std::io::stdin().is_terminal() && std::io::stdout().is_terminal();
    let interactive_tui =
        interactive_terminal && options.prompt.is_empty() && options.images.is_empty();
    let worker_handoff = managed_session::enter_agent_process(arguments, interactive_tui)?;
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(run_agent(options, resume, worker_handoff))
}

async fn run_agent(
    options: RunOptions,
    resume: Option<ResumeSelector>,
    worker_handoff: Option<managed_session::WorkerHandoff>,
) -> Result<()> {
    let input = if !options.prompt.is_empty() || !options.images.is_empty() {
        Some(UserInput::from_paths(
            options.prompt,
            &options.images,
            options.image_detail,
        )?)
    } else {
        None
    };
    let requested_cwd = std::env::current_dir()?;
    let mut agent = match resume {
        Some(selector) => Agent::resume(&requested_cwd, selector)?,
        None => Agent::new(&requested_cwd)?,
    };
    let cwd = agent.cwd().to_path_buf();
    if let Some(input) = input {
        let answer = submit_cli_input(&mut agent, input).await?;
        write_stdout_line(format_args!("{answer}"))?;
        return Ok(());
    }

    if std::io::stdin().is_terminal() && std::io::stdout().is_terminal() {
        return tui::run(agent, cwd, worker_handoff).await;
    }

    run_line_mode(&mut agent).await
}

async fn run_line_mode(agent: &mut Agent) -> Result<()> {
    write_stderr_line(format_args!(
        "bettercodex · {MODEL} · max · session {}",
        agent.session_id()
    ))?;
    write_stderr_line(format_args!(
        "Commands run with your user permissions. Ctrl-D exits.\n"
    ))?;
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    loop {
        write_stdout(format_args!("> "))?;
        let Some(line) = lines.next_line().await? else {
            write_stdout_line(format_args!(""))?;
            break;
        };
        if line.trim().is_empty() {
            continue;
        }
        match submit_cli_input(agent, UserInput::text(line)).await {
            Ok(answer) => write_stdout_line(format_args!("{answer}\n"))?,
            Err(error) => write_stderr_line(format_args!("error: {error:#}\n"))?,
        }
    }
    Ok(())
}

async fn submit_cli_input(agent: &mut Agent, input: UserInput) -> Result<String> {
    let invocation = quality_loop::parse_invocation_with_mode(
        input.submitted_text(),
        input.has_attachments(),
        false,
    )?;
    let Some(invocation) = invocation else {
        return agent.submit_user_input(input).await;
    };
    let (events_tx, mut events_rx) = unbounded_channel();
    let progress = tokio::spawn(async move {
        while let Some(event) = events_rx.recv().await {
            match event {
                events::AgentEvent::LoopProgress(progress) => {
                    write_stderr_line(format_args!("{}", progress.stderr_line()))?;
                }
                events::AgentEvent::Warning(warning) => {
                    write_stderr_line(format_args!("warning: {warning}"))?;
                }
                _ => {}
            }
        }
        Ok::<(), io::Error>(())
    });
    let (_, control) = agent::TurnControl::non_steerable_channel();
    let outcome =
        quality_loop::submit_with_control(agent, input, invocation, events_tx, control).await?;
    progress.await??;
    match outcome {
        agent::SubmitOutcome::Completed(answer) => Ok(answer),
        agent::SubmitOutcome::Cancelled => Err(anyhow!("quality loop was cancelled")),
    }
}

enum Command {
    Run(RunOptions),
    Resume {
        selector: ResumeSelector,
        options: RunOptions,
    },
    Help,
    Version,
    ToolCatalogue,
    ToolCatalogueStats,
    ToolContextJson,
    InternalPackageSmoke,
    InternalSourceRevision,
    Login(LoginCommand),
    Logout,
    LogoutHelp,
    Update,
    UpdateHelp,
    InternalLoopState {
        run_root: PathBuf,
        contract: PathBuf,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LoginCommand {
    Browser,
    DeviceCode,
    Status,
    Help,
}

#[derive(Default)]
struct RunOptions {
    prompt: String,
    images: Vec<PathBuf>,
    image_detail: ImageDetail,
}

impl Command {
    fn parse(arguments: impl IntoIterator<Item = String>) -> Result<Self> {
        let mut arguments = arguments.into_iter().peekable();
        if arguments
            .peek()
            .is_some_and(|argument| argument == "--internal-package-smoke")
        {
            arguments.next();
            if arguments.next().is_some() {
                return Err(anyhow!(
                    "internal package smoke helper received extra arguments"
                ));
            }
            return Ok(Self::InternalPackageSmoke);
        }
        if arguments
            .peek()
            .is_some_and(|argument| argument == "--internal-source-revision")
        {
            arguments.next();
            if arguments.next().is_some() {
                return Err(anyhow!(
                    "internal source revision helper received extra arguments"
                ));
            }
            return Ok(Self::InternalSourceRevision);
        }
        if arguments
            .peek()
            .is_some_and(|argument| argument == "--internal-loop-state")
        {
            arguments.next();
            let run_root = arguments
                .next()
                .ok_or_else(|| anyhow!("internal loop state helper requires a run directory"))?;
            let contract = arguments
                .next()
                .ok_or_else(|| anyhow!("internal loop state helper requires a contract"))?;
            if arguments.next().is_some() {
                return Err(anyhow!(
                    "internal loop state helper received extra arguments"
                ));
            }
            return Ok(Self::InternalLoopState {
                run_root: PathBuf::from(run_root),
                contract: PathBuf::from(contract),
            });
        }
        if arguments
            .peek()
            .is_some_and(|argument| argument == "--help" || argument == "-h")
        {
            return Ok(Self::Help);
        }
        if arguments
            .peek()
            .is_some_and(|argument| argument == "--version" || argument == "-V")
        {
            return Ok(Self::Version);
        }
        if arguments
            .peek()
            .is_some_and(|argument| argument == "--tool-catalogue" || argument == "--tool-catalog")
        {
            return Ok(Self::ToolCatalogue);
        }
        if arguments.peek().is_some_and(|argument| {
            argument == "--tool-catalogue-stats" || argument == "--tool-catalog-stats"
        }) {
            return Ok(Self::ToolCatalogueStats);
        }
        if arguments
            .peek()
            .is_some_and(|argument| argument == "--tool-context-json")
        {
            return Ok(Self::ToolContextJson);
        }
        if arguments.peek().is_some_and(|argument| argument == "login") {
            arguments.next();
            return parse_login_command(arguments);
        }
        if arguments
            .peek()
            .is_some_and(|argument| argument == "logout")
        {
            arguments.next();
            return parse_logout_command(arguments);
        }
        if arguments
            .peek()
            .is_some_and(|argument| argument == "update")
        {
            arguments.next();
            return parse_update_command(arguments);
        }

        let resume = arguments
            .peek()
            .is_some_and(|argument| argument == "resume");
        if resume {
            arguments.next();
        }
        let mut selector = ResumeSelector::LatestForCwd;
        let mut options = RunOptions::default();
        let mut prompt = Vec::new();
        let mut positional_only = false;
        while let Some(argument) = arguments.next() {
            if positional_only {
                prompt.push(argument);
                continue;
            }
            match argument.as_str() {
                "--" => positional_only = true,
                "--help" | "-h" => return Ok(Self::Help),
                "--version" | "-V" => return Ok(Self::Version),
                "--last" if resume => selector = ResumeSelector::LatestForCwd,
                "--image" | "-i" => {
                    let path = arguments
                        .next()
                        .ok_or_else(|| anyhow!("{argument} requires a file path"))?;
                    options.images.push(PathBuf::from(path));
                }
                "--image-detail" => {
                    let detail = arguments
                        .next()
                        .ok_or_else(|| anyhow!("--image-detail requires a value"))?;
                    options.image_detail = ImageDetail::from_str(&detail)?;
                }
                value if value.starts_with('-') => {
                    return Err(anyhow!("unknown option `{value}`"));
                }
                value
                    if resume
                        && prompt.is_empty()
                        && matches!(selector, ResumeSelector::LatestForCwd) =>
                {
                    if let Ok(id) = Uuid::parse_str(value) {
                        selector = ResumeSelector::Id(id);
                    } else {
                        prompt.push(value.to_string());
                    }
                }
                value => prompt.push(value.to_string()),
            }
        }
        options.prompt = prompt.join(" ");
        if resume {
            Ok(Self::Resume { selector, options })
        } else {
            Ok(Self::Run(options))
        }
    }
}

fn parse_login_command(arguments: impl IntoIterator<Item = String>) -> Result<Command> {
    let arguments = arguments.into_iter().collect::<Vec<_>>();
    match arguments.as_slice() {
        [] => Ok(Command::Login(LoginCommand::Browser)),
        [argument] if argument == "--device-auth" => Ok(Command::Login(LoginCommand::DeviceCode)),
        [argument] if argument == "status" => Ok(Command::Login(LoginCommand::Status)),
        [argument] if argument == "--help" || argument == "-h" => {
            Ok(Command::Login(LoginCommand::Help))
        }
        [argument, ..] => Err(anyhow!("unknown login argument `{argument}`")),
    }
}

fn parse_logout_command(arguments: impl IntoIterator<Item = String>) -> Result<Command> {
    let arguments = arguments.into_iter().collect::<Vec<_>>();
    match arguments.as_slice() {
        [] => Ok(Command::Logout),
        [argument] if argument == "--help" || argument == "-h" => Ok(Command::LogoutHelp),
        [argument, ..] => Err(anyhow!("unknown logout argument `{argument}`")),
    }
}

fn parse_update_command(arguments: impl IntoIterator<Item = String>) -> Result<Command> {
    let arguments = arguments.into_iter().collect::<Vec<_>>();
    match arguments.as_slice() {
        [] => Ok(Command::Update),
        [argument] if argument == "--help" || argument == "-h" => Ok(Command::UpdateHelp),
        [argument, ..] => Err(anyhow!("unknown update argument `{argument}`")),
    }
}

fn write_help() -> io::Result<()> {
    write_stdout_line(format_args!(
        "bcodex {}\n\nUsage:\n  bcodex [OPTIONS] [PROMPT]\n  bcodex resume [SESSION_ID] [OPTIONS] [PROMPT]\n  bcodex login [--device-auth]\n  bcodex login status\n  bcodex logout\n  bcodex update\n  bcodex --tool-catalogue\n  bcodex --tool-catalogue-stats\n  bcodex --tool-context-json\n\nCommands:\n  login                      Sign in with ChatGPT\n  logout                     Remove stored ChatGPT credentials\n  resume                     Resume a saved bettercodex session\n  update                     Build and install the latest integrated source\n\nOptions:\n  -i, --image FILE           Attach a PNG, JPEG, WEBP, or GIF; repeat for more\n      --image-detail DETAIL  low, high, original, or auto [default: original]\n      --last                 Resume the latest session for the current directory\n      --tool-catalogue       Print the exact exec tool catalogue sent to Sol\n      --tool-catalogue-stats Summarize active tools and model-context cost\n      --tool-context-json    Print the rendered request-prefix audit input\n  -h, --help                 Show this help\n  -V, --version              Show the version\n\nWith no prompt, starts the interactive terminal UI. Use /loop <task> there, or include $loop in any prompt, to run the opt-in evaluator-backed quality loop (three working sessions by default). Run /tmux at any time to move the live session into a detachable c1, c2, … tmux session; macOS agent runs prevent idle sleep. Sessions are saved automatically under the Codex home directory.",
        env!("CARGO_PKG_VERSION")
    ))
}

fn write_login_help() -> io::Result<()> {
    write_stdout_line(format_args!(
        "Sign in with ChatGPT\n\nUsage:\n  bcodex login [OPTIONS]\n  bcodex login status\n\nOptions:\n      --device-auth  Use device code authentication for remote or headless machines\n  -h, --help         Show this help"
    ))
}

fn write_logout_help() -> io::Result<()> {
    write_stdout_line(format_args!(
        "Remove stored ChatGPT credentials\n\nUsage:\n  bcodex logout\n\nOptions:\n  -h, --help  Show this help"
    ))
}

fn write_update_help() -> io::Result<()> {
    write_stdout_line(format_args!(
        "Build and install the latest integrated bettercodex source\n\nUsage:\n  bcodex update\n\nThe update uses the authenticated GitHub CLI, resolves the current main commit, compiles that immutable source snapshot for this machine, smoke-tests its runtime and embedded resources, and then replaces the installed binary."
    ))
}

fn tool_catalogue_stats() -> String {
    let tools = tools::display_tools();
    let names = |route| {
        tools
            .iter()
            .filter(|tool| tool.route == route)
            .map(|tool| tool.name.as_str())
            .collect::<Vec<_>>()
    };
    let request = names(tools::CatalogueRoute::Request);
    let nested = names(tools::CatalogueRoute::InsideExec);
    let metrics = tools::catalogue_metrics();
    let window_share =
        metrics.estimated_tokens as f64 * 100.0 / context::EFFECTIVE_CONTEXT_WINDOW as f64;
    format!(
        "Tool catalogue\n\nRequest tools ({}): {}\nInside exec ({}): {}\n\nExec description: {} bytes\nComplete additional_tools item: {} bytes\nEstimated context cost: {} tokens (bytes/4)\nEffective-window share: {window_share:.2}%",
        request.len(),
        request.join(", "),
        nested.len(),
        nested.join(", "),
        metrics.description_bytes,
        metrics.request_bytes,
        metrics.estimated_tokens,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_resume_images_and_detail() {
        let id = Uuid::new_v4();
        let command = Command::parse([
            "resume".to_string(),
            id.to_string(),
            "--image".to_string(),
            "screen.png".to_string(),
            "--image-detail".to_string(),
            "low".to_string(),
            "inspect".to_string(),
        ])
        .unwrap();
        let Command::Resume { selector, options } = command else {
            panic!("expected resume command");
        };
        assert_eq!(selector, ResumeSelector::Id(id));
        assert_eq!(options.images, vec![PathBuf::from("screen.png")]);
        assert_eq!(options.image_detail, ImageDetail::Low);
        assert_eq!(options.prompt, "inspect");
    }

    #[test]
    fn rejects_unknown_options() {
        assert!(Command::parse(["--model".to_string()]).is_err());
    }

    #[test]
    fn parses_upstream_login_and_logout_commands() {
        assert!(matches!(
            Command::parse(["login".to_string()]).unwrap(),
            Command::Login(LoginCommand::Browser)
        ));
        assert!(matches!(
            Command::parse(["login".to_string(), "--device-auth".to_string()]).unwrap(),
            Command::Login(LoginCommand::DeviceCode)
        ));
        assert!(matches!(
            Command::parse(["login".to_string(), "status".to_string()]).unwrap(),
            Command::Login(LoginCommand::Status)
        ));
        assert!(matches!(
            Command::parse(["logout".to_string()]).unwrap(),
            Command::Logout
        ));
        assert!(Command::parse(["login".to_string(), "unexpected".to_string()]).is_err());
        assert!(Command::parse(["logout".to_string(), "unexpected".to_string()]).is_err());
    }

    #[test]
    fn parses_update_command_and_help() {
        assert!(matches!(
            Command::parse(["update".to_string()]).unwrap(),
            Command::Update
        ));
        assert!(matches!(
            Command::parse(["update".to_string(), "--help".to_string()]).unwrap(),
            Command::UpdateHelp
        ));
        assert!(Command::parse(["update".to_string(), "unexpected".to_string()]).is_err());
    }

    #[test]
    fn positional_separator_preserves_login_as_a_prompt() {
        let command = Command::parse(["--".to_string(), "login".to_string()]).unwrap();
        let Command::Run(options) = command else {
            panic!("expected run command");
        };
        assert_eq!(options.prompt, "login");
    }

    #[test]
    fn parses_tool_catalogue_flag() {
        assert!(matches!(
            Command::parse(["--tool-catalogue".to_string()]).unwrap(),
            Command::ToolCatalogue
        ));
    }

    #[test]
    fn parses_tool_context_json_flag() {
        assert!(matches!(
            Command::parse(["--tool-context-json".to_string()]).unwrap(),
            Command::ToolContextJson
        ));
    }

    #[test]
    fn internal_package_smoke_is_strictly_parsed() {
        assert!(matches!(
            Command::parse(["--internal-package-smoke".to_string()]).unwrap(),
            Command::InternalPackageSmoke
        ));
        assert!(
            Command::parse([
                "--internal-package-smoke".to_string(),
                "unexpected".to_string(),
            ])
            .is_err()
        );
    }

    #[test]
    fn internal_source_revision_is_strictly_parsed() {
        assert!(matches!(
            Command::parse(["--internal-source-revision".to_string()]).unwrap(),
            Command::InternalSourceRevision
        ));
        assert!(
            Command::parse([
                "--internal-source-revision".to_string(),
                "unexpected".to_string(),
            ])
            .is_err()
        );
    }

    #[test]
    fn tool_catalogue_stats_are_derived_from_the_active_request() {
        assert!(matches!(
            Command::parse(["--tool-catalogue-stats".to_string()]).unwrap(),
            Command::ToolCatalogueStats
        ));
        assert_eq!(
            tool_catalogue_stats(),
            "Tool catalogue\n\nRequest tools (2): exec, wait\nInside exec (12): apply_patch, exec_command, log_papercut, update_plan, view_image, write_stdin, openaiDeveloperDocs__fetch_openai_doc, openaiDeveloperDocs__get_openapi_spec, openaiDeveloperDocs__list_api_endpoints, openaiDeveloperDocs__list_openai_docs, openaiDeveloperDocs__search_openai_docs, web__run\n\nExec description: 6094 bytes\nComplete additional_tools item: 6824 bytes\nEstimated context cost: 1706 tokens (bytes/4)\nEffective-window share: 0.66%"
        );
    }

    #[test]
    fn only_the_first_resume_positional_can_select_a_session() {
        let id = Uuid::new_v4();
        let command =
            Command::parse(["resume".to_string(), "compare".to_string(), id.to_string()]).unwrap();
        let Command::Resume { selector, options } = command else {
            panic!("expected resume command");
        };
        assert_eq!(selector, ResumeSelector::LatestForCwd);
        assert_eq!(options.prompt, format!("compare {id}"));
    }
}
