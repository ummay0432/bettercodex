mod agent;
mod ansi_escape;
mod api;
mod assistant_message;
mod audio;
mod auth;
mod cache;
mod compaction;
mod context;
mod events;
mod file_search;
mod fuzzy_match;
mod http_client;
mod image;
mod input;
mod login;
mod managed_session;
mod openai_docs;
mod paths;
mod platform_fs;
mod process_runtime;
mod prompt_history;
mod protocol;
mod repository;
mod rollout;
mod shell_command;
mod skill_settings;
mod skills;
mod state_file;
mod system_skills;
mod terminal_color;
mod text;
mod time;
mod tools;
mod truncation;
mod tui;
mod update;
mod url_encoding;
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
        Command::InternalInstallSmoke => {
            tools::package_smoke_test().map_err(anyhow::Error::msg)?;
            let home = paths::bettercodex_home().ok_or_else(|| {
                anyhow!("install smoke test requires BCODEX_HOME or HOME to be set")
            })?;
            system_skills::install(&home)?;
            write_stdout_line(format_args!(
                "bcodex {} install smoke passed",
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
        Command::InternalInstallStage {
            destination,
            revision,
            build_input_hash,
        } => update::stage_current_binary(&destination, &revision, &build_input_hash),
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
    // Resolve signed-out and malformed credential states before terminal startup emits any
    // capability queries. Signed-out interactive launches continue into the onboarding UI.
    let tui_login_status = interactive_tui.then(login::status).transpose()?;
    let tui_startup = interactive_tui.then(tui::begin_startup).transpose()?;
    let mut runtime = tokio::runtime::Builder::new_multi_thread();
    if std::env::var_os("TOKIO_WORKER_THREADS").is_none() {
        // bettercodex serves one operator and moves blocking work off the async pool. Two workers
        // retain scheduler redundancy without eagerly creating one startup thread per CPU.
        runtime.worker_threads(2);
    }
    runtime.enable_all().build()?.block_on(run_agent(
        options,
        resume,
        worker_handoff,
        tui_startup,
        tui_login_status,
    ))
}

async fn run_agent(
    options: RunOptions,
    resume: Option<ResumeSelector>,
    worker_handoff: Option<managed_session::WorkerHandoff>,
    tui_startup: Option<tui::Startup>,
    tui_login_status: Option<login::LoginStatus>,
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
    if input.is_none() && std::io::stdin().is_terminal() && std::io::stdout().is_terminal() {
        let startup = match tui_startup {
            Some(startup) => startup,
            None => tui::begin_startup()?,
        };
        let login_status = match tui_login_status {
            Some(status) => status,
            None => login::status()?,
        };
        return tui::run(requested_cwd, resume, worker_handoff, startup, login_status).await;
    }

    let mut agent = match resume {
        Some(selector) => Agent::resume(&requested_cwd, selector)?,
        None => Agent::new(&requested_cwd)?,
    };
    if let Some(input) = input {
        let answer = agent.submit_user_input(input).await?;
        write_stdout_line(format_args!("{answer}"))?;
        return Ok(());
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
        let prompt = line.trim();
        if prompt.is_empty() {
            continue;
        }
        match agent.submit(prompt).await {
            Ok(answer) => write_stdout_line(format_args!("{answer}\n"))?,
            Err(error) => write_stderr_line(format_args!("error: {error:#}\n"))?,
        }
    }
    Ok(())
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
    InternalInstallSmoke,
    InternalInstallStage {
        destination: PathBuf,
        revision: String,
        build_input_hash: String,
    },
    InternalSourceRevision,
    Login(LoginCommand),
    Logout,
    LogoutHelp,
    Update,
    UpdateHelp,
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
            .is_some_and(|argument| argument == "--internal-install-stage")
        {
            arguments.next();
            let destination = arguments
                .next()
                .ok_or_else(|| anyhow!("internal install stage helper requires a destination"))?;
            let revision = arguments
                .next()
                .ok_or_else(|| anyhow!("internal install stage helper requires a revision"))?;
            let build_input_hash = arguments.next().ok_or_else(|| {
                anyhow!("internal install stage helper requires a build-input hash")
            })?;
            if arguments.next().is_some() {
                return Err(anyhow!(
                    "internal install stage helper received extra arguments"
                ));
            }
            return Ok(Self::InternalInstallStage {
                destination: PathBuf::from(destination),
                revision,
                build_input_hash,
            });
        }
        if arguments
            .peek()
            .is_some_and(|argument| argument == "--internal-install-smoke")
        {
            arguments.next();
            if arguments.next().is_some() {
                return Err(anyhow!(
                    "internal install smoke helper received extra arguments"
                ));
            }
            return Ok(Self::InternalInstallSmoke);
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
    let tmux_help = if cfg!(unix) {
        " Run /tmux at any time to move the live session into a detachable c1, c2, … tmux session; macOS agent runs prevent idle sleep."
    } else {
        ""
    };
    write_stdout_line(format_args!(
        "bcodex {}\n\nUsage:\n  bcodex [OPTIONS] [PROMPT]\n  bcodex resume [SESSION_ID] [OPTIONS] [PROMPT]\n  bcodex login [--device-auth]\n  bcodex login status\n  bcodex logout\n  bcodex update\n  bcodex --tool-catalogue\n  bcodex --tool-catalogue-stats\n\nCommands:\n  login                      Sign in with ChatGPT\n  logout                     Remove stored ChatGPT credentials\n  resume                     Resume a saved bettercodex session\n  update                     Install the latest public main revision\n\nOptions:\n  -i, --image FILE           Attach a PNG, JPEG, WEBP, or GIF; repeat for more\n      --image-detail DETAIL  low, high, original, or auto [default: original]\n      --last                 Resume the latest session for the current directory\n      --tool-catalogue       Print the exact exec tool catalogue sent to Sol\n      --tool-catalogue-stats Summarize active tools and model-context cost\n  -h, --help                 Show this help\n  -V, --version              Show the version\n\nWith no prompt, starts the interactive terminal UI. Use /review <target> there, or include $review <target> in any prompt, for active engineering review and refactoring; the agent may also select review proactively during implementation work.{tmux_help} Sessions are saved automatically under the Codex home directory.",
        env!("CARGO_PKG_VERSION"),
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
        "Install the latest public bettercodex main revision\n\nUsage:\n  bcodex update\n\nThe updater compares the binary's embedded source revision with public main. If they match, it exits immediately. Otherwise it pins that exact commit and runs the source installer, which reuses Cargo's fine-grained compilation cache, builds only changed artifacts, stamps and smoke-tests the candidate, and atomically replaces the installed command. Package versions do not control update freshness."
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
        let id = uuid::Uuid::new_v4();
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

        let command = Command::parse(["--".to_string(), "update".to_string()]).unwrap();
        let Command::Run(options) = command else {
            panic!("expected run command");
        };
        assert_eq!(options.prompt, "update");
    }

    #[test]
    fn internal_install_verification_commands_are_strictly_parsed() {
        assert!(matches!(
            Command::parse([
                "--internal-install-stage".to_string(),
                "/tmp/candidate".to_string(),
                "1".repeat(40),
                "2".repeat(64),
            ])
            .unwrap(),
            Command::InternalInstallStage {
                destination,
                revision,
                build_input_hash,
            }
                if destination.as_path() == std::path::Path::new("/tmp/candidate")
                    && revision == "1".repeat(40)
                    && build_input_hash == "2".repeat(64)
        ));
        assert!(matches!(
            Command::parse(["--internal-install-smoke".to_string()]).unwrap(),
            Command::InternalInstallSmoke
        ));
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
        assert!(
            Command::parse([
                "--internal-install-stage".to_string(),
                "/tmp/candidate".to_string(),
                "1".repeat(40),
            ])
            .is_err()
        );
        assert!(
            Command::parse([
                "--internal-install-stage".to_string(),
                "/tmp/candidate".to_string(),
                "1".repeat(40),
                "2".repeat(64),
                "unexpected".to_string(),
            ])
            .is_err()
        );
    }

    #[test]
    fn parses_tool_catalogue_flag() {
        assert!(matches!(
            Command::parse(["--tool-catalogue".to_string()]).unwrap(),
            Command::ToolCatalogue
        ));
    }

    #[test]
    fn only_the_first_resume_positional_can_select_a_session() {
        let id = uuid::Uuid::new_v4();
        let command =
            Command::parse(["resume".to_string(), "compare".to_string(), id.to_string()]).unwrap();
        let Command::Resume { selector, options } = command else {
            panic!("expected resume command");
        };
        assert_eq!(selector, ResumeSelector::LatestForCwd);
        assert_eq!(options.prompt, format!("compare {id}"));
    }
}
