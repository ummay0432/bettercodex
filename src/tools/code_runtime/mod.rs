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
mod description;
// Protocol variant names deliberately mirror the upstream wire representation.
#[allow(clippy::enum_variant_names)]
mod protocol;
mod runtime;
mod service;
mod session_runtime;
mod v8_init;

pub(crate) type TaskFailureHandler = std::sync::Arc<dyn Fn(String) + Send + Sync>;

pub(crate) use description::*;
pub(crate) use protocol::*;
pub(crate) use service::InProcessCodeModeSession;

pub(crate) const PUBLIC_TOOL_NAME: &str = "exec";
pub(crate) const WAIT_TOOL_NAME: &str = "wait";
// Codex core overrides the protocol crate's 10-second fallback with this session default.
pub(crate) const DEFAULT_CODE_MODE_EXEC_YIELD_TIME_MS: u64 = DEFAULT_EXEC_YIELD_TIME_MS * 3;

pub(crate) fn prewarm() {
    v8_init::prewarm_v8();
}

pub(crate) async fn ensure_initialized() -> Result<(), String> {
    v8_init::ensure_v8_initialized_async().await
}

pub(crate) fn package_smoke_test() -> Result<(), String> {
    v8_init::ensure_v8_initialized()
}
