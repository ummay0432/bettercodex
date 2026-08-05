# Papercuts

- While benchmarking CLI startup, the repository environment had no `/usr/bin/time`, so the usual elapsed-time and peak-RSS command failed. Document an available benchmark command or include a portable helper for startup measurements.
- Using the main worktree's CARGO_TARGET_DIR to validate a detached Git worktree reused a test binary built from the concurrently dirty main checkout, so the supposedly isolated run executed the wrong source. Keep per-worktree Cargo target directories (or document a safe isolation procedure) when validating concurrent work.
