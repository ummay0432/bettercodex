# Native Windows support

Status: developer preview

## Authority

Current upstream Codex source and the bettercodex implementation are
authoritative. This file records only bettercodex's Windows support boundary and
the checks required to promote it; it is not an implementation plan or a copy
of upstream internals.

## Support boundary

- Native 64-bit Windows 11 on x86-64 is the supported target. Windows 10 and
  non-x86-64 targets are best effort.
- WSL uses the Linux build and Linux behavior, not the native Windows path.
- PowerShell is the default native shell. Git Bash does not change model-facing
  shell guidance or command construction.
- Windows-only instructions, tools, and examples stay out of Unix model
  context, and Unix shell guidance stays out of native Windows context.
- bettercodex remains one user-facing `bcodex.exe`.
- Commands run with the invoking user's permissions. Native Windows support
  does not add a sandbox or approval framework.

## Porting rules

- Port retained process, ConPTY, console, input, clipboard, path, and terminal
  behavior from current upstream Codex rather than maintaining a parallel
  implementation.
- Keep platform boundaries at the module or target-dependency level. Do not
  spread `cfg(windows)` branches through platform-independent inference or UI
  logic when a platform adapter owns the distinction.
- Preserve native paths and Windows error information until a wire or display
  boundary requires conversion.
- Pipe mode must keep stdout and stderr distinct. PTY mode must preserve the
  merged terminal stream and resize semantics expected by the TUI.
- Process interruption and teardown must cover the spawned process tree without
  terminating unrelated processes. Use upstream job-object behavior where it
  applies.
- Every terminal mode, handle, job, child process, lock, and temporary file must
  have explicit ownership and cleanup on success, failure, cancellation, and
  panic paths.

## Distribution and documentation

The active installer, updater, and user-facing contract live in
`scripts/install.ps1`, `src/update.rs`, and `docs/install.md`. Do not duplicate
their transaction details here. Public installation and updates use the
published prebuilt Windows binary and do not install build prerequisites.

## Promotion gate

Before changing the status from developer preview:

1. Run format, check, tests, Clippy, and a locked release build on native
   Windows x86-64 with the pinned toolchain and V8 artifacts.
2. Exercise both pipe and ConPTY commands, output streaming, resize,
   interruption, timeout, descendant cleanup, and non-ASCII paths.
3. Exercise Windows Terminal and VS Code's integrated terminal with keyboard
   input, bracketed and ordinary paste, IME input, clipboard operations,
   hyperlinks, resize/reflow, resume, login, and clean shutdown.
4. Run the public install and update flow from a clean profile and from an
   existing installation, including rollback and locked-file failure paths.
5. Confirm no platform-specific model context leaks to the other target family
   and no task-owned process, stage, cache, or temporary file remains after
   failure.

Compile-only cross-checks from Linux do not satisfy this gate. The Rust Windows
target without a native MSVC environment is not evidence of Windows support.
