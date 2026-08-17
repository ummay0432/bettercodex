//! Focused shell detection and command-summary port from OpenAI Codex, re-audited
//! at `a16863f8704831d13e041ed7dba2c4a57a2a940b`.

mod bash;

pub(crate) mod parse_command;
pub(crate) mod shell_detect;

pub(crate) fn is_only_plain_ripgrep_script(script: &str) -> bool {
    let Some(commands) = bash::parse_shell_script_into_commands(script) else {
        return false;
    };
    !commands.is_empty()
        && commands
            .iter()
            .all(|command| command.first().is_some_and(|program| program == "rg"))
}
