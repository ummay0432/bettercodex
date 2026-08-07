# Test-suite audit

This directory contains only tests that need to launch the compiled `bcodex`
binary. The managed-session tests use a pseudo-terminal; the CLI-output test
controls the process's output descriptor directly. It is not the whole test
suite. Most tests sit beside the Rust modules they exercise so they can inspect
private behavior and exact wire values.

As audited on 2026-08-07, the repository has 622 Rust unit tests, three PTY
integration tests, and one process-level CLI test. Nine of the unit-test
entries are ignored performance benchmarks. Before this audit, CI ran Clippy
but did not run the test suite or check formatting; the validation workflow now
runs all of them.

## Upstream Codex comparison

bettercodex pins OpenAI Codex commit
`1669c2403f793d0230065397dfc25f52b844244e`. That source contains broad unit,
integration, scenario, and snapshot coverage. Most of it targets Codex
components that bettercodex does not contain, so copying the entire test tree
would create tests for a different architecture.

The in-process Code Mode runtime is a direct source port, but its upstream test
suite had been omitted. This audit ported all 63 tests from these upstream
sources:

| Upstream source under `codex-rs/code-mode-runtime/src/` | Tests | Upstream SHA-256 |
| --- | ---: | --- |
| `service_contract_tests.rs` | 8 | `a5aba5172835573896d68aba779dea32205d7357934a063a9f496e0ae98ca225` |
| `service_tests.rs` | 29 | `5a9d0a5e250efdf8fac90143d4ee45daaa5dbf380d4423acc16d49aa915a9986` |
| `session_runtime/tests.rs` | 5 | `3e106c8d77acc4557a00a4f6a128b6c8f040527fe17e721069cf1db6cc178912` |
| `cell_actor/tests.rs` | 13 | `6e01c71b781a21331ebea10d123463e1c8dcaecb6a58d1a95558c71a1c13d001` |
| `cell_actor/callbacks_tests.rs` | 3 | `64cab6c279a894cafc1d689977ef3195ec6838aa99787c0a6fa73adef00fa357` |
| Inline tests in `runtime/mod.rs` | 5 | See the pinned source and provenance comment in the local module. |

The test bodies are unchanged except for local crate paths, one `Arc` wrapper
required by bettercodex's shared-state representation, and the test-only
pending-runtime mode used by upstream. One upstream Clippy `expect` is an
`allow` locally because Codex's workspace lint configuration triggers that lint
while bettercodex's does not. Provenance comments remain in every ported file.

The restored `linked_v8_has_sandbox_enabled` test immediately found a real
regression: bettercodex linked a non-sandboxed V8 archive. Builds now enable the
V8 sandbox and use OpenAI's pinned pointer-compression/sandbox artifacts with
checked SHA-256 values. The developer helper also verifies that its artifact
version matches `Cargo.lock`, preventing a dependency update from silently
using stale binaries.

The local patch engine was compared with Codex's 23 fixture scenarios. Existing
tests cover its parsing, add/update/delete/move behavior, Unicode and whitespace
tolerance, cancellation, and failure paths. One behavior intentionally differs:
Codex's historical scenario 015 leaves earlier filesystem mutations behind
after a later operation fails, while bettercodex validates the complete patch
before mutation and tests atomic failure. Porting that weaker outcome would be
a regression rather than useful parity.

## What these tests establish

Rust tests use deterministic mock HTTP and WebSocket servers, exact request and
history comparisons, temporary filesystems, parser cases, terminal snapshots,
and runtime cancellation/concurrency checks. They test code outcomes rather
than judging model prose. The live behavioral evaluations under `evaluations/`
are deliberately separate because stochastic inference cannot replace a
deterministic regression gate.
