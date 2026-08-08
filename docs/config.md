# Configuration

bettercodex deliberately has no general configuration framework. Its model,
reasoning effort, context limits, provider, and runtime behavior are fixed by
the project. Upstream Codex `config.toml`, profiles, managed requirements, MCP,
provider, and lifecycle-hook settings do not apply.

The supported path overrides are:

- `CODEX_HOME` for the shared Codex credential and prompt-history directory;
- `BCODEX_HOME` for bettercodex sessions, skills, and state; and
- `BCODEX_INSTALL_DIR` for the installer-managed binary directory.

Set `BCODEX_SKIP_UPDATE_CHECK=1` to disable the failure-silent background
release check. These environment variables are narrow operational overrides,
not a general settings system.

The official [Codex configuration documentation](https://developers.openai.com/codex/config-basic)
describes upstream Codex, not bettercodex.
