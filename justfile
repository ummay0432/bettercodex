set positional-arguments

rust_min_stack := "8388608"
cargo_with_v8 := justfile_directory() + "/scripts/cargo-with-v8.sh"

# Display help.
help:
    just -l

# Run bettercodex from source.
alias c := bcodex
bcodex *args:
    {{ cargo_with_v8 }} run --bin bcodex -- "$@"

# Match the Cargo-facing development commands used by upstream Codex.
fmt:
    cargo fmt --all

fmt-check:
    cargo fmt --all -- --check

fix *args:
    {{ cargo_with_v8 }} clippy --fix --tests --allow-dirty "$@"

clippy *args:
    {{ cargo_with_v8 }} clippy --tests "$@"

install:
    rustup show active-toolchain
    cargo fetch

test *args:
    RUST_MIN_STACK={{ rust_min_stack }} NEXTEST_PROFILE=local {{ cargo_with_v8 }} nextest run --no-fail-fast "$@"
