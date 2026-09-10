# Governance history migration to the Collab v1 global daemon

Status: migration-specific contract. Runtime ownership and implementation
milestones remain in the canonical v1 refactor design. The inventory below is a
historical read-only snapshot captured at `2026-09-09T16:12:32Z`; it is retained
for provenance only. The current read-only refresh is
[`docs/evidence/governance-history-live-refresh-r3-20260909.md`](./evidence/governance-history-live-refresh-r3-20260909.md)
and supersedes the snapshot's migration classifications. The older command
transcript and digests are in
[`docs/evidence/governance-history-inventory-20260909.md`](./evidence/governance-history-inventory-20260909.md).
Counts, PIDs, branches and live bindings are observations with an expiry time;
they are not durable claims that the migration has already run.

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
| Collab | `/Volumes/extension/code/collab` contains the v1 integration branch `codex/v1-collab-refactor-main-20260909` at `ac54d09dcab14ec35d5a5291207af9edda00fcf7`; its `.agent-collab/server/journal.jsonl` had 72 lines (digest `c47e0f95…1fffc83`) and `events.jsonl` 442 lines (digest `08b65142…7999b1f`) at the snapshot. The journal includes a prior `MigrationUpdated` record with `from_version=v1-legacy`, `to_version=v1-low-intervention`, `phase=verified`. The v2 branch `codex/v2-cordis-architecture` is dirty and ahead of its remote with untracked governance/evidence files. The root has 98 playground directories. `server.pid` and `daemon.lock` were empty, so current liveness and single-writer ownership are unknown. | The complete, hash-valid v1 journal prefix can be migrated directly after replay validation. Legacy role/heartbeat/continuation fields are adapted and discarded. v2 untracked records and playgrounds are archived as source evidence, not imported as active state. Any duplicate writer, malformed record, unresolved wait/owner or dirty candidate is `reset_required`. |
| AppSDK | `/Users/fanzhang/Documents/github/appsdk` is on `chore/project-memory-snapshot` at `2d14efed9d7f6454d119cb7a1aea24a384e966e0` with unresolved `UU`/`DU` paths in maps, migration contract, integration docs, Rust CLI and smoke tests. The root has `.appsdk-control/long-task-goal.json` with `active=false`, `desired=recovery_required`, `remote_state=unknown`, `GOAL_STATUS_SUBSCRIPTION_ID_MISSING` and `GOAL_SUBJECT_RECONCILIATION_FAILED`; it has no `.appsdk/` directory in this checkout. `.agent-collab` contains 98 run files, 541 mailbox files, 999 review files, 3 claims and 2 handoffs. | Do not migrate this dirty root as an active quality candidate. Preserve the AppSDK contract/migration files and collaboration records as references. Import only verified record references after the AppSDK owner resolves conflicts. A Collab reset must not delete or rewrite AppSDK source, contracts, Active or Protected history. |
| RouteCodex | `/Users/fanzhang/Documents/github/routecodex` is on `codex/root-dirty-recovery-0908` at `d876adea1d5c57a73cf643f5c8d89b56bd3d42c4` with extensive staged and unstaged changes across `.appsdk`, V3 and UI files. `.agent-collab/server/journal.jsonl` had 39,190 lines (digest `905e96be…108aca`), `events.jsonl` 26,086 (digest `00a35bf5…8edd6`) and `log.txt` 70,558 at the snapshot; `server.pid` and `daemon.lock` were empty. The `.appsdk` project declares protected/active/generated zones and an `appsdk-0.1.5-to-0.1.6` migration record with four map digests. Project memory records V3 as the production baseline and V4 as an architecture/refactor surface; this distinction is not inferred from filenames. | Keep AppSDK quality records under RouteCodex/AppSDK ownership and import only immutable references. Migrate Collab coordination facts only after journal prefix/count/hash and owner checks. V3 active evidence can be referenced; V4 experiments and dirty worktrees are archive/adapt inputs, never active runtime state. The existing `reset-governance --discard-legacy` command is a separate AppSDK operation and is not a substitute for Collab migration. |
| codexapp | `/Users/fanzhang/Documents/github/codexapp` has no `.git`, no `.agent-collab`, and only a Node transport prototype (`src/bridge.js`, `src/app-server-adapter.js`, `src/ws-jsonrpc.js`, JSONL helper and two test files). Its external runtime state is `/Users/fanzhang/.codex-communication/journal.jsonl` (54 lines, digest `c3d995d8…f485a4`) and `/Users/fanzhang/.codex-communication/sockets/commd.sock` (socket exists; writer ownership was not proven). The tests use `MockAppServerAdapter`; the six-test run observed earlier proves an in-memory bridge only. A later `npm test -- --runInBand` invocation failed at Node argument parsing (`bad option: --runInBand`), so that command is not test evidence. | Treat codexapp as a new transport endpoint bootstrap. Inspect and archive the external journal/socket before registration. Snapshot and hash the native adapter seam, then register it with Collab after capability negotiation. Do not import its in-memory registry, role, message, scheduler or mock receipts. |

The inventory intentionally records `unknown` where an empty PID or missing
directory cannot prove absence. A later `inspect` must refresh these values
before taking a migration lease.

## Current refresh supersession

The r3 refresh observed mutable journal/event drift, multiple `collab serve`
processes, lost or unknown workers, dirty roots and missing runtime bindings.
Its current disposition is `reset_required` for Collab, AppSDK and RouteCodex,
and `needs_operator` for codexapp. No source is currently admitted for direct
active replay. The historical table above must not be used to skip the fresh
inspect, writer freeze, immutable archive, or new-epoch reset gates.

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
source_epoch
target_epoch
project_admission
owner_authority
blocker_code / first_failed_boundary
archive_ref / archive_digest
mapping_status
```

Each source record has its own `source_record_id`, type and digest, plus the
target epoch/sequence, target entity, AppSDK reference (when applicable),
identity binding fields, mapping class/status, source disposition, owner
authority, blocker code, raw archive reference and the first failed
boundary/error. The idempotency key is:

```text
(source_project_id, source_record_id, source_record_digest, target_epoch)
```

Repeating inspect or apply with that key returns the existing mapping and
outcome. It cannot create another task, bug, message, claim, wake or
notification. A changed digest is a new source record or a conflict; it is
never overwritten in place.

`project_admission` and `mapping_status` are deliberately separate. The
admission is the project-level decision (`verified`, `reset_required`,
`needs_operator` or `aborted`) and controls whether the target may become
active. `mapping_status` is the transaction lifecycle (`planned`, `running`,
`verified`, and so on). A project can therefore be `reset_required` while its
inspection manifest is still `planned` and its source bytes remain untouched.

`mapping_class` is the classifier's backward-compatible observation
(`direct`, `adapt`, `reset`, `unknown`). `source_disposition` is the migration
controller's action boundary: `direct_replay`, `adapt_reconcile`,
`archive_only` or `rebuild_required`. In particular, `unknown` records are
`archive_only` until an operator supplies evidence; a `reset` classification is
`rebuild_required`, not permission to delete the old source. These dimensions
are intentionally not a one-to-one enum: a structurally direct record may
still need `adapt_reconcile` when its runtime, scope or identity binding must
be re-established, and a record may be retained as `archive_only` when a
project admission gate prevents active replay. Conversely, `unknown` always
forces `archive_only`, while `reset` always forces `rebuild_required`; neither
may become an active mapped record without a new verified observation.

`source_epoch` is nullable because legacy projects may have no trustworthy
epoch. A null source epoch forces a fresh target epoch for active state. The
manifest and every record carry `owner_authority`, `blocker_code` and
`first_failed_boundary` so a reset or archive decision remains attributable
and replay cannot silently convert a missing owner or unknown outcome into a
new task, grant, message or notification.

For a `mapped` record, `target_sequence` and `target_entity_id` are mandatory,
and `target_epoch` must equal the manifest `target_epoch`. The schema checks
presence; the reducer checks equality, uniqueness and monotonic ordering. An
`unknown` record can never be marked `mapped`. A `verified` manifest must carry
an explicit non-empty source identity and immutable archive reference, even
when the source is a non-Git path such as codexapp. Source/target identity
fields are required to be present in every manifest; a non-Git source uses an
explicit `null` branch/head/tree rather than omitting those fields.

An inspection manifest uses top-level `mapping_status=planned`. Its records
retain their classification in `mapping_class` and use `mapping_status=planned`
until a target sequence or archive is actually committed. A `reset` record in
this phase carries its exact error and first failed boundary but may leave
`raw_archive_ref` null. The schema requires the archive reference and final
error evidence only once that record leaves the planned phase; a top-level
`reset_required` manifest likewise requires a real archive digest. This keeps
classification separate from a completed reset and prevents a worker from
inventing a future path, digest or target sequence.

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

Rollback is a fenced transition. First freeze the new epoch, mark it
`superseded/aborted`, revoke its active bindings and stop its projections, then
reconcile any facts already appended after the epoch boundary. The durable
rollback intent and reconciliation receipt bind `source_epoch`, `target_epoch`,
`expected_active_epoch`, `expected_active_revision`, the migration-lease
fencing token, `command_id` and `operation_id`. The reducer atomically compares
those values before committing the active-epoch pointer; a changed epoch,
revision or fencing token rejects the transition. Only after that receipt and
CAS succeed may the pointer switch to the previous verified epoch. Pointer
switching never revives its historical bindings or grants: the target live
runtime, endpoint generation and user master grant must be revalidated before
admission resumes. The archive and both journals remain immutable; no second
writer is started. If the new epoch has an unknown side effect or the
reconciliation is incomplete, keep admission stopped and request an operator
decision instead of switching pointers.

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

## Runtime prerequisite and ownership

This document owns only governance-history migration: inspect, classify,
snapshot/archive, direct/adapt/reset mapping, reconciliation, rollback fencing
and cutover evidence. It does not redefine the runtime foundation. The unique
runtime contract is
[`docs/design/collab-v1-refactor-architecture-20260909.md`](./design/collab-v1-refactor-architecture-20260909.md),
which owns R1 identity/scope, R2 journal/reducer and daemon, R3 native
adapters, R4 notification projection, and R5 bug/worktree/Loop integration.
The migration controller may start only after the exact reviewed candidate and
tree receipts for the required runtime rounds are available. A missing,
conflicting or non-reproducible receipt leaves migration admission stopped; it
does not create a second implementation of those rounds.

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

The migration-specific stages are:

1. **Inspect and classify.** Refresh each source checkout, data root, writer
   ownership, journal prefix and digest. Classify every record as
   `direct/mapped`, `adapt/needs_reconciliation`, `reset_required` or
   `unknown`; do not acquire a write lease while any source fact is unresolved.
2. **Archive and map.** Acquire one migration lease, freeze legacy writers,
   create an immutable archive, and write the schema-validated manifest. Replay
   only the complete direct prefix; adapt legacy fields without retaining their
   authority; keep reset and unknown records archive-only with exact error and
   first-failed-boundary evidence.
3. **Reconcile and rehearse.** Create a fresh target epoch, re-register live
   identities and grants, import only confirmed active bugs/tasks/goal state,
   rebuild JSONL/latest-state projections, and run corruption, duplicate,
   missing-owner, changed-cwd, unknown-outcome and rollback tests on copied
   fixtures. No live project root is changed during rehearsal.
4. **Cut over after approval.** Only after a separate release approval may the
   owner freeze each live source, archive it, install the exact reviewed build,
   restart the one daemon, rebind endpoints, replay real TUI/Desktop and
   notification/bug/Loop paths, then record push and cleanup receipts.

Every implementation round uses a clean `playground/` worktree from the
current integration branch, a single owner, independent review and exact
candidate/tree evidence. No worker edits dirty `main`, and no migration step
cleans another project's worktree.

## Evidence still required

This document does not claim that migration or cutover has run. The following
remain live gates: current daemon PID/socket ownership, complete source hashes,
journal corruption classification, per-record owner mapping, real AppServer
capability negotiation, cursor continuation, P0 stop observation, two-minute
notification batching, reset archive receipt and post-restart replay. Earlier
runtime worker attempts and the detailed audit were interrupted by
`unexpected status 502 Bad Gateway: network error, url:
http://127.0.0.1:4444/v1/responses`; their partial output is retained as a
blocker, not a PASS. A later fallback candidate still needs its own independent
review and runtime prerequisite receipt before migration admission can open.
