mod agent;
mod api;
mod assistant_message;
mod auth;
mod compaction;
mod context;
mod events;
mod input;
mod managed_session;
mod paths;
mod prompt_history;
mod repository;
mod rollout;
mod skill_settings;
mod skills;
mod state_file;
mod system_skills;
mod tools;
mod tui;
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
        Command::Run(options) => run_agent_command(&arguments, options, None),
        Command::Resume { selector, options } => {
            run_agent_command(&arguments, options, Some(selector))
        }
    }
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
        let answer = agent.submit_user_input(input).await?;
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
    ToolContextJson,
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

fn write_help() -> io::Result<()> {
    write_stdout_line(format_args!(
        "bcodex {}\n\nUsage:\n  bcodex [OPTIONS] [PROMPT]\n  bcodex resume [SESSION_ID] [OPTIONS] [PROMPT]\n  bcodex --tool-catalogue\n  bcodex --tool-catalogue-stats\n  bcodex --tool-context-json\n\nOptions:\n  -i, --image FILE           Attach a PNG, JPEG, WEBP, or GIF; repeat for more\n      --image-detail DETAIL  low, high, original, or auto [default: original]\n      --last                 Resume the latest session for the current directory\n      --tool-catalogue       Print the exact exec tool catalogue sent to Sol\n      --tool-catalogue-stats Summarize active tools and model-context cost\n      --tool-context-json    Print the rendered request-prefix audit input\n  -h, --help                 Show this help\n  -V, --version              Show the version\n\nWith no prompt, starts the interactive terminal UI. Run /tmux at any time to move the live session into a detachable c1, c2, … tmux session; macOS agent runs prevent idle sleep. Sessions are saved automatically under the Codex home directory.",
        env!("CARGO_PKG_VERSION")
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
    fn tool_catalogue_stats_are_derived_from_the_active_request() {
        assert!(matches!(
            Command::parse(["--tool-catalogue-stats".to_string()]).unwrap(),
            Command::ToolCatalogueStats
        ));
        assert_eq!(
            tool_catalogue_stats(),
            "Tool catalogue\n\nRequest tools (2): exec, wait\nInside exec (7): apply_patch, exec_command, log_papercut, update_plan, view_image, write_stdin, web__run\n\nExec description: 4353 bytes\nComplete additional_tools item: 5073 bytes\nEstimated context cost: 1269 tokens (bytes/4)\nEffective-window share: 0.49%"
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
