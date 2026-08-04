mod agent;
mod api;
mod auth;
mod context;
mod events;
mod input;
mod rollout;
mod tools;
mod tui;
mod usage;

use agent::Agent;
use anyhow::Result;
use anyhow::anyhow;
use input::ImageDetail;
use input::UserInput;
use rollout::ResumeSelector;
use std::io::IsTerminal;
use std::io::Write;
use std::path::PathBuf;
use std::str::FromStr;
use tokio::io::AsyncBufReadExt;
use tokio::io::BufReader;
use uuid::Uuid;

const MODEL: &str = "gpt-5.6-sol";

#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("error: {error:#}");
        std::process::exit(1);
    }
}

async fn run() -> Result<()> {
    let command = Command::parse(std::env::args().skip(1))?;
    match command {
        Command::Help => {
            print_help();
            Ok(())
        }
        Command::Version => {
            println!("bcodex {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        Command::ToolCatalogue => {
            println!("{}", tools::catalogue_text());
            Ok(())
        }
        Command::Run(options) => run_agent(options, None).await,
        Command::Resume { selector, options } => run_agent(options, Some(selector)).await,
    }
}

async fn run_agent(options: RunOptions, resume: Option<ResumeSelector>) -> Result<()> {
    let requested_cwd = std::env::current_dir()?;
    let mut agent = match resume {
        Some(selector) => Agent::resume(&requested_cwd, selector)?,
        None => Agent::new(&requested_cwd)?,
    };
    let cwd = agent.cwd().to_path_buf();
    if !options.prompt.is_empty() || !options.images.is_empty() {
        let input = UserInput::from_paths(options.prompt, &options.images, options.image_detail)?;
        let answer = agent.submit_user_input(input).await?;
        println!("{answer}");
        return Ok(());
    }

    if std::io::stdin().is_terminal() && std::io::stdout().is_terminal() {
        return tui::run(agent, cwd).await;
    }

    run_line_mode(&mut agent).await
}

async fn run_line_mode(agent: &mut Agent) -> Result<()> {
    eprintln!(
        "BetterCodex · {MODEL} · max · session {}",
        agent.session_id()
    );
    eprintln!("Commands run with your user permissions. Ctrl-D exits.\n");
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    loop {
        print!("> ");
        std::io::stdout().flush()?;
        let Some(line) = lines.next_line().await? else {
            println!();
            break;
        };
        let prompt = line.trim();
        if prompt.is_empty() {
            continue;
        }
        match agent.submit(prompt).await {
            Ok(answer) => println!("{answer}\n"),
            Err(error) => eprintln!("error: {error:#}\n"),
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

fn print_help() {
    println!(
        "bcodex {}\n\nUsage:\n  bcodex [OPTIONS] [PROMPT]\n  bcodex resume [SESSION_ID] [OPTIONS] [PROMPT]\n  bcodex --tool-catalogue\n\nOptions:\n  -i, --image FILE           Attach a PNG, JPEG, WEBP, or GIF; repeat for more\n      --image-detail DETAIL  low, high, original, or auto [default: original]\n      --last                 Resume the latest session for the current directory\n      --tool-catalogue       Print the exact exec tool catalogue sent to Sol\n  -h, --help                 Show this help\n  -V, --version              Show the version\n\nWith no prompt, starts the interactive terminal UI. Sessions are saved automatically under the Codex home directory.",
        env!("CARGO_PKG_VERSION")
    );
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
