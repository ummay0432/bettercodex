# Changelog

## [0.2.4] - 2026-08-19

- compaction aligned with upstream codex
- $manifest brought back

## [0.2.3] - 2026-08-18

- update notice says `Run bcodex update` again <- fixed

## [0.2.2] - 2026-08-18

- response.completed did not match completed output items <- fixed
- sometimes diff on edits wouldnt show on files above 2k lines <- fixed

## [0.2.1] - 2026-08-17

- im tired bruh

- harness improvements (session recovery), (network resilience), (terminal rendering), (skill loading)

- context compaction is now identical to upstream Codex, preserving active context, tool state, and conversation continuity across long sessions, interruptions, and resume

## [0.2.0] - 2026-08-14

- upstream Codex's Code Mode/V8 tool stack was refactored, cutting roughly 12,500 lines of Rust
- parallel tool calling improved
- added `/tools`
- Windows support removed (debloat)
- tools refactored into a hybrid of pi's and Codex's approaches, combining the best of both worlds

## [0.1.9] - 2026-08-13

- agents given more freedom with tool use

- terminal performance improved

- message streaming made smooth

## [0.1.8] - 2026-08-13

- openai had dedicated tools being injected into agent context just for fetching docs from their website. this shit is gone now; the agent can websearch it if needed

- empty chats no longer take up rows in `/resume`

- papercut logging is now opt-in instead of enabled by default, improving long-horizon task adherence

- startup now prewarms the first Responses connection

- pressing Enter on a bare `/` no longer accidentally starts `/review`

## [0.1.7] - 2026-08-11

- terminal resizing and UI repainting are fixed
- added `/status` to check usage and shit

## [0.1.6] - 2026-08-11

- sysprompt and tool catalogue are good now (trust me)
- xhigh is now default, max is only 2% better for 200% the price
- message streaming in terminal now supports 120fps (for whatever reason)

## [0.1.5] - 2026-08-10

- Fixed programmatic tool calling drift (dw about it)
- Improved harness performance
- Fixed `/resume` (chats show the full transcript)
- `/fast` stays enabled after quitting

## [0.1.4] - 2026-08-10

- Added `/model` selector.
- Added `/fast` mode (consumes 1.5x usage).
- Added patch notes at startup (hello).
- Improved model switching and session continuity.
- Improved long-session performance and reduced binary size.
