# Unified config and persistent subagents

Approved scope: ~/.appsdk/config.toml is the only policy source; project
overrides live there. Collab owns runtime state. AppSDK delegates commands.

Default runtime is Cursor CLI. Codex remains an explicit `runtime = "codex"`
opt-in. `collab-mcp` is the shared MCP for every agent. It speaks
newline JSON-RPC (Codex) and Content-Length frames (Cursor and other
stdio MCP clients). `collab init` merges `mcpServers.collab` into
project `.cursor/mcp.json` and `.mcp.json` without overwriting other
servers, using an absolute `collab-mcp` path when one is found.
It also writes the CLI permissions those agents need to talk to tmux:
Codex `.codex/config.toml` uses `danger-full-access` / `never`, Cursor
`.cursor/cli.json` disables sandbox and allows `collab` / `collab-mcp`,
and Claude Code `.claude/settings.json` allows the same Bash patterns.
Cursor launch uses `--yolo --trust --approve-mcps --sandbox disabled`.
Codex launch injects the same binary as `appsdk-subagent` and starts
with `--sandbox danger-full-access --ask-for-approval never`.
The `collab` CLI remains a complete fallback when MCP tools are not
listed. Do not treat CLI as a substitute for MCP discovery.
After the child pane exists, the parent writes the session identity and
registers it with the daemon. The child does not run `collab init`,
`worker recover`, or ask the user for identity.

Cursor start flags are required on every launch so new worktrees do not
depend on a prior `mcp enable` or directory login:

`agent --yolo --trust --approve-mcps --sandbox disabled --workspace <project-cwd>`

Do not pass `--worktree` or `persist`. Health probe is
`agent --print --mode ask --trust` and never uses `--yolo`.
