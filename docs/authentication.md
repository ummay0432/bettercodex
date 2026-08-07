# Authentication

bettercodex uses ChatGPT authentication and shares Codex's file-backed credential
cache. Sign in with the browser flow:

```sh
bcodex login
```

For a remote or headless machine, use Codex's device-code flow:

```sh
bcodex login --device-auth
```

Check or remove the stored sign-in with:

```sh
bcodex login status
bcodex logout
```

Inside the terminal UI, `/logout` removes the credentials and exits. There is no
`/login` command in the upstream Codex TUI; login happens before launching a
session.

Credentials are stored in `$CODEX_HOME/auth.json`, or `~/.codex/auth.json` when
`CODEX_HOME` is unset. This deliberately lets Codex and bettercodex reuse the
same ChatGPT sign-in. Treat that file as a password.

bettercodex's fixed runtime uses the ChatGPT Codex backend, so it does not expose
Codex's Platform API-key login mode. See the [official Codex authentication
documentation](https://developers.openai.com/codex/auth) for account, workspace,
device-auth, and credential-security details.
