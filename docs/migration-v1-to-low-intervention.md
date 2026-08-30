# v1 migration: low-intervention Collab

This is the supported migration path for an existing `.agent-collab` project.
It retains journal, mailbox, claims, evidence, worktrees, and durable
identities. tmux remains the only live notification channel; tmux is wake-only.

## Lifecycle

1. Run `collab init` from the project main tree. It creates missing directories
   and starts the project daemon; it does not delete or rebuild state.
2. Inspect with `collab context`, `collab who`, `collab task status`, and
   `collab inbox`. Record active claims, unfinished tasks, and live panes.
3. Install the new binary, then perform the controlled restart:
   `collab down` followed by `collab up`, authenticated as master/operator.
4. Re-register each live pane with `collab init` or `collab worker recover`.
   Identity is rebound through the server; task ownership is not recreated by
   hand.
5. Verify context, tasks, inbox, single-daemon state, and journal replay before
   resuming work.

## Wait and resume

Every `waiting` task records blocker, responsible actor, reason, deadline,
resume events, and escalation. The server rejects direct and transitive wait
cycles. A deadline becomes `blocked` and notifies master; it never silently
resumes, releases a claim, or fabricates success. A wake prompts a fresh
`collab context` query. If tmux delivery is lost, mailbox and journal recover
the state.

## Deprecated paths

Deleting `.agent-collab` and reinitializing, manually editing task/claim/journal
JSON, clearing mailboxes, copying worker tokens, starting a second daemon,
mixed runtime writes, and inferring identity from pane labels/cwd are
unsupported. Use server operations and this lifecycle instead.
