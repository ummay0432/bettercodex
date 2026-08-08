//! Shared discovery of the active Git worktree boundary.

use std::path::Path;
use std::path::PathBuf;

pub(crate) fn find_root(cwd: &Path) -> Option<PathBuf> {
    cwd.ancestors()
        .find(|directory| directory.join(".git").exists())
        .map(Path::to_path_buf)
}
