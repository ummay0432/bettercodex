# Local development install

Perform a **local development install**: build the current bettercodex worktree with `cargo build --release --locked`, atomically install the resulting binary as the user-local `bcodex` command on this development environment's `PATH`, verify that the installed executable exactly matches the build, and do not create, tag, publish, or otherwise initiate a repository release.
