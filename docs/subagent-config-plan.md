# Unified config and persistent subagents

Approved scope: ~/.appsdk/config.toml is the only policy source; project
overrides live there. Collab owns runtime state. AppSDK delegates commands.

Runtime is Codex only. The per-start override is `--runtime codex`.
`collab-mcp` is the shared MCP for every agent. It speaks
newline JSON-RPC and Content-Length frames for stdio MCP clients. `collab init`
merges `mcpServers.collab` into project `.mcp.json` without overwriting other
servers, using an absolute `collab-mcp` path when one is found.
Codex launch injects `appsdk-subagent` and does not override project
`sandbox_mode`.
The `collab` CLI remains a complete fallback when MCP tools are not
listed. Do not treat CLI as a substitute for MCP discovery.
After the child App Server thread exists, the parent writes the session identity and
registers it with the daemon. The child does not run `collab init`,
`worker recover`, or ask the user for identity.

Do not pass `--worktree` or `persist`. Codex health is `codex exec` with the
configured expected response, bounded by `[subagent.health] timeout_seconds =
90`. A probe timeout keeps the record; check `status` again without closing.
Snapshot is only a bounded read of the registered App Server thread.
