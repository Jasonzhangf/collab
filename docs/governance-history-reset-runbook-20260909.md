# Governance history reset and rebuild runbook

Status: **planned, docs-only**. This runbook defines the controlled procedure
for migrating the existing project histories to the Collab v1 host-wide
daemon. It does not authorize or perform a live archive, reset, replay,
identity rebind, daemon restart, install, merge, push, or cleanup.

The current observations are in the latest r4 field evidence
[`governance-history-live-refresh-r4-20260909.md`](./evidence/governance-history-live-refresh-r4-20260909.md),
whose reviewed candidate content is at commit `8c627beeb50b98f6e913906e233ae476a0bd8db7`.
Older refresh documents are historical clues only and are not current
migration inputs.
The migration goal and contract are in
[`collab-v1-governance-history-migration-plan.md`](./goals/collab-v1-governance-history-migration-plan.md),
and the schema is
[`migration-v1-history-manifest.schema.json`](./migration-v1-history-manifest.schema.json).
The runtime owner is the v1 architecture in
[`collab-v1-refactor-architecture-20260909.md`](./design/collab-v1-refactor-architecture-20260909.md).

## 1. Safety contract

The migration has one host-wide daemon, one reducer and one journal writer.
Each project keeps its own canonical project scope. Collab owns coordination
facts; AppSDK owns quality and release records; mailbox JSONL and notification
views are rebuildable projections. Raw source bytes, errors and historical
relations remain available in an immutable archive.

The target lineage is project-specific. Only the Collab project is integrated
into the Collab v1 integration lineage. AppSDK records bind to an AppSDK
owner-selected target and remain under AppSDK quality/release ownership;
RouteCodex records bind to a RouteCodex owner-selected target and remain under
RouteCodex ownership. Collab may retain immutable coordination references to
those projects, but it is not their product or release integration target.
codexapp first binds to its verified native AppServer target.

`reset_required` means that safe historical replay cannot be proven. It does
not mean that reset has happened, that old facts were deleted, or that the new
epoch is valid. `needs_operator` means an external decision or runtime
bootstrap is required. Neither state satisfies migration completion.

These rules apply to every project and every attempt:

- Never edit, truncate, rename, delete or hand-rewrite a live journal,
  events file, mailbox, identity token, claim, task, PID, lock or socket.
- Never use a line number as a replacement for a missing source sequence or
  record ID. Never convert an absent owner, timeout or ambiguous result into
  success.
- Never start a second daemon. A PID, socket, empty lock file, historical
  receipt, ACK, goal projection or mock transport is not proof of ownership,
  liveness or completion.
- Only the migration controller may write after it holds the authenticated
  migration lease and the source writers are frozen. The controller itself
  must use the canonical reducer and writer; it must not create a parallel
  epoch or registry.
- A live apply requires explicit operator authorization naming the projects,
  source roots, target build, archive destination and allowed operation. This
  document round has no such live authorization.

## 2. Current classification to carry into the next inspect

The r4 evidence is a bounded observation, not a live migration snapshot. Its
hashes and counts must be refreshed immediately before a lease is taken. The
expected starting disposition is:

| Project | Source to inspect | Current disposition | Why a reset or operator gate is required |
| --- | --- | --- | --- |
| Collab | `/Volumes/extension/code/collab` and its `.agent-collab` data | `reset_required` | Dirty v2 root, 151 worktrees, 135 branches not merged to local `main`, one current-round `SubagentUpdated=closed` projection and four current-round `KeepaliveUpdated=absent` projections, 21 processes in the clean `collab serve` census, and no verified scope/binding/generation/epoch fields. |
| AppSDK | `/Users/fanzhang/Documents/github/appsdk` and its `.agent-collab` data | `reset_required` | Dirty root with five unresolved paths, 101 worktrees, 42 branches not merged to local `main`, mixed historical worker projections, active blocked task/bug facts, and a recovery-required goal projection. |
| RouteCodex | `/Users/fanzhang/Documents/github/routecodex` and its `.agent-collab` data | `reset_required` | Dirty mixed V3/V4 root, 74 worktrees, 378 branches not merged to local `main`, current keepalive projection of 22 absent, four idle, one unknown and one working, and task edges requiring fresh owner/runtime proof. |
| codexapp | `/Users/fanzhang/Documents/github/codexapp` plus `/Users/fanzhang/.codex-communication` | `needs_operator` | No Git or Collab source, external journal only, no successful `commd.sock` connection, and no native initialize/capability/runtime binding receipt. |

The r4 refresh recorded these source projections, which are useful anchors for
the next inspect but are not reusable apply inputs:

| Project | Journal/event source observation | Digest anchor |
| --- | --- | --- |
| Collab | 72 journal lines; 476 event lines | journal `c47e0f95…1fffc83`; events `45710463…1709ad` |
| AppSDK | 46,777 journal lines; 5,244 event lines | journal `316c9f58…f5b5d25`; events `cf8c5b1d…66d4dce` |
| RouteCodex | 39,431 journal lines; 26,251 event lines | journal `9dd93d73…941273`; events `7ae997f8…f04c1a00` |
| codexapp | 54 external journal lines; no external events file | journal `c3d995d8…f485a4` |

The r4 comparison found differences across evidence windows. It does not
establish that the files changed during the nine-second main capture. A later
inspect must produce new source digests, counts, process ownership, worktree
inventory and classification before any operation lease.

The admission decision has two separate layers. **Hard admission** concerns
the operation that is about to write: an explicitly authorized migration
operator, an authenticated lease, all legacy writers frozen with one target
writer proven, an exact reviewed target build, stable source digests and
canonical project roots, and a non-conflicting target scope. A missing or
ambiguous hard-admission fact stops the operation at `needs_operator` or
`reset_required`.

**Historical record classification** concerns facts being preserved. A
record with a missing owner, lost runtime, unresolved merge conflict, stale
worktree or unverifiable delivery is retained in the immutable archive with
its exact error and first failed boundary. It is `archive-only` and its active
import remains blocked until a later owner/runtime/evidence reconciliation.
It does not require repairing every old owner before the source can be
archived. Historical defects become hard admission blockers only when they
prevent source-byte/digest integrity, writer fencing, scope isolation, or
safe operation of the migration controller itself.

`archive-only` is an import disposition, not a mapping class. A record that
fails deterministic replay keeps `mapping_class=reset`; an ambiguous side
effect keeps `mapping_class=unknown`. At manifest stage S1 (`planned`), the
archive pointer is still null because no immutable archive receipt exists.
Only after archive equality and immutability are verified may an archive-only
record receive its archive reference. These record-level results do not change
the project-level disposition above.

## 3. State machine and authority

Each project is one finite migration Loop:

```text
planned
  -> inspecting
  -> classified
  -> needs_operator | reset_required | replayable
  -> lease_acquired
  -> writers_frozen
  -> archived
  -> epoch_created
  -> replaying/reconciling
  -> projections_rebuilt
  -> verified
  -> resumed
```

Any failed or ambiguous transition stops the project in `failure`,
`unknown`, `needs_operator`, or `aborted`; it never advances by timeout.
`reset_required` is a classification before an archive and target epoch, not
an operation result. A transition is durable only after its journal append,
fsync and reducer commit succeed.

The operator authorizes the irreversible boundary. The migration owner
prepares the manifest, holds the lease, and records receipts. The runtime
owner supplies the reviewed daemon build and its exact candidate/tree. The
project owner resolves worktree, AppSDK and active-fact ownership. No master
grant, worker status, or project role supplies operator authority.

Before an apply, the controller must persist a plan containing:

```text
migration_id
operator_authorization_id
source_project_id and canonical_project_cwd
source schema/version, source branch/head/tree when applicable
source journal/events/mailbox/claims/worktree digests and counts
source_epoch and target_epoch
mapping policy and first failed boundaries
archive destination and expected archive digest
runtime candidate/artifact/environment/entrypoint/producer identity
lease and fencing token
rollback owner and stop conditions
```

The plan is immutable input to the attempt. A changed source digest,
canonical cwd, target build, lease token, writer count or active revision
requires a new inspect and plan.

## 4. Preflight: read-only and mandatory

Run preflight separately for all four projects. It may read live state and
write only a new evidence record outside the source roots. It must not acquire
the migration lease, start a daemon, change a branch, alter a worktree, send a
notification, or import a fact.

1. Resolve the canonical cwd, repository identity, branch, HEAD, tree,
   `origin/main`, status, unresolved paths, all worktrees, and every local
   branch not merged to the project’s `main`. Record each path, branch, owner,
   task, claim, base commit, candidate commit, review receipt and delivery
   state. A branch count alone is not a merge decision.
2. Resolve every data source: journal, events, mailbox JSONL, claims, panes,
   runs, AppSDK records, external transport journal, PID, socket and lock.
   Hash exact bytes, including newline framing, and record size and count.
   Validate JSONL framing and the typed v1 event schema without rewriting the
   source.
3. Enumerate every writer process by PID and cwd, prove the one-writer lock
   holder, and check that all writers use the same reviewed binary and target
   root. Any duplicate, unknown or unaccounted writer is `reset_required` or
   `needs_operator`; an empty PID or lock does not prove that no writer exists.
4. Build the owner graph: agent, runtime, AppServer/native thread, endpoint
   generation, project scope, app scope, task, worktree, branch, claim,
   evidence, bug and goal. Missing, conflicting or stale edges remain
   unresolved. `mcp_session_id`, pane title, process name and session
   compression/fork history are query clues, not identity authorization.
5. Check the source epoch and latest reducer revision. If either is absent,
   non-monotonic, duplicated or not bound to the source digest, use a fresh
   target epoch and keep the source archive-only.
6. Query active bugs, tasks and goals through their owning systems. Preserve
   IDs and exact source references; do not import a goal solely because a
   control projection says `subscribed`. Desktop/codexapp never registers a
   `goal subscribe`.
7. Produce a schema-valid manifest with `mapping_status=planned`. Every
   record has a source ID, type, digest, observed class and first failed
   boundary when applicable. Target sequence/entity, archive reference and
   final mapped status remain null until the corresponding durable operation.

Preflight stops if source bytes change during inspection. It also stops for a
dirty candidate presented as delivered when that candidate is proposed for
active import, an unknown command outcome that would be replayed, a source
scope collision, an unavailable runtime prerequisite, or an unverified
AppServer capability. A historical missing owner, lost runtime or unresolved
merge conflict is instead recorded as `archive-only` and blocks that record's
active import; it does not by itself prevent an authorized preservation
archive.

### Read-only inspect versus lease and epoch admission

The inspect phase and the write phase have different authorities and outputs:

| Phase | Allowed work | Admission effect |
| --- | --- | --- |
| Read-only inspect/classify | Read source files and process/socket metadata; hash and frame JSONL; inventory branches/worktrees; classify project and record facts; write a new evidence record outside the source roots. | No lease, writer freeze, archive, reset, epoch, rebind, notification, task reassignment or source mutation. `reset_required` is only a classification. |
| Operator admission | Verify explicit operator authorization, acquire one migration lease, bind it to the source digest/canonical cwd/target build and fencing token, and persist the immutable migration plan. | A rejected or unknown authorization/lease leaves the source untouched and stops the attempt. A lease does not by itself create an epoch or permit replay. |
| Epoch apply | Freeze legacy writers, create and verify the immutable archive, fence the old epoch, allocate one fresh target epoch/writer, then map and reconcile records. | Only the authenticated migration controller may write. Archive/reset/replay/rebind cannot begin before the lease and writer-freeze receipts. |

The operation therefore always starts with a fresh read-only inspect. Only
after the operator admission receipt and lease are durable may the controller
enter the archive/fencing path. Only after archive equality, immutability and
old-epoch fencing are proven may it create the fresh epoch. A planned
manifest, status row, historical receipt or local goal projection cannot stand
in for any of these gates.

## 5. Worktree and branch inventory gate

Migration must not silently lose implementation work. Before freezing a
project, produce an inventory row for every worktree and every branch not
merged to the project’s `main`:

```text
project, worktree_path, branch, HEAD, base, status, unresolved_paths,
task_id, issue_id, claim_id, owner_agent, runtime/binding, review_receipt,
delivery_receipt, merged_to_main, merge_commit, disposition, reason
```

Use the project’s exact local `main` and `origin/main` values from the fresh
inspect; do not assume either is current. Classify each entry as:

- **integrate**: clean candidate, exact task/issue owner, review PASS, valid
  evidence, and an authorized merge path;
- **retain-and-reconcile**: active work or valid source evidence whose owner,
  task, worktree or scope needs rebind in the fresh epoch;
- **archive-only**: stale, superseded, experimental, dirty or historical
  work that must remain available but cannot become active state. This
  includes a historical record with a missing owner, lost runtime, unresolved
  merge conflict, unknown delivery, or broken worktree/claim edge;
- **needs_operator**: missing or conflicting hard-admission facts such as
  operator authorization, lease ownership, writer fencing, target build,
  source digest, canonical scope or rollback authority. It is not a label for
  every old record whose owner has disappeared.

No branch is deleted, reset, squashed, merged or cleaned by migration. A
candidate may enter the new active state only after its owner proves the
candidate, base, tree, review, integration and delivery edges. An old PASS or
receipt without those edges is archive evidence only. Branches and worktrees
remain in their owning project until a separate authorized cleanup records a
receipt.

## 6. Archive and old-epoch fencing

After preflight passes, the operator authorizes one project operation and the
controller **acquires one migration transaction lease before freezing legacy
writers**. The lease binds the source digest, source epoch, target build,
canonical cwd and fencing token. If lease acquisition is rejected or unknown,
no writer is frozen and no archive/reset action starts.

The controller then, in order:

1. Appends `MigrationStarted` and `WritersFreezeRequested`. Wait for explicit
   acknowledgements from each supported writer. A timeout or ambiguous
   acknowledgement is `unknown`; it does not authorize archive or retry.
2. Stops old writers through their supported lifecycle and proves there is one
   remaining writer owner. It does not use broad process kills or remove a
   socket/lock to force progress.
3. Copies source bytes into an operator-controlled immutable archive without
   changing source files. The archive includes journals/events, mailbox JSONL,
   claims, panes/runs, worktree/branch inventory, AppSDK references,
   codexapp external journal, source manifests, exact errors and the preflight
   evidence. The archive records source path, byte count, newline framing,
   digest, capture time and tool version.
4. Re-hashes the archive and verifies byte-for-byte equality with the frozen
   source. Make the archive read-only or use the platform’s immutable storage
   operation, then prove that a write attempt is rejected. If immutability or
   equality cannot be proven, stop at `needs_operator`.
5. Appends `ArchiveCreated` with the archive path, digest, source digest,
   lease token and manifest ID. A partial copy remains an incomplete archive;
   it is never treated as success.
6. Marks the source epoch `archived/blocked` and fences its command, task,
   claim, message, wake, notification and identity admissions. Old IDs cannot
   be reused in the target epoch, and old bindings cannot write even if a
   process later reconnects.

Archive creation is a preservation boundary. It does not delete the source,
close bugs, merge branches, or prove the target epoch.

## 7. Mapping and fresh epoch

The controller allocates a new globally unique `target_epoch` only after the
archive receipt. Target journal sequence numbers are allocated by the single
new writer; source line numbers and legacy sequences are evidence only.

### Direct

Use `direct` only for a complete verified prefix whose typed schema, record ID,
timestamp, project scope, owner graph, task/worktree/evidence relation,
single-writer interval and outcome transitions are all proven. The target
record receives a new target sequence and entity ID, and the manifest records
the source ID/digest, target epoch/sequence/entity and all binding references.
One missing edge moves the record out of direct mapping.

### Adapt

Use `adapt` for a typed legacy record whose meaning is recoverable but whose
old shape lacks v1 controls. Preserve the raw source in the archive and write
only the canonical v1 fact. Adaptation discards legacy role authority,
heartbeat assumptions, line-number sequencing, implicit project scope and
transport delivery claims. The manifest names each discarded field and the
reason. An adapted fact is `needs_reconciliation` until its current owner,
scope, runtime and evidence are checked.

### Reset

Use `reset` when replay cannot safely establish the current state: malformed
or ambiguous JSONL, a corrupt middle record, conflicting duplicate IDs,
multiple writers, missing owner or scope, dirty/stale candidate, broken
claim/worktree edge, lost runtime, wait cycle, unknown delivery outcome,
missing source manifest/digest, or a historical PASS without exact candidate,
artifact, environment, entrypoint and producer evidence. Keep the record in
the archive with `mapping_class=reset`, exact error and
`first_failed_boundary`; do not replay it as a success.

### Unknown

Use `unknown` when neither a deterministic adaptation nor a safe reset
decision can be established, including a transport timeout or a side effect
whose outcome cannot be queried. Preserve the raw bytes and operation ID.
Query the original operation once through the authoritative owner; if the
result remains unknown, stop for an operator decision. `unknown` can never be
marked `mapped` by a retry.

After the target epoch is allocated, append `EpochCreated` with the source
epoch, target epoch, archive digest, migration ID and fencing token. The epoch
is not admitted until identity, active facts, projections and invariants pass.

## 8. Identity, scope and runtime rebind

`agent_id` is the stable logical identity. A fresh process receives a new
`runtime_id`; the AppServer/native endpoint receives a `binding_id`; reconnect
increments `endpoint_generation`. TUI may use a verified tmux pane as wake
evidence. Desktop uses its native AppServer endpoint and never invents a tmux
session. `mcp_session_id` is query context only.

Scopes are checked independently:

- same AppServer and same cwd: same app and project scope;
- same AppServer and different cwd: same app scope, different project scope;
- different AppServers and same cwd: same project, different communication
  scope;
- different AppServers and different cwd: different app and project scopes.

Peers may communicate only within the allowed app/project scope. Master to
master collaboration uses the cross-project bug reference path and has no
subordination. A master grant comes only from a new explicit user approval;
old role fields, session IDs, pane titles, goal files and historical master
receipts do not survive the epoch boundary.

For each live runtime, verify:

```text
agent_id ↔ runtime_id ↔ binding_id ↔ endpoint_generation
                 ↘ project scope / app scope / canonical cwd
                 ↘ current task / worktree / claim
```

The controller appends `IdentityRebound` only after native initialize and
capability negotiation succeed. A dead or unavailable endpoint remains
unbound; it is not replaced by a guessed tmux or session identity.

## 9. Active bug, task and goal reconciliation

Reconciliation imports current facts, not a historical projection wholesale.
For each candidate fact, record source ID/digest, target ID, owner, project
scope, runtime/binding, worktree and evidence references, and a reconciliation
receipt.

### Bugs

Query the owning AppSDK/git-bug system first and preserve the authoritative bug
ID. Active bugs become inputs to the master’s priority queue. P0 bugs block
the project and produce an urgent latest-state notification; P1/P2 bugs use
the normal two-minute aggregate. Do not create a duplicate because a legacy
notification or mailbox row exists. A cross-project issue is registered in
the bug system before the target master is notified.

### Tasks and worktrees

Import an open task only if one real owner, one current worktree/branch, one
project scope and a recoverable evidence path are proven. A task tied to a
lost identity, dirty candidate, unresolved merge, or unknown delivery is
preserved as `archive-only` with active import blocked (or marked
`needs_reconciliation` only after a current owner explicitly takes it over).
The master may re-dispatch only after the fresh runtime accepts a new task
reservation; old task IDs are not silently reassigned. No old owner must be
repaired before the archive is accepted.

### Goals

Import a goal only after its current owner and plan file are confirmed. A
historical `subscribed` or `active` projection is not proof of a live
subscription. Only the master may own a long-horizon wakeup. Workers have no
goal subscription; worker `working → idle` produces one idempotent fact to the
master. Desktop does not register `goal subscribe`.

Each imported item stays `needs_reconciliation` until its owner and runtime
binding are proven. Archive-only items remain excluded from active state until
that reconciliation is complete. Append the reconciliation receipt before
enabling an item.
Unresolved active facts are visible and prioritized as blocked work; they are
not silently dropped.

## 10. Projection rebuild and notification semantics

Rebuild projections only from committed target-epoch journal facts after the
reducer passes replay. Mailbox JSONL is append-only raw fact history with one
record per line; each record includes event ID, time, project/app scope,
sender, recipient, entity, title, priority, state/process/result kind,
operation ID and source/archive references. Keep raw content available for an
agent that needs detail.

The notification view is a low-noise latest-state projection. For each
`(from, to, entity)` key, present only the latest title, timestamp and
priority, with a pointer to JSONL. Repeated messages do not create repeated
visible notices. `sendmessage` remains immediate. Idle, progress, delivery,
bug and worker-idle notices aggregate and send at most once per two minutes;
P0 interrupts immediately. Master idle probes are event-driven and stop after
three consecutive reminders without a `working` transition. Workers do not
receive idle wakeups.

Projection failure does not roll back a committed journal fact and does not
mark a notification delivered. Append `repair_needed`, rebuild from the
journal, and keep admission or close blocked according to the affected gate.

Verify after rebuilding:

- every visible item points to a committed target fact;
- no duplicate latest-state key is presented;
- raw JSONL count, digest and source references match the manifest;
- P0 urgency, master-idle three-reminder stop and two-minute aggregation are
  observed through the real endpoint where those features are in scope;
- no notification is mistaken for message execution, task delivery or read
  acknowledgement.

## 11. Rollback, failure and unknown-error handling

Operations are `success`, `failure`, `unknown` or `unsupported`.

- `failure`: the authoritative reducer rejected the operation. Preserve the
  exact error; a repair uses a new attempt ID and expected revision.
- `unknown`: timeout, socket close, daemon loss, ambiguous append, missing
  owner or unverified side effect. Look up the original operation by its
  idempotency key. Do not blindly resend or create a second daemon.
- `unsupported`: the native method explicitly does not exist. Use only the
  documented adapter after proving unsupported; a timeout is not unsupported.
- journal open/append/sync/replay failure poisons the writer. No projection,
  wake, admission or close follows until the first failed boundary is
  reconciled.

For rollback, first freeze the target epoch, mark it `superseded/aborted`,
revoke its bindings and stop its projections. Reconcile every fact after the
epoch boundary and write a receipt binding:

```text
source_epoch, target_epoch, expected_active_epoch,
expected_active_revision, migration lease fencing token,
command_id, operation_id, reconciliation digest
```

The reducer performs a compare-and-set of the active epoch. A changed epoch,
revision or fencing token rejects the switch. The old archive and both
journals remain immutable. Switching the pointer never revives old bindings or
grants; runtime, endpoint generation and a new user master grant must pass
again. If any external side effect or reconciliation result is unknown, keep
admission stopped and escalate to the operator.

## 12. Verify, stop and receipts

The migration owner may declare `verified` only when all applicable checks
have durable evidence:

1. exact source and immutable archive digest/count equality;
2. archive immutability and old-epoch fencing;
3. one reviewed daemon binary, one writer, one reducer and one active epoch;
4. schema-valid manifest, complete direct prefix and explicit adapt/reset/
   unknown records with errors and boundaries;
5. unique target sequences/entities and replay determinism;
6. owner graph, project/app scope and runtime/binding/generation checks;
7. active bug/task/goal reconciliation receipts;
8. rebuilt mailbox JSONL/latest-state projection and notification semantics;
9. rollback negative test and unknown-error stop test;
10. real TUI and Desktop native endpoint checks where applicable.

Each receipt names `migration_id`, project, source/target epoch, operation ID,
lease/fencing token, candidate/artifact/environment/entrypoint/producer,
source/archive/target digests, reducer revision, actor, timestamp and exact
result. A test, plan, status command, old receipt, ACK, or file name is not a
receipt for an operation it did not perform.

Stop immediately and record the exact first failed boundary when a hard
admission or active target operation fails:

- a source changes after preflight or a writer appears;
- archive equality or immutability cannot be proved;
- the migration operator, lease, target writer, target build, scope, rollback
  authority, or an active item's required binding is missing or conflicts;
- the reviewed build or native capability is unavailable;
- an append, projection, endpoint or rollback result is unknown;
- a P0 issue blocks the project or a required operator decision is absent.

For a historical record whose owner, runtime, merge relation, worktree,
delivery outcome or claim is missing, record the exact error and keep that
record archive-only with active import blocked. Do not stop preservation of
the rest of the source solely to repair that historical owner.

`needs_operator`, `reset_required` and `aborted` are incomplete states. The
next action is a new bounded inspect or an explicit operator decision, not a
fixed-interval retry and not a wakeup loop.

## 13. Per-project execution strategy

### Collab

Start from the clean v1 integration lineage; Collab is the only project that
uses this integration target. Preserve the dirty v2 root, all unmerged
worktrees and old v1 records in the archive. Acquire the migration lease, then
resolve duplicate `collab serve` writers and establish one host daemon before
any replay. The current round provides one closed worker projection and four
absent keepalive projections; it does not establish a live master or current
worker liveness. Keep old tasks and claims archive-only until live endpoints
are re-registered and a new user master grant is obtained. Do not treat the
historical low-intervention migration receipt as current global ownership.

### AppSDK

Keep the five unresolved paths and their ownership evidence in the archive;
the AppSDK owner must resolve them before any affected quality candidate or
task is admitted as active, but does not need to repair them before an
authorized preservation archive. Keep `.appsdk` quality records under AppSDK
ownership and bind active records to the AppSDK owner-selected target. Collab
may retain immutable coordination references, but it is not the AppSDK product
or release integration target. The failed
`GOAL_STATUS_SUBSCRIPTION_ID_MISSING` / reconciliation state remains a
blocked fact; it is not a goal success. Preserve active bug IDs and reconcile
their tasks only after owners and worktrees are proven. Do not use Collab
reset to delete AppSDK records.

### RouteCodex

Keep V3 production evidence separate from V4 design/refactor experiments.
Preserve the dirty root and all 378 unmerged branches until each entry is
classified. The current keepalive projection is 22 absent, four idle, one
unknown and one working; task edges remain archive-only until a current
owner/runtime is proven. Bind active records to a RouteCodex owner-selected
target. Collab may retain immutable coordination references, but it is not the
RouteCodex product or release integration target. The earlier 23-lost/two-
unknown result is a prior-round clue only and does not require historical
repair before the preservation archive. RouteCodex AppSDK records remain under
their quality owner; AppSDK `reset-governance` is a separate authorized
operation, never a Collab migration shortcut.

### codexapp

Treat codexapp as a transport bootstrap, not a legacy Collab project. An
operator must first prove the native AppServer initialize/capability handshake,
successful `commd.sock` listener, endpoint generation, project cwd and
explicitly authorized master registration if needed. Archive the 54-line
external journal before deciding whether any typed records can be adapted.
Do not import the Node mock registry, mock adapter tests, role, scheduler or
in-memory receipts. Desktop remains ineligible for goal subscription.

## 14. Cutover boundary and no-apply rule for this round

After the docs candidate is reviewed and merged, a separate operator-approved
cutover may run the following sequence for one project at a time:

```text
fresh read-only inspect
→ operator admission
→ acquire migration lease
→ freeze legacy writers
→ immutable archive
→ old-epoch fence
→ fresh epoch and reviewed daemon
→ direct/adapt/reset/unknown mapping
→ identity/runtime/scope rebind
→ active bug/task/goal reconciliation
→ reducer replay and projection rebuild
→ invariant and real endpoint verification
→ controlled resume
```

Install, daemon restart, push, main replacement, branch merge, worktree
cleanup and production TUI/Desktop replay are separate operations with
separate receipts. This runbook round performs none of them and creates no
live archive or reset. If the runtime prerequisite, one-writer proof,
immutable archive, fresh epoch, active-fact reconciliation or new master grant
is missing, leave the project frozen at its explicit blocked state and retain
the exact evidence for the next operator decision.
