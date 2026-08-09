//! Windows process support ported from OpenAI Codex `codex-utils-pty` at
//! `646f7c0a91b8e327d263335da68ae8ef212895ce`.

pub mod pipe;
mod process;
pub mod process_group;
pub mod pty;
#[cfg(test)]
mod tests;
mod win;
mod windows_input;

pub use pipe::spawn_process as spawn_pipe_process;
pub use pipe::spawn_process_no_stdin as spawn_pipe_process_no_stdin;
pub use process::ProcessDriver;
pub use process::ProcessHandle;
pub use process::ProcessSignal;
pub use process::SpawnedProcess;
pub use process::TerminalSize;
pub use process::combine_output_receivers;
pub use process::spawn_from_driver;
pub use pty::conpty_supported;
pub use pty::spawn_process as spawn_pty_process;
pub use win::JobObject;
pub use win::PsuedoCon;
pub use win::conpty::RawConPty;
pub use windows_input::WindowsTtyInputNormalizer;

pub const DEFAULT_OUTPUT_BYTES_CAP: usize = 1024 * 1024;
