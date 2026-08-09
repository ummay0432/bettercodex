//! Windows process support ported from OpenAI Codex `codex-utils-pty` and
//! re-audited at `a16863f8704831d13e041ed7dba2c4a57a2a940b`.

mod pipe;
mod process;
mod pty;
#[cfg(test)]
mod tests;
mod win;
mod windows_input;

#[cfg(test)]
pub use pipe::spawn_process as spawn_pipe_process;
pub use pipe::spawn_process_no_stdin as spawn_pipe_process_no_stdin;
pub use process::ProcessHandle;
pub use process::ProcessSignal;
pub use process::SpawnedProcess;
pub use process::TerminalSize;
#[cfg(test)]
pub use process::combine_output_receivers;
pub use pty::spawn_process as spawn_pty_process;
#[cfg(test)]
pub use win::JobObject;
