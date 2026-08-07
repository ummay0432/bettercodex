# Slash commands

For an overview of Codex CLI slash commands, see [this documentation](https://developers.openai.com/codex/cli/slash-commands).

bettercodex adds `/tmux`, which immediately moves the current live TUI into a
new detachable `c1`, `c2`, … tmux session. It remains available while an agent
turn is running and does not restart or interrupt that turn.

As in upstream Codex, `/logout` removes stored authentication credentials and
exits the TUI. Sign in before starting a session with `bcodex login`.
