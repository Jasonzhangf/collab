# Governance history migration to the Collab v1 global daemon

Status: implementation contract for the v1 refactor. The inventory below is a
read-only snapshot taken on 2026-09-09. Counts, PIDs, branches and live
bindings are observations with an expiry time; they are not durable claims that
the migration has already run.

## Decision

One host has one writable Collab daemon, one reducer and one journal writer.
Each project registers a canonical project root with that daemon. AppSDK owns
quality and release evidence; Collab owns coordination facts. The migration
imports references to AppSDK records, never a second copy of AppSDK's quality
state. Mailbox JSONL and notification views are rebuildable projections of the
Collab journal.

The migration is source-preserving and idempotent:

```text
inspect → classify → plan/lease → freeze writers → snapshot
→ archive source → replay direct records / adapt legacy records
→ rebind live identities → verify counts, digests and owners → resume
```

If the source cannot be proved safe, the run stops at `needs_operator` or
`reset_required`. It never silently drops a record, invents an owner, starts a
second daemon or converts an unknown result into success.

## Read-only field inventory

| Project | Observed source and state | Migration decision |
|---|---|---|
| Collab | `/Volumes/extension/code/collab` contains the v1 integration branch `codex/v1-collab-refactor-main-20260909` at `ac54d09dcab14ec35d5a5291207af9edda00fcf7`; its `.agent-collab/server/journal.jsonl` had 72 lines and `events.jsonl` 442 lines at inspection. The journal includes a prior `MigrationUpdated` record with `from_version=v1-legacy`, `to_version=v1-low-intervention`, `phase=verified`. The v2 branch `codex/v2-cordis-architecture` is dirty and ahead of its remote with untracked governance/evidence files. The root has about 98 playground directories. `server.pid` and `daemon.lock` were empty, so current liveness and single-writer ownership are unknown. | The complete, hash-valid v1 journal prefix can be migrated directly after replay validation. Legacy role/heartbeat/continuation fields are adapted and discarded. v2 untracked records and playgrounds are archived as source evidence, not imported as active state. Any duplicate writer, malformed record, unresolved wait/owner or dirty candidate is `reset_required`. |
| AppSDK | `/Users/fanzhang/Documents/github/appsdk` is on `chore/project-memory-snapshot` at `2d14efed9d7f6454d119cb7a1aea24a384e966e0` with unresolved `UU`/`DU` paths in maps, migration contract, integration docs, Rust CLI and smoke tests. The root has `.appsdk-control/long-task-goal.json` with `active=false`, `desired=recovery_required`, `remote_state=unknown`, `GOAL_STATUS_SUBSCRIPTION_ID_MISSING` and `GOAL_SUBJECT_RECONCILIATION_FAILED`; it has no `.appsdk/` directory in this checkout. `.agent-collab` contains 98 run files, 541 mailbox files, 999 review files, 3 claims and 2 handoffs. | Do not migrate this dirty root as an active quality candidate. Preserve the AppSDK contract/migration files and collaboration records as references. Import only verified record references after the AppSDK owner resolves conflicts. A Collab reset must not delete or rewrite AppSDK source, contracts, Active or Protected history. |
| RouteCodex | `/Users/fanzhang/Documents/github/routecodex` is on `codex/root-dirty-recovery-0908` at `d876adea1d5c57a73cf643f5c8d89b56bd3d42c4` with extensive staged and unstaged changes across `.appsdk`, V3 and UI files. `.agent-collab/server/journal.jsonl` had 39,162 lines, `events.jsonl` 26,086 and `log.txt` 70,212; `server.pid` and `daemon.lock` were empty. The `.appsdk` project declares protected/active/generated zones and an `appsdk-0.1.5-to-0.1.6` migration record with four map digests. Project memory records V3 as the production baseline and V4 as an architecture/refactor surface; this distinction is not inferred from filenames. | Keep AppSDK quality records under RouteCodex/AppSDK ownership and import only immutable references. Migrate Collab coordination facts only after journal prefix/count/hash and owner checks. V3 active evidence can be referenced; V4 experiments and dirty worktrees are archive/adapt inputs, never active runtime state. The existing `reset-governance --discard-legacy` command is a separate AppSDK operation and is not a substitute for Collab migration. |
| codexapp | `/Users/fanzhang/Documents/github/codexapp` has no `.git`, no `.agent-collab`, and only a Node transport prototype (`src/bridge.js`, `src/app-server-adapter.js`, `src/ws-jsonrpc.js`, JSONL helper and two test files). The tests use `MockAppServerAdapter`; the six-test run observed earlier proves an in-memory bridge only. A later `npm test -- --runInBand` invocation failed at Node argument parsing (`bad option: --runInBand`), so that command is not test evidence. | Treat codexapp as a new transport endpoint bootstrap. Snapshot and hash the native adapter seam, then register it with Collab after capability negotiation. Do not import its in-memory registry, role, message, scheduler or mock receipts. |

The inventory intentionally records `unknown` where an empty PID or missing
directory cannot prove absence. A later `inspect` must refresh these values
before taking a migration lease.

## Manifest and idempotency

Each project migration writes a manifest validated by
[`migration-v1-history-manifest.schema.json`](./migration-v1-history-manifest.schema.json).
The manifest is itself a journal fact, not an editable control file. Required
identity fields are:

```text
migration_id
source_project_id
canonical_project_cwd
source_repo / source_branch / source_head / source_tree
source_schema_version / target_schema_version
source_snapshot_digest
target_epoch
archive_ref / archive_digest
mapping_status
```

Each source record has its own `source_record_id`, type and digest, plus the
target epoch/sequence, target entity, AppSDK reference (when applicable),
identity binding fields, mapping class/status, raw archive reference and the
first failed boundary/error. The idempotency key is:

```text
(source_project_id, source_record_id, source_record_digest, target_epoch)
```

Repeating inspect or apply with that key returns the existing mapping and
outcome. It cannot create another task, bug, message, claim, wake or
notification. A changed digest is a new source record or a conflict; it is
never overwritten in place.

`target_epoch` is a new immutable epoch for every reset or schema cutover.
`target_sequence` is allocated by the global journal writer. Old source
sequence numbers are evidence only and are not reused as target ordering.

## Classification

### Direct migration

A record is `direct/mapped` only when all of these checks pass:

- the schema is known and the complete JSONL record (including newline and
  digest) replays without error;
- record ID, timestamp, source project and canonical cwd are present;
- task, claim, worktree, branch, base commit and evidence references form a
  closed relation;
- the commit/tree/hash is still the one named by the record;
- exactly one writer and one project scope are proven for the source interval;
- the state can be mapped without guessing `accepted`, `delivered`,
  `executed`, `replied`, `read` or `consumed`.

The target preserves the source digest and records a source reference. It does
not copy a historical PASS into a new acceptance gate.

### Adapted migration

An `adapt/needs_reconciliation` record is source-preserved but cannot retain
legacy control semantics. Examples are a serialized role field, old
heartbeat/continuation fields, a per-message mailbox file, a missing
`runtime_id`/`binding_id`/`endpoint_generation`, or a valid worktree that must
be rebound to a new runtime. The adapter creates typed current identity and
binding fields, discards legacy role authority, and keeps the task blocked until
the owner and current evidence are rechecked. A legacy record without an
outcome remains `unknown`; it is never synthesized as `Success`.

### Reset and rebuild

`reset_required` is selected when a safe replay cannot be proven:

- malformed JSONL, a corrupt middle record, or a partial/ambiguous tail;
- duplicate command/record IDs with conflicting outcomes;
- multiple writers or no proof of a single daemon owner;
- a task with no real owner, a broken claim/worktree/branch/evidence edge, or a
  wait cycle/missing deadline/resume path;
- a dirty root or stale branch presented as a delivered candidate;
- changed canonical cwd or project-scope collision;
- a dead runtime that cannot be live-rebound;
- a historical PASS/receipt with no canonical candidate, artifact,
  environment, entrypoint and producer evidence;
- a state that cannot distinguish accepted/queued/delivered/executed/replied/read;
- a missing or unverifiable source manifest/digest.

## Reset without data loss

Reset is a controlled new epoch, not deletion:

1. Authenticate an operator and acquire the migration transaction lease. Freeze
   new mutations and stop every old writer through its supported lifecycle.
2. Create an immutable archive of the source journal, events, mailbox, claims,
   worktree/branch inventory, migration record, source hashes and exact error.
   Record the archive digest and path in the manifest. Never hand-edit the
   source files.
3. Mark the old epoch `archived/blocked` and poison admission for it. Do not
   reuse its command, task, message, bug, claim or wake IDs.
4. Generate a fresh `target_epoch` and start the global daemon with one writer.
   Replay only the verified prefix required to explain the archive; ambiguous
   records remain archive-only and visible as `unknown`.
5. Re-register every live agent as a stable `agent_id` with a new
   `runtime_id`, `binding_id` and `endpoint_generation`. A user must explicitly
   grant master capability again. Session IDs, pane titles and process names
   are not authorization.
6. Import only user-confirmed active bugs, open tasks with a proven owner and
   the current goal. Mark every imported item `needs_reconciliation` until its
   worktree, scope and evidence are checked. Do not import old notification
   delivery as a new wake.
7. Rebuild mailbox JSONL and latest-state notification projections from the new
   journal. Emit one current summary per `(from_agent_id, to_agent_id,
   entity_key)`; retain all raw lines in the archive and project JSONL.
8. Verify the new epoch, counts, source/archive digests, owner bindings,
   active-bug priority order, P0 stop policy and one-writer lock. Only then
   resume admission.

Rollback changes the active epoch pointer to the previous verified epoch. It
does not delete the archive, rewrite the journal or start a second writable
daemon. If rollback itself is unknown, stop admission and request an operator
decision.

## Error and retry contract

Every operation has one of four durable outcomes:

```text
success | failure | unknown | unsupported
```

- `failure` means the native or reducer explicitly rejected the operation. A
  repair may create a new attempt with a new attempt ID.
- `unknown` means timeout, socket close, daemon loss, ambiguous write, missing
  owner or any result whose side effect cannot be established. Query the
  original operation or stop for an operator decision; do not blindly resend.
- `unsupported` means the native method is explicitly absent. It permits a
  documented adapter choice only when the first method is proven unsupported;
  a timeout or transport error never authorizes fallback.
- Journal open/append/sync/serialization/replay failure poisons the writer.
  No state mutation, projection update, wake or admission follows the failed
  append. A partial write is an ambiguous tail and blocks subsequent writes.
- Projection or notification failure leaves the committed journal fact intact
  and records `repair_needed`; it cannot become an empty backlog or successful
  close.
- Stale runtime generation, wrong scope, wrong turn or unauthorized consumer
  is an explicit failure with the typed error chain preserved.

Retries are bounded by operation ID and expected revision. The same command ID
returns its original outcome; a retry after `unknown` first performs operation
lookup. Timer ticks with no accumulator change do not append a record.

## Runtime identity and binding after migration

`agent_id` is the stable logical peer. `runtime_id` identifies one process;
`binding_id` binds that runtime to an AppServer/native thread; and
`endpoint_generation` fences reconnects. Compaction retains the agent and
binding while advancing epoch/watermark. Fork creates a new runtime and
binding, defaulting to peer. Reconnect increments generation and rejects old
generation mutations. TUI may use tmux as wake evidence; Desktop uses its
AppServer endpoint and never invents a tmux identity. `mcp_session_id` is query
context only.

## Project execution order

Migration itself is a finite Loop for every project:

```text
Trigger: user-approved migration goal and a healthy candidate daemon
Work: inspect → classify → snapshot/archive → replay or reset → rebind
Gate: manifest schema, source digest, journal replay, owner graph, one writer,
      scope/binding checks, AppSDK reference checks and negative cases
State: append migration facts, manifest mappings, archive receipt and next step
Stop: verified target epoch, or frozen needs_operator/reset_required; never a
      timer-only retry and never a second daemon
```

The implementation order on the v1 integration branch is:

1. M1 journal writer/replay fail-closed and typed outcomes.
2. M2 singleton daemon, durable binding and projection seams.
3. M3 native TUI/Desktop adapter and cursor/operation lookup.
4. M4 notification/mailbox projector and master-only wake policy.
5. M5 bug, worktree and Loop contracts.
6. M6 migration manifest, per-project adapters and reset archive/rebuild.
7. M7 isolated replay/negative tests and Astra milestone review.
8. M8a candidate integration and cutover rehearsal in a clean worktree.
9. M8b, only after explicit release approval: replace main, push, install,
   restart the singleton daemon, rebind endpoints, run real TUI/Desktop replay,
   verify notifications/bugs/Loops, and clean worktrees with receipts.

Every implementation round uses a clean `playground/` worktree from the
current integration branch, a single owner, independent review and exact
candidate/tree evidence. No worker edits dirty `main`, and no migration step
cleans another project's worktree.

## Evidence still required

This document does not claim that migration or cutover has run. The following
remain live gates: current daemon PID/socket ownership, complete source hashes,
journal corruption classification, per-record owner mapping, real AppServer
capability negotiation, cursor continuation, P0 stop observation, two-minute
notification batching, reset archive receipt and post-restart replay. The GCM
workers that were supposed to produce M1 and the detailed audit were interrupted
by `unexpected status 502 Bad Gateway: network error, url:
http://127.0.0.1:4444/v1/responses`; their partial output is retained as a
blocker, not a PASS.
