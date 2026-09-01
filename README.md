# collab

Project-local coordination for independent coding agents. One Rust daemon owns
the append-only journal, durable mailbox, task/resource state, and migration
transaction. tmux is the only live notification channel and is wake-only.

## Model

- Every registered identity is an equal peer. Declared `master`/`worker` roles
  are removed.
- There is no central task dispatch, idle-worker assignment, progress report,
  heartbeat, or ACK loop.
- Each peer self-registers and owns its complete task lifecycle: latest-main
  sync, private worktree, implementation, tests, exact commit, candidate
  verification, short integration lease, main merge/verification/push, and
  cleanup.
- Peers communicate only for shared-resource occupancy and release. The Server
  persists the notice before attempting a tmux wake.
- The daemon may wake a confirmed waiting-agent pane for an actionable local
  continuation. A shell, offline pane, or Braille-spinner working agent fails
  closed and is never interrupted.
- `/goal` delegation and interactive task recognition are deferred.

Scoped capabilities replace roles: task owner for one task, resource holder
for one resource, integration lease for one main merge, and daemon operator for
one maintenance action.

## Install

```sh
cargo install --path . --force
command -v collab
command -v collab-mcp
```

## Start and identity

```sh
cd <project-main-tree>
collab init
collab up
collab whoami
collab who
collab context
```

Identity creation requires a live tmux pane. Commands outside tmux fail before
writing an identity declaration. A tmux session is the stable peer identity;
its current pane is only the wake endpoint. Token proves access to that peer's
mailbox and lifecycle.

`collab role`, `collab master`, `collab transfer-master`, `collab task claim`,
`collab task dispatch`, `collab remove-worker`, and `collab reset` are
deprecated and fail explicitly.

## Independent task lifecycle

```text
read latest main
→ declare private worktree/branch
→ register own task
→ sync latest main into own branch
→ implement/test
→ commit exact change set
→ sync latest main again
→ verify candidate against latest main
→ acquire short integration lease
→ merge exact commit to main
→ verify/push main
→ mark merged
→ close performs mandatory cleanup and persists a cleanup receipt
```

```sh
collab task register <id> \
  --feature <feature-id> \
  --worktree ./playground/<short-slug> \
  --branch codex/<branch> \
  --base-commit <sha> \
  --priority p2 \
  --next "implement and verify"

collab task update <id> --status verifying --next "run gates"
collab task update <id> --status reviewed --next "record delivery"
collab task deliver <id> --evidence "commit=<sha>; gates=pass" \
  --worktree ./playground/<short-slug>
collab task update <id> --status merged --next "main verified and pushed"
collab task close <id>
```

Normal states are `working`, `blocked`, `waiting`, `verifying`, `reviewed`,
`delivered`, `rework`, `merged`, `closed`, and `cancelled`. `waiting` must be
entered through `collab task wait`; `delivered` through `task deliver`; and
`closed` through `task close`. `reviewed` cannot transition directly to
`merged`: successful delivery evidence is mandatory. Only the task owner may
mutate or close it.

Delivery is a local durable milestone. It sends no peer message. Every task
with a declared worktree carries a cleanup obligation. Close fails before
mutation unless the task is merged, its declared worktree is clean and inside
`./playground/`, and its branch is merged into current main. Close removes the
exact worktree/branch, verifies absence, persists a durable cleanup receipt,
and only then marks the task closed. A terminal task with a missing receipt or
an existing declared worktree fails audit; cancellation cannot bypass cleanup.

## Resource conflicts and waits

When a registration conflicts on `feature_id` or `worktree_path`, the Server:

1. persists the requested task as `blocked`;
2. persists `RESOURCE_OCCUPIED` notices for requester and holder;
3. journals a wake-attempt lease and asks tmux to wake the holder; and
4. returns structured `TASK_RESOURCE_CONFLICT` data.

The blocked owner may record a bounded wait:

```sh
collab task wait <task-id> --for <blocking-task-id>
```

Every wait stores waiter, blocking task, blocking owner as responsible actor,
reason, deadline, resume events, and `resource_owner_and_waiter_recheck`
escalation. Direct, two-peer, and transitive cycles fail closed. Missing owner,
missing deadline, unrelated resource, and delivered/terminal waits fail
closed. Timeout changes the waiter to explicit `blocked`, notifies only waiter
and holder, and never releases either claim automatically. Holder close moves
each waiter from `waiting` to `blocked`, clears the obsolete wait edge, and
persists `RESOURCE_RELEASED` before attempting its wake.

Manual P2P messages are restricted to `RESOURCE_OCCUPIED ...` and
`RESOURCE_RELEASED ...`:

```sh
collab send --to <peer> --type notify "RESOURCE_OCCUPIED ..."
```

Never type peer messages into tmux. The daemon owns literal text plus Enter.
Only a successful tmux transaction becomes `Delivered`. Failure remains
pending; the 10-second durable attempt lease prevents concurrent duplicate
wakes and allows retry when the pane later becomes a confirmed waiting agent.

## Local continuation

```sh
collab config --continuation-minutes 15
collab context
collab inbox
```

For an active task whose pane is a confirmed waiting agent, the daemon persists
one `CONTINUE_TASK` record and attempts one tmux wake. A durable pending marker
and wake-attempt lease deduplicate scheduler races. `collab context` consumes
the calling peer's local continuation and marks that mailbox record read, so no
explicit ACK loop is required. Shell, working, and offline panes fail closed;
a lost wake remains pending and is retried after the pane safely waits.

## Existing-project migration

Never delete/rebuild `.agent-collab`, edit task/claim/journal JSON, clear the
mailbox, copy tokens, start a second daemon, or invent an owner. Use:

```text
collab migrate inspect
→ collab migrate plan
→ collab migrate apply          # admission freeze + deterministic snapshot
→ cargo install reviewed binary
→ collab down
→ collab up
→ collab worker recover         # each live tmux peer
→ collab migrate verify         # verify and resume
```

Any authenticated peer may acquire the single migration transaction lease.
Another peer cannot replace an active plan/apply operator. Legacy role fields
and continuation-field names are accepted during replay but omitted from new
state. Legacy `available` tasks or waits without a real blocker owner require
explicit operator resolution; the daemon never fabricates ownership. Malformed
journal lines fail startup, and a snapshot/count mismatch remains frozen.

See [docs/migration-v1-to-low-intervention.md](docs/migration-v1-to-low-intervention.md).
The active lifecycle contract is
[docs/collab-v1-lifecycle.manifest.json](docs/collab-v1-lifecycle.manifest.json),
with source-bound adjacent edges in
[docs/mainline-call-map.json](docs/mainline-call-map.json).

## Daemon maintenance

`collab down` is an explicit, one-invocation daemon-operator capability; it is
not derived from an agent role or mailbox text. It writes `DOWN`, records the
request, and stops only the PID holding the project socket. `collab up` clears
the marker and starts one daemon. A second live socket writer is rejected.

## Core invariants

- One Server writer; append-only journal; deterministic replay.
- tmux is wake-only; no control truth is inferred from terminal text.
- No fallback, silent strip, automatic ownership, automatic claim release, or
  success projection from failed wake/cleanup/migration.
- No uncommitted product edits in main as an intermediate test step.
- One task owner, one declared worktree, one resource holder, and one migration
  transaction operator at a time.
