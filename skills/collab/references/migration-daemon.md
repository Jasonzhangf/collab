# Migration and Daemon Maintenance

## Formal v1 migration

```text
inspect -> plan -> admission freeze -> snapshot
        -> install reviewed binary -> controlled daemon restart
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
- Keep an explicitly stopped daemon down through implementation and review.
- Restart only after reviewed source reaches verified latest main.
- After restart prove one PID/socket, preserved durable state, identity rebind,
  migration verify, and one real subscribed notice.

## Deprecated commands

Never use implicit first-register master, master recovery, transfer-master,
legacy central dispatch (`collab task dispatch`), task claim queue,
remove-worker, reset, or heartbeat recovery. A live master may use the
explicit scheduler assignment (`collab subagent dispatch`) to create one
durable task/message reservation for an eligible peer. Collab master is a
separate explicit user-approved authority
decision, not Codex/Cursor root, migration, or daemon recovery. If a live
master exists, only that master may delegate. If none exists, a peer may
promote itself after recording user approval and verifying a live pane.
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
collab reset
```
