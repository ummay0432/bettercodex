# Preserved BetterCodex material

This directory is the deliberately small record retained when the former
Pi-based BetterCodex harness was removed on 2026-08-04 and the project restarted
from OpenAI Codex.

These files are reference material, not an instruction to restore the old
implementation wholesale:

- [Chat composer and status line](chatbox-and-statusline.md) records the accepted
  terminal design and its behavior.
- [Legacy Codex request identity](request-identity.md) records the Pi-only
  ChatGPT subscription compatibility layer. Native Codex already owns this
  behavior, so the workaround must not be ported into the Codex fork.
- [System prompt](system-prompt.md) preserves the accepted prompt and its former
  assembly code verbatim.
- [AGENTS.md](AGENTS.md) is the former repository onboarding file.
- [Lessons learned](lessons-learned.md) records the expensive findings worth
  carrying into the new codebase.

The screenshots under [`assets/`](assets/) are canonical visual references.
No access token, account identifier, session, credential file, or other secret
is preserved here.
