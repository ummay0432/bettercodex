//! Focused shell detection and command-summary port from OpenAI Codex commit
//! `1669c2403f793d0230065397dfc25f52b844244e`.

mod bash;
mod powershell;

pub(crate) mod parse_command;
pub(crate) mod shell_detect;
