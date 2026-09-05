# Collab v1 peer low-intervention implementation plan

## Objective

Keep tmux as the only live wake channel while removing declared roles, central
dispatch, normal peer reporting, heartbeat/ACK noise, and silent two-way waits.
Each peer owns its complete task/resource/worktree/integration/cleanup
lifecycle. The daemon owns durable state, bounded waits, P2P conflict/release
notices, local continuation wake, and existing-project migration.

`/goal` delegation and interactive task recognition are a later milestone.

## Architecture

### Identity

- Registration requires a live tmux pane.
- tmux session is stable identity; pane is current wake endpoint.
- There is no persisted `role` field and no master promotion/transfer/recovery.
- Legacy role fields are accepted only during replay and discarded.

### Task owner lifecycle

```text
latest main
→ private declared worktree/branch
→ self-register working task
→ implement/test/exact commit
→ sync and verify against latest main
→ short integration lease
→ reviewed → successful delivery evidence
→ exact merge/main verify/push
→ merged
→ owner close/cleanup
```

Only owner mutates, delivers, merges, or closes. Delivery is local durable
evidence and sends no notification. No task offer, claim, available queue,
automatic assignment, or normal block/progress/close report exists.

### Resource coordination

- Registration conflict persists blocked requester plus durable occupancy
  notices for requester and holder before wake.
- Wait records waiter, blocker task, blocker owner, reason, deadline, resume
  events, and P2P escalation.
- Direct/transitive cycles, missing owner/deadline/resume path, unrelated
  resources, and terminal waits fail closed.
- Holder close moves each waiter to `blocked`, clears the obsolete wait edge,
  persists release truth, then attempts the wake. Failed wake stays pending.

### Continuation

- Daemon distinguishes a confirmed waiting agent from shell, offline, and
  Braille-spinner working panes; ambiguous state fails closed.
- A waiting owner with an actionable active task receives one durable
  `CONTINUE_TASK` wake per pending revision/interval.
- Every attempt is journaled before tmux input. A 10-second attempt lease
  deduplicates immediate/timer races; only success becomes `Delivered`.
- Failed wakes stay pending and retry only after the pane safely waits.
- `collab context` consumes the local continuation; no explicit ACK loop.

### Migration and maintenance

```text
inspect → plan/lease → freeze → snapshot → down/install/up
→ replay/normalize → tmux rebind → verify → resume
```

Migration lease is transaction-scoped to one authenticated peer. Daemon
operator is one explicit maintenance invocation, not a role. Malformed journal,
snapshot mismatch, second writer, legacy available task, or unresolved wait
remains frozen and requires explicit evidence; no owner is fabricated.

## Files and owners

- `src/server/state.rs`: journaled identity/task/wait/migration contracts.
- `src/server/mod.rs`: peer registration, self lifecycle, conflict/wait/release,
  context, migration, admission freeze, replay.
- `src/server/timers.rs`: deadline and local continuation wake.
- `src/server/knock.rs`: one tmux command queue with unique-buffer bracketed preview plus final `C-m`.
- `src/identity.rs`: tmux-session identity; no outside-tmux declaration.
- `src/proto.rs`, `src/main.rs`, `src/bin/collab-mcp.rs`: typed CLI/MCP surface
  and explicit deprecation errors.
- `src/scope.rs`, `README.md`, migration/maps/global skill: operational truth.

## Verification

1. Unit/state-machine:
   - first/later peers; legacy role removal;
   - owner-only full lifecycle and cleanup;
   - conflict occupancy and release;
   - legal wait, direct/two/three-peer cycles, missing owner, terminal wait;
   - deadline timeout, waiter exits waiting on release, correct P2P targets;
   - shell/working/offline not interrupted; wake failure remains pending;
   - lost-wake retry, attempt lease, one successful wake/one `Delivered`;
   - context consumes local continuation without an explicit ACK loop;
   - inspect/plan/apply/verify, lease conflict, snapshot mismatch, malformed
     journal, legacy field normalization and state preservation.
2. Static gates: maps agree with source; no live master/dispatch/offer/report,
   Herdr, `/goal`, or business-payload control leakage.
3. `cargo fmt --check`, `cargo test`, release build.
4. Isolated real tmux blackbox with two peers, independent tasks/worktrees,
   P2P conflict/wait/release, lost wake recovery, owner merge/close/cleanup,
   controlled migration restart, and journal replay.
5. AGY Review only after all source/runtime gates pass.
6. Exact commit/merge to latest main; main verification; global install; hash
   match; current daemon remains down until reviewed binary is installed.
7. Official `collab up`, one-instance check, identity rebind, migration verify,
   and post-restart lifecycle replay.

## Completion evidence

- reviewed commit/tree and exact file list;
- test/build/gate/tmux blackbox/AGY PASS;
- migration record and preservation evidence;
- release/global binary hashes;
- daemon down/up receipts and old/new PID;
- one socket writer;
- post-restart peer/context/task/wait/inbox/journal replay;
- no declared role, central dispatch, normal report, heartbeat/ACK loop, or
  `/goal` runtime path.
