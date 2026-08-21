# Local development install

Run the canonical local development installer:

```sh
./scripts/local-development-install.sh
```

The script runs `cargo lint --locked`, builds the current bettercodex worktree with `cargo build --release --locked`, atomically installs the resulting binary as the user-local `bcodex` command on this development environment's `PATH`, and verifies that the installed executable exactly matches the build. It deliberately retains Cargo's `target/` directory so repeated installs reuse compiled artifacts. It does not create, tag, publish, or otherwise initiate a repository release.
