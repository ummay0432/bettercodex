//! In-process Codex Code Mode runtime.
//!
//! Ported from OpenAI Codex commit
//! `1669c2403f793d0230065397dfc25f52b844244e`,
//! `codex-rs/code-mode-runtime/src`. The cell, yield, cancellation, and session
//! mechanics track upstream; JavaScript timer waits use cell-owned Tokio tasks
//! so clearing or ending a cell releases them immediately. Upstream protocol
//! identifiers are retained in this port for source-level auditability;
//! bettercodex exposes the runtime unconditionally.

mod cell_actor;
mod runtime;
mod service;
mod session_runtime;
mod v8_init;

pub(crate) type TaskFailureHandler = std::sync::Arc<dyn Fn(String) + Send + Sync>;

pub use codex_code_mode_protocol::*;
pub use service::InProcessCodeModeSession;
