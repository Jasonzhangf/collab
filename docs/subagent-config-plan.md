# Unified config and persistent subagents

Approved scope: ~/.appsdk/config.toml is the only policy source; project
overrides live there. Collab owns runtime state. AppSDK delegates commands.

Default runtime is Cursor CLI. Codex remains an explicit `runtime = "codex"`
opt-in, or a per-start `--runtime cursor|codex` override. `collab-mcp` is the shared MCP for every agent. It speaks
newline JSON-RPC (Codex) and Content-Length frames (Cursor and other
stdio MCP clients). `collab init` merges `mcpServers.collab` into
project `.cursor/mcp.json` and `.mcp.json` without overwriting other
servers, using an absolute `collab-mcp` path when one is found.
It also writes Cursor project `.cursor/cli.json` permissions for `collab`
(`allow` plus required `deny: []`; no `sandbox` key — that belongs only in
`~/.cursor/cli-config.json`). Codex launch injects `appsdk-subagent` and
does not override project `sandbox_mode`. Cursor launch is
`--yolo --trust --approve-mcps --sandbox disabled --model auto --workspace <cwd>`.
The `collab` CLI remains a complete fallback when MCP tools are not
listed. Do not treat CLI as a substitute for MCP discovery.
After the child pane exists, the parent writes the session identity and
registers it with the daemon. The child does not run `collab init`,
`worker recover`, or ask the user for identity.

Cursor start flags are required on every launch so new worktrees do not
depend on a prior `mcp enable` or directory login:

`agent --yolo --trust --approve-mcps --sandbox disabled --model auto --workspace <project-cwd>`

Do not pass `--worktree` or `persist`. Cursor health probe is official
`agent status --format json` (`loggedIn: true`), bounded by
`[subagent.health] timeout_seconds = 90`. It is not Codex `exec` and not
tmux snapshot. A probe timeout keeps the record; check `status` again
without closing. Snapshot is only session-screen progress after a pane
exists. Codex health remains `codex exec` with the configured expected
response.
