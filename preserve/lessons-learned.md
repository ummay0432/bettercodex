# Lessons from the Pi-based BetterCodex harness

## Why the foundation changed

The former project started as a lean Pi customization but increasingly rebuilt
Codex inside Pi. At the reset boundary it contained:

- 230 project commits in nine days;
- 131 tracked files under `.pi/`;
- custom ports for Codex request identity, model metadata, compaction,
  transcript rendering, composer behavior, status UI, shell sessions,
  `apply_patch`, planning, web search, and programmatic tool composition; and
- lifecycle and installation code needed only to make those ports coexist with
  a separately installed Pi host.

That was evidence of a foundation mismatch, not merely unfinished cleanup. If
the desired product is “Codex made personal,” starting from Codex removes more
complexity than maintaining an ever-wider compatibility harness.

## Differential findings worth retaining

The initial comparison used 46 substantive Pi sessions and, after correcting
inherited rollout metadata, 14 primary Codex CLI sessions plus eight spawned
rollouts. It identified three concrete mechanism-level gaps:

1. Codex served programmatic tool composition that could parallelize, filter,
   aggregate, and selectively project intermediate results before they entered
   model history.
2. Pi exposed competing shell contracts. Real sessions repeatedly switched
   between them, while Codex presented one bounded execution/session model.
3. Codex's structured patch workflow supported cohesive multi-file patches;
   Pi's exact-string editing produced recurring match and uniqueness failures.

Subsequent source audits found additional native Codex behavior that was costly
to reproduce around Pi: queue-aware compaction boundaries, request-scoped
ChatGPT transport identity, first-party web transport, plan retention,
selective image detail, persistent composer history, and TUI transcript
lifecycle.

Counts and one favorable transcript were never treated as proof by themselves.
The useful method was:

1. pin both upstream revisions;
2. define a concrete user-visible case and expected trace;
3. inspect the maintained implementation;
4. compare matched behavior;
5. name counterevidence and limits; and
6. port only a demonstrated advantage.

Keep that evidence standard even though Codex is now the base.

## Rules for the Codex downstream

### Prefer deletion over parallel machinery

Use Codex's native path first. Do not keep a second implementation for tools,
auth, compaction, sessions, transport, transcript state, or TUI lifecycle unless
a current, demonstrated product requirement cannot be met in core.

### Keep the downstream difference legible

- Keep `origin` pointed at the private personal repository.
- Keep `upstream` pointed at `https://github.com/openai/codex.git`.
- Organize custom work as focused commits that can be reviewed and replayed
  during upstream updates.
- Record the upstream commit used for every behavior-sensitive adaptation.
- Prefer direct core changes over monkeypatches, global proxies, generated
  overlays, duplicate package identities, or installation-time rewriting.

### Preserve native ChatGPT authentication

Use Codex's own login, account selection, request identity, catalog, transport,
and refresh behavior. The old Pi “cloaking” layer is historical documentation,
not a component to rebuild.

Never commit OAuth credentials, account identifiers, sessions, or captured
request payloads. Tests should use synthetic claims and local transports.

### Preserve product identity deliberately

The accepted composer/status-line design and system prompt are worth retaining
because they express the product rather than compensate for Pi. They should be
reintroduced as explicit, tested Codex changes—not copied blindly before the
corresponding upstream ownership paths are understood.

The former defaults were `gpt-5.6-sol`, maximum reasoning, compact terminal
chrome, and concise completion reports. Reconfirm model/version assumptions
against the current upstream catalog rather than freezing old metadata.

### Reconsider security as a first-class Codex decision

The Pi harness previously chose full ambient user permissions and deliberately
omitted a sandbox/approval subsystem. Codex owns a real sandbox and approval
product. Do not silently delete or bypass it merely to reproduce the former
runtime. Any different default should be an explicit product decision with
concrete validation.

### Validate the system, not just the patch

For every downstream change, validate:

- the requested user-visible behavior;
- relevant native behavior that the change touches;
- startup and long-session lifecycle;
- upstream-update/rebase behavior;
- failure, cancellation, and retry paths; and
- the final rendered output, not only internal tests.

Avoid prompt text or compatibility code when a deterministic runtime contract
can solve the issue more directly.

## What was intentionally discarded

- the complete former Git history and all rollback bundles;
- Pi extensions, install/sync scripts, host bridges, package overlays, tests,
  differential reports, and runtime documentation;
- Pi-specific request-identity and WebSocket interception code;
- duplicated implementations already owned by Codex; and
- GitHub repository metadata, of which there were no issues, pull requests,
  releases, hooks, deploy keys, environments, Actions secrets, or variables.

The preserved files in this directory are the complete intentional carry-over.
Anything else must earn its way back from the Codex foundation.
