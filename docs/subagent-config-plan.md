# Unified config and persistent subagents

Approved scope: ~/.appsdk/config.toml is the only policy source; project
overrides live there. Collab owns runtime state. AppSDK delegates commands.

1. Implement typed TOML defaults/project overrides and notification scheduling.
2. Add authenticated parent-owned subagent lifecycle to the existing journal.
3. Probe profiles once in priority order; start isolated tmux Codex, await
   explicit ready, reuse mailbox for tasks/results, close exact owned session.
4. Test config, scheduling, auth, replay, bounded failure and live isolated CLI.
5. Review, merge, install, restart existing daemons preserving task/message
   history; update skills; preserve evidence and remove owned worktrees.

No task reset, second registry, global profile mutation, automatic restart,
automatic task redispatch, production test notices, or implicit task completion.

Status: implementation started. Existing defaults remain usable without config.
