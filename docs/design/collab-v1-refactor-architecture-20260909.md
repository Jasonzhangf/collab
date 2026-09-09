# Collab v1 refactor architecture

Status: design freeze candidate `v1-refactor-r1`  
Integration branch: `codex/v1-collab-refactor-main-20260909`  
Baseline: `origin/main` at `1881c6ff356b2c5c57a5f30fd9f15101f1bd3943`

This document is the implementation contract for the v1 refactor. It chooses
the tested v1 lifecycle as the migration baseline and adopts only the useful
v2 ideas: a typed Rust reducer, command IDs, ordered journal replay, explicit
write errors, and a thin Cordis-style adapter boundary. The existing v2
playground is a design reference; it is not the runtime to extend in place.

The human remains the final authority for goals, master authorization,
irreversible actions, release, cost, and new scope. Collab is the durable
runtime owner for registered identities, bindings, tasks, messages, waits and
delivery observations. AppSDK remains the quality/evidence owner and is not a
second Collab registry.

## Decision

The refactor has one semantic core and one integration line:

```text
v1 lifecycle and delivery semantics
        ↓
Rust typed reducer and journal (one writer)
        ↓
global resident Collab daemon
        ↓
thin adapters (TUI, Desktop AppServer, CLI, MCP, tmux)
        ↓
read-only projections (status, mailbox JSONL, notifications)
```

The following choices are binding:

- v1 is the source of lifecycle semantics: task/worktree/branch/base binding,
  review, delivery, integration, main verification, push, cleanup and close.
- Rust is the only owner of mutable Collab control state. Node or Cordis code
  may compose dependencies and own adapter/resource lifetimes, but may not
  keep a second map of identities, roles, tasks or messages.
- The daemon is globally unique per host. A project registers its scope with
  that daemon; a client never starts a second writable core when the socket is
  unavailable.
- The journal is the write truth. Mailbox JSONL, task status and briefings are
  rebuildable projections. A projection failure is explicit and does not roll
  back a committed journal event.
- v1 and v2 storage are never dual-written. Migration is a controlled
  inspect/freeze/snapshot/replay/rebind/verify operation.
- Every implementation round is a branch from the current integration branch,
  an independent worktree, an independent review, and a fast-forwardable merge
  back into this integration branch. No worker edits `main`.

Existing v1 contracts remain normative where they do not conflict with this
document: [`docs/collab.md`](../collab.md),
[`docs/migration-v1-to-low-intervention.md`](../migration-v1-to-low-intervention.md),
and [`docs/goals/collab-runtime-queue-handoff-plan.md`](../goals/collab-runtime-queue-handoff-plan.md).
This document supersedes their project-level daemon and identity assumptions
only where the sections below explicitly say so.

## Goals and non-goals

### Goals

- Let TUI and Desktop communicate through their native AppServer endpoints,
  with the same typed Collab contract and without requiring Desktop to have a
  tmux session.
- Make master authorization, agent/runtime binding and two-level scope
  explicit and durable across compaction, fork, restart and reconnect.
- Let a master drive workers and managed subagents without worker idle wakeups
  or bidirectional idle loops.
- Make every task, bug fix and peer operation a finite Loop with Trigger, Work,
  Gate, State and Stop components.
- Make direct messages immediate while batching idle/progress/delivery/bug and
  worker-idle notifications into one low-noise update at most every two minutes.
- Preserve every raw message in project JSONL while presenting only the latest
  state for one `(from, to, entity)` notification key.
- Keep bug intake, worktree isolation, review, merge, push, install, daemon
  restart and live replay as separate evidence states.

### Non-goals

- Do not introduce a second scheduler, task queue, worker registry or writable
  local substitute for the daemon.
- Do not make Cordis a business-payload bus or move AppSDK governance rules
  into Collab runtime code.
- Do not infer master from first registration, a process name, a pane title, a
  prompt, a session ID or a missing record.
- Do not remove existing journal, mailbox, identity or task history to make a
  migration appear clean.
- Do not rewrite all v1 behavior before the first real TUI/Desktop Loop passes.

## Ownership map

| Concern | Single owner | Consumers |
| --- | --- | --- |
| Identity, role grant, parent and binding | Rust reducer in Collab daemon | adapters, status |
| App/project scope and authorization | Rust reducer | command validation |
| Task/worktree/branch lifecycle | v1 task reducer and lifecycle commands | CLI, AppSDK evidence |
| Message facts and delivery observations | journal and mailbox projector | CLI, JSONL readers |
| Notification batching and wake budget | daemon accumulator | tmux/AppServer adapters |
| Bug intake and priority ordering | AppSDK bug system | master briefing, task dispatch |
| Loop goal and gates | task/goal record plus project contract | master and reviewers |
| Adapter detection and submission details | runtime adapter | daemon coordinator |
| Dependency composition and disposal | Cordis-style host adapter | daemon bootstrap |

No projection may become a new write path. AppSDK may reject or accept an
evidence claim, but it cannot mutate Collab identity or task state behind the
daemon.

## Scope, identity and runtime binding

### Two-level scope

The canonical route scope is:

```text
route_scope = (app_scope_id, project_scope_id)
```

- `app_scope_id` identifies one logical AppServer instance. TUI and Desktop
  attached to different AppServers have different app scopes, even when they
  use the same project directory.
- `project_scope_id` is derived from the normalized, exact **registered
  project root `cwd`** supplied by an AppServer registration. Different
  registered project roots are different project scopes, even under one
  AppServer. The task worktree `cwd` is an execution location, not a new
  project registration.
- Same project with different AppServers is the same project scope but a
  different app scope.
- `codex_app` and `codex_tui` are endpoint attributes, never scope identity.

Every message and task command carries the sender binding and target
`route_scope`. The daemon checks both levels before persisting a command.

An implementation worktree is bound explicitly at dispatch:

```text
WorktreeBinding {
  worktree_root,
  owning_project_scope,
  task_id,
  owner_agent_id,
  binding_id,
  base_commit
}
```

The daemon verifies that the worker's exact execution path matches this
durable binding and that the path is an allowed `playground/` worktree. A
worker may therefore report to its dispatching master even when its process
`cwd` is the worktree path: that path is authorized by the parent task, not
treated as an independently registered project. A path with no binding, a
binding for another project, or a client-supplied ancestor guess is rejected.
Ordinary peer-to-peer scope checks still use the registered project root and
continue to reject cross-project communication. A `WorktreeBinding` authorizes
the execution location for an already-authorized task; it never changes the
sender's registered `project_scope_id` or creates a cross-project exception.

### Stable identity versus an execution instance

The following identifiers are distinct:

```text
agent_id                 stable logical peer identity
runtime_id               one process/runtime execution instance
appserver_id             logical AppServer instance
endpoint_generation      connection/reconnect fence
binding_id               current agent ↔ runtime/thread binding
native_thread_id         AppServer thread/session handle
turn_id                  one model execution turn
message_id               one durable message
dispatch_id              one assignment intent across retries/recovery
```

Rules:

- Registration creates a `peer` by default. Only a user-approved master grant
  can add the `master` capability for a specific `agent_id`, project scope and
  boundary.
- A grant is not a role string supplied by a client. The daemon verifies the
  recorded user approval and the live binding before allowing master commands.
- Compaction keeps `agent_id` and `binding_id`; it advances a compaction epoch
  and read watermark. It does not create a new identity.
- Fork creates a new native thread, `runtime_id` and `binding_id`, and defaults
  to a peer. It inherits a master grant only after an explicit handoff record.
- Reconnect increments `endpoint_generation`. Commands from older generations
  fail with a typed stale-binding error and cannot append facts.
- TUI tmux sessions are optional wake/liveness evidence. Desktop uses the
  AppServer endpoint and native thread binding; it must not invent a tmux
  identity.
- `mcp_session_id` is a query connection and cannot authorize an agent or a
  master command.

## Global daemon and reducer

There is one resident daemon per host. It owns one authenticated socket,
one journal writer and one in-memory reducer instance. Project registration is
an operation on that daemon, not a project-local daemon startup.

The command path is:

```text
client request
  → authenticate binding and scope
  → validate command schema and expected revision
  → append command/event to journal
  → flush/sync journal
  → apply event to reducer
  → update rebuildable projections
  → return typed outcome with sequence and operation ID
```

The daemon must fail closed when the journal cannot be opened, appended,
flushed or replayed. A projection or wake failure is returned as a separate
diagnostic after the durable commit; it is never converted to success.

Every mutating command includes `command_id`, `actor_binding_id`, `scope`,
`expected_revision` when a compare-and-swap is needed, and a bounded
`operation_id`. Replaying the same `command_id` returns the original outcome;
it does not create a second task, assignment, message or wake.

The reducer publishes a monotonic sequence. Recovery replays complete journal
records, preserves a valid prefix, and reports a malformed middle record or
ambiguous tail as corruption requiring operator action. It never silently
skips history or resets wake budgets.

Cordis-style code is limited to:

- constructing the reducer, journal, adapter registry and dependency graph;
- selecting a typed adapter for TUI, Desktop, CLI or MCP;
- disposing resources on controlled shutdown;
- exposing read-only projections.

It must not implement a second state machine or call a writable fallback core.

## The Loop contract

Every unattended round is explicitly described and user-approved before it is
scheduled. The five components are mandatory:

```text
Trigger → Work → Gate → State → Stop
```

The execution order is:

```text
Discover
→ persist dispatch intent
→ Hand off
→ Verify
→ persist result
→ Schedule
```

The visible plan may abbreviate this as
`Discover → Hand off → Verify → Persist → Schedule`, but a dispatch side
effect is forbidden until its intent is durable.

Each Loop records:

- `loop_id`, goal/bug/task reference and current revision;
- trigger source and whether it is user, event, timer or webhook driven;
- work owner, input version, allowed/forbidden paths and resource lease;
- objective gate commands and expected outcomes;
- durable state location and recovery watermark;
- completion predicate, time/budget/attempt ceiling and stop owner;
- error policy for known failures, timeout and unknown outcomes.

The four required Loop families are:

| Loop | Owner | Minimum gate |
| --- | --- | --- |
| Master goal | master | goal evaluator, dependency state, mainline evidence |
| Bug report/fix | bug owner/master | reproduction, candidate tests, review, integration |
| Master → subagent | master/parent | scoped diff, declared tests, delivery receipt |
| Peer task | peer | task gate, worktree/cleanup evidence, conflict handling |

An idle observation, an ACK or a notification read is not Loop progress. A
Loop advances only on a durable dispatch, verified work result, gate result,
state persistence or explicit stop.

## Roles and communication permissions

Default identity is `peer`. `master` is a user-authorized capability, not an
automatic rank. A master coordinates only its project boundary and retains no
ownership of another peer's worktree.

| Relationship | Required authorization and scope | Result |
| --- | --- | --- |
| master → master | Both are live user-authorized masters; explicit coordination or bug reference. This is the only ordinary cross-scope channel. | Allowed across app and project scopes after target-master verification. |
| master → own worker/subagent | Active parent grant plus `WorktreeBinding`; the binding's `owning_project_scope` must equal the dispatching master's registered project scope. The worker may use a different app scope, and its worktree `cwd` is only an execution location. | Allowed only for that task and parent chain. |
| worker → own master/parent | Active task and binding with the same owning project scope as the parent; app scope may differ. | Allowed only to the bound parent/master. |
| independent peer → peer | Both are independent peers and both `app_scope_id` and registered `project_scope_id` are equal. No `WorktreeBinding` shortcut. | Allowed only within that app/project scope. |
| subagent → sibling or non-parent subagent | No direct grant, even when the processes share an AppServer or project. | Rejected. |
| Desktop → daemon | Registered endpoint generation; a user master grant is required when the Desktop identity is promoted to master. | Scope and role checked before any write. |

Same app with different registered projects is therefore rejected for
master↔worker, worker↔parent and peer↔peer. A task worktree belongs to the
parent's project through `owning_project_scope`; it cannot be used to smuggle a
message across registered projects. Different app scopes with the same
registered project are valid for an explicitly bound master↔worker relation.

Different masters have no parent/child relation. Their work is coordinated by
explicit messages and AppSDK bug records, not by assigning one master under the
other. P0 issues may interrupt according to policy; all other updates use the
batched notification path.

## Message facts and notifications

### Two channels, one fact history

- `sendmessage` is a direct command. It persists one message immediately and
  requests delivery according to recipient state.
- Idle, progress, delivery, bug and worker-idle events are notification facts.
  They enter the accumulator and are flushed together at most once per two
  minutes per recipient/generation.
- A notification title includes a short subject (maximum 50 characters) and
  timestamp. The batched preview names changed entities and the next action;
  detailed content is read from JSONL when needed.
- The project mailbox is append-only JSONL. Each line contains message ID,
  timestamp, source/target binding, scope, entity key, priority, subject,
  event kind, and raw body/reference. Partial writes are explicit errors.
- Presentation is idempotent by `(from_agent_id, to_agent_id, entity_key)`:
  only the latest state is shown in a briefing. Older raw lines remain
  available for audit and replay.

### Wake policy

- The daemon wakes only the master for scheduling responsibility.
- A worker has no long-horizon wake. A `working → idle` transition creates at
  most one idempotent worker-idle fact per idle episode, addressed to its live
  master.
- A master idle reminder is level-triggered. The daemon probes the master,
  sends one reminder, and stops the episode after three consecutive reminders
  without an observed return to `working`. It does not stack reminders.
- `working`, `unknown`, absent, stale or unverified runtime states do not accept
  an ordinary operational wake. Unknown is diagnostic, never permission to
  retry.
- A P0 issue is the only exception. Its durable record names the affected
  project scope, target master `binding_id`, `endpoint_generation` and active
  `turn_id`. The daemon submits one typed adapter interrupt request for that
  exact generation/turn and records `accepted`, `failed` or `unknown`.
  `accepted` is still not proof that the turn stopped: a separate observation
  of the native stop/cancel result is required. `queued` or a normal immediate
  message is never recorded as `interrupted`.
- While an affected P0 remains unresolved, ordinary new dispatches in that
  project scope are rejected. Only a remediation dispatch explicitly linked to
  the P0 bug is allowed. An adapter that cannot interrupt returns an explicit
  failure/unknown and leaves the P0 pending; it does not retry through tmux or
  invent a stopped turn.
- Timer ticks with unchanged accumulator state do not append a journal event.

The accumulator stores only reasons, IDs and revisions. It never copies full
task or bug payloads into a wake prompt. One pane snapshot per tick is shared
by health, keepalive, batching and delivery decisions.

## Bugs, worktrees and integration rounds

Every user feature or defect is represented in the AppSDK bug system. A bug
record carries priority, owner, project scope, reproduction, affected
component, and current evidence. Active bugs are part of the master goal
backlog and are sorted before new discretionary work. A P0 bug blocks the
affected project until a verified resolution or an explicitly recorded human
decision.

Each implementation task owns exactly one clean worktree under
`playground/` and one branch. It starts from the current integration branch
after that branch has been rebased or fast-forwarded to the latest
`origin/main`. The dispatch contract includes:

```text
dispatch_id, task_id, task_revision, owner, parent/master,
worktree, branch, base_commit, allowed_paths, forbidden_paths,
done_iff, gate, stop, budget, and recovery condition
```

The worker must not edit the integration worktree or `main`. A feature may use
several worktrees only when their path ownership is disjoint. The integration
owner alone merges validated candidates into the v1 refactor branch.

The per-round sequence is:

```text
create clean worker worktree
→ reproduce and implement
→ sync the current refactor branch into the worker worktree
→ candidate tests/build
→ independent review
→ deliver the unchanged reviewed candidate
→ merge that exact candidate into refactor branch
→ refactor-branch tests/build
→ push the refactor branch when authorized
```

The sync happens before candidate verification and review. If the integration
branch advances after review, the candidate must be rebased or merged again,
then the affected tests and independent review must run on the new commit/tree
before integration. A merge conflict resolution is a source change and cannot
reuse the old review.

After all rounds pass, the refactor branch is **integration-ready** when every
candidate is reviewed and merged, required work is accounted for, and each
worker worktree is clean or has an explicit open cleanup obligation. Cleanup
receipts are not a prerequisite for this intermediate state.

The release owner then syncs the refactor branch with the latest `origin/main`,
runs the final verification and independent review on the exact resulting
tree, verifies the real public entrypoint, pushes the tested main ref, installs
the canonical binary and restarts the one daemon. Only after those mainline
receipts may task owners perform their normal merged-task cleanup/close and may
the owner remove the refactor branch and its worktrees. A cleanup receipt is a
post-main delivery fact, never a pre-main permission to replace `main`.

## Error and recovery contract

All external operations return one of `success`, `failure` or `unknown` with
an operation ID and evidence reference.

- **Known failure:** preserve the exact error, durable command/event IDs and
  owner. Fix the unique source owner or leave the Loop blocked with a concrete
  next condition.
- **Timeout:** query the original operation by ID and revision before any
  retry. Never send the same command through tmux as an unverified fallback.
- **Unknown:** do not infer delivery, acceptance, idle, completion or
  authorization. Persist the unknown observation and require a bounded
  diagnostic or human decision.
- **Daemon unavailable:** preserve socket/PID/log evidence; do not start a
  second writable daemon, copy tokens, clear journal/mailbox, or re-register a
  different identity.
- **Stale binding:** reject the command, refresh the endpoint generation and
  rebind the named agent only after live identity verification.
- **Journal/projection fault:** durable append failure blocks state mutation;
  projection failure leaves the journal fact committed and reports repair
  needed. No empty backlog or successful close is synthesized.
- **Worker blocker:** worker investigates first, then reports root cause,
  attempted actions, proposed fix and exact decision needed to its live master
  (and parent when different). Master resolves, reassigns or auditable-force-
  closes; it does not leave a worker waiting without an owner decision.

## Efficiency rules

- Use one reducer, one journal writer and one daemon; do not poll each client
  independently.
- Share one pane/runtime snapshot per tick across all schedulers.
- Use level-triggered sets and revisions, not one event per unchanged timer
  tick or observation.
- Keep wake payloads bounded; fetch details lazily from JSONL and typed status.
- Deduplicate by command ID, dispatch ID, notification entity key and
  endpoint generation.
- Do not test a ten-second deadline merely to stress the system. Timeouts are
  bounded by the operation's real service contract and must tolerate a busy
  daemon through one deadline, status lookup and explicit recovery.
- Do not create work to fill worker capacity. An idle worker is capacity, not a
  reason for a speculative task.

## Migration and cutover

Migration is staged and reversible until final main replacement:

```text
inspect current v1 state
→ acquire migration lease
→ freeze new mutations
→ snapshot journal/mailbox/tasks/bindings/worktrees
→ start the new global daemon with the same durable store
→ replay into the Rust reducer
→ rebind live TUI/Desktop endpoints
→ verify counts, sequence, scopes, permissions and waits
→ resume admission
```

The migration gate fails closed on malformed journal records, snapshot/hash
drift, more than one socket writer, unresolved owner/wait edges, missing
worktree evidence, or a required identity that cannot be live-verified.

The v2 playground is not replayed into the v1 store by copying records. Any
useful v2 behavior is reimplemented behind the v1 command/event contract and
receives new evidence. Old records remain immutable history.

## Implementation rounds and file ownership

The following rounds are intentionally independent. A later round starts only
after its dependency has merged into this branch. Paths are exclusive within a
round.

### R1 — runtime identity, scope and command envelope

Owner: one GCM worker. Allowed paths: `src/identity.rs`, `src/scope.rs`,
`src/proto.rs`, their focused tests and the matching contract docs. R1 only
adds compileable typed fields, serialization and pure validation helpers for
`agent_id`/`runtime_id`/`binding_id`/generation, two-level scope and
`command_id`/`operation_id`. It does not claim durable ownership, write the
journal, or enforce a master grant. Its done-iff is type/serialization/negative
validation evidence with no task-state behavior change.

### R2 — global daemon and single reducer

Owner: one GCM worker after R1. Allowed paths: `src/server/mod.rs`,
`src/server/state.rs`, `src/config.rs`, a new
`src/server/notification_contract.rs`, daemon tests and protocol fixtures.
R2 is the sole owner of durable `WorktreeBinding`, master-grant enforcement,
replay/sequence/idempotency, one resident writer and explicit journal errors.
It consumes the R1 types and must not add a second storage format.

Before delivery, R2 must extract the producer-side seam that later rounds use
without editing R2 files: `NotificationStateView` (read-only reducer state),
`NotificationSink` (typed event submission), the reducer event interface, and
the module export/daemon wiring needed for those types to compile on the real
serve path. The seam must prove one journal event can be read by a consumer
without a second writer or JSONL implementation. R2's done-iff includes this
compiled seam and its focused tests; a document-only interface is insufficient.

### R3 — AppServer adapters and real bidirectional Loop

Owner: one GCM worker after R2. Allowed paths: new `src/adapters/` modules,
`src/client.rs`, `src/bin/collab-mcp.rs`, adapter tests and replay fixtures.
R3 consumes the R2 command/reducer seam; it owns the adapter module declaration
inside `src/client.rs`, adapter detection/submission, and the native P0
interrupt mapping. It does not edit `src/server/mod.rs`, `src/server/state.rs`,
or notification scheduling. If the final daemon bootstrap needs a one-line
wire-only change outside these paths, the integration owner performs it after
R3 review and reruns the affected gate; R3 may not leave an unreferenced
adapter module. Map TUI and Desktop AppServer operations into the same typed
surface; tmux is optional. Prove both directions with separate request/turn/
cursor evidence and negative stale/unknown/timeout/wrong-turn cases.

### R4 — notification accumulator, batching and JSONL projection

Owner: one GCM worker after R2. Allowed paths: `src/server/timers.rs`,
`src/server/keepalive.rs`, `src/server/knock.rs`, new
`src/server/mailbox.rs`, notification tests and
`skills/collab/references/notifications.md`. R4 consumes the R2
`NotificationStateView`/`NotificationSink` seam and is the sole writer of the
accumulator, JSONL projection and wake policy in these paths. It must not edit
`src/server/mod.rs`, `src/server/state.rs` or R2 identity/role/reducer code. If
production module wiring is required, the integration owner makes the single
wire-only change after R4 review and reruns the notification gate; R4 may not
introduce a second mailbox or leave its projector disconnected. Implement
two-minute batching, latest-state presentation, raw JSONL retention,
master-only wake, worker idle episode idempotency and three-reminder stop.

### R5 — bug/worktree/Loop integration and skill contract

Owner: one GCM worker after R1–R4. Allowed paths: task/bug command owner,
`src/main.rs`, `docs/`, `skills/collab/`, and focused integration tests.
Bind bug priority to master backlog, encode the five-part Loop contract,
preserve v1 delivery evidence, and remove contradictory skill guidance.

### R6 — independent architecture review, integration and cutover rehearsal

The implementation owners do not review their own changes. The integration
owner verifies the exact merged tree, runs the full test/build/replay matrix,
rebuilds the global binary, restarts the single daemon once, and records the
cutover rehearsal. Astra is reserved for design-level or repeated same-root
failures; an independent Luna/GCM reviewer handles ordinary diffs.

## Gates and definition of done

Each round must provide:

- exact base commit, candidate commit/tree and allowed file list;
- focused red/green tests, `cargo fmt --check`, `cargo test` and applicable
  release build evidence;
- independent review with concrete PASS/FAIL findings;
- no duplicate truth, hidden fallback, silent error conversion or scope leak;
- clean worktree and task delivery receipt.

The following gates are separate; passing one does not imply the next.

### Refactor-branch integration-ready

The refactor branch reaches this state when:

1. Rust reducer replay is deterministic and all journal/write/unknown errors
   remain visible.
2. User-authorized master, peer, worker and managed-subagent permissions pass
   positive and negative tests across both scope levels.
3. TUI ↔ Desktop AppServer communication passes both directions on the real
   entrypoints; stale binding, wrong turn, timeout/unknown and recovery cases
   fail closed, and a working-turn P0 test observes an actual native stop
   before the remediation dispatch continues.
4. Master-only wake, worker-idle episode deduplication, two-minute batching,
   latest-state presentation, JSONL retention and three-reminder stop pass.
5. Bug priority, independent worktree, review and merge evidence are recorded
   for every required round, and the merged branch has passed its tests/build.
6. Every worker worktree is clean or has an explicitly owned open cleanup
   obligation. Cleanup receipts, installation and daemon restart are not
   prerequisites for this intermediate state.

### Allowed to replace `main`

The release owner may replace `main` only after the exact candidate is synced
with the latest `origin/main`, its final tests/build and independent review pass,
the candidate/main ancestry and diff are verified, the remote ref is checked,
and every unmerged refactor worktree/branch is either explicitly retained with
no required work lost or proven to contain no required work. This gate records
the authorization to change the main ref; it does not require post-main
installation or cleanup receipts.

### Mainline delivery complete

After the tested main ref is replaced and pushed, delivery is complete only when
the canonical global binary is rebuilt and installed, the one daemon is
controlled-restarted, runtime bindings are revalidated, the real public
entrypoint replay passes, and each retained task/worktree has an actual cleanup
receipt and close record. Push, install, restart, replay and cleanup evidence
remain separate facts even when one command performs more than one action.

No source implementation is considered complete from this design document
alone. The next action is Astra's independent design review of this contract;
only after a PASS may the Luna orchestrator dispatch the R1–R4 GCM workers.
