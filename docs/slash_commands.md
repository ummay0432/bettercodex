# Slash commands

For an overview of Codex CLI slash commands, see [this documentation](https://developers.openai.com/codex/cli/slash-commands).

On macOS and Linux, bettercodex adds `/tmux`, which immediately moves the
current live TUI into a new detachable `c1`, `c2`, … tmux session. It remains
available while an agent turn is running and does not restart or interrupt that
turn. Native Windows omits `/tmux`; it does not inject a substitute command or
Windows Terminal control mechanism.

## Engineering review

Use `/review <target>` in the TUI to run bettercodex's active engineering
review on a specified product or system:

```text
/review the auto-updating logic
/review recovery after an interrupted WebSocket response
```

`$review` explicitly starts the same workflow in interactive and non-interactive
prompts and can target any repository scope:

```text
$review the authentication changes
$review src/tui for avoidable redraw work
```

This is intentionally not a read-only review. It first develops a deep
understanding of the target, then evaluates whether the affected system can be
simpler, faster, more resource-efficient, or easier to maintain. When
repository evidence supports a clear net improvement, the agent refactors as
deeply as needed and removes everything made obsolete. When the target already
meets those standards, it leaves the implementation unchanged and explains the
evidence for that conclusion.

Explicit review requests submitted during another turn queue as the next task
instead of steering the active implementation. The agent may also invoke the
skill proactively during implementation work when its engineering-review
criteria match the task; `$review` or `/review` is not required. A plain
read-only review request does not authorize edits.

As in upstream Codex, `/logout` removes stored authentication credentials and
exits the TUI. Sign in before starting a session with `bcodex login`.
