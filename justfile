set positional-arguments

rust_min_stack := "8388608"

# Display help.
help:
    just -l

# Run bettercodex from source.
alias c := bcodex
bcodex *args:
    cargo run --bin bcodex -- {args}

# Match the Cargo-facing development commands used by upstream Codex.
fmt:
    cargo fmt --all

fmt-check:
    cargo fmt --all -- --check

fix *args:
    cargo clippy --fix --tests --allow-dirty {args}

clippy *args:
    cargo clippy --tests {args}

install:
    rustup show active-toolchain
    cargo fetch

test *args:
    RUST_MIN_STACK={{ rust_min_stack }} NEXTEST_PROFILE=local cargo nextest run --no-fail-fast "$@"
