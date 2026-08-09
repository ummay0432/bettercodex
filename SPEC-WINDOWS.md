# Native Windows terminal support specification

Status: implementation candidate; native validation pending

Last updated: 2026-08-09

## 1. Status and authority

This document specifies the work required to make the native `bcodex.exe`
client a supported Windows terminal application. It remains the design and
acceptance contract, not by itself a claim that every supported-release gate
has passed.

[`progressive_disclosure/product-direction.md`](progressive_disclosure/product-direction.md)
now authorizes native Windows 11 x64 as a target. The source port, target-gated
dependencies, PowerShell build and install paths, and native workflow are in
the tree. Native workflow and interactive terminal evidence are still pending,
so public documentation labels the port a developer preview rather than a
supported release.

This specification governs the Windows port together with the existing product
invariants. Where it retains behavior that current OpenAI
Codex already implements, the implementation must inspect and port the then-
current upstream source rather than reconstructing the behavior from this prose
or from memory. The upstream revision inspected while drafting this document is
evidence for the design, not a permanent source pin.

The terms **must**, **must not**, **should**, and **may** express requirement
strength. A deliberate departure from a **must** or **must not** requires an
explicit update to this specification or the applicable product-direction
document before code lands.

## 2. Executive decision

Native Windows support is feasible without changing what bettercodex is. The
recommended port keeps one Cargo package, one locally built `bcodex.exe`, the
fixed model/runtime choices, exact-public-`main` updates, and full-user-
permission command execution. It does not import upstream Codex's sandbox,
configuration framework, app server, release packaging, or plugin system.

This is not principally a shortcut-labeling patch. The current checkout has
Unix-only assumptions in process creation, PTY management, signals, shell
detection, terminal probing, file locking, durable replacement, permissions,
tmux migration, Git helpers, and installer logic. A TUI that
merely compiles on Windows while command execution, paste, interruption,
updates, or recovery remain unreliable does not satisfy this specification.

The first supported target should be:

- 64-bit Windows 11 on `x86_64-pc-windows-msvc`;
- Windows Terminal and the VS Code integrated terminal as primary terminal
  hosts;
- PowerShell 7 when installed, with Windows PowerShell 5.1 and `cmd.exe`
  fallbacks; and
- the same unsandboxed execution contract as Linux and macOS.

Windows 10 version 1809 or newer may be documented as best effort because
Microsoft's
[`CreatePseudoConsole`](https://learn.microsoft.com/en-us/windows/console/createpseudoconsole)
API begins at version 1809. It must not be described as fully supported until
the project continuously tests it.

## 3. Goals

### 3.1 Product goals

An operator on a supported Windows system must be able to:

1. install bettercodex from public `main` with a PowerShell command;
2. run `bcodex login`, launch the interactive TUI, resume sessions, and update
   the installed command;
3. use the composer, history, slash commands, skills, attachments, clipboard,
   and terminal transcript without Windows-specific corruption or accidental
   submission;
4. let the agent execute both piped and interactive commands, send input to a
   live command, resize its pseudo-terminal, interrupt it, and clean up its
   process tree;
5. use repositories and state stored at native Windows paths, including paths
   containing spaces and non-ASCII characters;
6. receive the same model-facing behavior and retained Codex behavior as an
   equivalent Linux or macOS session, except for explicitly documented
   platform capabilities.

### 3.2 Engineering goals

The port must:

- follow current upstream Codex for retained Windows behavior;
- isolate OS-specific dependencies and code with target configuration so the
  Linux and macOS artifacts do not absorb Windows machinery;
- use one small platform boundary for each real semantic difference rather
  than scattering fake portability wrappers or introducing a general platform
  framework;
- preserve native `Path` and `OsString` values instead of forcing Windows paths
  through UTF-8 strings;
- restore every terminal mode, process resource, file lock, and temporary
  artifact on normal exit and failure;
- include actual Windows execution and terminal tests, not only a Linux cross-
  compilation check; and
- establish measured source, artifact, startup, memory, and build-cache costs
  before declaring support complete.

## 4. Non-goals

The initial Windows port must not add any of the following:

- the upstream Codex Windows sandbox or any other command sandbox;
- a second binary, resident service, app server, SDK, or launcher installed
  beside `bcodex.exe`;
- MSI, MSIX, Microsoft Store, `winget` package publication, GitHub Release, or
  semantic-version update channels;
- a bettercodex configuration framework or configurable TUI keymap;
- native Windows Terminal tab migration as a substitute for `/tmux`;
- native Windows on ARM64;
- automatic migration or path translation between WSL and native Windows;
- guaranteed operation in the legacy Console Host or terminals that cannot
  provide the required virtual-terminal and key-event behavior;
- shell emulation that makes PowerShell pretend to use POSIX quoting; or
- broad upstream workspace, Bazel, Node, packaging, MCP, plugin, or app-server
  machinery merely because upstream Windows code happens to live near it.

WSL remains a valid way to run the Linux build. A process inside WSL is a Linux
installation with Linux paths and Linux session state; it is not the native
Windows target described here.

## 5. Retained bettercodex invariants

Windows support changes the supported platform list, not the core product
shape. The following existing contracts remain mandatory:

- bettercodex is one Cargo package and one user-facing binary;
- the fixed model is `gpt-5.6-sol` at `max` reasoning effort with the existing
  context and output limits;
- public installations track the exact commit at public `main` rather than a
  package version, tag, or release;
- the installer builds the selected immutable source revision locally using
  the checked-in lockfile and pinned Rust toolchain;
- no candidate replaces an installed command before build, revision, V8,
  embedded-resource, and smoke-test verification succeeds;
- commands and patches run with the invoking user's permissions and are not
  sandboxed;
- ChatGPT credentials remain under `${CODEX_HOME}` or the platform's
  `.codex` default, while bettercodex-owned settings and sessions remain under
  `${BCODEX_HOME}` or the platform's `.bcodex` default;
- the current system prompt, inference, history, tool, and compaction contracts
  do not gain Windows-specific model behavior unless the platform genuinely
  requires it; and
- `/tmux` is a deliberate Unix capability, not a reason to fabricate a Windows
  equivalent.

The product-direction update should replace the prohibition with a concise
statement of the approved targets and point to this specification. It should
not duplicate this document's implementation details.

## 6. Support contract

### 6.1 Operating systems and architectures

| Environment | Initial status | Requirement |
| --- | --- | --- |
| Windows 11 x64, current servicing release | Supported | Required CI and interactive coverage |
| Windows 10 x64 1809+ | Best effort | ConPTY-capable; periodic compatibility coverage |
| Windows Server 2019+ | CI/headless only | Process and noninteractive tests; no desktop UX promise |
| Windows on ARM64 | Unsupported initially | Revisit only after x64 is stable and upstream V8 artifacts are verified |
| 32-bit Windows | Unsupported | Fail before download or compilation |
| WSL 1 or WSL 2 | Linux target | Use the Linux installer and Linux behavior |

The Windows 11 requirement follows current official
[native Codex Windows guidance](https://learn.chatgpt.com/docs/windows/windows-sandbox)
and gives a maintainable primary target. This reference does not import Codex's
sandbox into bettercodex. The technical floor remains Windows 10 1809 because
that is the first client release with ConPTY. The installer must report an
actionable error on unsupported architecture or OS, rather than continuing
until compilation or terminal initialization fails mysteriously.

### 6.2 Terminal hosts

| Terminal host | Initial status | Notes |
| --- | --- | --- |
| Windows Terminal | Primary | Full shortcut, paste, color, resize, title, clipboard, and scrollback coverage |
| VS Code integrated terminal | Primary | xterm.js behavior, rapid-character paste, resize, and scrollback coverage |
| WezTerm on Windows | Compatible | Smoke coverage; no terminal-specific feature dependency |
| Current ConPTY-based IDE terminals | Compatible | Best effort after the primary matrix passes |
| Legacy Console Host | Unsupported/best effort | No release blocker unless product direction later promotes it |
| Redirected stdin/stdout | Noninteractive only | Interactive launch must fail clearly when no usable terminal exists |

"Supported terminal" means the tested combination, not merely any process that
sets `TERM` or accepts ANSI bytes. A terminal-owned shortcut can prevent an
application from receiving a chord; bettercodex must provide documented
fallbacks instead of claiming it can override the terminal.

### 6.3 Shells

The native shell preference order is:

1. an explicitly supplied, valid model-selected shell path;
2. PowerShell 7 (`pwsh.exe`);
3. Windows PowerShell (`powershell.exe`); and
4. Command Prompt (`cmd.exe`).

PowerShell command execution must use PowerShell argument semantics. `cmd.exe`
must use `/c`. POSIX shells retain `-c` or `-lc` only on Unix. Git Bash may work
when explicitly selected, but it is not an initial compatibility target and
must not displace native PowerShell as the Windows default.

The installer script itself must run on Windows PowerShell 5.1 as well as
PowerShell 7. The runtime may prefer PowerShell 7 without requiring it.

## 7. Authoritative basis

Implementation work must re-check all of these sources because terminal and
upstream Codex behavior continue to change:

- current [OpenAI Codex source](https://github.com/openai/codex), especially
  [`codex-rs/utils/pty`](https://github.com/openai/codex/tree/main/codex-rs/utils/pty),
  the TUI event/console code, shell detection, paste-burst handling, clipboard
  code, and PowerShell installer;
- current [Codex CLI interactive documentation](https://developers.openai.com/codex/cli/features)
  and official Windows guidance;
- Microsoft's
  [pseudoconsole overview](https://learn.microsoft.com/en-us/windows/console/pseudoconsoles),
  which specifies UTF-8 pseudoconsole streams;
- Microsoft's
  [virtual-terminal sequence documentation](https://learn.microsoft.com/en-us/windows/console/console-virtual-terminal-sequences)
  and [console-mode definitions](https://learn.microsoft.com/en-us/windows/console/high-level-console-modes);
- the documented
  [Windows Terminal actions and default key bindings](https://learn.microsoft.com/en-us/windows/terminal/customize-settings/actions);
  and
- the Rust standard library and dependency source at the repository's pinned
  toolchain and lockfile revisions.

The drafting audit compared bettercodex with upstream Codex `main` at
`646f7c0a91b8e327d263335da68ae8ef212895ce` on 2026-08-09. That hash records
what informed this proposal. It must not be used instead of fetching current
upstream for implementation or review work.

The implementation-completion source audit re-fetched upstream `main` at
`a16863f8704831d13e041ed7dba2c4a57a2a940b` on 2026-08-09. Retained ConPTY,
pipe, Job Object, Windows input-normalization, console-mode, shell-detection,
paste-burst, clipboard, and terminal behavior was compared against that source.
The reduced one-package layout and bettercodex's no-sandbox/no-configuration
scope explain the retained structural differences. Future changes must still
re-fetch current upstream rather than treating either revision as a source pin.

## 8. Pre-port gap inventory

When this specification was drafted, the package could not compile or operate
natively on Windows. The gaps that drove the port are preserved below as
historical design evidence; they do not describe the implementation candidate
now in the tree. Validation must inspect the current tree rather than treating
this table as a live checklist.

| Area | Current files | Current limitation | Required direction |
| --- | --- | --- | --- |
| Product policy | `progressive_disclosure/product-direction.md`, `docs/install.md`, `README.md` | Windows explicitly unsupported | Approve and document the support contract |
| Cargo and V8 | `Cargo.toml`, `scripts/cargo-with-v8.sh` | Unconditional Unix dependency and shell-only target mapping | Target-gated dependencies and a verified PowerShell build path |
| Process runtime | `src/process_runtime.rs`, `src/tools/process_session.rs` | Unix file descriptors, `openpty`, process groups, signals, and `/dev/fd` | Port current upstream ConPTY, pipe, and Job Object behavior |
| Shell detection | `src/shell_command/shell_detect.rs` | `getpwuid_r`, Unix fallback paths, no `Cmd` variant | Target-specific detection and native Windows argument derivation |
| TUI lifecycle | `src/tui/terminal.rs` | Unix descriptors, `libc`, `/dev/tty`, Unix polling, unconditional terminal features | Windows console-mode guard and nonblocking native probes |
| Composer input | `src/tui/view.rs`, `src/tui/editor.rs` | Relies on explicit paste events; macOS-only hint labels | Paste-burst state machine and platform-aware shortcut hints |
| Clipboard | `src/tui/clipboard.rs` | Native copy implementations only for Linux and macOS | Native Windows clipboard with terminal fallback |
| State and auth | `src/auth.rs`, `src/state_file.rs`, `src/prompt_history.rs`, `src/rollout.rs` | Unix modes, `flock`, directory `fsync`, replacement assumptions | Cross-platform locking, durable replacement, and Windows path handling |
| Embedded skills | `src/system_skills.rs` | Unix modes, `O_NOFOLLOW`, `flock`, rename and directory-sync assumptions | Preserve integrity with Windows-safe locks and reparse-point handling |
| Git and tools | `src/tui/git_diff.rs`, `src/tools/patch.rs`, `src/tools/papercuts.rs` | `/dev/null`, Unix byte paths, Unix error constants and open flags | Native path/null-device/error and safe-open behavior |
| Managed terminal | `src/managed_session.rs` | Unix sockets, descriptor passing, PTYs, signals, and tmux | Compile-time Windows stub; hide `/tmux` |
| Installer/updater | `scripts/install.sh`, `src/update.rs`, `spec-install.md` | `/bin/sh`, Unix cache/PATH/atomic-replace assumptions | PowerShell installer and Windows-safe update finalization |
| CI | repository workflows and local recipes | No Windows target or interactive matrix | Native Windows compile, test, installer, and ConPTY coverage |

The transcript scrollback path must continue using full-screen line-feed
scrolling rather than assuming `CSI S` inserts displaced rows into terminal
history. Windows Terminal and xterm.js can treat `CSI S` as active-page editing
and discard those rows. This portability invariant must remain covered while
the broader Windows lifecycle is ported.

## 9. Architectural rules

### 9.1 Port, do not reimplement

For every retained behavior, start with the current upstream source and reduce
it to the one-package bettercodex architecture. The existing focused Unix
runtime was itself reduced from upstream `codex-utils-pty`; the Windows backend
should complete that port, not introduce an unrelated process library with
different cancellation and descendant semantics.

Upstream workspace crates may be folded into private modules because
bettercodex intentionally has one package. File organization may differ, but
behavioral departures must be explained by a bettercodex requirement.

### 9.2 Keep platform boundaries semantic and narrow

Use `#[cfg(unix)]` and `#[cfg(windows)]` modules around operations that really
differ:

- process and pseudo-terminal creation;
- process-tree signalling and cleanup;
- terminal console-mode acquisition/restoration;
- secure file opening, replacement, and directory durability;
- home/cache/install path discovery; and
- the managed tmux entry point.

Do not create wrappers for ordinary `std::fs`, `Path`, or terminal operations
that already work cross-platform. Conversely, do not scatter raw Win32 calls
through the TUI, state, and installer modules. A small private
platform helper is justified when several security-sensitive call sites need
the same replacement or reparse-point invariant.

### 9.3 Preserve native values

Program paths, working directories, environment keys and values, and file paths
must stay as `OsStr`/`OsString`/`Path`/`PathBuf` until an external textual
protocol explicitly requires encoding. The current process-session path that
converts a shell path and the complete environment to UTF-8 strings must be
removed or constrained to Unix-only protocol boundaries.

Windows path comparison must account for case-insensitive filesystem behavior
where identity matters, but ordinary user-visible paths should retain their
original spelling. Code must not manually split Windows paths on `:` or `\`.

### 9.4 Fail closed for integrity, degrade gracefully for decoration

Failure to verify an installer input, protect a sensitive state transition,
create a command process tree, or restore a valid persisted state is fatal to
that operation. Failure to enable an optional keyboard enhancement, query a
terminal palette, set a title, or send a notification should produce a bounded
diagnostic or fallback without preventing the TUI from starting.

Terminal initialization errors that leave the console in an unknown mode are
not decorative. They must unwind through the mode guard and restore the modes
captured at entry.

## 10. Build, dependency, and V8 requirements

### 10.1 Rust target

The initial Rust target is `x86_64-pc-windows-msvc`. GNU ABI targets must not be
advertised or silently selected. The MSVC target matches current upstream
Codex, the available rusty V8 artifact, ConPTY/Win32 dependencies, and the
standard supported Rust toolchain on Windows.

The manifest must move Unix-only dependencies such as `libc` under a Unix
target table. Windows-only dependencies must be under a Windows target table so
they are absent from Linux and macOS resolution and linking whenever Cargo
permits. Shared dependencies already used by the TUI, such as `crossterm`,
`ratatui`, and `arboard`, should not be duplicated.

The likely direct additions, subject to the current upstream implementation,
are:

| Dependency | Purpose | Scope |
| --- | --- | --- |
| `portable-pty` | Shared PTY API retained by current upstream | Prefer target/use minimization consistent with the port |
| `filedescriptor` | Windows descriptor adaptation used by upstream PTY code | Windows only |
| `shared_library` | ConPTY compatibility loading used by upstream | Windows only |
| `winapi` | Job Object, process, pipe, and pseudoconsole APIs in the retained upstream implementation | Windows only |
| `windows-sys` | Console modes and focused modern Win32 filesystem/terminal calls | Windows only |

`arboard` already selects its Windows clipboard backend on Windows. Do not add
a second clipboard framework unless measured behavior demonstrates a gap.

Current upstream Codex patches `crossterm` to an OpenAI-maintained revision.
Implementation must inspect why that patch exists at port time. Adopt the
current patch only when its retained Windows behavior is needed and covered by
a bettercodex test; do not pin the historical drafting revision merely because
it appears in upstream today.

Every new dependency must pass license, duplicate-functionality, transitive-
size, and target-gating review. A newer Win32 binding is not automatically an
improvement if it requires rewriting known-good current upstream code. Avoid
linking both broad `windows` and `windows-sys` crates when the narrow bindings
above are sufficient.

### 10.2 V8 artifacts

The checked-in Cargo wrapper currently recognizes Linux and macOS only. Windows
support must resolve and verify the exact upstream artifacts for:

```text
x86_64-pc-windows-msvc
rusty_v8_ptrcomp_sandbox_release_x86_64-pc-windows-msvc.lib.gz
src_binding_<rusty-v8-version>_rusty_v8_ptrcomp_sandbox_release_x86_64-pc-windows-msvc.rs
```

The exact names and checksum source must be derived from current upstream at
implementation time. The sandbox word in the V8 build configuration describes
V8's pointer-compression/sandbox build feature; it does not add the Codex
command sandbox that bettercodex intentionally omits.

Add `scripts/cargo-with-v8.ps1`, or replace only the artifact-resolution portion
with a genuinely cross-platform helper if that is smaller. The PowerShell entry
point must:

1. read the pinned rusty V8 crate version from the lockfile or the same source
   used by the Unix wrapper;
2. map the selected Rust target explicitly and reject unknown targets;
3. download over HTTPS with bounded timeouts and response sizes;
4. verify both artifact and generated-binding SHA-256 values before use;
5. handle checksum manifests with LF or CRLF without changing the bytes being
   checked;
6. use a cache scoped by target, V8 version, build features, and digest;
7. never accept a partial cache entry after interruption;
8. set the same V8 environment variables as the Unix build; and
9. delegate the caller's Cargo arguments without reparsing or lossy quoting.

Raw Cargo remains unsupported for build/check/test commands that need the
published V8 artifacts. Documentation and CI must use the platform's checked-in
wrapper.

### 10.3 Developer prerequisites

A native source build requires:

- 64-bit Windows 11 or a compatible Windows 10 installation;
- PowerShell 5.1 or newer;
- the pinned Rust MSVC toolchain;
- Microsoft C++ Build Tools and a compatible Windows SDK/linker;
- several gigabytes of persistent cache space; and
- network access to GitHub and the official Rust/upstream V8 hosts used by the
  existing source-build policy.

Git for Windows is required for repository features that invoke `git`, though
the installer may download a source archive without Git. Missing Git should be
reported when Git-backed behavior is requested and may be warned about during
installation; it must not be confused with a compiler failure.

The public installer must detect the MSVC linker and SDK before starting the
long Cargo build. It should print an exact Visual Studio Build Tools workload
command or official setup route when they are missing. Automatically installing
the multi-gigabyte Visual Studio workload should require explicit operator
consent; a piped bootstrap must not silently add it merely because `winget`
exists.

## 11. Installation and update requirements

### 11.1 PowerShell installer

Add `scripts/install.ps1` as the Windows counterpart to `scripts/install.sh`.
It must implement the existing [`spec-install.md`](spec-install.md) exact-
revision contract rather than upstream Codex's versioned release installer.
Current upstream PowerShell code is useful for strict-mode scripting, bounded
downloads, hash verification, user PATH mutation, architecture detection,
locking, and Windows path handling; its GitHub Release selection and multi-file
package layout do not belong in bettercodex.

The canonical bootstrap should have a documented form equivalent to:

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -Command "irm 'https://raw.githubusercontent.com/ummay0432/bettercodex/main/scripts/install.ps1' | iex"
```

An equivalent `pwsh` command may be documented. The script must not require
users to weaken their machine-wide execution policy permanently.

Unless overridden by an absolute `BCODEX_INSTALL_DIR`, the Windows command
directory should be:

```text
%LOCALAPPDATA%\Programs\bettercodex\bin
```

The installed command is `bcodex.exe`. The installer should update the current
process PATH for its own verification and the user's persistent PATH for future
terminal windows. It must compare semicolon-delimited PATH entries
case-insensitively, avoid duplicates, preserve unrelated entries, and report an
older `bcodex.exe` that wins PATH precedence.

The default persistent build/cache root should be:

```text
%LOCALAPPDATA%\bettercodex\cache
```

An explicit absolute cache override may reuse the current cross-platform
environment contract if one exists at implementation time. Source extraction,
staging, and smoke-test homes remain disposable. `BCODEX_HOME` and `CODEX_HOME`
must not be repurposed as compiler caches.

### 11.2 Exact-main transaction

The Windows installer must preserve the same transaction as Unix:

1. Validate repository and optional revision inputs.
2. Resolve `refs/heads/main` once to one full 40-character commit unless a
   validated immutable revision was supplied by the updater.
3. Download the source archive for exactly that commit.
4. Validate the checked-in lockfile, toolchain file, PowerShell V8 wrapper, and
   required checksums.
5. Reuse target-specific Cargo, registry, Rust toolchain, and V8 caches without
   trusting stale partial entries.
6. Hash all release-relevant source bytes and build `--release --locked --bin
   bcodex` with that content hash tracked independently of archive mtimes.
7. Verify version metadata, release-input hash, V8 initialization, embedded
   resources, and the internal smoke test in isolated Windows user/application
   homes.
8. Stamp and verify the exact source revision using the existing fixed-size
   marker contract.
9. Stage the verified executable beside the destination without following a
   reparse point or replacing unrelated bytes.
10. Replace or schedule replacement of `bcodex.exe` using the Windows-safe
    finalization protocol below.
11. Verify the visible command and embedded revision after replacement before
    reporting final success.

The selected commit remains fixed even if public `main` advances during the
build. The next launch discovers a later commit through the existing bounded
background check.

### 11.3 Running-executable replacement

Windows can deny deletion or replacement of a mapped executable. The updater
must not assume the Unix `rename(temp, destination)` behavior works while
`bcodex.exe` is running.

The implementation must use a tested two-process finalization protocol. A
recommended shape is:

1. The existing process and installer build and fully verify a candidate in a
   private same-volume staging location.
2. A verified candidate or tightly bounded PowerShell finalizer receives the
   parent PID, destination, stage, expected digest, and expected source
   revision through validated arguments or a private manifest.
3. The existing `bcodex.exe` exits before destination replacement begins.
4. The finalizer waits for that exact process to exit, revalidates the stage,
   and performs a same-volume Windows replace with an old-binary backup.
5. It executes the visible command's version/revision verification.
6. On failure it restores the backup; on success it removes the backup and
   records completion.
7. A later installer invocation safely recovers only finalizer artifacts named
   in its private transaction record.

The finalizer is temporary installation machinery, not a second installed
product binary. It must not trust only a reusable PID, unbounded path from the
environment, or a writable global temporary directory. Tests must force parent
delay, PID exit, destination contention, antivirus-style transient sharing
violations, final verification failure, and process crash at every transition.

If a reliable no-extra-binary replace API is available in the supported
Windows baseline, it may replace this protocol only after equivalent failure-
atomicity tests demonstrate that it works for a running installed executable.

### 11.4 Locking, cleanup, and recovery

Installation must use an exclusive per-install-root lock. Lock ownership and
transaction records must distinguish an active installer from stale debris
without deleting another process's stage. PowerShell process IDs alone are not
sufficient identity after PID reuse; include a private random transaction ID
and validated paths.

Cleanup must:

- run after success and catchable failure;
- remove only transaction-owned extraction, staging, smoke, and compiler
  scratch paths;
- reject or avoid reparse points in installer-owned directory traversal;
- preserve an existing verified `bcodex.exe` on every pre-commit failure;
- preserve Cargo, registry, Rust, and V8 caches that remain valid;
- leave a bounded recovery record for an uncatchable crash; and
- never recursively scan or delete targets by name, age, or broad parent path.

The installer should retry bounded transient Windows sharing violations with
jitter. Permission denial, untrusted reparse points, malformed metadata, and
checksum mismatch are hard failures, not retry loops.

### 11.5 Update command

`bcodex update` must choose `scripts/install.ps1` on Windows and
`scripts/install.sh` on Unix. The Windows path must fetch the script from the
already resolved immutable commit, validate a bounded PowerShell script
response, and pass the selected repository/revision/install directory without
shell-string interpolation vulnerabilities.

The command must not require `curl` or `/bin/sh` on Windows. It may use the
existing Rust HTTP client and invoke PowerShell with an on-disk verified script,
which is preferable to embedding a long script in `-Command` quoting.

The background freshness check remains failure-silent. Explicit update errors
remain actionable and preserve the installed command. Update checks continue
to compare full source revisions only.

## 12. Process and pseudo-terminal runtime

### 12.1 Upstream basis

Port the retained current upstream process utility, including its common
process, pipe, PTY, process-group, Windows input-normalization, ConPTY, and Job
Object behavior. In the drafting baseline these sources total roughly 2,850
lines across common and Windows modules; bettercodex's existing focused Unix
runtime is roughly 680 lines. The goal is a reduced behavioral port, not an
unreviewed wholesale copy and not a second package.

### 12.2 Required process modes

Both existing process modes must work:

- **Piped mode:** separate bounded stdout/stderr collection for commands that
  do not need terminal interaction.
- **PTY mode:** one ConPTY-backed terminal stream for interactive commands,
  resize, and later `write_stdin` calls.

For both modes, the runtime must preserve:

- the exact executable and argument boundaries;
- the native working directory and environment;
- bounded output retention and polling semantics;
- independent process exit and output-reader completion;
- cancellation and timeout behavior;
- no zombie, orphaned reader, leaked pipe, pseudoconsole, process, thread, or
  Job Object handle; and
- the existing model-visible process-session contract.

### 12.3 ConPTY behavior

The Windows PTY backend must:

1. create the pseudoconsole with the requested rows and columns;
2. connect synchronous input/output handles as required by
   `CreatePseudoConsole`;
3. use the UTF-8 stream contract documented by Microsoft;
4. attach the child through the required process-thread attribute list;
5. resize with `ResizePseudoConsole` and serialize resize against teardown;
6. close all duplicated and inherited handles on every branch;
7. avoid deadlock when the root process exits before descendants close output;
8. normalize TTY input as current upstream requires on Windows, including LF to
   carriage return and Backspace to DEL where appropriate; and
9. close the pseudoconsole only after readers and process-lifetime rules allow
   it.

Input normalization applies to bytes sent to the PTY, not to arbitrary piped
stdin or persisted prompt text. The implementation must port the current
upstream tests for newline, Backspace, split UTF-8, close, and flush behavior.

### 12.4 Process trees and interruption

Windows has no Unix `setsid`, `killpg`, or portable `SIGINT` equivalent. The
runtime must use current upstream Job Object/process-group behavior so explicit
interrupt and teardown affect the intended command tree rather than only the
immediate shell.

The externally visible semantics are:

- a soft interrupt should first use the terminal/control path appropriate for
  an interactive process;
- a hard kill or expired cleanup deadline must terminate the owned process
  tree;
- normal root-process exit must not prematurely destroy still-relevant output
  from descendants;
- cancellation must not kill unrelated processes that escaped or were never
  assigned to the owned job;
- dropping the session must eventually release the complete owned tree and all
  handles; and
- exit code and interruption status must remain coherent for the model and TUI.

Do not approximate this with `Child::kill()` alone. Port upstream's Job Object
logic and its tests, then add bettercodex-specific tests for the session polling
and bounded-output adapter.

### 12.5 Shell command construction

Add `ShellType::Cmd`. Shell executable detection must be case-insensitive and
must recognize `.exe` file names. The Windows implementation must search
ordinary PowerShell 7 and Windows PowerShell locations in addition to `PATH`,
without calling Unix account APIs.

Command argument derivation is:

| Shell | Login/profile request | Arguments |
| --- | --- | --- |
| Bash/Zsh/Sh | login | `-lc <command>` |
| Bash/Zsh/Sh | non-login | `-c <command>` |
| PowerShell | profile/login | `-Command <command>` |
| PowerShell | no profile | `-NoProfile -Command <command>` |
| Cmd | either | `/c <command>` |

Arguments must be passed as an argument vector. Do not build another quoting
layer around the complete command line.

Unix locale overrides such as `LANG=C.UTF-8`, `LC_CTYPE`, and `LC_ALL` should be
target-gated; they are not a Windows encoding mechanism. Portable controls such
as `NO_COLOR`, `PAGER`, and `GIT_PAGER` may remain. ConPTY itself supplies the
UTF-8 stream boundary.

### 12.6 Process regression suite

At minimum, native Windows tests must cover:

- successful and failing piped commands with independent stdout/stderr;
- output larger than retained head and tail limits;
- a command path and working directory containing spaces and non-ASCII text;
- PowerShell 7, Windows PowerShell, and cmd fallback selection;
- PTY TTY detection, resize, Unicode output, and split UTF-8 reads;
- `write_stdin` text, newline, Backspace, and EOF/close behavior;
- soft interrupt, hard kill, timeout, and cancellation while output is active;
- a shell that starts a child and grandchild;
- root exit while a descendant retains output handles;
- descendant cleanup after explicit cancellation;
- simultaneous exit and reader completion;
- repeated spawn/interrupt cycles with stable process and handle counts; and
- failure injection at every pipe, pseudoconsole, attribute-list, process, and
  job-assignment step.

## 13. Terminal lifecycle and rendering

### 13.1 Console-mode guard

Windows TUI startup must own a guard that captures every console mode it
changes and restores the exact relevant original bits on normal return, error,
panic cleanup, login-screen transitions, and alternate-screen transitions.

The output path must enable `ENABLE_PROCESSED_OUTPUT` and
`ENABLE_VIRTUAL_TERMINAL_PROCESSING` on usable stdout and stderr console
handles. A redirected or non-console handle should be recognized rather than
treated as a failing console API call.

Current upstream crossterm handling uses Windows input records and therefore
clears `ENABLE_VIRTUAL_TERMINAL_INPUT`, records whether that bit was originally
set, reasserts record mode while polling, and restores the original setting on
exit. Port the then-current upstream behavior and its tests. Do not independently
toggle mode bits based on old crossterm assumptions.

The mode guard must be nest-safe for actual bettercodex lifecycle transitions
or explicitly reject unsupported nesting. A static stack that can silently
restore another TUI instance's mode is unacceptable.

### 13.2 Optional terminal capabilities

Windows support must follow these degradation rules:

- keyboard-enhancement flags are optional; an unsupported push must not abort
  startup;
- focus-change reporting should remain disabled on Windows unless current
  upstream and native tests prove it reliable;
- bracketed paste should be enabled where supported, but the composer must not
  depend on receiving `Event::Paste`;
- mouse capture remains off unless bettercodex separately adopts it;
- title, palette, hyperlink, and notification probing must be bounded and may
  fall back cleanly; and
- every escape sequence enabled during startup must have a matching disable or
  reset on teardown when the protocol requires one.

The TUI must continue accepting key press and repeat events according to its
existing behavior. It must not double-apply a key because Windows emits both a
record and a synthesized virtual-terminal sequence.

### 13.3 Startup probes

The current color probe uses Unix descriptors, `/dev/tty`, `fcntl`, `poll`, and
`read`. On Windows it must use console handles or a current upstream terminal-
probe path with the same hard timeout and response bound. A palette query must
never consume ordinary user key events, block startup indefinitely, or leave
probe responses in the composer.

If safe palette probing is unavailable in a terminal, use the existing default
palette behavior. Do not make cosmetic color discovery a condition for login or
chat.

### 13.4 Scrollback and viewport behavior

The normal-screen inline viewport must preserve committed transcript rows in
the terminal host's scrollback. In particular:

- scrolling the full page must use line feeds at the bottom of the full
  scrolling region rather than relying on `CSI S` history insertion;
- the mutable viewport must be repainted after history insertion;
- resize must preserve the transcript and composer without duplicated or
  missing rows;
- very tall cells must remain bounded by the existing history-row cap; and
- Windows Terminal and VS Code tests must verify that early transcript markers
  remain reachable in actual scrollback.

Parser-only tests are useful but insufficient because a VT parser may model
`CSI S` differently from the host terminal. Keep byte-level assertions for the
portable scrolling sequence and perform an interactive smoke test in both
primary terminals.

### 13.5 Terminal shutdown

Normal exit, Ctrl+C exit, panic handling, failed initialization, login restart,
and updater exit must all leave the terminal usable. Acceptance requires:

- raw mode disabled when bettercodex enabled it;
- cursor shown;
- colors and attributes reset;
- bracketed paste and any enabled keyboard protocol disabled;
- original Windows input/output mode bits restored;
- no partial escape response left on screen; and
- subsequent PowerShell input, selection, copy, and Enter behavior working
  without reopening the terminal.

## 14. Composer input and paste

### 14.1 Why explicit paste events are insufficient

Windows terminals can deliver pasted text as a rapid sequence of character and
Enter key events instead of one bracketed-paste event. Treating those Enter
events as ordinary input can submit the prompt halfway through a multiline
paste. A Windows port without a paste-burst detector is therefore not usable,
even if bracketed paste works in one tested host.

Port the current upstream `paste_burst` state machine, reduced only where the
local composer genuinely differs. Preserve upstream's Windows-specific active
idle tolerance unless newer upstream evidence changes it.

### 14.2 Paste-burst requirements

The composer must:

1. recognize rapid plain-character sequences without delaying normal typing
   perceptibly;
2. buffer a likely burst and retroactively collect the already inserted prefix
   when required;
3. suppress Enter submission while a burst remains active;
4. flush buffered content through the same `insert_paste` path as an explicit
   `Event::Paste`;
5. normalize pasted CRLF for prompt editing without corrupting Unicode;
6. flush pending ordinary typed characters before modified keys, navigation,
   submission, focus changes, or shutdown;
7. clear stale burst windows after non-character input;
8. preserve the current compact representation for large pastes; and
9. never classify reasonably paced human typing as a paste in deterministic
   timing tests.

The event loop must schedule a tick soon enough to flush a pending burst even
when no other UI event arrives. Timing constants must remain private behavior
backed by tests, not a user configuration surface.

### 14.3 Paste and attachment paths

Explicit paste and detected burst paste must share image-path interpretation.
Windows tests must cover:

- `C:\Users\Name\Pictures\image.png`;
- a quoted path containing spaces;
- slash-separated `C:/...` paths accepted by Windows;
- supported `file:///C:/...` URLs if current upstream/local URL normalization
  retains them;
- a UNC path when it exists and can be read;
- CRLF-terminated clipboard text;
- non-ASCII user and file names;
- ordinary prose containing a colon or backslash, which must remain prose; and
- missing, directory, unsupported-format, and oversized files, which must
  produce the existing bounded errors.

Path paste must not perform network discovery merely because text resembles a
UNC path. It should inspect only the path the user actually pasted under the
same attachment rules used on other platforms.

### 14.4 IME and Unicode input

The primary manual matrix must include an Input Method Editor, composed
characters, emoji/surrogate pairs, combining marks, wide glyphs, and halfwidth
characters. The TUI must operate on Rust characters/grapheme-aware display
width where it already does so and must not expose UTF-16 code units as separate
composer actions.

IME composition is terminal-host behavior. bettercodex need not implement an
IME, but committed text must arrive once, render correctly, edit correctly, and
survive submit/resume. Any uncommitted composition limitations should be
documented against the specific terminal host.

## 15. Shortcut contract

### 15.1 Ownership rule

Windows Terminal processes its configured actions before the application. Its
documented defaults use Ctrl+C/Ctrl+Shift+C/Ctrl+Insert, and Enter in applicable
selection behavior, for copy and Ctrl+V/Ctrl+Shift+V/Shift+Insert for paste.
When no selection exists, its copy action sends Ctrl+C through to the
application. bettercodex must design around that ownership rather than
attempting to seize terminal-level chords.

The first Windows release should retain a fixed, platform-aware shortcut set.
It must not import upstream's configurable `/keymap` and configuration
framework solely for Windows. Terminal-owned conflicts can already be changed
in Windows Terminal settings, and application fallbacks cover the required
actions with far less source and maintenance cost.

### 15.2 Required Windows shortcut matrix

| Action | Primary chord | Fallback or alias | Owner and required behavior |
| --- | --- | --- | --- |
| Submit prompt | Enter | None | bettercodex; only unmodified Enter outside an active paste submits |
| Steer active turn | Enter while working | None | bettercodex; preserve current step-boundary semantics |
| Queue follow-up | Tab while working | Existing UI action | bettercodex |
| Insert newline | Shift+Enter | Ctrl+J | bettercodex; Ctrl+J must work when the terminal cannot distinguish Shift+Enter |
| Move one word | Ctrl+Left / Ctrl+Right | Alt+Left / Alt+Right where delivered | bettercodex; hints prefer Windows `Ctrl` terminology |
| Delete previous word | Ctrl+Backspace | Ctrl+W; retain Alt+Backspace where delivered | bettercodex; Ctrl+Backspace must not degrade to one-character deletion |
| Clear/cancel/exit | Ctrl+C | Esc for active interruption where existing | Terminal passes Ctrl+C when no selection; preserve staged clear, interrupt, idle-exit behavior |
| Copy terminal selection | Ctrl+C or Ctrl+Shift+C | Ctrl+Insert; Enter where configured/defaulted | Windows Terminal when text is selected; bettercodex receives no chord |
| Copy latest final response | Ctrl+O | `/copy` | bettercodex; native clipboard first, raw Markdown unchanged |
| Paste clipboard | Ctrl+V | Ctrl+Shift+V or Shift+Insert | Terminal-owned; bettercodex consumes explicit paste or detected character burst |
| Edit queued follow-up | Alt+Up | Shift+Left | bettercodex; retain both because terminal profiles differ |
| Interrupt active work | Esc | Ctrl+C according to current staged behavior | bettercodex |
| Prompt history | Up / Down | None | bettercodex when composer state allows |
| History search | Ctrl+R / Ctrl+S | Existing raw-control fallback | bettercodex |
| File completion | `@` | None | bettercodex text trigger |
| Skill completion | `$` | None | bettercodex text trigger |

Modified Enter must be handled carefully. The existing generic behavior inserts
a newline for Shift, Alt, or Control plus Enter. Windows coverage must verify
the events each primary terminal actually emits; unsupported modifier
combinations need not be advertised merely because the match arm accepts them.

### 15.3 Shortcut hints and help

The shortcut overlay must not display macOS-only `Option` labels on Windows.
Hints may be generated from a tiny compile-time platform label helper or use
portable wording such as `Alt/Ctrl` when the actual handler accepts both. Do not
build a runtime keymap subsystem to change nouns.

The Windows overlay must show at least:

- `Ctrl+Left / Right` for word movement;
- `Ctrl+Backspace / Ctrl+W` for word deletion;
- `Shift+Enter / Ctrl+J` for newline;
- `Ctrl+O or /copy` for the latest response;
- the staged Ctrl+C behavior; and
- a concise note that terminal selection owns copy and terminal paste chords
  insert clipboard content.

Tests must assert both rendered hints and actual key behavior. A help-only fix
without the event handler, or a handler without discoverable Windows wording,
is incomplete.

### 15.4 Shortcut conflict policy

When a supported terminal reserves a chord, use this order:

1. preserve the terminal's standard copy/paste/navigation behavior;
2. provide an application chord that the terminal normally delivers;
3. provide a slash-command or visible UI fallback for important actions; and
4. document how advanced users can unbind a terminal action.

Do not silently change a cross-platform shortcut to solve one user's custom
Windows Terminal profile. Do not use the Windows key as an application modifier
because the operating system reserves most such chords.

## 16. Clipboard, title, notification, and terminal integration

### 16.1 Clipboard copy

Native Windows clipboard copy must be available for Ctrl+O and `/copy` through
the existing `arboard` dependency or current upstream equivalent. The fallback
order for a local native Windows session should be:

1. native Windows clipboard;
2. terminal-mediated OSC 52 when safe and supported; and
3. a bounded actionable error.

The Unix `/dev/tty` OSC 52 path must be target-gated. Windows fallback output
must use an appropriate console/output handle without corrupting the Ratatui
frame. tmux clipboard integration remains Unix-only.

Clipboard operations must preserve raw Markdown exactly, including CR/LF
normalization chosen by the existing copy contract, and must not keep sensitive
response text in logs. If the Windows clipboard backend needs an ownership
lease, hold it only as long as required by that backend and release it on
replacement or shutdown.

### 16.2 Clipboard paste

The application should not bind Ctrl+V directly. Windows Terminal pastes text
through terminal input, after which explicit-paste and paste-burst handling own
the data. This preserves users' normal terminal configuration and supports
Ctrl+Shift+V and Shift+Insert without duplicate code.

### 16.3 Terminal title

OSC title updates may remain when virtual-terminal output is enabled. Title
failure is nonfatal, and shutdown must restore or clear title state according
to the existing cross-platform contract. Tests must ensure title sequences do
not leak into redirected output.

### 16.4 Notifications

Windows Terminal supports OSC 9 notifications. Automatic capability detection
should recognize current upstream evidence such as `WT_SESSION`, while BEL
remains a fallback where appropriate. Notification failure must not fail a turn
or write visible escape garbage into the conversation.

### 16.5 Hyperlinks and color

OSC 8 hyperlinks and truecolor should work through virtual-terminal output when
the host supports them. Capability detection must remain conservative and
bounded. Rendering tests need Windows-width cases, but the model-visible text
and copyable transcript must remain free of styling bytes.

## 17. Paths, state, locking, and durable writes

### 17.1 Home and cache discovery

Environment variables retain first priority:

| Purpose | Override | Windows default |
| --- | --- | --- |
| bettercodex settings/sessions | `BCODEX_HOME` | `%USERPROFILE%\.bcodex` |
| shared Codex credentials/history | `CODEX_HOME` | `%USERPROFILE%\.codex` |
| installed command | `BCODEX_INSTALL_DIR` | `%LOCALAPPDATA%\Programs\bettercodex\bin` |
| reusable build cache | applicable cache override | `%LOCALAPPDATA%\bettercodex\cache` |

Empty overrides are ignored consistently with the current Unix behavior.
Fallback home discovery may use `USERPROFILE`, then the documented Windows home
combination or a small established helper. It must not call Unix passwd APIs on
Windows. A missing home must produce the same explicit inability-to-persist
error as other platforms rather than selecting the process working directory.

Native Windows and WSL defaults intentionally point to different filesystems.
No automatic session or credential migration is part of this port.

### 17.2 File locking

Replace direct `libc::flock` call sites with the pinned Rust standard library's
cross-platform file-locking APIs where their semantics match the current
exclusive/shared lock contract. Keep locks attached to owned `File` values and
release them deterministically. Nonblocking acquisition must distinguish
contention from malformed state or permission failure.

Tests must use separate processes, not only threads, because the relevant
locking semantics protect installer, history, rollout, state, and skills across
processes.

### 17.3 Atomic replacement

Create one reviewed Windows replacement helper for state/auth/skill files and
directories where `std::fs::rename` cannot provide the existing replace-
existing guarantee. It must:

- stage on the same volume and preferably in the same parent directory;
- flush file contents before commit where the existing contract requires it;
- preserve the old destination until the replacement operation commits;
- distinguish files from directory trees;
- reject an unexpected reparse-point destination;
- recover a backup only when it belongs to the same transaction; and
- leave either the complete old value or complete new value after injected
  failure, never a truncated hybrid.

Use the narrow supported Win32 replacement operation appropriate to each file
type, following current upstream or Rust behavior. Do not assume opening a
directory and calling `sync_all` is portable; Unix directory durability may be
target-gated while Windows uses its supported flush/replace semantics.

### 17.4 Permissions and access control

Unix `0600`, `0700`, and umask operations must be target-gated. Windows state
created below the user's profile should inherit that user's normal DACL, but
sensitive auth and transaction files must not be knowingly created in a
world-writable location or through a reparse point.

The Windows implementation must define and test the practical equivalent of
the existing security invariants:

- another ordinary local user must not receive intentionally broad access to
  auth tokens or private transaction manifests;
- an attacker-controlled symlink, junction, mount point, or other reparse point
  must not redirect installer cleanup or embedded-skill replacement;
- temporary files must use unpredictable names and exclusive creation; and
- diagnostics must not print token contents or private file bytes.

Do not build a general ACL-management subsystem unless inherited profile ACLs
fail these checks. If explicit ACL changes are required, keep them narrow and
test on supported Windows editions.

### 17.5 Path fidelity

All persistence and tool paths must cover:

- spaces;
- non-ASCII BMP and supplementary characters;
- drive-rooted paths;
- slash and backslash separators where Rust accepts both;
- case variants that name the same path where identity matters;
- UNC paths for ordinary repository access; and
- long paths within the supported Rust/Win32 and external-tool behavior.

Do not canonicalize merely for display or command construction. Canonicalize or
open by handle only where containment or identity is a security requirement.
External tools such as Git may impose their own path limitations; bettercodex
must report those tool failures accurately rather than corrupting the path first.

## 18. Git, patch, papercut, and repository tools

### 18.1 Git diff and hooks

Windows Git helpers must replace `/dev/null` with the Windows null device
(`NUL`) where a null path is genuinely required. Hook suppression must use a
Git-supported Windows value and be tested with Git for Windows; it must not
depend on a POSIX compatibility shell.

Remove Unix-only `OsStringExt::from_vec` assumptions. Keep repository paths as
native arguments to `Command`. Git stdout/stderr protocols that are defined as
bytes may be decoded according to the existing Git parser, but path arguments
must not become lossy strings.

### 18.2 Patch application

Patch behavior must remain the same logical repository operation on all
platforms. Windows coverage must include:

- relative paths containing spaces and non-ASCII names;
- model-emitted `/` separators on a Windows host;
- drive-qualified and UNC absolute paths wherever the current patch policy
  allows or rejects absolute paths;
- existing CRLF files, including edits that do not rewrite unrelated lines;
- file creation and deletion;
- directory/file type mismatches;
- a reparse-point path at a protected boundary; and
- portable `ErrorKind` mapping instead of constructing errors from Unix errno
  constants.

The port must not globally convert the checkout to CRLF or LF. Any line-ending
normalization remains an explicit patch behavior and must be tested against
current upstream Codex.

### 18.3 Papercut logging

Papercut file append and lock behavior must compile and remain process-safe on
Windows. Unix `O_NOFOLLOW` and `O_NONBLOCK` flags need a Windows-safe open and
identity check, not an unconditional removal of the anti-redirection
protection. Contention remains nonfatal according to the current papercut
contract; unsafe destination identity is not treated as ordinary contention.

## 19. Managed tmux behavior

`src/managed_session.rs` is intentionally Unix-specific. It uses Unix sockets,
descriptor passing, pseudoterminal ownership transfer, signals, and tmux. The
native Windows port must not reproduce this with Windows Terminal tabs or a
second supervisor architecture.

On Windows:

- interactive startup proceeds directly without the managed tmux supervisor;
- `is_tmux_active()` is false;
- `/tmux` is absent from slash-command discovery and help;
- entering or preparing a tmux handoff is not representable in Windows-only
  action enums/call paths where compile-time gating can remove it cleanly; and
- an old resumed transcript containing `/tmux` text remains ordinary history,
  not a migration request.

On Unix, current live-migration behavior and tests must remain unchanged. WSL
users running the Linux binary retain the Linux `/tmux` capability.

## 20. Authentication, sessions, skills, and application lifecycle

### 20.1 Authentication

Device-code/ChatGPT login must work in native PowerShell and the primary
terminal hosts. Browser launch is optional convenience; if Windows cannot open
the browser, the login screen must continue showing the URL and one-time code.

Auth persistence must use the Windows-safe private-file and replacement
primitives defined above. Tests must verify:

- a first login creates the expected `%USERPROFILE%\.codex` state;
- replacement of existing credentials works despite Windows replace semantics;
- interrupted replacement preserves the old valid credential file;
- logout removes only the intended credential state;
- diagnostics redact tokens; and
- a non-ASCII profile path works.

The HTTP/TLS stack must use its current native-root behavior on Windows. Do not
add a separate certificate store or proxy subsystem merely for the port; follow
current upstream if the retained client requires Windows-specific native-root
or system-proxy handling.

### 20.2 Rollouts and resume

Saved JSONL sessions, rollout indexes, history search, compaction state, and
resume ordering must behave identically across supported platforms. JSONL is an
application format and may continue using LF regardless of the host's text-file
convention.

Windows tests must cover concurrent readers/writers, stale lock recovery,
interrupted writes, timestamp ordering, non-ASCII paths, and resume after a
process crash. Unix creation modes and directory flushes may differ internally;
the externally visible atomicity and privacy promises may not.

### 20.3 Embedded system skills

Embedded skill installation must retain content hashing, lock exclusion,
staging, backup recovery, symlink/redirection resistance, and complete resource
materialization. On Windows, junctions and other reparse points are part of the
threat model. A failed skill update must leave either the complete old embedded
set or the complete verified new set.

The implementation must not turn a Unix `O_NOFOLLOW` compile error into an
unconditional follow of an installer-controlled destination. Handle-based or
metadata checks must be performed close enough to replacement to avoid an
obvious check/use race under the supported threat model.

### 20.4 Application startup

Startup ordering should remain:

1. validate homes and materialize embedded resources;
2. authenticate or render login as needed;
3. initialize the terminal through the platform guard;
4. render the first frame;
5. begin the bounded background update check; and
6. process input and streamed events.

Windows-specific work must not move network lookup or V8 initialization into a
new serial hot path without measurement. Terminal console-mode setup should be
milliseconds, not a visible startup phase.

## 21. Error handling and diagnostics

Windows errors should identify the failed operation and relevant path or
program while retaining `std::io::Error`/Win32 context. Raw numeric OS errors
may be included as secondary detail; they are not a substitute for an
actionable message.

Required diagnostics include:

- unsupported Windows version or architecture;
- missing MSVC Build Tools/Windows SDK before compilation;
- missing PowerShell, Git, or required shell at the point it matters;
- no usable interactive terminal or virtual-terminal output mode;
- ConPTY unavailable despite a claimed supported OS;
- command creation, Job Object assignment, resize, input, interrupt, and
  cleanup failure;
- invalid or inaccessible `BCODEX_HOME`, `CODEX_HOME`, cache, install, or
  repository path;
- lock contention versus stale transaction state;
- unsafe reparse-point destination;
- sharing violation during bounded replacement retry;
- failed source, V8, or staged-binary verification; and
- terminal clipboard failure with `/copy` context.

No diagnostic may include auth tokens, full clipboard contents, full prompt
attachments, or unbounded command output. Platform-specific errors sent to the
model must remain concise and describe what the model can do next; verbose
installer and developer diagnostics belong in the terminal/logging path.

## 22. Footprint and bloat budget

### 22.1 Baseline and methodology

The drafting baseline measured approximately:

- 69,600 lines across tracked Rust, shell, Python, and PowerShell source;
- 67,100 lines of Rust;
- 2,400 lines of build/install scripts;
- a 58 MiB stripped Linux release binary; and
- roughly 6 GiB for a clean native development target according to the current
  development guide, with the warm checkout observed higher.

These are order-of-magnitude baselines, not reproducible Windows artifact
measurements. The implementation phase must record exact commands, source
revision, Rust target, profile, V8 artifact, filesystem allocation, and machine
for before/after comparisons.

### 22.2 Source estimate

The expected net source growth is:

| Workstream | Production/docs/CI | Tests | Notes |
| --- | ---: | ---: | --- |
| ConPTY, pipes, process tree, shell | 1,200–1,800 LOC | 400–800 LOC | Largest runtime addition; existing Unix code remains reusable |
| Console lifecycle, paste, shortcuts, clipboard | 500–800 LOC | 400–700 LOC | Paste-burst tests are intentionally substantial |
| Paths, locks, replacement, auth/state | 250–450 LOC | 300–500 LOC | Shared primitives prevent duplicate fixes |
| PowerShell V8/install/update | 700–1,100 LOC | 300–600 LOC | Exact-main and running-executable handling dominate |
| Git/tool portability | 150–350 LOC | 150–350 LOC | Depends on reuse of process/filesystem primitives |
| CI and user documentation | 200–400 LOC | Included above | Avoid duplicated platform manuals |

After replacing obsolete Unix-only branches and deduplicating common behavior,
the total expected repository growth is **5,000–8,000 lines**, or roughly
**7–12%** of the measured Rust/script baseline. This is an estimate, not a
requirement to consume the full range.

A pull request exceeding 8,000 net lines should explain which retained
capability invalidated the estimate and show why current upstream code could not
be reduced further. A smaller implementation is desirable only if it preserves
the behavioral and test contract; deleting tests to hit the estimate is not an
improvement.

### 22.3 Binary size

V8 dominates the current release artifact. Win32 API wrappers mostly call
system DLLs, and target-only dependencies do not enter Unix binaries. The
Windows compatibility code itself is expected to add approximately **1–4 MiB**
to a comparable optimized artifact. If V8 linking and PE overhead are similar,
an indicative `bcodex.exe` size is **60–65 MiB**.

That is not a hard absolute promise because PE/COFF and the upstream Windows V8
static library may differ materially from ELF. The actual gate is:

- record an unmodified or minimal Windows-link baseline on the same toolchain;
- attribute V8/platform-format delta separately from bettercodex code delta;
- keep compatibility-code delta below 5 MiB unless measurements justify it;
- keep Linux and macOS release growth below 1 MiB or 2%, whichever is smaller,
  for target-gated Windows work; and
- investigate duplicate Win32 binding/framework linkage before accepting a
  regression.

Do not quote debug binary size as installed footprint. Report the stripped
release executable and, if compressed for transport during installation, both
compressed and installed sizes.

### 22.4 Persistent disk use

The installed command is small relative to a local source-build environment.
Expected persistent Windows disk use is:

| Component | Approximate cost | When incurred |
| --- | ---: | --- |
| Installed `bcodex.exe` | About 60–65 MiB, to be measured | Always |
| Bettercodex Cargo/V8 build cache | About 6–10 GiB | Retained for fast exact-main updates |
| Rust MSVC toolchain and registry cache | About 1–3 GiB | If not already available/shared |
| Visual Studio C++ Build Tools and Windows SDK | Often 5–10+ GiB | If not already installed |
| Disposable source/staging/compiler scratch | Several GiB peak | Removed after success/failure |

Therefore a new machine with no Rust or C++ toolchain may temporarily or
persistently consume roughly **12–20+ GiB** to honor bettercodex's local-build
update policy. A developer machine that already has MSVC and Rust should see
mainly the 6–10 GiB target/cache cost plus the executable.

The installer must show a preflight free-space estimate before beginning a
first build and distinguish installed application size from reusable developer
tool/cache size. Documentation must not market the 60 MiB executable while
hiding the source-build prerequisites.

### 22.5 Runtime resources

Target-gated compatibility code should not materially increase steady-state
application memory. Each active command adds a pseudoconsole, pipes, a Job
Object, process/thread handles, output buffers, and the normal Windows console-
host resources. Those resources are workload costs, not permanent idle
allocation.

Acceptance measurements must compare:

- idle TUI private working set after startup settles;
- startup wall time to first frame;
- handle count before and after 100 short command sessions;
- memory and handle count after repeated PTY interrupt/cleanup cycles; and
- Linux/macOS startup and idle metrics before and after target-gated changes.

No fixed memory number is specified before a Windows baseline exists. Any
monotonic handle growth or retained per-command process resource is a release
blocker.

### 22.6 Dependency and maintenance cost

The expected direct manifest increase is four to six focused dependencies,
mostly Windows-only, plus approximately ten to twenty transitive packages.
Measure the actual target-specific `cargo tree` and duplicate versions in CI.

The largest footprint is maintenance rather than bytes. Every future change to
terminal lifecycle, composer input, command execution, file replacement, or
installation gains a native Windows branch and test
lane. Windows support must therefore land with ownership in tests and
documentation, not as a compile-only community tier mislabeled as supported.

### 22.7 Avoided keymap/configuration bloat

Importing upstream's configurable keymap would pull in several thousand lines
of key-chord, command, configuration, migration, UI, and test surface. It would
also violate bettercodex's current no-configuration-framework direction and
would not override terminal-owned copy/paste bindings.

The initial fixed alias matrix is the deliberate smaller design. A future
operator with a concrete bettercodex use may reopen key remapping as a separate
product decision with its own footprint and migration analysis.

## 23. Delivery plan

Each phase must leave Linux and macOS green. A phase may use a draft pull
request, but the public documentation must continue saying Windows is
unsupported until the preview or supported gate explicitly changes.

### Phase 0: approve scope and refresh evidence

Deliverables:

- update product direction to authorize the support contract;
- fetch current upstream Codex `main` and record the exact inspected revision;
- verify the current Windows V8 artifact and checksum publication path;
- inventory every unconditional Unix import and platform-specific test in the
  then-current tree;
- choose the narrow current upstream crossterm/PTY dependency revisions; and
- create measured Linux/macOS source, binary, startup, and test baselines.

Gate: approved target/non-goal list and no unresolved upstream source question
that would change architecture materially.

### Phase 1: make platform primitives compile

Deliverables:

- target-gated manifest dependencies;
- Windows home/path discovery;
- shared file-lock and replace interfaces with native unit tests;
- Windows shell enum/detection/argument construction;
- compile-time managed-session stub and hidden `/tmux`; and
- PowerShell V8/Cargo wrapper.

Gate: `cargo check --tests` and Clippy through the V8 wrapper on
`x86_64-pc-windows-msvc`, with Linux/macOS behavior unchanged. This is not yet a
usable preview.

### Phase 2: port command execution

Deliverables:

- upstream-based pipe, ConPTY, Windows input, and Job Object backend;
- process-session integration and native environment/path preservation;
- resize, stdin, interrupt, timeout, and descendant cleanup; and
- native process failure-injection tests.

Gate: the complete process regression suite passes on a real Windows runner
with stable process/handle counts.

### Phase 3: port the TUI

Deliverables:

- Windows console-mode guard and probe path;
- optional capability degradation and reliable restoration;
- paste-burst integration;
- Windows shortcut handlers and rendered hints;
- native clipboard, title, notifications, Unicode, resize, and scrollback; and
- login/resume screens under the same lifecycle.

Gate: automated TUI tests plus manual Windows Terminal and VS Code scenarios
pass without accidental paste submission or damaged terminal modes.

### Phase 4: port persistence and repository tools

Deliverables:

- auth, state, history, rollout, and embedded-skill locking/replacement;
- Windows path and reparse-point safety;
- Git null device/path handling;
- patch and papercut portability; and
- crash/failure recovery tests.

Gate: concurrent-process and failure-atomicity suites pass, including private
state and reparse-point cases.

### Phase 5: install and update

Deliverables:

- strict PowerShell installer;
- prerequisite, disk, cache, PATH, and exact-main handling;
- source/V8/build/stamp/smoke transaction;
- running-executable update finalizer;
- stale transaction recovery; and
- Windows installer tests mirroring every retained `spec-install.md` invariant.

Gate: install, same-revision no-op, changed-revision incremental update, forced
failure rollback, and uninstall-by-documented-manual-removal scenarios pass on
fresh Windows test machines.

### Phase 6: supported release

Deliverables:

- complete CI and interactive matrix;
- measured footprint report against this budget;
- updated README, install, development, slash-command, security, and support
  documentation;
- fresh-machine installation instructions; and
- a final current-upstream diff review for every retained Windows behavior.

Gate: all definition-of-done criteria below pass. Only then may the project say
that native Windows is supported.

## 24. Validation strategy

### 24.1 Continuous integration

Add a native `windows-latest` lane that uses the pinned Rust toolchain and
PowerShell V8 wrapper. At minimum it must run:

```text
format verification
cargo check --locked --tests
tests through the repository's retained Cargo/nextest workflow
clippy with warnings denied
PowerShell installer unit/integration tests
release build and internal install smoke test
```

Do not invent a parallel test framework when the current Rust tests, upstream
tests, and existing installer-test style can express the requirement. In
particular, do not add Pester solely because the installer is PowerShell if the
existing Python or Rust harness can test the script more simply. The public
installer itself must not require Python.

CI caches must be target-specific. A job may clean only its task-owned
temporary/artifact root and must never delete a shared checkout target or a
target used by a live Cargo/rustc process.

Cross-compilation from Linux may supplement dependency checks but cannot
replace the native Windows lane. Wine cannot validate ConPTY, Windows Terminal,
clipboard, console modes, Job Objects, or installer replacement semantics.

### 24.2 Automated native integration tests

Native tests should create isolated temporary profile, Codex, bettercodex,
cache, repository, and install roots. They must clean those task-owned roots on
success and failure and preserve diagnostics when cleanup itself fails.

Required suites include:

- shell detection and direct argument construction;
- PTY/pipe lifecycle and handle leak detection;
- console mode transform and restoration;
- explicit and burst paste state machines with deterministic time;
- Windows path, CRLF, clipboard, and image-path parsing;
- file locks across processes;
- file and directory replacement under injected failure;
- junction/reparse-point refusal;
- Git diff and hooks suppression with Git for Windows;
- session/auth/skill crash recovery;
- installer source pinning, caches, PATH, update finalization, and rollback.

Tests that mutate real console modes or the user PATH must use an isolated
process/registry environment where possible and restore exact prior state in a
finally guard.

### 24.3 Interactive terminal matrix

CI cannot prove the complete interactive contract. Before supported release,
run and record this matrix:

| OS | Terminal | Shell | Priority |
| --- | --- | --- | --- |
| Windows 11 x64 | Windows Terminal current | PowerShell 7 | Required every release candidate |
| Windows 11 x64 | Windows Terminal current | Windows PowerShell 5.1 | Required every release candidate |
| Windows 11 x64 | VS Code integrated terminal current | PowerShell 7 | Required every release candidate |
| Windows 11 x64 | WezTerm current | PowerShell 7 | Required compatibility smoke |
| Windows 10 1809+ x64 | Windows Terminal/ConPTY host available | Windows PowerShell | Periodic best-effort check |

Record OS build, terminal version, shell version, keyboard layout, bcodex source
revision, Rust target, and result. "Works on my Windows machine" without those
details is not a support result.

### 24.4 Interactive scenarios

Each primary terminal/shell pair must cover:

1. first launch, login, logout, and resume;
2. narrow, ordinary, and very large terminal sizes;
3. repeated resize while output streams and while idle;
4. transcript history larger than one screen and one large cell;
5. light/dark/default colors, title, hyperlinks, and notification fallback;
6. ordinary typing versus very fast typing;
7. Ctrl+V, Ctrl+Shift+V, Shift+Insert, right-click, and multiline paste;
8. paste containing Enter, CRLF, tabs, Unicode, and more than 1,000 characters;
9. Shift+Enter and Ctrl+J newline behavior;
10. Ctrl+Left/Right, Ctrl+Backspace, Ctrl+W, history search, and queued editing;
11. terminal selection followed by Ctrl+C, then Ctrl+C with no selection;
12. Ctrl+O and `/copy`, including clipboard failure fallback;
13. IME composition, combining/wide characters, and a non-US keyboard layout;
14. pasted native image path and ordinary path-like prose;
15. piped and PTY commands, resize, input, interrupt, timeout, and descendant
    cleanup;
16. agent patches to a CRLF repository under a path with spaces;
17. update availability, exact-main update, and restart; and
18. terminal usability immediately after normal exit, crash-path cleanup, and
    updater exit.

### 24.5 Installer failure matrix

Installer tests must inject failure before and after every commit boundary:

- malformed main-ref response;
- archive timeout, truncation, and checksum mismatch;
- missing lockfile/toolchain/V8 manifest;
- insufficient disk space;
- missing compiler/linker/SDK;
- Cargo or V8 build failure;
- source-content hash mismatch;
- revision-stamp mismatch;
- smoke-test failure;
- active install lock and stale transaction;
- destination reparse point;
- PATH persistence denial;
- existing command running;
- sharing violation during replacement;
- parent/finalizer crash;
- final visible-command verification failure; and
- cleanup failure.

Every case before commit must preserve the installed command byte-for-byte.
Every case during finalization must leave or restore one verified command and a
bounded recoverable transaction record.

### 24.6 Performance and footprint measurements

For the final candidate, record:

- release executable bytes and allocated disk size;
- compressed V8/source downloads and persistent cache size;
- clean and warm build wall time;
- exact-main no-op update time;
- one-file incremental update time;
- startup to first frame over at least 20 runs;
- idle private working set and handle count;
- command spawn latency for pipe and PTY modes; and
- handle/memory state after 100 short sessions and 25 interrupted PTY sessions.

Compare Linux/macOS release size and startup against their Phase 0 baselines to
prove target gating did not impose material regressions.

## 25. Definition of done

Native Windows support is complete only when all of the following are true.

### 25.1 Policy and build

- Product direction explicitly authorizes the support matrix.
- `x86_64-pc-windows-msvc` builds, tests, and lints through the verified V8
  wrapper on a native Windows runner.
- Windows-only dependencies are target-gated and Unix-only dependencies no
  longer prevent compilation.
- The release remains one `bcodex.exe` with no required installed sidecar.

### 25.2 Installation and update

- A clean supported machine can install from an immutable public-`main`
  selection using the documented PowerShell command.
- Preflight accurately reports architecture, prerequisites, disk use, and
  destination.
- Same-revision update is a no-op; changed-revision update reuses valid caches.
- A running executable is replaced only through the verified Windows
  finalization protocol.
- Every injected failure preserves or restores the previous verified command.
- PATH and stale-transaction behavior are deterministic and recoverable.

### 25.3 Terminal and shortcuts

- Windows Terminal and VS Code pass the complete interactive scenarios.
- Multiline paste never submits partially, whether delivered as `Event::Paste`
  or rapid key events.
- The Windows shortcut overlay matches actual handlers and uses Windows terms.
- Terminal-owned copy/paste works without disabling standard terminal actions.
- Ctrl+C, Ctrl+O, `/copy`, Ctrl+J, word movement/deletion, queue editing, and
  history search satisfy the matrix.
- Console modes, cursor, paste mode, and attributes are restored after every
  tested exit path.
- Transcript rows remain in host scrollback after overflow and resize.

### 25.4 Commands and tools

- Piped and ConPTY commands preserve paths, environment, output, resize, input,
  exit, interrupt, and process-tree semantics.
- Repeated commands leak no process, pseudoconsole, pipe, thread, or job handles.
- PowerShell 7, Windows PowerShell, and cmd fallback behavior is tested.
- Git diff, patch application, clipboard image paths, and papercut logging work
  in native repositories with spaces, Unicode, and CRLF.

### 25.5 Persistence and quality

- Auth, state, history, rollouts, and embedded skills are private enough for the
  documented Windows threat model and failure-atomic.
- Locks exclude real competing processes and stale state is handled safely.
- Reparse points cannot redirect protected install, cleanup, or integrity
  operations in the tested cases.
- `/tmux` is absent on native Windows and unchanged on Unix.

### 25.6 Regression and footprint

- Linux and macOS retained tests remain green.
- Current upstream Codex was re-inspected at implementation completion and
  every retained divergence is explained.
- Binary, source, dependency, disk, startup, memory, and handle measurements are
  published against the budget above.
- No unexplained source growth beyond the estimated range or Windows machinery
  in Unix artifacts remains.
- User and developer documentation matches the actual support status.

## 26. Risks and mitigations

### 26.1 Upstream drift

**Risk:** Codex's active Windows implementation changes while this port is in
progress, especially crossterm input, ConPTY teardown, paste timing, or
installer behavior.

**Mitigation:** record an upstream revision per phase, inspect current `main`
again before each retained subsystem lands, and port regression tests with the
behavior. Do not freeze this drafting revision as design authority.

### 26.2 Terminal protocol differences

**Risk:** Windows Terminal, xterm.js, and other ConPTY hosts deliver modified
keys, paste, focus, scrollback, and query responses differently.

**Mitigation:** keep optional protocols optional, provide shortcut fallbacks,
use paste-burst detection, retain byte-level tests, and run the explicit
interactive matrix.

### 26.3 Process-tree leaks or overbroad termination

**Risk:** a naive `Child::kill` leaks descendants, while an incorrectly managed
Job Object kills a process not owned by the command session or drops output
prematurely.

**Mitigation:** port current upstream Job Object semantics, separate soft and
hard interruption, test root/descendant races, and measure handle/process state
after repeated cycles.

### 26.4 Running executable and antivirus contention

**Risk:** Windows file sharing and security software can prevent or delay
replacement of `bcodex.exe`, producing a broken updater.

**Mitigation:** fully verify before finalization, exit the mapped old process,
use same-volume stage/backup replacement, retry only transient sharing errors,
and failure-inject every transition.

### 26.5 Reparse-point and ACL mistakes

**Risk:** mechanically deleting Unix `O_NOFOLLOW` and mode code weakens
installer, auth, or skill integrity.

**Mitigation:** define the actual security promise, use narrow Windows
handle/reparse checks, rely on user-profile ACLs only after testing them, and
attack the implementation with junction/reparse fixtures.

### 26.6 Local-build installation burden

**Risk:** MSVC, Windows SDK, Rust, V8, and Cargo caches consume far more disk and
setup time than the installed executable suggests.

**Mitigation:** preflight before download/build, report the full 12–20+ GiB
first-machine possibility, reuse exact caches, require consent before installing
large prerequisites, and document safe cache removal.

### 26.7 Scope creep

**Risk:** Windows work becomes a vehicle for sandbox, keymap, configuration,
installer-package, ARM64, or terminal-tab features that bettercodex did not ask
for.

**Mitigation:** enforce the non-goals, use current upstream only for retained
behavior, and require a separate concrete product decision and footprint review
for each expansion.

### 26.8 False support from compile-only validation

**Risk:** CI goes green while multiline paste submits early, Ctrl+C is trapped,
terminal modes remain damaged, or descendants leak.

**Mitigation:** do not label Windows supported until the native process,
failure, and interactive terminal matrices are recorded.

## 27. Documentation and rollout

### 27.1 Status stages

Use these public labels precisely:

- **Unsupported:** current state; no native support claim.
- **Developer preview:** native builds are available for contributors, known
  missing capabilities are listed, and no compatibility promise is made.
- **Preview:** install/TUI/process/state paths work, but clearly documented GA
  acceptance criteria remain.
- **Supported:** every definition-of-done item passes for the primary matrix.

Do not call compile success or one maintainer's terminal test a preview by
itself.

### 27.2 Documentation updates required at support time

Update at least:

- `progressive_disclosure/product-direction.md` with the approved target;
- `README.md` with native Windows status and install command;
- `docs/install.md` with prerequisites, first-build disk cost, PowerShell
  install/update, cache, PATH, and troubleshooting;
- `spec-install.md` with the Windows exact-main transaction and finalizer;
- `progressive_disclosure/development.md` with the PowerShell V8 wrapper and
  native validation workflow;
- `docs/slash_commands.md` to state that `/tmux` is Unix-only;
- `SECURITY.md` if Windows path/reparse or unsandboxed-execution wording needs a
  platform clarification; and
- terminal shortcut/help text and any screenshot/snapshot documentation.

The install guide must distinguish native Windows from WSL and must state that
commands run unsandboxed with the Windows user's permissions.

### 27.3 Rollback

Because public installs track exact `main`, a broken Windows commit cannot be
hidden behind an unchanged package version. Windows support should therefore
land behind passing native CI before product documentation advertises it.

If a post-support regression lands, fix or revert the offending source commit
on public `main`; do not manipulate tags or Releases. The installer/updater must
continue preserving the previous local binary when the new `main` candidate
cannot build or verify.

## 28. Implementation map

This map identifies expected touch points, not a mandate to edit every file.
Re-run the platform search before implementation because the tree evolves.

| File or area | Expected work |
| --- | --- |
| `progressive_disclosure/product-direction.md` | Approve Windows target and reference this specification |
| `Cargo.toml`, `Cargo.lock` | Target-gate Unix dependencies; add current upstream Windows PTY/Win32 dependencies |
| `scripts/cargo-with-v8.ps1` | Verified Windows V8 artifact and Cargo entry point |
| `scripts/install.ps1` | Exact-main local-build installer and transaction recovery |
| `scripts/install_tests.py` or retained installer harness | Windows transaction/failure tests without adding an unnecessary framework |
| `src/process_runtime.rs` or private platform children | ConPTY, pipes, jobs, handles, normalization, resize, cleanup |
| `src/tools/process_session.rs` | Native argv/environment, Windows shell args, process integration |
| `src/shell_command/shell_detect.rs` | cfg split, PowerShell paths, `.exe`, Cmd fallback |
| `src/tui/terminal.rs` | console guard, probes, restoration, scrollback regression preservation |
| `src/tui/view.rs`, `src/tui/editor.rs` | paste burst, Ctrl+Backspace, platform hints, event flushing |
| new focused `paste_burst` module | Current upstream state machine and tests |
| `src/tui/clipboard.rs`, `clipboard_paste.rs` | Native Windows copy and Windows image/path cases |
| `src/tui/notifications.rs`, `terminal_title.rs` | Windows Terminal capability detection/fallback |
| `src/paths.rs` | Native home discovery and environment precedence |
| focused private filesystem helper if justified | File locks, replace-existing, reparse checks, durable transaction primitives |
| `src/auth.rs`, `state_file.rs`, `prompt_history.rs`, `rollout.rs` | Use portable primitives and target-gate Unix modes/fsync |
| `src/system_skills.rs` | Windows-safe lock, stage, replace, recovery, reparse defense |
| `src/tui/git_diff.rs` | Native path arguments and Windows null device |
| `src/tools/patch.rs`, `papercuts.rs` | Portable errors and Windows-safe file opening |
| `src/managed_session.rs` and callers | Unix cfg plus Windows no-op/hidden command path |
| `src/update.rs` and tests | Select immutable PowerShell installer and coordinate finalization |
| CI workflows | Native Windows check/test/clippy/release/installer lanes |
| user/developer docs | Support matrix, shortcuts, install burden, diagnostics, non-goals |

## 29. Pre-implementation decisions and spikes

The product decisions are made above: x64 MSVC first, Windows 11 primary,
Windows 10 best effort, no sandbox, no keymap framework, no native tmux
replacement, and exact-main local builds. The following narrow engineering
questions must be answered with current source and executable probes during
Phase 0:

1. Which exact current upstream crossterm patch, if any, is required for
   Windows input-record and keyboard behavior?
2. Which current upstream V8 URLs/checksum manifest entries publish the Windows
   `.lib.gz` and generated binding for the pinned crate?
3. Which supported Windows replace API and finalizer shape gives the simplest
   proven old-or-new guarantee for a running `bcodex.exe`?
4. Can all state/history/rollout locks use the pinned standard library API, or
   does any shared-lock behavior require a focused dependency?
5. Which handle/file-ID APIs give the smallest reliable reparse-point defense
   on the supported OS floor?
6. Does current upstream require its patched crossterm to clear VT input mode,
   or has released crossterm incorporated that behavior by implementation time?
7. What is the measured absolute Windows release size and clean/warm cache cost
   with the actual V8 artifact?

Each spike must end in a source-backed decision and a focused test or benchmark,
not an enduring alternate implementation. If a result materially changes the
scope or footprint, update this specification before proceeding.

## 30. Final recommendation

Proceed only as a deliberate platform project. The leanest credible design is
to port current upstream's ConPTY/Job Object, console mode, shell, clipboard,
and paste behavior; add Windows-safe persistence and exact-main PowerShell
installation; and hide tmux on native Windows.

The likely installed binary increase is modest because V8 already dominates.
The meaningful costs are 5,000–8,000 repository lines, a new native CI and
interactive terminal matrix, and a 12–20+ GiB first-machine source-build
environment when Rust/MSVC caches are absent. Avoiding a configurable keymap,
sandbox, release package system, ARM64 target, and fabricated tmux replacement
keeps that cost bounded while delivering the Windows behavior operators
actually need.
