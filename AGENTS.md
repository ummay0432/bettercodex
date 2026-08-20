# bettercodex

bettercodex is a focused port of [OpenAI Codex](https://github.com/openai/codex),
not an independent implementation.

Published builds are immutable snapshots of a complete public `main` revision.
The release tag pins the exact source and installer; the latest published full
release's semantic version determines update freshness.

## Repository constraints

- The startup art is off limits: do not modify or delete it.
- Do not create, edit, regenerate, or otherwise tweak model-facing context
  without the user's explicit approval, including `AGENTS.md`, `prompts/*.md`,
  tool descriptions, and model-visible errors.
- Do not add audio or video support, dependencies, protocol items, runtime
  helpers, tool descriptions, fixtures, or tests; bettercodex does not use
  either modality.
- Keep Responses API reasoning summaries disabled; they add latency and
  output-token cost.
- For retained Codex behavior, inspect and port the current upstream source
  rather than reimplementing it. Any intentional departure requires explicit
  user direction.
- When comparing bettercodex with upstream Codex, clone the upstream repository
  into the workspace, work from that checkout instead of making repeated remote
  Git calls, and remove it afterward.
- Mirror current upstream Cargo, build, packaging, release, and test
  infrastructure unless an explicit bettercodex requirement demands otherwise.
- After Rust changes, run `cargo lint` as a validation step.
- Rust builds and tests must clean up task-owned temporary files, fixtures,
  caches, and isolated compiled artifacts, including on failure. Never remove
  the checkout's shared `target/`, another session's artifact root, or a target
  still referenced by a live Cargo or rustc process.
- Only the user decides when a release is ready or may be published.

## Repository tree

`docs/` is excluded from ripgrep by `.rgignore` for context hygiene.

```text
.
├── assets/ — Release assets
├── bundled-skills/ — Embedded skill packages
├── docs/ — Project documentation
│   └── case-studies/ — Harness research
├── prompts/ — Model prompts
├── scripts/ — Installer scripts
├── src/ — Rust source
│   ├── http_client/ — HTTP support
│   ├── login_assets/ — Login pages
│   ├── shell_command/ — Shell parsing
│   ├── truncation/ — Output truncation
│   └── tui/ — Terminal interface
│       ├── bottom_pane/ — Composer UI
│       ├── markdown_render/ — Markdown rendering
│       ├── render/ — Rendering utilities
│       └── terminal/ — Terminal lifecycle
└── subagents/ — Specialist prompt assets
```
