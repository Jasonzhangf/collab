# S2 migration runtime prerequisite audit

Audit time: `2026-09-09T23:29:44Z` (UTC)

Decision: **FAIL — blocked at `runtime_prerequisite`.**

This is a read-only admission audit for S2 of the governance-history
migration. It checks whether the current v1 integration candidate exposes the
single runtime/reducer/journal owner that S2 is required to consume. It does
not implement that owner or run migration.

## Contract and stop rule

The migration goal states that its runtime foundation is owned by the R1–R5
refactor rounds. Migration starts only after those rounds have exact reviewed
candidate/tree receipts on the v1 integration line; a missing, conflicting or
non-reproducible receipt leaves the goal blocked at `runtime_prerequisite`
and must not be reimplemented inside the migration adapter
(`docs/goals/collab-v1-governance-history-migration-plan.md:36-47`).

S2 consequently requires one migration lease and frozen legacy writers before
it can create an immutable archive, replay/adapt records, create a fresh target
epoch, re-register identities, or rebuild projections
(`docs/goals/collab-v1-governance-history-migration-plan.md:82-106`). The
architecture contract assigns one Rust reducer and one journal writer to the
resident global daemon and requires every mutating command to carry
`command_id`, `operation_id`, actor binding, scope and any compare-and-swap
revision (`docs/design/collab-v1-refactor-architecture-20260909.md:195-226`).

## Candidate provenance

| Field | Observed value |
| --- | --- |
| source repository | `/Volumes/extension/code/collab` |
| audited worktree | `/Volumes/extension/code/collab/playground/v1-s2-migration-runtime-audit-20260909` |
| audited branch | `codex/v1-s2-migration-runtime-audit-20260909` |
| integration source commit | `247bfdb38010ba22144a14bb95a1ec3bf2797feb` |
| integration source tree | `0e392dc62a5673e3c082775720eee24c364a85e6` |
| source commit contents | `docs/evidence/unmerged-branch-inventory-r3-20260909.md` only; no runtime source change in `247bfdb` |
| worktree state at audit | clean before this evidence file |
| allowed write | this evidence file only |

The source tree is pinned to the parent integration commit above. The final
audit commit necessarily has a different tree because it adds this document.
The source integration worktree was not edited.

## Requirement result

Status meanings: `PASS` means the migration request path reaches the resident
global owner with the required durable fact; `PARTIAL` means a local model or
legacy helper exists but does not establish that path; `FAIL` means the current
candidate cannot satisfy the prerequisite.

| S2 prerequisite | Status | Evidence in the candidate | Finding |
| --- | --- | --- | --- |
| `command_id` | **FAIL** | The typed global receipt includes `command_id` (`src/server/global_state.rs:458-467`) and its in-memory idempotency logic is present (`src/server/global_state.rs:904-963`). The active server helper accepts a string parameter but writes the legacy receipt shape (`src/server/mod.rs:200-245`; `src/server/state.rs:75-83`). Migration requests carry only `worker_id` and `token` (`src/proto.rs:269-284`). | The typed command ID is not carried by `MigrationInspect/Plan/Apply/Verify` and does not reach the active reducer through a command envelope. |
| `operation_id` | **FAIL** | The typed global receipt and `record_command` path include `operation_id` (`src/server/global_state.rs:458-467`, `src/server/global_state.rs:904-963`). The legacy helper accepts one and returns it (`src/server/mod.rs:200-251`), but the migration request variants have no operation field (`src/proto.rs:269-284`). | There is no operation identity or retry lookup on the migration command path. |
| journal receipts | **PARTIAL** | The legacy server has one `journal` file and `State` reducer (`src/server/mod.rs:127-136`), persists event bytes before applying them (`src/server/mod.rs:262-299`), and can append a legacy `CommandRecorded` event (`src/server/state.rs:534-544`). | Migration handlers append only `MigrationUpdated` records (`src/server/mod.rs:1733-1780`, `src/server/mod.rs:1783-1822`, `src/server/mod.rs:1824-1886`). `MigrationRecord` has no source/archive digest, source/target epoch, fencing token, command/operation IDs, target mapping or migration receipt graph (`src/server/state.rs:28-43`). |
| migration lease | **FAIL** | `verify_migration_lease` only compares the current legacy `MigrationRecord.operator` for phases `planned`/`applied` (`src/server/mod.rs:1670-1686`). | This is an operator-name conflict check, not a durable lease bound to source digest, canonical cwd, target build and fencing token. `handle_migration_plan` creates the legacy record directly without those fields (`src/server/mod.rs:1733-1764`). |
| epoch | **FAIL** | `GlobalState` models an epoch and validates typed receipts against it (`src/server/global_state.rs:495-583`). | The resident `Server` owns legacy `State`, whose counters are only `sequence` and `revision` (`src/server/state.rs:557-585`). The migration record and handlers never allocate or CAS a source/target/active epoch. |
| runtime binding | **FAIL** | Typed `RuntimeBinding` contains project/app/agent/runtime/binding/generation/native-thread identity and has reconnect fencing (`src/server/global_state.rs:209-272`, `src/server/global_state.rs:697-814`). `CommandEnvelope` also validates binding, generation and route scope (`src/proto.rs:9-90`). | The active registration path still stores `WorkerRec` and derives runtime from a tmux pane (`src/server/mod.rs:1889-2005`); migration authenticates `worker_id/token` through `verify` and never validates a typed binding or generation. |

## First divergence: the typed model is unreachable

`src/server/global_state.rs` explicitly identifies itself as an “unconnected
slice” and says that the existing reducer remains the current journal owner
until integration wires the model (`src/server/global_state.rs:1-7`). The
server module exports `state` but has no `global_state` module declaration
(`src/server/mod.rs:1-7`), and a repository-wide source search finds the
`GlobalState`/typed receipt symbols only in that file and its unit tests.

The active `Server` therefore remains `Mutex<State>` plus a legacy journal
file (`src/server/mod.rs:127-136`). The typed `CommandEnvelope` and identity
validation are compileable protocol helpers, but no migration request or
server dispatch arm accepts one: dispatch destructures each migration request
as `worker_id/token` and calls the legacy handlers
(`src/server/mod.rs:5247-5255`). The migration classifier is deliberately
read-only and has no daemon or filesystem-state owner; it hashes and classifies
caller-supplied bytes and leaves archive/target decisions to a later adapter
(`src/migration.rs:1-7`, `src/migration.rs:197-209`, `src/migration.rs:374-380`).

This is the first blocking divergence. Adding lease, epoch, receipt or binding
fields in `migration.rs` or in the legacy migration handlers would create a
second control-state owner and violate the migration goal's explicit stop rule.

## Minimal next boundary

The next implementation must be a reviewed runtime integration candidate, not
an S2 adapter patch. It should, in the owner assigned by the refactor
architecture's R2 contract, do only the following before S2 resumes:

1. Make one `GlobalState` (or the reviewed successor of that exact model) the
   resident daemon's single reducer/state owner.
2. Connect one host journal writer and replay path to that reducer, including a
   typed command envelope and immutable command/operation receipt path.
3. Add the durable migration lease/fencing and active-epoch CAS to that same
   owner, then connect runtime binding/generation rebind and grant revocation
   to it.
4. Expose the typed seam for the migration adapter. Do not add a second lease,
   epoch, receipt or runtime-binding state machine in `migration.rs` or the
   legacy migration handlers.

The architecture's R2 contract requires the physical reducer/journal wiring,
not a document-only interface (`docs/design/collab-v1-refactor-architecture-20260909.md:498-524`).
The current evidence still lists the M1 journal/replay candidate and
deterministic replay evidence, one-writer fencing, archive receipts and native
runtime/binding rebind as unresolved P1 prerequisites
(`docs/evidence/governance-history-live-refresh-r4-20260909.md:419-435`).

## Non-actions and validation

This audit did not start or stop a daemon, acquire a migration lease, freeze a
writer, archive/reset/replay/rebind any source, or modify a live root. It only
read the pinned candidate, its source files and contract/evidence documents,
then added this evidence file in the isolated worktree.

Validation performed against the pinned source tree:

| Check | Result |
| --- | --- |
| `cargo test --all-targets` with task-specific target directory | **PASS** — 294 unit tests passed, 8 MCP tests passed, 1 integration test passed; 2 tests ignored |
| `cargo test global_state -- --list` | **PASS as an observation** — no matching tests are registered, consistent with `global_state.rs` not being declared by `server/mod.rs`; this is evidence of reachability, not a runtime acceptance test |
| `cargo fmt --check` | **FAIL on pre-existing baseline formatting** in unrelated source files; no source formatting was changed because this audit's sole write is the evidence document |
| audit worktree status before commit | one untracked file at the allowed path above; no other changed paths |

The passing test suite establishes that the existing legacy candidate builds and
its existing tests pass. It does not turn the unreachable typed model into a
runtime prerequisite, and it does not authorize S2 migration admission.
