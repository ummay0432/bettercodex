Warning: truncated output (original token count: 21136)
Total output lines: 2531

use super::bash::extract_bash_command;
use super::bash::try_parse_shell;
use super::bash::try_parse_word_only_commands_sequence;
use super::powershell::extract_powershell_command;
use crate::protocol::ParsedCommand;
use shlex::split as shlex_split;
use shlex::try_join as shlex_try_join;
use std::path::PathBuf;

pub fn shlex_join(tokens: &[String]) -> String {
    shlex_try_join(tokens.iter().map(String::as_str))
        .unwrap_or_else(|_| "<command included NUL byte>".to_string())
}
/// Extracts the shell and script from a command, regardless of platform
pub fn extract_shell_command(command: &[String]) -> Option<(&str, &str)> {
    extract_bash_command(command).or_else(|| extract_powershell_command(command))
}

/// DO NOT REVIEW THIS CODE BY HAND
/// This parsing code is quite complex and not easy to hand-modify.
/// The easiest way to iterate is to add unit tests and have Codex fix the implementation.
/// To encourage this, the tests have been put directly below this function rather than at the bottom of the
///
/// Parses metadata out of an arbitrary command.
/// These commands are model driven and could include just about anything.
/// The parsing is slightly lossy due to the ~infinite expressiveness of an arbitrary command.
/// The goal of the parsed metadata is to be able to provide the user with a human readable gis
/// of what it is doing.
pub fn parse_command(command: &[String]) -> Vec<ParsedCommand> {
    // Parse and then collapse consecutive duplicate commands to avoid redundant summaries.
    let parsed = parse_command_impl(command);
    let mut deduped: Vec<ParsedCommand> = Vec::with_capacity(parsed.len());
    for cmd in parsed.into_iter() {
        if deduped.last().is_some_and(|prev| prev == &cmd) {
            continue;
        }
        deduped.push(cmd);
    }
    if deduped
        .iter()
        .any(|cmd| matches!(cmd, ParsedCommand::Unknown { .. }))
    {
        vec![single_unknown_for_command(command)]
    } else {
        deduped
    }
}

fn single_unknown_for_command(command: &[String]) -> ParsedCommand {
    if let Some((_, shell_command)) = extract_shell_command(command) {
        ParsedCommand::Unknown {
            cmd: shell_command.to_string(),
        }
    } else {
        ParsedCommand::Unknown {
            cmd: shlex_join(command),
        }
    }
}

#[cfg(test)]
#[allow(clippy::items_after_test_module)]
/// Tests are at the top to encourage using TDD + Codex to fix the implementation.
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use std::path::PathBuf;
    use std::string::ToString;

    fn shlex_split_safe(s: &str) -> Vec<String> {
        shlex_split(s).unwrap_or_else(|| s.split_whitespace().map(ToString::to_string).collect())
    }

    fn vec_str(args: &[&str]) -> Vec<String> {
        args.iter().map(ToString::to_string).collect()
    }

    fn assert_parsed(args: &[String], expected: Vec<ParsedCommand>) {
        let out = parse_command(args);
        assert_eq!(out, expected);
    }

    #[test]
    fn git_status_is_unknown() {
        assert_parsed(
            &vec_str(&["git", "status"]),
            vec![ParsedCommand::Unknown {
                cmd: "git status".to_string(),
            }],
        );
    }

    #[test]
    fn supports_git_grep_and_ls_files() {
        assert_parsed(
            &shlex_split_safe("git grep TODO src"),
            vec![ParsedCommand::Search {
                cmd: "git grep TODO src".to_string(),
                query: Some("TODO".to_string()),
                path: Some("src".to_string()),
            }],
        );
        assert_parsed(
            &shlex_split_safe("git grep -l TODO src"),
            vec![ParsedCommand::Search {
                cmd: "git grep -l TODO src".to_string(),
                query: Some("TODO".to_string()),
                path: Some("src".to_string()),
            }],
        );
        assert_parsed(
            &shlex_split_safe("git ls-files"),
            vec![ParsedCommand::ListFiles {
                cmd: "git ls-files".to_string(),
                path: None,
            }],
        );
        assert_parsed(
            &shlex_split_safe("git ls-files src"),
            vec![ParsedCommand::ListFiles {
                cmd: "git ls-files src".to_string(),
                path: Some("src".to_string()),
            }],
        );
        assert_parsed(
            &shlex_split_safe("git ls-files --exclude target src"),
            vec![ParsedCommand::ListFiles {
                cmd: "git ls-files --exclude target src".to_string(),
                path: Some("src".to_string()),
            }],
        );
    }

    #[test]
    fn handles_git_pipe_wc() {
        let inner = "git status | wc -l";
        assert_parsed(
            &vec_str(&["bash", "-lc", inner]),
            vec![ParsedCommand::Unknown {
                cmd: inner.to_string(),
            }],
        );
    }

    #[test]
    fn bash_lc_redirect_not_quoted() {
        let inner = "echo foo > bar";
        assert_parsed(
            &vec_str(&["bash", "-lc", inner]),
            vec![ParsedCommand::Unknown {
                cmd: "echo foo > bar".to_string(),
            }],
        );
    }

    #[test]
    fn handles_complex_bash_command_head() {
        let inner =
            "rg --version && node -v && pnpm -v && rg --files | wc -l && rg --files | head -n 40";
        assert_parsed(
            &vec_str(&["bash", "-lc", inner]),
            vec![ParsedCommand::Unknown {
                cmd: inner.to_string(),
            }],
        );
    }

    #[test]
    fn supports_searching_for_navigate_to_route() -> anyhow::Result<()> {
        let inner = "rg -n \"navigate-to-route\" -S";
        assert_parsed(
            &vec_str(&["bash", "-lc", inner]),
            vec![ParsedCommand::Search {
                cmd: "rg -n navigate-to-route -S".to_string(),
                query: Some("navigate-to-route".to_string()),
                path: None,
            }],
        );
        Ok(())
    }

    #[test]
    fn handles_complex_bash_command() {
        let inner = "rg -n \"BUG|FIXME|TODO|XXX|HACK\" -S | head -n 200";
        assert_parsed(
            &vec_str(&["bash", "-lc", inner]),
            vec![ParsedCommand::Search {
                cmd: "rg -n 'BUG|FIXME|TODO|XXX|HACK' -S".to_string(),
                query: Some("BUG|FIXME|TODO|XXX|HACK".to_string()),
                path: None,
            }],
        );
    }

    #[test]
    fn supports_rg_files_with_path_and_pipe() {
        let inner = "rg --files webview/src | sed -n";
        assert_parsed(
            &vec_str(&["bash", "-lc", inner]),
            vec![ParsedCommand::ListFiles {
                cmd: "rg --files webview/src".to_string(),
                path: Some("webview".to_string()),
            }],
        );
    }

    #[test]
    fn supports_rg_files_then_head() {
        let inner = "rg --files | head -n 50";
        assert_parsed(
            &vec_str(&["bash", "-lc", inner]),
            vec![ParsedCommand::ListFiles {
                cmd: "rg --files".to_string(),
                path: None,
            }],
        );
    }

    #[test]
    fn keeps_mutating_xargs_pipeline() {
        let inner = r#"rg -l QkBindingController presentation/src/main/java | xargs perl -pi -e 's/QkBindingController/QkController/g'"#;
        assert_parsed(
            &vec_str(&["bash", "-lc", inner]),
            vec![ParsedCommand::Unknown {
                cmd: inner.to_string(),
            }],
        );
    }

    #[test]
    fn collapses_plain_pipeline_when_any_stage_is_unknown() {
        let command = shlex_split_safe(
            "rg -l QkBindingController presentation/src/main/java | xargs perl -pi -e 's/QkBindingController/QkController/g'",
        );
        assert_parsed(
            &command,
            vec![ParsedCommand::Unknown {
                cmd: shlex_join(&command),
            }],
        );
    }

    #[test]
    fn collapses_pipeline_with_helper_when_later_stage_is_unknown() {
        let command = shlex_split_safe("rg --files | nl -ba | foo");
        assert_parsed(
            &command,
            vec![ParsedCommand::Unknown {
                cmd: shlex_join(&command),
            }],
        );
    }

    #[test]
    fn rg_files_with_matches_flags_are_search() {
        assert_parsed(
            &shlex_split_safe("rg -l TODO src"),
            vec![ParsedCommand::Search {
                cmd: "rg -l TODO src".to_string(),
                query: Some("TODO".to_string()),
                path: Some("src".to_string()),
            }],
        );
        assert_parsed(
            &shlex_split_safe("rg --files-with-matches TODO src"),
            vec![ParsedCommand::Search {
                cmd: "rg --files-with-matches TODO src".to_string(),
                query: Some("TODO".to_string()),
                path: Some("src".to_string()),
            }],
        );
        assert_parsed(
            &shlex_split_safe("rg -L TODO src"),
            vec![ParsedCommand::Search {
                cmd: "rg -L TODO src".to_string(),
                query: Some("TODO".to_string()),
                path: Some("src".to_string()),
            }],
        );
        assert_parsed(
            &shlex_split_safe("rg --files-without-match TODO src"),
            vec![ParsedCommand::Search {
                cmd: "rg --files-without-match TODO src".to_string(),
                query: Some("TODO".to_string()),
                path: Some("src".to_string()),
            }],
        );
        assert_parsed(
            &shlex_split_safe("rga -l TODO src"),
            vec![ParsedCommand::Search {
                cmd: "rga -l TODO src".to_string(),
                query: Some("TODO".to_string()),
                path: Some("src".to_string()),
            }],
        );
    }

    #[test]
    fn supports_cat() {
        let inner = "cat webview/README.md";
        assert_parsed(
            &vec_str(&["bash", "-lc", inner]),
            vec![ParsedCommand::Read {
                cmd: inner.to_string(),
                name: "README.md".to_string(),
                path: PathBuf::from("webview/README.md"),
            }],
        );
    }

    #[test]
    fn zsh_lc_supports_cat() {
        let inner = "cat README.md";
        assert_parsed(
            &vec_str(&["zsh", "-lc", inner]),
            vec![ParsedCommand::Read {
                cmd: inner.to_string(),
                name: "README.md".to_string(),
                path: PathBuf::from("README.md"),
            }],
        );
    }

    #[test]
    fn supports_bat() {
        let inner = "bat --theme TwoDark README.md";
        assert_parsed(
            &vec_str(&["bash", "-lc", inner]),
            vec![ParsedCommand::Read {
                cmd: inner.to_string(),
                name: "README.md".to_string(),
                path: PathBuf::from("README.md"),
            }],
        );
    }

    #[test]
    fn supports_batcat() {
        let inner = "batcat README.md";
        assert_parsed(
            &vec_str(&["bash", "-lc", inner]),
            vec![ParsedCommand::Read {
                cmd: inner.to_string(),
                name: "README.md".to_string(),
                path: PathBuf::from("README.md"),
            }],
        );
    }

    #[test]
    fn supports_less() {
        let inner = "less -p TODO README.md";
        assert_parsed(
            &vec_str(&["bash", "-lc", inner]),
            vec![ParsedCommand::Read {
                cmd: inner.to_string(),
                name: "README.md".to_string(),
                path: PathBuf::from("README.md"),
            }],
        );
    }

    #[test]
    fn supports_more() {
        let inner = "more README.md";
        assert_parsed(
            &vec_str(&["bash", "-lc", inner]),
            vec![ParsedCommand::Read {
                cmd: inner.to_string(),
                name: "README.md".to_string(),
                path: PathBuf::from("README.md"),
            }],
        );
    }

    #[test]
    fn cd_then_cat_is_single_read() {
        assert_parsed(
            &shlex_split_safe("cd foo && cat foo.txt"),
            vec![ParsedCommand::Read {
                cmd: "cat foo.txt".to_string(),
                name: "foo.txt".to_string(),
                path: PathBuf::from("foo/foo.txt"),
            }],
        );
    }

    #[test]
    fn cd_with_double_dash_then_cat_is_read() {
        assert_parsed(
            &shlex_split_safe("cd -- -weird && cat foo.txt"),
            vec![ParsedCommand::Read {
                cmd: "cat foo.txt".to_string(),
                name: "foo.txt".to_string(),
                path: PathBuf::from("-weird/foo.txt"),
            }],
        );
    }

    #[test]
    fn cd_with_multiple_operands_uses_last() {
        assert_parsed(
            &shlex_split_safe("cd dir1 dir2 && cat foo.txt"),
            vec![ParsedCommand::Read {
                cmd: "cat foo.txt".to_string(),
                name: "foo.txt".to_string(),
                path: PathBuf::from("dir2/foo.txt"),
            }],
        );
    }

    #[test]
    fn bash_cd_then_bar_is_same_as_bar() {
        // Ensure a leading `cd` inside bash -lc is dropped when followed by another command.
        assert_parsed(
            &shlex_split_safe("bash -lc 'cd foo && bar'"),
            vec![ParsedCommand::Unknown {
                cmd: "cd foo && bar".to_string(),
            }],
        );
    }

    #[test]
    fn bash_cd_then_cat_is_read() {
        assert_parsed(
            &shlex_split_safe("bash -lc 'cd foo && cat foo.txt'"),
            vec![ParsedCommand::Read {
                cmd: "cat foo.txt".to_string(),
                name: "foo.txt".to_string(),
                path: PathBuf::from("foo/foo.txt"),
            }],
        );
    }

    #[test]
    fn supports_ls_with_pipe() {
        let inner = "ls -la | sed -n '1,120p'";
        assert_parsed(
            &vec_str(&["bash", "-lc", inner]),
            vec![ParsedCommand::ListFiles {
                cmd: "ls -la".to_string(),
                path: None,
            }],
        );
    }

    #[test]
    fn supports_eza_exa_tree_du() {
        assert_parsed(
            &shlex_split_safe("eza --color=always src"),
            vec![ParsedCommand::ListFiles {
                cmd: "eza '--color=always' src".to_string(),
                path: Some("src".to_string()),
            }],
        );
        assert_parsed(
            &shlex_split_safe("exa -I target ."),
            vec![ParsedCommand::ListFiles {
                cmd: "exa -I target .".to_string(),
                path: Some(".".to_string()),
            }],
        );
        assert_parsed(
            &shlex_split_safe("tree -L 2 src"),
            vec![ParsedCommand::ListFiles {
                cmd: "tree -L 2 src".to_string(),
                path: Some("src".to_string()),
            }],
        );
        assert_parsed(
            &shlex_split_safe("du -d 2 ."),
            vec![ParsedCommand::ListFiles {
                cmd: "du -d 2 .".to_string(),
                path: Some(".".to_string()),
            }],
        );
    }

    #[test]
    fn supports_head_n() {
        let inner = "head -n 50 Cargo.toml";
        assert_parsed(
            &vec_str(&["bash", "-lc", inner]),
            vec![ParsedCommand::Read {
                cmd: inner.to_string(),
                name: "Cargo.toml".to_string(),
                path: PathBuf::from("Cargo.toml"),
            }],
        );
    }

    #[test]
    fn supports_head_file_only() {
        let inner = "head Cargo.toml";
        assert_parsed(
            &vec_str(&["bash", "-lc", inner]),
            vec![ParsedCommand::Read {
                cmd: inner.to_string(),
                name: "Cargo.toml".to_string(),
                path: PathBuf::from("Cargo.toml"),
            }],
        );
    }

    #[test]
    fn supports_cat_sed_n() {
        let inner = "cat tui/Cargo.toml | sed -n '1,200p'";
        assert_parsed(
            &vec_str(&["bash", "-lc", inner]),
            vec![ParsedCommand::Read {
                cmd: inner.to_string(),
                name: "Cargo.toml".to_string(),
                path: PathBuf::from("tui/Cargo.toml"),
            }],
        );
    }

    #[test]
    fn supports_tail_n_plus() {
        let inner = "tail -n +522 README.md";
        assert_parsed(
            &vec_str(&["bash", "-lc", inner]),
            vec![ParsedCommand::Read {
                cmd: inner.to_string(),
                name: "README.md".to_string(),
                path: PathBuf::from("README.md"),
            }],
        );
    }

    #[test]
    fn supports_tail_n_last_lines() {
        let inner = "tail -n 30 README.md";
        let out = parse_command(&vec_str(&["bash", "-lc", inner]));
        assert_eq!(
            out,
            vec![ParsedCommand::Read {
                cmd: inner.to_string(),
                name: "README.md".to_string(),
                path: PathBuf::from("README.md"),
            }]
        );
    }

    #[test]
    fn supports_tail_file_only() {
        let inner = "tail README.md";
        assert_parsed(
            &vec_str(&["bash", "-lc", inner]),
            vec![ParsedCommand::Read {
                cmd: inner.to_string(),
                name: "README.md".to_string(),
                path: PathBuf::from("README.md"),
            }],
        );
    }

    #[test]
    fn supports_npm_run_build_is_unknown() {
        assert_parsed(
            &vec_str(&["npm", "run", "build"]),
            vec![ParsedCommand::Unknown {
                cmd: "npm run build".to_string(),
            }],
        );
    }

    #[test]
    fn supports_grep_recursive_current_dir() {
        assert_parsed(
            &vec_str(&["grep", "-R", "CODEX_SANDBOX_ENV_VAR", "-n", "."]),
            vec![ParsedCommand::Search {
                cmd: "grep -R CODEX_SANDBOX_ENV_VAR -n .".to_string(),
                query: Some("CODEX_SANDBOX_ENV_VAR".to_string()),
                path: Some(".".to_string()),
            }],
        );
    }

    #[test]
    fn supports_grep_recursive_specific_file() {
        assert_parsed(
            &vec_str(&[
                "grep",
                "-R",
                "CODEX_SANDBOX_ENV_VAR",
                "-n",
                "core/src/spawn.rs",
            ]),
            vec![ParsedCommand::Search {
                cmd: "grep -R CODEX_SANDBOX_ENV_VAR -n core/src/spawn.rs".to_string(),
                query: Some("CODEX_SANDBOX_ENV_VAR".to_string()),
                path: Some("spawn.rs".to_string()),
            }],
        );
    }

    #[test]
    fn supports_egrep_and_fgrep() {
        assert_parsed(
            &shlex_split_safe("egrep -R TODO src"),
            vec![ParsedCommand::Search {
                cmd: "egrep -R TODO src".to_string(),
                query: Some("TODO".to_string()),
                path: Some("src".to_string()),
            }],
        );
        assert_parsed(
            &shlex_split_safe("fgrep -l TODO src"),
            vec![ParsedCommand::Search {
                cmd: "fgrep -l TODO src".to_string(),
                query: Some("TODO".to_string()),
                path: Some("src".to_string()),
            }],
        );
    }

    #[test]
    fn grep_files_with_matches_flags_are_search() {
        assert_parsed(
            &shlex_split_safe("grep -l TODO src"),
            vec![ParsedCommand::Search {
                cmd: "grep -l TODO src".to_string(),
                query: Some("TODO".to_string()),
                path: Some("src".to_string()),
            }],
        );
        assert_parsed(
            &shlex_split_safe("grep --files-with-matches TODO src"),
            vec![ParsedCommand::Search {
                cmd: "grep --files-with-matches TODO src".to_string(),
                query: Some("TODO".to_string()),
                path: Some("src".to_string()),
            }],
        );
        assert_parsed(
            &shlex_split_s…11136 tokens truncated…        // `head <file>` or `head -n50`/`head -c100`
                [_, arg] => arg.starts_with('-'),
                // `head -n 40` / `head -c 100` (no file operand)
                [_, flag, count]
                    if (flag == "-n" || flag == "-c")
                        && count.chars().all(|c| c.is_ascii_digit()) =>
                {
                    true
                }
                _ => false,
            }
        }
        "tail" => {
            // Treat as formatting when no explicit file operand is present.
            // Common forms: `tail -n +10`, `tail -n 30`, `tail -c 100`.
            // Keep cases like `tail -n 30 file`.
            match tokens {
                // `tail`
                [_] => true,
                // `tail <file>` or `tail -n30`/`tail -n+10`
                [_, arg] => arg.starts_with('-'),
                // `tail -n 30` / `tail -n +10` (no file operand)
                [_, flag, count]
                    if flag == "-n"
                        && (count.chars().all(|c| c.is_ascii_digit())
                            || (count.starts_with('+')
                                && count[1..].chars().all(|c| c.is_ascii_digit()))) =>
                {
                    true
                }
                // `tail -c 100` / `tail -c +10` (no file operand)
                [_, flag, count]
                    if flag == "-c"
                        && (count.chars().all(|c| c.is_ascii_digit())
                            || (count.starts_with('+')
                                && count[1..].chars().all(|c| c.is_ascii_digit()))) =>
                {
                    true
                }
                _ => false,
            }
        }
        "sed" => {
            // Keep `sed -n <range> file` (treated as a file read elsewhere);
            // otherwise consider it a formatting helper in a pipeline.
            sed_read_path(&tokens[1..]).is_none()
        }
        _ => false,
    }
}

fn is_mutating_xargs_command(tokens: &[String]) -> bool {
    xargs_subcommand(tokens).is_some_and(xargs_is_mutating_subcommand)
}

fn xargs_subcommand(tokens: &[String]) -> Option<&[String]> {
    if tokens.first().map(String::as_str) != Some("xargs") {
        return None;
    }
    let mut i = 1;
    while i < tokens.len() {
        let token = &tokens[i];
        if token == "--" {
            return tokens.get(i + 1..).filter(|rest| !rest.is_empty());
        }
        if !token.starts_with('-') {
            return tokens.get(i..).filter(|rest| !rest.is_empty());
        }
        let takes_value = matches!(
            token.as_str(),
            "-E" | "-e" | "-I" | "-L" | "-n" | "-P" | "-s"
        );
        if takes_value && token.len() == 2 {
            i += 2;
        } else {
            i += 1;
        }
    }
    None
}

fn xargs_is_mutating_subcommand(tokens: &[String]) -> bool {
    let Some((head, tail)) = tokens.split_first() else {
        return false;
    };
    match head.as_str() {
        "perl" | "ruby" => xargs_has_in_place_flag(tail),
        "sed" => xargs_has_in_place_flag(tail) || tail.iter().any(|token| token == "--in-place"),
        "rg" => tail.iter().any(|token| token == "--replace"),
        _ => false,
    }
}

fn xargs_has_in_place_flag(tokens: &[String]) -> bool {
    tokens.iter().any(|token| {
        token == "-i" || token.starts_with("-i") || token == "-pi" || token.starts_with("-pi")
    })
}

fn drop_small_formatting_commands(mut commands: Vec<Vec<String>>) -> Vec<Vec<String>> {
    commands.retain(|tokens| !is_small_formatting_command(tokens));
    commands
}

fn summarize_main_tokens(main_cmd: &[String]) -> ParsedCommand {
    match main_cmd.split_first() {
        Some((head, tail)) if matches!(head.as_str(), "ls" | "eza" | "exa") => {
            let flags_with_vals: &[&str] = match head.as_str() {
                "ls" => &[
                    "-I",
                    "-w",
                    "--block-size",
                    "--format",
                    "--time-style",
                    "--color",
                    "--quoting-style",
                ],
                "eza" | "exa" => &[
                    "-I",
                    "--ignore-glob",
                    "--color",
                    "--sort",
                    "--time-style",
                    "--time",
                ],
                _ => &[],
            };
            let path =
                first_non_flag_operand(tail, flags_with_vals).map(|p| short_display_path(&p));
            ParsedCommand::ListFiles {
                cmd: shlex_join(main_cmd),
                path,
            }
        }
        Some((head, tail)) if head == "tree" => {
            let path = first_non_flag_operand(
                tail,
                &["-L", "-P", "-I", "--charset", "--filelimit", "--sort"],
            )
            .map(|p| short_display_path(&p));
            ParsedCommand::ListFiles {
                cmd: shlex_join(main_cmd),
                path,
            }
        }
        Some((head, tail)) if head == "du" => {
            let path = first_non_flag_operand(
                tail,
                &[
                    "-d",
                    "--max-depth",
                    "-B",
                    "--block-size",
                    "--exclude",
                    "--time-style",
                ],
            )
            .map(|p| short_display_path(&p));
            ParsedCommand::ListFiles {
                cmd: shlex_join(main_cmd),
                path,
            }
        }
        Some((head, tail)) if head == "rg" || head == "rga" || head == "ripgrep-all" => {
            let args_no_connector = trim_at_connector(tail);
            let has_files_flag = args_no_connector.iter().any(|a| a == "--files");
            let candidates = skip_flag_values(
                &args_no_connector,
                &[
                    "-g",
                    "--glob",
                    "--iglob",
                    "-t",
                    "--type",
                    "--type-add",
                    "--type-not",
                    "-m",
                    "--max-count",
                    "-A",
                    "-B",
                    "-C",
                    "--context",
                    "--max-depth",
                ],
            );
            let non_flags: Vec<&String> = candidates
                .into_iter()
                .filter(|p| !p.starts_with('-'))
                .collect();
            if has_files_flag {
                let path = non_flags.first().map(|s| short_display_path(s));
                ParsedCommand::ListFiles {
                    cmd: shlex_join(main_cmd),
                    path,
                }
            } else {
                let query = non_flags.first().cloned().map(String::from);
                let path = non_flags.get(1).map(|s| short_display_path(s));
                ParsedCommand::Search {
                    cmd: shlex_join(main_cmd),
                    query,
                    path,
                }
            }
        }
        Some((head, tail)) if head == "git" => match tail.split_first() {
            Some((subcmd, sub_tail)) if subcmd == "grep" => parse_grep_like(main_cmd, sub_tail),
            Some((subcmd, sub_tail)) if subcmd == "ls-files" => {
                let path = first_non_flag_operand(
                    sub_tail,
                    &["--exclude", "--exclude-from", "--pathspec-from-file"],
                )
                .map(|p| short_display_path(&p));
                ParsedCommand::ListFiles {
                    cmd: shlex_join(main_cmd),
                    path,
                }
            }
            _ => ParsedCommand::Unknown {
                cmd: shlex_join(main_cmd),
            },
        },
        Some((head, tail)) if head == "fd" => {
            let (query, path) = parse_fd_query_and_path(tail);
            if query.is_some() {
                ParsedCommand::Search {
                    cmd: shlex_join(main_cmd),
                    query,
                    path,
                }
            } else {
                ParsedCommand::ListFiles {
                    cmd: shlex_join(main_cmd),
                    path,
                }
            }
        }
        Some((head, tail)) if head == "find" => {
            // Basic find support: capture path and common name filter
            let (query, path) = parse_find_query_and_path(tail);
            if query.is_some() {
                ParsedCommand::Search {
                    cmd: shlex_join(main_cmd),
                    query,
                    path,
                }
            } else {
                ParsedCommand::ListFiles {
                    cmd: shlex_join(main_cmd),
                    path,
                }
            }
        }
        Some((head, tail)) if matches!(head.as_str(), "grep" | "egrep" | "fgrep") => {
            parse_grep_like(main_cmd, tail)
        }
        Some((head, tail)) if matches!(head.as_str(), "ag" | "ack" | "pt") => {
            let args_no_connector = trim_at_connector(tail);
            let candidates = skip_flag_values(
                &args_no_connector,
                &[
                    "-G",
                    "-g",
                    "--file-search-regex",
                    "--ignore-dir",
                    "--ignore-file",
                    "--path-to-ignore",
                ],
            );
            let non_flags: Vec<&String> = candidates
                .into_iter()
                .filter(|p| !p.starts_with('-'))
                .collect();
            let query = non_flags.first().cloned().map(String::from);
            let path = non_flags.get(1).map(|s| short_display_path(s));
            ParsedCommand::Search {
                cmd: shlex_join(main_cmd),
                query,
                path,
            }
        }
        Some((head, tail)) if head == "cat" => {
            if let Some(path) = single_non_flag_operand(tail, &[]) {
                let name = short_display_path(&path);
                ParsedCommand::Read {
                    cmd: shlex_join(main_cmd),
                    name,
                    path: PathBuf::from(path),
                }
            } else {
                ParsedCommand::Unknown {
                    cmd: shlex_join(main_cmd),
                }
            }
        }
        Some((head, tail)) if matches!(head.as_str(), "bat" | "batcat") => {
            if let Some(path) = single_non_flag_operand(
                tail,
                &[
                    "--theme",
                    "--language",
                    "--style",
                    "--terminal-width",
                    "--tabs",
                    "--line-range",
                    "--map-syntax",
                ],
            ) {
                let name = short_display_path(&path);
                ParsedCommand::Read {
                    cmd: shlex_join(main_cmd),
                    name,
                    path: PathBuf::from(path),
                }
            } else {
                ParsedCommand::Unknown {
                    cmd: shlex_join(main_cmd),
                }
            }
        }
        Some((head, tail)) if head == "less" => {
            if let Some(path) = single_non_flag_operand(
                tail,
                &[
                    "-p",
                    "-P",
                    "-x",
                    "-y",
                    "-z",
                    "-j",
                    "--pattern",
                    "--prompt",
                    "--tabs",
                    "--shift",
                    "--jump-target",
                ],
            ) {
                let name = short_display_path(&path);
                ParsedCommand::Read {
                    cmd: shlex_join(main_cmd),
                    name,
                    path: PathBuf::from(path),
                }
            } else {
                ParsedCommand::Unknown {
                    cmd: shlex_join(main_cmd),
                }
            }
        }
        Some((head, tail)) if head == "more" => {
            if let Some(path) = single_non_flag_operand(tail, &[]) {
                let name = short_display_path(&path);
                ParsedCommand::Read {
                    cmd: shlex_join(main_cmd),
                    name,
                    path: PathBuf::from(path),
                }
            } else {
                ParsedCommand::Unknown {
                    cmd: shlex_join(main_cmd),
                }
            }
        }
        Some((head, tail)) if head == "head" => {
            // Support `head -n 50 file` and `head -n50 file` forms.
            let has_valid_n = match tail.split_first() {
                Some((first, rest)) if first == "-n" => rest
                    .first()
                    .is_some_and(|n| n.chars().all(|c| c.is_ascii_digit())),
                Some((first, _)) if first.starts_with("-n") => {
                    first[2..].chars().all(|c| c.is_ascii_digit())
                }
                _ => false,
            };
            if has_valid_n {
                // Build candidates skipping the numeric value consumed by `-n` when separated.
                let mut candidates: Vec<&String> = Vec::new();
                let mut i = 0;
                while i < tail.len() {
                    if i == 0 && tail[i] == "-n" && i + 1 < tail.len() {
                        let n = &tail[i + 1];
                        if n.chars().all(|c| c.is_ascii_digit()) {
                            i += 2;
                            continue;
                        }
                    }
                    candidates.push(&tail[i]);
                    i += 1;
                }
                if let Some(p) = candidates.into_iter().find(|p| !p.starts_with('-')) {
                    let path = p.clone();
                    let name = short_display_path(&path);
                    return ParsedCommand::Read {
                        cmd: shlex_join(main_cmd),
                        name,
                        path: PathBuf::from(path),
                    };
                }
            }
            if let [path] = tail
                && !path.starts_with('-')
            {
                let name = short_display_path(path);
                return ParsedCommand::Read {
                    cmd: shlex_join(main_cmd),
                    name,
                    path: PathBuf::from(path),
                };
            }
            ParsedCommand::Unknown {
                cmd: shlex_join(main_cmd),
            }
        }
        Some((head, tail)) if head == "tail" => {
            // Support `tail -n +10 file` and `tail -n+10 file` forms.
            let has_valid_n = match tail.split_first() {
                Some((first, rest)) if first == "-n" => rest.first().is_some_and(|n| {
                    let s = n.strip_prefix('+').unwrap_or(n);
                    !s.is_empty() && s.chars().all(|c| c.is_ascii_digit())
                }),
                Some((first, _)) if first.starts_with("-n") => {
                    let v = &first[2..];
                    let s = v.strip_prefix('+').unwrap_or(v);
                    !s.is_empty() && s.chars().all(|c| c.is_ascii_digit())
                }
                _ => false,
            };
            if has_valid_n {
                // Build candidates skipping the numeric value consumed by `-n` when separated.
                let mut candidates: Vec<&String> = Vec::new();
                let mut i = 0;
                while i < tail.len() {
                    if i == 0 && tail[i] == "-n" && i + 1 < tail.len() {
                        let n = &tail[i + 1];
                        let s = n.strip_prefix('+').unwrap_or(n);
                        if !s.is_empty() && s.chars().all(|c| c.is_ascii_digit()) {
                            i += 2;
                            continue;
                        }
                    }
                    candidates.push(&tail[i]);
                    i += 1;
                }
                if let Some(p) = candidates.into_iter().find(|p| !p.starts_with('-')) {
                    let path = p.clone();
                    let name = short_display_path(&path);
                    return ParsedCommand::Read {
                        cmd: shlex_join(main_cmd),
                        name,
                        path: PathBuf::from(path),
                    };
                }
            }
            if let [path] = tail
                && !path.starts_with('-')
            {
                let name = short_display_path(path);
                return ParsedCommand::Read {
                    cmd: shlex_join(main_cmd),
                    name,
                    path: PathBuf::from(path),
                };
            }
            ParsedCommand::Unknown {
                cmd: shlex_join(main_cmd),
            }
        }
        Some((head, tail)) if head == "awk" => {
            if let Some(path) = awk_data_file_operand(tail) {
                let name = short_display_path(&path);
                ParsedCommand::Read {
                    cmd: shlex_join(main_cmd),
                    name,
                    path: PathBuf::from(path),
                }
            } else {
                ParsedCommand::Unknown {
                    cmd: shlex_join(main_cmd),
                }
            }
        }
        Some((head, tail)) if head == "nl" => {
            // Avoid treating option values as paths (e.g., nl -s "  ").
            let candidates = skip_flag_values(tail, &["-s", "-w", "-v", "-i", "-b"]);
            if let Some(p) = candidates.into_iter().find(|p| !p.starts_with('-')) {
                let path = p.clone();
                let name = short_display_path(&path);
                ParsedCommand::Read {
                    cmd: shlex_join(main_cmd),
                    name,
                    path: PathBuf::from(path),
                }
            } else {
                ParsedCommand::Unknown {
                    cmd: shlex_join(main_cmd),
                }
            }
        }
        Some((head, tail)) if head == "sed" => {
            if let Some(path) = sed_read_path(tail) {
                let name = short_display_path(&path);
                ParsedCommand::Read {
                    cmd: shlex_join(main_cmd),
                    name,
                    path: PathBuf::from(path),
                }
            } else {
                ParsedCommand::Unknown {
                    cmd: shlex_join(main_cmd),
                }
            }
        }
        Some((head, tail)) if is_python_command(head) => {
            if python_walks_files(tail) {
                ParsedCommand::ListFiles {
                    cmd: shlex_join(main_cmd),
                    path: None,
                }
            } else {
                ParsedCommand::Unknown {
                    cmd: shlex_join(main_cmd),
                }
            }
        }
        // Other commands
        _ => ParsedCommand::Unknown {
            cmd: shlex_join(main_cmd),
        },
    }
}

fn is_abs_like(path: &str) -> bool {
    if std::path::Path::new(path).is_absolute() {
        return true;
    }
    let mut chars = path.chars();
    match (chars.next(), chars.next(), chars.next()) {
        // Windows drive path like C:\
        (Some(d), Some(':'), Some('\\')) if d.is_ascii_alphabetic() => return true,
        // UNC path like \\server\share
        (Some('\\'), Some('\\'), _) => return true,
        _ => {}
    }
    false
}

fn join_paths(base: &str, rel: &str) -> String {
    if is_abs_like(rel) {
        return rel.to_string();
    }
    if base.is_empty() {
        return rel.to_string();
    }
    let mut buf = PathBuf::from(base);
    buf.push(rel);
    buf.to_string_lossy().to_string()
}
