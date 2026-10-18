# PitlaneMCP navigation skill

Companion agent skill for [PitlaneMCP](https://github.com/eresende/pitlane-mcp), an open-source local MCP server for token-efficient code intelligence.

This package is **instructions plus setup examples**, not an MCP server binary. Install `pitlane-mcp` separately and configure it in your agent; the skill then guides an agent on when and how to use the available tools. The standalone `SKILL.md` works in skill-aware agents; exact skill installation locations vary between products.

## Install the server

Choose one of the methods documented in the upstream [README](https://github.com/eresende/pitlane-mcp#installation): download a [release](https://github.com/eresende/pitlane-mcp/releases/latest), `brew install pitlane-mcp` after tapping the repository, `cargo binstall pitlane-mcp`, or `cargo install pitlane-mcp`.

Verify the `pitlane-mcp` executable is on the PATH seen by your agent (or use its absolute path in MCP configuration). Restart the agent after changing MCP settings.

## Configure MCP

- **Claude Code:** `claude mcp add pitlane-mcp -- pitlane-mcp`
- **OpenCode:** merge [configs/opencode.json](configs/opencode.json) into `opencode.json` or `opencode.jsonc`.
- **Cursor:** merge [configs/cursor-mcp.json](configs/cursor-mcp.json) into `.cursor/mcp.json`.
- **VS Code / Copilot:** merge [configs/vscode-mcp.json](configs/vscode-mcp.json) into `.vscode/mcp.json`.

These JSON examples are fragments of an agent's overall configuration: **merge the appropriate entries; do not overwrite existing settings**. For additional clients and semantic-search options, refer to the [upstream configuration documentation](https://github.com/eresende/pitlane-mcp/blob/main/docs/configuration.md).

## Install the skill

Keep the folder containing `SKILL.md` together as a skill package. Use your agent's documented skill installation procedure to install it at user or project scope. MCP registration and skill installation are separate steps. You may also use the [SKILL.md](SKILL.md) guidance directly in an agent's instructions if it does not support skill packages.

## Verify

1. Confirm your agent exposes `pitlane-mcp` tools.
2. In a code repository, request a symbol lookup or an execution-path explanation.
3. Confirm the agent selects `ensure_project_ready` if needed and retrieves concrete symbol/file evidence.
4. Try a simple literal lookup and verify the agent can use native search where appropriate.

## Packaging for Agensi

Create a ZIP with this **directory's contents at the ZIP root** (`SKILL.md`, `README.md` and `configs/`). Before upload, check the marketplace's current validator; some skill distributors instead require one enclosing directory. Publish it free and link to the canonical upstream repository. No binaries, credentials, model endpoints or private configuration are included.

## Maintenance

The upstream repository is canonical. This skill deliberately excludes project-contributor rules from the repository's `AGENTS.md`. Update guidance when the MCP tool surface changes, and validate claims about efficiency with task-level benchmarks rather than assuming every MCP call is an improvement.

License: same dual MIT OR Apache-2.0 terms as the upstream project.
