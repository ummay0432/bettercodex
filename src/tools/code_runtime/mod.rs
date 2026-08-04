//! In-process Codex Code Mode runtime.
//!
//! Ported from OpenAI Codex commit
//! `1669c2403f793d0230065397dfc25f52b844244e`,
//! `codex-rs/code-mode-runtime/src`. The module paths are the only structural
//! adaptation; the cell, yield, cancellation, and session mechanics remain
//! upstream's implementation. BetterCodex links the standard `rusty_v8`
//! archive so one Cargo package builds on Linux and macOS. Codex release builds
//! instead inject target-specific pointer-compression and V8-sandbox artifacts.
//! Upstream protocol identifiers are retained in this port for source-level
//! auditability; BetterCodex exposes the runtime unconditionally.

mod cell_actor;
mod runtime;
mod service;
mod session_runtime;
mod v8_init;

pub(crate) type TaskFailureHandler = std::sync::Arc<dyn Fn(String) + Send + Sync>;

pub use codex_code_mode_protocol::*;
pub use service::InProcessCodeModeSession;
