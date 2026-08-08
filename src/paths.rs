//! Locations for bettercodex-owned operator state.

use std::path::PathBuf;

pub(crate) fn bettercodex_home() -> Option<PathBuf> {
    std::env::var_os("BCODEX_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".bcodex")))
}
