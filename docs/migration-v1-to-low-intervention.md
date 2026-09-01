# v1 migration: peer low-intervention Collab

This is the only supported upgrade path for an existing `.agent-collab`
project. It preserves journal, mailbox, tasks, waits, evidence, declared
worktrees, branches, and tmux-session identities. tmux remains the only live
notification channel and remains wake-only.

## Preconditions

- Work from the project main tree.
- Confirm only one daemon owns `.agent-collab/server/server.sock`.
- Record active tasks, resource conflicts, waits, inbox, journal size/hash,
  worktrees, branches, installed binary path/hash, daemon PID, and pane/session
  bindings.
- Stop new task/resource admission before replacing the daemon.
- Do not delete or rebuild `.agent-collab`.

## Lifecycle

```text
inspect
→ plan / acquire migration transaction lease
→ admission freeze
→ deterministic snapshot
→ controlled daemon down
→ install reviewed binary
→ controlled daemon up
→ journal replay and legacy field normalization
→ live tmux identity rebind
→ verify snapshot/counts/tasks/mailbox/journal
→ resume admission
```

Commands:

```sh
collab migrate inspect
collab migrate plan
collab migrate apply
collab down
cargo install --path /Users/fanzhang/code/collab --force
collab up
collab worker recover
collab migrate verify
```

Any authenticated peer may initiate migration, but the Server grants one
transaction lease. A second peer cannot replace the active `planned` or
`applied` operator. `apply` freezes task/resource mutations and persists a
deterministic state hash plus worker/task/message counts. Registration is still
allowed so existing tmux identities can rebind after restart. `verify` resumes
admission only when the snapshot, counts, tmux bindings, waits, journal, and
mailbox all remain valid.

## Legacy normalization

- Serialized `master`/`worker` role fields are accepted and discarded. New
  state has no declared role field.
- Legacy `heartbeat_*` and `continuation_*` configuration/task fields are
  ignored during replay and are never emitted into new runtime state.
- Legacy master transfer, recovery, central `available` queue, task claim,
  dispatch, remove-worker, reset, and `/goal` task registration paths fail
  explicitly.
- An `available` task has no valid peer lifecycle meaning and causes
  `migration_needs_operator`. The operator must identify the real task owner
  from evidence; the daemon never invents one.
- A waiting task without its real waiter, blocking task owner, deadline, resume
  events, or escalation causes `migration_needs_operator`.
- Legacy Herdr/non-tmux bindings must rebind through a real tmux session before
  verification.

## Wait and wake verification

Every current `waiting` task must contain:

- `waiter` equal to task owner;
- `waiting_for` pointing to an existing active blocking task;
- `responsible_actor` equal to that blocking task's owner;
- a matching active `feature_id` or declared worktree resource held by that
  blocking task;
- `reason`;
- future `deadline_ms`;
- non-empty `resume_on`; and
- `resource_owner_and_waiter_recheck` escalation.

The wait graph must be acyclic. Deadline expiry becomes explicit `blocked` and
does not release a claim or send an unsolicited message. Resource release first
changes the waiter to `blocked` and clears the obsolete wait edge. Only an
exact, finite, Agent-owned subscription creates a notification. Each wake
attempt is journaled; replay never resets the immutable lifetime cap of three.
tmux receives only a short message id in one command sequence. No registration,
shell/absent, unknown, working, expired, cancelled, consumed, or exhausted
subscriptions fail closed with zero input. `collab context` is read-only.

## Fail-closed conditions

Migration remains frozen or fails startup when any of these is true:

- malformed/manual journal line;
- changed snapshot hash or counts;
- second live daemon/socket writer;
- non-tmux or offline identity required by active state;
- unresolved legacy available task or wait owner;
- wait cycle, expired/missing deadline, or missing resume/escalation;
- missing task/worktree/evidence referenced by the durable state.

Fix the unique truth through supported Server operations or explicit operator
evidence, then create a new plan. Do not clear the migration record or edit JSON
by hand.

## Deprecated upgrade methods

Unsupported and fail-fast:

- delete/recreate `.agent-collab`;
- edit task, claim, journal, mailbox, or migration JSON;
- clear mailbox or copy identity tokens;
- infer owner from role, pane label, cwd, or terminal text;
- start a second daemon;
- direct `kill`, `pkill`, or `killall` instead of `collab down`;
- restore master/worker roles, central dispatch, available queue, task offer,
  progress report, heartbeat, or ACK loops;
- restore periodic `CONTINUE_TASK`, implicit idle delivery, inferred wake
  registration, body/prompt injection, or unbounded retry;
- mix tmux and another runtime;
- manually type a wake into a peer pane.

## Evidence after verify

Record:

- candidate commit/tree and release/global binary hashes;
- migration record id, operator, snapshot hash, counts, and verified phase;
- daemon stop/start receipts and old/new PID;
- one-socket/one-process evidence;
- peer/session/pane rebinds;
- task, wait, mailbox, journal, evidence, worktree, and branch preservation;
- real post-restart explicit sendmessage and subscribed resource/deadline replay.
