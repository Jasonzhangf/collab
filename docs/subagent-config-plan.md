# Unified config and persistent subagents

Approved scope: ~/.appsdk/config.toml is the only policy source; project
overrides live there. Collab owns runtime state. AppSDK delegates commands.

Default runtime is Cursor CLI. Codex remains an explicit `runtime = "codex"`
opt-in. Launch does not inject MCP; both runtimes use preconfigured `collab`
MCP or the `collab` CLI.

Cursor start flags are required on every launch so new worktrees do not
depend on a prior `mcp enable` or directory login:

`agent --yolo --trust --approve-mcps --workspace <project-cwd>`

Do not pass `--worktree` or `persist`. Health probe is
`agent --print --mode ask --trust` and never uses `--yolo`.
