//! Focused shell detection and command-summary port from OpenAI Codex, re-audited
//! at `a16863f8704831d13e041ed7dba2c4a57a2a940b`.

mod bash;
pub(crate) mod powershell;

pub(crate) mod parse_command;
pub(crate) mod shell_detect;
