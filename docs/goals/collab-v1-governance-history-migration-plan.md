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
history, running Desktop `goal subscribe`, or production cutover before M8b
approval.

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

## Milestones and worker contracts

All code work starts from the current clean v1 integration branch in a new
`playground/` worktree. Workers must report exact base/candidate/tree, allowed
files, tests and remaining boundaries. A failed candidate is preserved and a
new repair worktree is based on the explicit latest integration commit.

### M1 — journal/replay truth

Owner: one GCM worker. Allowed paths are the existing server reducer, journal
and focused contract tests. It must make append/sync/replay return typed
outcomes, poison on ambiguous writes, preserve command idempotency and keep
legacy missing outcomes as unknown. It must not change notification policy,
adapters, skills or daemon bootstrap. Candidate requires focused tests, all
targets, check, format and diff gates, then independent Astra review.

### M2–M5 — runtime foundation

Run sequentially where ownership depends on M1; independent design/test work
may run concurrently in separate worktrees:

- M2 owns the host singleton lock, global reducer, durable bindings and
  projection seams.
- M3 owns the native AppServer adapter: initialize/capability evidence,
  accepted versus delivered/executed/replied/read, cursor watermark,
  timeout/unknown lookup, stale generation and P0 stop observation.
- M4 owns notification accumulation and project JSONL: direct messages are
  immediate; idle/progress/delivery/bug/worker-idle updates are merged at most
  every two minutes; one latest state is presented per entity; only the daemon
  wakes a master; a worker `working→idle` transition is one idempotent episode;
  three unchanged master-idle reminders stop.
- M5 owns bug/worktree/Loop and skill contracts: active bugs enter the master
  backlog by priority, P0 blocks the project, every fix has an independent
  worktree/branch/review/merge/cleanup receipt, and each Loop has Trigger,
  Work, Gate, State and Stop.

Each milestone has a red test before the smallest implementation and an
independent review on the exact candidate. No milestone may claim live
TUI/Desktop capability from mock tests.

### M6 — migration adapters and reset

Owner: one GCM worker after M1–M5 seams are available. Allowed paths are the
migration manifest schema, server migration module, project adapters and
focused migration tests. It must:

1. implement read-only inspect and classification for the four audited
   projects;
2. write an idempotent manifest keyed by source project/record/digest/epoch;
3. preserve raw archive references and AppSDK record digests;
4. direct-map only complete records, adapt only typed legacy fields, and mark
   all other records reset/unknown;
5. implement archive → new epoch → re-register/regrant → confirmed active
   import → projection rebuild; and
6. prove rollback by epoch pointer without deleting an archive or starting a
   second writer.

It must not execute migration against the four live roots during development.
The first run uses copied read-only fixtures with corruption, duplicate,
dirty-candidate, missing-owner, changed-cwd and unknown-outcome cases.

### M7 — independent milestone review

Astra reviews the plan and exact M6 candidate for source ownership, archive
immutability, manifest/idempotency, direct/adapt/reset classification,
unknown/error/poison handling, identity rebind, AppSDK/RouteCodex boundaries,
and worktree/merge gates. Any P0/P1 or owner ambiguity blocks integration.

### M8a — rehearsal

The integration owner fast-forwards the reviewed candidates into a clean v1
integration worktree, runs the full replay/negative matrix and a no-write
rehearsal using copied fixtures. It records source and target digests, counts,
epoch, lock owner, projections and rollback receipt. The old project roots and
their daemons remain untouched.

### M8b — production cutover (separate approval)

Only after a separate explicit release approval: freeze each source, archive,
install the exact reviewed build, restart one singleton daemon, rebind TUI and
Desktop endpoints, and run real bidirectional send/read/reply, continuation,
P0 stop, notification batching, bug backlog and Loop smoke tests. Any failure,
unknown or P0/P1 halts the sequence. Main replacement, push, global install,
daemon restart and cleanup are separate receipts.

## Project-specific runbooks

### Collab

Start from the v1 integration lineage, not the dirty v2 branch. Inspect the
72-line journal and 442-line event projection observed on 2026-09-09, then
replay the complete source prefix. The existing `v1-low-intervention` verified
record is evidence of a prior local migration only; it does not prove global
daemon ownership. Preserve approximately 98 playground directories until each
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
Port only `app-server-adapter.js`/WebSocket transport behavior behind the M3
adapter seam. Require native initialize/capability evidence and a real TUI or
Desktop endpoint before registration. The Mock adapter and the two Node tests
are fixtures, not migration evidence.

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
- `operator_required`: the exact unresolved source fact and next decision are
  recorded, with admission frozen.

The following do not satisfy completion: a file named `migration`, a historical
PASS, an empty PID, a mock test, a goal projection, a mailbox ACK, a worker
message, or a successful `accepted` queue response without native execution
and read evidence.
