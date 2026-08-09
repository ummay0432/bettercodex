//! Native Windows has no tmux handoff. Keep the Unix-only session supervisor
//! out of Windows builds without inventing a different user-visible mechanism.

use anyhow::Result;
use anyhow::bail;
use std::path::Path;

pub(crate) struct WorkerHandoff;

impl WorkerHandoff {
    pub(crate) fn transfer(&mut self, _prepared: &PreparedTmuxSession) -> Result<()> {
        bail!("tmux handoff is unavailable on Windows")
    }
}

pub(crate) struct PreparedTmuxSession;

impl PreparedTmuxSession {
    pub(crate) fn commit(self) -> String {
        String::new()
    }
}

pub(crate) fn enter_agent_process(
    _arguments: &[String],
    _interactive_tui: bool,
) -> Result<Option<WorkerHandoff>> {
    Ok(None)
}

pub(crate) fn is_tmux_active() -> bool {
    false
}

pub(crate) fn run_relay_command(_arguments: &[String]) -> Option<Result<()>> {
    None
}

pub(crate) fn prepare_tmux_session(
    _cwd: &Path,
    _size: (u16, u16),
) -> Result<PreparedTmuxSession> {
    bail!("tmux handoff is unavailable on Windows")
}
