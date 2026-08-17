# Authentication

bettercodex uses ChatGPT authentication and shares Codex's file-backed credential
cache. On the first interactive launch, `bcodex` presents browser and device-code
sign-in choices before starting the session. You can also start the browser flow
directly:

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

A nonempty `CODEX_ACCESS_TOKEN` overrides the file-backed credentials. Set
`CHATGPT_ACCOUNT_ID` as well when the token does not carry its ChatGPT account
ID. This is externally managed authentication: bettercodex uses the access
token as supplied and cannot refresh or unset it. `bcodex logout` still removes
`auth.json`, so rotate or unset the environment token separately. Treat the
token as a password.

For HTTPS inspection through an enterprise proxy, set `CODEX_CA_CERTIFICATE` to
a PEM file containing the required root certificates. When that variable is
unset, bettercodex also honors `SSL_CERT_FILE`. The selected bundle applies to
browser and device-code login, API requests (including secure Responses
WebSockets), and update checks.

bettercodex's fixed runtime uses the ChatGPT Codex backend, so it does not expose
Codex's Platform API-key login mode. See the [official Codex authentication
documentation](https://developers.openai.com/codex/auth) for account, workspace,
device-auth, and credential-security details.
