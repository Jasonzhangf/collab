# Migration and Daemon Maintenance

## Global truth and project-local state

The host-wide runtime truth is the Collab state root resolved by the installed
binary:

```text
$COLLAB_STATE_DIR for isolated tests, else $HOME/.collab
  server.sock
  daemon.lock
  server.pid
  events.jsonl
  log.txt
```

Each registered project keeps its own `.agent-collab/` reducer input and
project-scoped journal, mailbox projection, tasks, claims, bindings, and
worktrees. It is project-local durable state, not a disposable cache and not
the host-wide runtime socket. The global `~/.collab/` state owns the host
socket, route table, and global identity/liveness records. AppSDK governance
has a separate truth owner and reset transaction; neither owner may delete or
rewrite the other's state.

For a new or explicitly authorized clean project, do not treat an existing
project-local `.appsdk/`, `.appsdk-control/`, or `.agent-collab/` directory as
the new version's baseline. The current global binary is the only runtime
baseline. AppSDK removal belongs to AppSDK's reset owner; Collab removal belongs
to the Collab reset/migration owner. Do not migrate or replay legacy local
history merely to make initialization look clean.

## Version upgrade and local installation cleanup

Use the repository's install sequence from the reviewed current source:

```sh
scripts/install-global-collab.sh
```

The installer performs one versioned release build and installs those exact
binaries under `$CARGO_HOME/bin`; the exact new binary refreshes the embedded
Skill. It never removes `~/.collab/`, a project's `.agent-collab/`, AppSDK
state, business source, or evidence. Legacy user-local copies are not removed
automatically: first prove an exact copy is Collab from its own version
response, then remove only that verified pair. Unverified path collisions
remain untouched and must be reported.

The global daemon can be shared by multiple projects. A binary upgrade alone
does not authorize stopping or restarting it. Keep the existing daemon running
unless an operator explicitly opens a maintenance window; then use only the
official lifecycle and record the old/new binary version and digest, PID,
socket, identity binding, and a live replay result. Never use `pkill`,
`killall`, broad process matching, a second socket, or an older binary as a
fallback.

## Formal v1 migration

```text
inspect -> plan -> admission freeze -> snapshot
        -> install reviewed binaries with scripts/install-global-collab.sh
        -> controlled daemon restart
        -> identity rebind -> verify -> resume
```

```sh
collab migrate inspect
collab migrate plan
collab migrate apply
# install and restart the exact reviewed build
collab migrate verify
```

Migration authenticates the initiator, holds one transaction lease, inspects
legacy fields/waits/bindings/journal/counts/single-writer state, freezes after a
deterministic snapshot, and verifies identity rebind plus snapshot/count
continuity before resume. Unresolved owner/count mismatch requires an explicit
operator decision; never invent an owner.

Never delete/recreate `.agent-collab`, edit its truth files, clear
evidence/mailbox, copy identity tokens, reset bindings, or mix old/new writers.

## Legacy project migration or retirement

Use this path when an existing project has `.agent-collab/` from an older
version and the operator wants to move to the current Collab baseline:

```sh
cd /abs/path/project
collab migrate inspect
collab migrate plan
collab migrate apply
# install the reviewed Collab binary
collab down
collab up
collab worker recover
collab migrate verify
```

`inspect` is read-only. `plan` does not freeze admission. `apply` freezes
admission and persists the deterministic snapshot. `verify` resumes admission
only after journal, mailbox, task, identity, and count continuity pass.

Run the same operation from the project root whose `.agent-collab/` is being
migrated. Do not run it in a worktree that merely points at another project's
scope, and do not start a second daemon for the migration.

If the result is `reset_required`, `needs_operator`, `unknown`, a malformed
journal, a count mismatch, or an unresolved owner:

1. Preserve the exact error, migration ID, snapshot, and source bytes.
2. Stop admission and dependent writes.
3. Resolve the named owner or blocker through the Collab server protocol.
4. Create a new plan only after the source can be proved safe.

When the operator explicitly decides to abandon the old epoch instead of
preserving it, and the project-local journal cannot be replayed under the
current contract, use the single offline reset owner:

```sh
collab down
collab reset --discard-legacy --approval "explicit user authorization text"
collab up
collab init
```

`collab reset` is not `collab migrate`. It never imports history, never
pretends to preserve counts, and never claims delivery, review, or install
evidence. It requires the daemon to be down, takes the same host writer lock,
archives the exact retired `.agent-collab/` and `.agent-collab-v2/` bytes under
`~/.collab/archives/`, verifies archive equality, removes only those
Collab-owned control roots plus the retired project stale host route, prunes
host routes whose canonical root is gone or no longer initialized, and
rebuilds the current empty baseline. It accepts an uninitialized project root
and therefore also repairs a missing baseline. It is idempotent; a second run
reports `already_reset: true`, including after the daemon has created an empty
current journal.

`.appsdk-control/` is AppSDK-owned and is never removed by this command. The
current project-local `.agent-collab/` is a project scope input, while the
host-wide runtime truth remains `~/.collab/` (`server.sock`, `events.jsonl`,
`log.txt`, and route state). It never removes business source, runtime data,
`active/`, or `protected/`, and it writes `delivery_verified: false`.

Never manually delete `.agent-collab/`, edit journal or mailbox JSON, clear
the mailbox, or copy identity tokens. Do not convert a failed migration into a
fresh registration by deleting the source outside this command. AppSDK
governance reset remains a separate owner and transaction; it never removes
`.agent-collab/`.

After `verify`, record the migration record ID, source and target versions,
snapshot digest, preserved counts, old/new daemon PID and socket, identity
rebinds, and one real post-restart subscribed message. Migration verification
does not prove AppSDK delivery, review, install, or freeze.

## Controlled daemon lifecycle

```sh
collab down
collab up
```

- These are explicit daemon-operator actions. Peer messages do not authorize
  maintenance.
- Use official lifecycle; never broad process-name kills.
- Resolve exact project socket/cwd/PID/tasks/migration/journal/mailbox first.
- Installing a binary does not replace a live daemon.
- Do not restart the global daemon merely to pick up an upgraded binary while
  other projects may still be using it. Schedule the maintenance explicitly.
- Keep an explicitly stopped daemon down through implementation and review.
- Restart only after reviewed source reaches verified latest main.
- After restart prove one PID/socket, preserved durable state, identity rebind,
  migration verify, and one real subscribed notice.

## Deprecated commands

Never use implicit first-register master, master recovery, transfer-master,
legacy central dispatch (`collab task dispatch`), task claim queue,
remove-worker, or heartbeat recovery. A live master may use the
explicit scheduler assignment (`collab subagent dispatch`) to create one
durable task/message reservation for an eligible peer. Collab master is a
separate explicit user-approved authority
decision, not Codex root, migration, or daemon recovery. If a live
master exists, only that master may delegate. If none exists, a peer may
promote itself after recording user approval and verifying a live registered
transport.
Journal `RootAssigned` events become `MasterAssigned` on daemon replay.
Hidden `collab root ...` commands run the same master protocol. Independent
peers may decline a master collaboration invite; managed subagents must obey
the master:

```text
collab master status
collab master promote --approval "<user text>"
collab master delegate <peer>
```

Deprecated:

```text
collab role
collab master recover
collab transfer-master
collab task claim
collab task dispatch
collab remove-worker
```
