# Collab State Paths and Components

## Global host runtime truth

The host-wide daemon state root is resolved by the installed binary:

```text
$COLLAB_STATE_DIR for isolated tests, else $HOME/.collab
  server.sock
  daemon.lock
  server.pid
  events.jsonl
  log.txt
  routes.jsonl
```

Usage:

- `server.sock` is the live daemon socket. It is created by `collab up` and
  removed by `collab down`. Never write to it by hand.
- `daemon.lock` prevents a second daemon. Do not delete or edit it.
- `server.pid` is the current daemon PID. Verify it with `collab status --all`
  after a controlled `collab down` / `collab up`.
- `events.jsonl` is the append-only durable daemon event stream. Do not edit or
  delete it.
- `log.txt` is diagnostic output. Read it for exact errors; never use it as a
  source of truth for task or identity state.
- `routes.jsonl` is the append-only host-wide registration table for app scopes
  and project roots. Never hand-edit it. Stale or missing-root routes are
  retired only through the Collab migration/reset owner.

For an explicitly authorized legacy reset, use:

```sh
collab down
collab reset --discard-legacy --approval "<explicit user authorization>"
collab up
collab init
collab status --all
```

Do not `cp`, `grep`, `mv`, truncate, or edit `routes.jsonl`; that bypasses the
owner and destroys route provenance. If existing `.agent-collab/` state must be
preserved, use migration instead of reset.

An ordinary version upgrade is not a reset. Install the current reviewed
binary and refresh the embedded Skill; leave `~/.collab/` and the running
daemon untouched. If the running daemon must load the new binary, use a
separately authorized maintenance window and follow the controlled lifecycle
in [migration-daemon.md](migration-daemon.md).

## Project-local durable state

Each registered project root has its own `.agent-collab/`:

```text
<project>/.agent-collab/
  server/
  runs/<worker-id>/identity.json
  mailbox/
  messages/
```

`.agent-collab/` is project-local durable state, not the host-wide truth.
AppSDK reset must never delete it. Use `collab migrate` or the explicit reset
lifecycle rather than deleting files by hand.

For a new project, initialize from the current global version and do not copy
or replay old project-local control files. An old local directory is ignored
unless the operator explicitly chooses the owner's migration or reset route.

## Identity and role

- The current client is Codex only. Identity is bound to the Codex sessionID
  through the internal App Server native thread.
- Default role is `peer`; master is explicit and user-approved.
- `collab context` is the single information endpoint for the current peer,
  binding, role, transport, liveness, tasks, and peers.
- `collab who` and `collab status --all` are diagnostics, not setup steps.

## Transport selection

- Workers never choose transport themselves. The server validates the candidate
  and selects the channel.
- App Server is the supported transport. A selected transport must include its
  server self-check and a live native thread.
- Do not inspect terminal environment paths or infer identity from a pane.

## Registration verification

Run `collab context`. If it says unregistered, run the idempotent
`appsdk init .` (or `collab init` for a standalone project), then run
`collab context` again. Registration must run from the canonical project main
tree, not a `playground/` worktree.

`collab init` success is not delivery proof. Verify the live binding, selected
transport, endpoint liveness, presence, and role through `collab context`.
Never edit `routes.jsonl`, `server.pid`, journal, mailbox, or identity files to
make a registration appear healthy.
