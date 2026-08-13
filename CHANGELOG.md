# Changelog

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
