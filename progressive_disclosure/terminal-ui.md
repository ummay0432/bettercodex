# Terminal UI

Read this before changing `src/tui/`, terminal lifecycle, transcript rendering,
composer behavior, completion, popups, or shortcuts.

OpenAI Codex's current `codex-rs/tui` source and snapshots are authoritative for
inherited operator-visible behavior. Before changing a matching surface, inspect
and port its view and shared components; command menus come from
`codex-rs/tui/src/bottom_pane/`. Local source and rendered tests are authoritative
only for deliberate bettercodex-specific behavior.

Test terminal changes through rendered output or terminal behavior, not only
the data used to produce it.
