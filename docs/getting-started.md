# Getting started with bettercodex

Install BetterCodex with the copyable command in the
[README](../README.md#install), then open a new terminal. Sign in with a ChatGPT
account that has Codex access:

```sh
bcodex login
```

From a project directory, launch the interactive terminal UI:

```sh
bcodex
```

Remote and headless machines can use `bcodex login --device-auth`; see
[Authentication](authentication.md) for the complete flow. BetterCodex runs
commands with your user account's permissions and does not sandbox them; read
[Sandbox and permissions](sandbox.md) before using it on sensitive systems.

For the retained upstream interaction model, see the official
[Codex CLI overview](https://developers.openai.com/codex/cli).
