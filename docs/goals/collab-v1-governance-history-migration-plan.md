# Goal: migrate existing project governance history to Collab v1

## Objective

Move existing governance history into one global Collab v1 daemon without
losing raw evidence or inventing current state. Where source history is not
provably replayable, archive it and rebuild a new epoch from confirmed active
facts. Keep AppSDK quality records in their owning projects and connect them by
digest/reference.

## Scope and non-goals

In scope: Collab `.agent-collab` history, AppSDK collaboration/evidence
references, RouteCodex V3/V4 governance boundaries, and codexapp's native
transport seam. Out of scope: repairing the dirty AppSDK or RouteCodex roots,
rewriting RouteCodex V4, importing codexapp's Node registry, deleting old
history, running Desktop `goal subscribe`, or production cutover before a
separate release approval.

## Loop contract

Each project is one independently auditable Loop:

| Component | Contract |
|---|---|
| Trigger | A user-approved migration goal starts one project run. A refreshed read-only inspect must show a candidate source and no unaccounted writer. |
| Work | Discover canonical cwd/version/branch/tree; hash journal/mailbox/claims/evidence; classify each record; acquire one lease; freeze writers; archive; replay direct records; adapt legacy records; or create a reset epoch and rebind identities. |
| Gate | Validate the manifest schema, source/archive digests, replay prefix, record relations, owner/scope/binding/generation, AppSDK references, one writer and negative cases for corruption/duplicate/unknown. |
| State | Journal `MigrationStarted`, per-record mapping, `ArchiveCreated`, `EpochCreated`, `IdentityRebound`, `MigrationVerified` or exact blocked error. Rebuild project JSONL and latest-state notifications only from committed facts. |
| Stop | Stop at `verified`, `needs_operator`, `reset_required` or `aborted`. A timeout, 502, ambiguous write or unknown owner is a stop condition, not permission to retry blindly. |

The execution order is `Discover → Hand off → Verify → Persist → Schedule`.
The dispatch intent and migration lease are durable before any writer or
daemon side effect.

## Runtime prerequisite and ownership

This goal owns migration of existing governance history. It does not redefine
the runtime foundation that performs journal writes, identity checks, native
transport, notification projection or task lifecycle. Those contracts have one
source of truth in
[`docs/design/collab-v1-refactor-architecture-20260909.md`](../design/collab-v1-refactor-architecture-20260909.md)
and its R1–R5 implementation rounds. Migration starts only after the required
rounds have exact reviewed candidate/tree receipts on the v1 integration line.
If a receipt is missing, conflicting or not reproducible, this goal remains
blocked at `runtime_prerequisite`; it must not reimplement that round inside a
migration adapter.

The latest read-only site evidence is
[`docs/evidence/governance-history-live-refresh-r3-20260909.md`](../evidence/governance-history-live-refresh-r3-20260909.md).
Its classifications supersede older inventory snapshots. At that refresh,
Collab, AppSDK and RouteCodex were `reset_required`, while codexapp was
`needs_operator`; no project was admitted for live active replay.

## Migration stages and worker contracts

All implementation work starts from the current clean v1 integration branch in
a new `playground/` worktree. Workers report exact base/candidate/tree, allowed
files, tests and remaining boundaries. A failed candidate is preserved and a
new repair worktree is based on the explicit latest integration commit.

### S1 — inspect, classify and snapshot

One migration worker owns read-only inventory and classification for Collab,
AppSDK, RouteCodex and codexapp. It records the exact checkout/data roots,
source identity, journal/mailbox/socket digests, writer ownership and unknowns
in the inventory evidence. It must produce a schema-valid manifest draft with
top-level `mapping_status=planned`; every inspected record carries its
`mapping_class` (`direct`, `adapt`, `reset` or `unknown`) and remains
`mapping_status=planned` until S2 creates a target mapping. Target sequence,
archive reference and final `mapped`/`reset_required` evidence are not invented
in S1. A reset classification still records its exact error and first failed
boundary, while its raw archive reference remains null until S2. S1 must not
mutate any live root or start a daemon. The codexapp external journal and
socket are mandatory inventory inputs even though the source directory has no
`.agent-collab`.

### S2 — archive, map and rebuild

After S1 passes and the runtime prerequisite is available, one migration worker
may acquire the lease and freeze legacy writers. It creates an immutable
archive, replays only a complete direct prefix, adapts typed legacy fields,
and creates a fresh target epoch for reset records. It re-registers stable
identities, re-grants master only from a new user approval, imports only
confirmed active bugs/tasks/goal state, and rebuilds JSONL/latest-state
projections from committed facts. The worker must preserve exact errors and
first failed boundaries and must not execute against the four live roots during
development; copied fixtures cover corruption, duplicate, missing-owner,
changed-cwd, dirty-candidate and unknown-outcome cases.

Rollback is allowed only before the new epoch has admission or external side
effects. Otherwise the worker freezes and supersedes the new epoch, revokes
its bindings, fences the writer, reconciles every post-boundary fact and
commits a reconciliation receipt before any epoch pointer changes. The durable
rollback intent and receipt must bind `source_epoch`, `target_epoch`,
`expected_active_epoch`, `expected_active_revision`, the migration-lease
fencing token, `command_id` and `operation_id`. The reducer performs an atomic
compare-and-set of the active epoch; a changed epoch/revision or fencing token
rejects the transition. Switching the pointer never revives historical
bindings or grants: live runtime, endpoint generation and user master grant
must be revalidated before admission resumes. An unknown side effect,
reconciliation gap or failed CAS keeps admission stopped.

### S3 — independent review and no-write rehearsal

Astra independently reviews the exact migration candidate for schema/state
constraints, archive immutability, idempotency, direct/adapt/reset mapping,
unknown/error handling, rollback fencing, identity rebind and AppSDK/
RouteCodex ownership. Any P0/P1 or owner ambiguity blocks integration. The
integration owner then runs the replay and negative matrix with copied
fixtures, records digests/counts/epoch/lock/projections/rollback receipt, and
verifies that no live project root changed.

### S4 — production cutover (separate approval)

Only after a separate release approval may the owner freeze each live source,
archive it, install the exact reviewed build, restart the single daemon,
rebind TUI/Desktop endpoints, run real bidirectional transport and notification
/bug/Loop smoke tests, and then record push and cleanup receipts. Any failure,
unknown or P0/P1 halts the sequence. Main replacement, push, install, restart,
replay and cleanup remain separate evidence facts.

## Project-specific runbooks

### Collab

Start from the v1 integration lineage, not the dirty v2 branch. Inspect the
72-line journal and the latest mutable event projection observed in the r3
refresh, then classify the source before deciding whether any prefix is safe
to replay. The r3 refresh found 472 event lines, 146 worktrees and 130
unmerged branches, plus multiple host `collab serve` processes and no live
master, so the current disposition is `reset_required`; no direct replay is
admitted from that snapshot. The existing `v1-low-intervention` verified
record is evidence of a prior local migration only; it does not prove global
daemon ownership. Preserve every playground directory until each
worktree/branch/claim is classified. Empty PID/lock files yield `unknown`, not
"daemon stopped".

### AppSDK

Do not touch the dirty root or resolve its conflicts as part of Collab
migration. The `.appsdk-control/long-task-goal.json` state is a local control
projection with `active=false`, `recovery_required` and `remote_state=unknown`;
it is not a successful goal subscription. Wait for the AppSDK owner to produce
a clean, hash-bound quality record. Import references to contracts, reviews,
evidence and mainline receipts only after their owner confirms them.

### RouteCodex

Treat RouteCodex's `.appsdk` zones and `appsdk-0.1.5-to-0.1.6` record as the
quality owner's source. Keep V3 production evidence distinct from V4 design
experiments. The large `.agent-collab` journal/events and empty PID/lock files
require a fresh inspect and writer check. Never invoke the AppSDK
`reset-governance --discard-legacy` command as a Collab shortcut; if AppSDK
reset is later authorized, archive and record it under its own owner and
epoch.

### codexapp

Create a source snapshot and digest because there is no Git or Collab history.
Port only `app-server-adapter.js`/WebSocket transport behavior behind the
native adapter seam defined by the canonical R3 round. Require native
initialize/capability evidence and a real TUI or Desktop endpoint before
registration. The Mock adapter and the two Node tests are fixtures, not
migration evidence.

## Failure handling and operator escalation

The migration controller must persist the first failed boundary and exact
error. `failure` can be repaired with a new attempt; `unknown` requires an
operation lookup or operator decision; `unsupported` allows only the documented
method mapping; malformed/ambiguous journal poisons the writer. A dirty source,
conflicting digest, missing owner or stale generation stops the project and
does not wake a worker or master repeatedly. Notifications summarize the
blocked entity and next action; raw details remain in project JSONL.

If three bounded checks find the same external blocker (for example the GCM
model endpoint returning `502`), mark the worker attempt failed with its exact
receipt and continue independent work. Do not start a duplicate migration or
claim a candidate that has no review.

## Completion evidence

The goal is complete only when every selected project has one of:

- `verified`: target epoch, manifest, source/archive digest, mappings, owner
  graph, identity rebind and projection rebuild all pass;
- `archived/reset`: immutable archive and new epoch pass, imported active facts
  are reconciled, and unresolved records are explicitly blocked; or
- an explicitly approved scope exclusion recorded with its owner, reason and
  approval evidence.

`needs_operator`, `reset_required` and `aborted` mean that an attempt stopped;
they are blocked/incomplete states and do not satisfy the goal. A project is
complete after reset only when its immutable archive, fresh epoch, confirmed
active import and reconciliation receipt have all passed. `needs_operator` is
therefore a handoff to the user, not a success status.

The following do not satisfy completion: a file named `migration`, a historical
PASS, an empty PID, a mock test, a goal projection, a mailbox ACK, a worker
message, or a successful `accepted` queue response without native execution
and read evidence.
