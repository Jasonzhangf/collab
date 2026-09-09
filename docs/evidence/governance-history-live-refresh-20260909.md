# Governance history live refresh evidence

This is a read-only audit candidate for the proposed governance-history
migration. It records bounded observations of the four named projects and the
external codexapp transport. It does not authorize a lease, archive, reset,
replay, identity rebind, writer transition, daemon restart, cleanup, or
production cutover.

The superseded observation window in the first evidence revision was
`2026-09-09T19:32:20Z` through `2026-09-09T19:37:03Z` (UTC). This repair
recheck was collected from `2026-09-09T19:54:14Z` through
`2026-09-09T19:56:55Z`; its original command/output index is
`/private/tmp/governance-history-live-refresh-20260909-repair-raw.log` (SHA-256
`3e66149667f624f213011005444cca3e75e6071d121c698f5ce50c9f2ea748e4`). The
oldest repair source-file observation was taken at `2026-09-09T19:54:14Z`; use
`expires_at=2026-09-09T20:14:14Z` and
`refresh_before=2026-09-09T20:09:14Z` for planning only. A new read-only
inspect is required immediately before any lease, archive, reset, replay,
identity rebind, writer transition, or cutover, even when the expiry has not
passed. The live JSONL and mailbox projections are mutable, so these values
are not an immutable migration snapshot.

## Candidate provenance

The repair candidate is an isolated Collab worktree created from the specified
v1 base. It is not any of the live source roots below. The document content was
carried forward from the first evidence revision only to make the two review
repairs; that prior revision is evidence content provenance, not the base
commit.

| Field | Observed value |
|---|---|
| source repository | `/Volumes/extension/code/collab` |
| base commit | `2e7c7630ea7d2787e07ceb00b56eebbfb30d297a` |
| base tree | `9763e5476c799068732c9f87a576310a20aa7ed3` |
| repair candidate worktree | `/Volumes/extension/code/collab/playground/v1-migration-audit-repair-20260909` |
| repair checkout before edit | detached HEAD at the base commit above |
| prior evidence revision | `ea2e76d44661051e56ba399c6fd94ec1b894db7f` (content source only) |
| repair candidate state | clean before the copied documentation and fixture set; candidate writes are limited to the 10 docs paths in this candidate |
| live roots changed | two authorized audit-delivery mailbox appends from the prior evidence delivery; no source truth was edited by this repair |

The repair raw-log index is:

| Capture | Path and lines | Content |
|---|---|---|
| 01 | `/private/tmp/governance-history-live-refresh-20260909-repair-raw.log:1-7` | base/repair worktree provenance |
| 02 | `/private/tmp/governance-history-live-refresh-20260909-repair-raw.log:8-36` | live Git branch, HEAD/tree, dirty and unresolved projections |
| 03 | `/private/tmp/governance-history-live-refresh-20260909-repair-raw.log:37-53` | journal/events line counts and digests |
| 04 | `/private/tmp/governance-history-live-refresh-20260909-repair-raw.log:54-80` | token-redacted identity/pane keys and target-field presence |
| 05-07 | `/private/tmp/governance-history-live-refresh-20260909-repair-raw.log:81-764` | three complete, separately captured `collab who` JSON responses (Collab, AppSDK, RouteCodex) |
| 08, 11 | `/private/tmp/governance-history-live-refresh-20260909-repair-raw.log:765-846` | daemon artifacts, PID/cwd/socket descriptors, and mailbox projections; `capture-11` was collected at `2026-09-09T19:56:01Z` |
| 09-10 | `/private/tmp/governance-history-live-refresh-20260909-repair-raw.log:783-803` | codexapp connection result and global `collab serve` count |
| 12 | `/private/tmp/governance-history-live-refresh-20260909-repair-raw.log:847-855` | OneStop stale-PID recheck |

The base checkout's local migration tests were run before the evidence
document was added:

| Check | Result |
|---|---|
| `cargo test migration -- --test-threads=1` | 6 passed, 0 failed |
| `cargo test malformed_journal_replay_fails_fast -- --test-threads=1` | 1 passed |
| `cargo test duplicate_daemon_rejection_preserves_authoritative_pid -- --test-threads=1` | 1 passed |

These tests cover the base checkout's local freeze/snapshot/replay and duplicate
daemon negative paths. They do not prove source-history migration, archive
immutability, per-record mappings, target epochs, identity rebind, or a
cross-project single writer.

## Git candidate state

Repair Git observations were collected at `2026-09-09T19:54:14Z` and are
indexed at `capture-02`, lines 8-36 in the raw log. `status_entries_all` uses
`git status --porcelain=v1 --untracked-files=all`; staged and unstaged counts
are separate projections and may overlap for an unresolved path.

| Project | Canonical cwd | Branch | HEAD | HEAD tree | status entries | staged | unstaged | untracked | unresolved | origin/main |
|---|---|---|---|---|---:|---:|---:|---:|---:|---|
| Collab | `/Volumes/extension/code/collab` | `codex/v2-cordis-architecture` | `064824c375a720450c43f4829679bb8e45d4a1d1` | `7f0bfc31628af30604a3b10e13098ac166bcd236` | 671 | 1 | 4 | 667 | 0 | `d7ac749dbc6c7af752c889c67ec4dce662a0fc68d` |
| AppSDK | `/Users/fanzhang/Documents/github/appsdk` | `chore/project-memory-snapshot` | `2d14efed9d7f6454d119cb7a1aea24a384e966e0` | `728960b0e100201045317a28e740d0e58e0558d4` | 7 | 7 | 9 | 0 | 5 | `8fa6be2412b9f4e84fcd296d66c96e8849d8723b` |
| RouteCodex | `/Users/fanzhang/Documents/github/routecodex` | `codex/root-dirty-recovery-0908` | `d876adea1d5c57a73cf643f5c8d89b56bd3d42c4` | `88edaa554f16ff17cd4e28bc49ac52c5fc2a84ae` | 50 | 38 | 19 | 4 | 0 | `6e86a7e606cae8d3e0915ce9b788d6ef2bdaf72b` |
| codexapp | `/Users/fanzhang/Documents/github/codexapp` | no Git checkout | no HEAD | no tree | n/a | n/a | n/a | n/a | n/a | n/a |

The five unresolved AppSDK paths are:

```text
contracts/maps/function-map.json
contracts/migrations/sdk-0.1.5-to-0.1.6.json
docs/design/appsdk-project-integration.md
rust/src/main.rs
rust/tests/cli_smoke.rs
```

The Collab v2 checkout, the AppSDK conflict checkout, and the mixed V3/V4
RouteCodex checkout are dirty source trees. None is a clean active migration
candidate. AppSDK quality records and RouteCodex V3/V4 records remain under
their project owners; this audit does not repair, reset, or merge them.

## Mutable source projections

The repair recheck journal and event rows below were collected at
`2026-09-09T19:54:14Z` with `wc -l -c` and full-file `shasum -a 256`; the raw
output is indexed at
`/private/tmp/governance-history-live-refresh-20260909-repair-raw.log#capture-03`
(lines 37-53). The mailbox rows were collected by repair `capture-11` at
`2026-09-09T19:56:01Z` and are indexed at lines 835-846. The mailbox hash is
over regular files in C-locale sorted absolute-path order, concatenated
byte-for-byte. A mailbox count or digest does not say that every message is
current or consumable.

| Project | Journal | Events | Mailbox | Source interpretation |
|---|---|---|---|---|
| Collab | 72 lines, 20,871 bytes; `c47e0f95b388809274ae2c43b8a23289e3d92e6afa6a61ea21033a7491fffc83` | 457 lines, 73,125 bytes; `d580c6168ef1caf84c42ca30169b987d2695e04c1b9fd8795dcd31e05c424268` | 7 files, 58 lines; `67ab5e8977150060574bfce98360eea8be9cc628e6fc8a006e536b59e8c01b7e` | live project-local v1 projection |
| AppSDK | 42,470 lines, 12,730,859 bytes; `5e6243e7754c18fe079c8b56f877502782a4faeb9d751f4c48b41ef40c87c270` | 5,223 lines, 1,346,139 bytes; `4880d81c5ecfb5e34c39dde6d369847277b114cb292317cc4e6de3af6ff6b2f6` | 552 files, 7,216 lines; `ef6982dbe95db02492af522f3a0236a215ec3599b8db43e4047aae50097417ac` | live project-local v1 projection |
| RouteCodex | 39,431 lines, 8,608,934 bytes; `9dd93d730fa221aa445ebbc5e79e5c8884ebc877b180ff704bcbe8249c941273` | 26,238 lines, 7,778,263 bytes; `090b34f2cce9b67aa9345a248bdabcbe153dff9d692a2941a581c11c51bceeab` | 3,660 files, 45,663 lines; `dd56eb28b50f4bbf8dd484ea88ba675cbfe4c98e9306cbe80961b2104bb2a941` | live project-local v1 projection |
| codexapp | external `/Users/fanzhang/.codex-communication/journal.jsonl`: 54 lines, 61,245 bytes; `c3d995d870cd45a3887c1f3eddba4e2a214db6b60158f3b037ed4fc826f485a4` | external events file absent | external mailbox absent | external transport journal only; no Collab history source observed |

The AppSDK mailbox row above (`552` files, `7,216` lines,
`ef6982dbe95db02492af522f3a0236a215ec3599b8db43e4047aae50097417ac`) includes
the authorized audit deliveries below and subsequent mailbox activity. Earlier
pre-delivery projections were `549` files/`7,177` lines with
`b817a5d301ac83d2c08fcd64f9a6672ffe94817f3db320df8eeadfd8a98eb6fb`, then
`2026-09-09T19:45:02Z`, the mailbox was `550` files and `7,190` lines with
aggregate SHA-256
`fe2b6894591181cebd5377513dc1e80424c03dac97486391d20e6925c00de30d`. The
new file was
`/Users/fanzhang/Documents/github/appsdk/.agent-collab/mailbox/m1788983066183-9.json`
(`2,627` bytes, mtime `2026-09-09T12:44:26-0700`). This append was the
explicit durable delivery of this audit to `appsdk-2`; it did not acknowledge,
close, reassign, or alter any existing task, bug, worker, claim, journal,
event, identity, or socket. Treat both mailbox projections as bounded
observations and do not use either to infer a frozen source archive.

The follow-up correction was another explicit audit delivery,
`m1788983149319-10`, at `2026-09-09T19:45:49Z` (843 bytes). The resulting
projection was observed at `2026-09-09T19:46:01Z` as `551` files and `7,203`
lines with aggregate SHA-256
`ab65e143db0c364b46c8c0c705ea79c409d15eed5b8921d359394edcb389bcb3`. Neither
delivery acknowledged, closed, reassigned, or repaired an existing record.

The repair `capture-11` at `2026-09-09T19:56:01Z` observed `552` AppSDK
mailbox files and `7,216` lines with the digest in the table above. The raw
mailbox projection is indexed at lines 835-846. It is a mutable source
projection and is not a frozen archive.

The source drift is itself an admission blocker. The same live roots changed
while this audit was being collected:

| Projection | Earlier observation | Final row above |
|---|---|---|
| Collab events | 447 lines, digest `01d4bd14ecb79534c60017219ea0abdeabe35c0689badb448a50e928177e1a18` at `19:28:16Z`; then 452 at `19:34:11Z` | 457 lines, digest `d580c6168ef1caf84c42ca30169b987d2695e04c1b9fd8795dcd31e05c424268` |
| AppSDK journal | 41,740 lines at `19:28:16Z`; then 41,848, 41,938, and 41,980 during later probes | 42,470 lines, digest `5e6243e7754c18fe079c8b56f877502782a4faeb9d751f4c48b41ef40c87c270` |
| AppSDK events | 5,211 lines, digest `e85ff48e8e399d6b8b859d489f24ff746db828297d69b0b7a9011f9dd14408db`; then 5,213 | 5,223 lines, digest `4880d81c5ecfb5e34c39dde6d369847277b114cb292317cc4e6de3af6ff6b2f6` |
| RouteCodex events | 26,231 lines, digest `8f2bebb12eb55418a2bc32649914e4599737d7444d52cbcd5ed51a1225ffd842` at `19:28:16Z`; then 26,233 | 26,238 lines, digest `090b34f2cce9b67aa9345a248bdabcbe153dff9d692a2941a581c11c51bceeab` |
| RouteCodex journal | 39,431 lines, digest `9dd93d730fa221aa445ebbc5e79e5c8884ebc877b180ff704bcbe8249c941273` | unchanged in this window |

No digest is copied forward after drift. A changed source projection requires
another inspect or an immutable archive before mapping.

## Daemon, socket and lock observations

The repair filesystem and process observations were collected at
`2026-09-09T19:54:17Z`; direct socket/cwd file-descriptor checks are indexed at
`capture-11`, lines 804-832. `ps` and `lsof` observed a live `collab serve`
process and its cwd and Unix socket for Collab, AppSDK, and RouteCodex at that
time. The OneStop control PID had already disappeared by this recheck: the
raw `capture-12` at lines 847-855 records zero `ps`, cwd, and Unix rows while
its PID, empty lock, and socket artifacts remained. The zero-byte lock files do
not identify a holder or prove an exclusive lock.

| Project | PID artifact and process | cwd and socket owner | lock artifact | socket artifact |
|---|---|---|---|---|
| Collab | `server.pid` value `34612`; `/Users/fanzhang/.cargo/bin/collab serve`; started Tue Sep 8 18:33:18 | PID 34612 cwd `/Volumes/extension/code/collab`; fd 11 holds `/Volumes/extension/code/collab/.agent-collab/server/server.sock` | regular file, 0 bytes, `-rw-r--r--`, mtime `2026-09-07T08:00:11-0700`; holder unknown | Unix socket, `srw-------`, mtime `2026-09-08T18:33:18-0700` |
| AppSDK | `server.pid` value `56952`; `/Users/fanzhang/.cargo/bin/collab serve`; started Wed Sep 9 08:31:36 | PID 56952 cwd `/Users/fanzhang/Documents/github/appsdk`; fd 11 holds `/Users/fanzhang/Documents/github/appsdk/.agent-collab/server/server.sock` | regular file, 0 bytes, `-rw-r--r--`, mtime `2026-09-07T01:57:55-0700`; holder unknown | Unix socket, `srw-------`, mtime `2026-09-09T08:31:37-0700` |
| RouteCodex | `server.pid` value `57862`; `/Users/fanzhang/.cargo/bin/collab serve`; started Wed Sep 9 06:50:08 | PID 57862 cwd `/Users/fanzhang/Documents/github/routecodex`; fd 11 holds `/Users/fanzhang/Documents/github/routecodex/.agent-collab/server/server.sock` | regular file, 0 bytes, `-rw-r--r--`, mtime `2026-09-07T05:12:43-0700`; holder unknown | Unix socket, `srw-------`, mtime not reread in final artifact check |
| OneStop (control sample) | `server.pid` artifact value `15303`; no live `collab serve` process in repair recheck | PID/cwd/socket owner unknown; `capture-12` records `ps_rows=0`, `cwd_rows=0`, `unix_rows=0` | regular file, 0 bytes, `-rw-r--r--`, mtime `2026-09-07T01:21:40-0700`; holder unknown | Unix socket, `srw-------`, mtime `2026-09-09T12:56:54-0700`; owner unknown |

These observations establish local PID/cwd/socket alignment for three
project-local daemons during the repair recheck. OneStop's stale artifacts
demonstrate why the PID file and socket cannot establish liveness. They do not
establish a global writer. At `2026-09-09T19:54:18Z`, the host had 20
processes matching the exact
`collab serve` command, including the three live project roots above, the
stale OneStop artifacts, other project roots, and temporary/test worktrees.
Representative additional process/cwd
observations include PID 30336 (AgentTeams), 38198 (AgentBrowser), 41012
(zterm), 99199 (appsdk-bug-integration), PIDs 6709 and 30122 (Collab test
worktrees), and PIDs 92313 and 98280 (temporary cross-project test
worktrees). This is project-local multi-daemon deployment, not one writable
global daemon.

## Identity, runtime and scope projections

The repair safe projection of every `identity.json` removed the `token` value
and retained only its non-secret fields. It is indexed at `capture-04`, lines
54-80 in the raw log. The identity files observed under the three Collab roots
have the legacy key set
`["pane", "session", "token", "worker_id"]`; the redacted projection has
`["pane", "session", "worker_id"]`. The pane files have the same non-secret
shape. No `runtime_id`, `binding_id`, `endpoint_generation`, or
`project_scope_id` field name was observed in the `.agent-collab` JSON/JSONL
projection for any of the three roots.

| Project | identity.json files | pane JSON files | redacted identity keys | durable runtime/binding/generation/scope fields |
|---|---:|---:|---|---|
| Collab | 9 | 3 | `pane`, `session`, `worker_id` | none observed |
| AppSDK | 11 | 13 | `pane`, `session`, `worker_id` | none observed |
| RouteCodex | 25 | 25 | `pane`, `session`, `worker_id` | none observed |

Canonical cwd and the daemon process cwd are therefore the only current scope
binding observed in this refresh. A pane label, role-like worker name, process
name, or socket path cannot be promoted into a durable runtime/binding or
project-scope identity. Token values are intentionally absent from this
artifact and were not copied.

## Authoritative worker/task projection

`collab who` is a read-only authoritative status query. The repair uses one
complete response per project, captured separately at `2026-09-09T19:54:16Z`:
Collab is raw `capture-05`, lines 81-155; AppSDK is raw `capture-06`, lines
156-298; RouteCodex is raw `capture-07`, lines 299-764. Aggregates and active
task rows below are derived from those same response bodies, so they are not
stitched together from calls made at different live-state revisions. Status
is current daemon state, not an authorization to import, close, or repair a
task.

| Project | workers | status counts | identity valid | endpoint live | active tasks | pending notifications |
|---|---:|---|---|---|---:|---:|
| Collab | 4 | 4 lost | 0 true / 4 false | 0 true / 4 false | 0 | 1 |
| AppSDK | 8 | 5 waiting / 3 lost | 5 true / 3 false | 5 true / 3 false | 3 | 6 |
| RouteCodex | 27 | 1 working / 1 waiting / 2 unknown / 23 lost | 4 true / 23 false | 4 true / 23 false | 11 | 59 |

The first evidence revision's AppSDK aggregate (`5 waiting / 3 lost`) and its
`gcm-timeout` detail (`working`) came from two separate live `collab who`
invocations. They therefore did not form one atomic status snapshot; the
earlier detail is retained only as an unbound observation. The repaired AppSDK
status and all three active-task rows below are derived from the single raw
response in `capture-06`, lines 156-298, where `gcm-timeout` is
`agent_state=waiting` and `status=waiting`. No historical working state is
silently merged into this response.

The active task IDs were:

| Project | Worker | Task | active status | current worker status | admission treatment |
|---|---|---|---|---|---|
| AppSDK | `appsdk-1` | `bug-238c3df-master-idle-wakeup-20260908` | delivered | waiting, present, identity valid | preserve; do not close or import from this audit |
| AppSDK | `appsdk-subagent-gcm-timeout-0908` | `bug-ead2041-gcm-timeout-20260908` | blocked | waiting, present, identity valid | preserve; blocked task needs owner decision |
| AppSDK | `appsdk-subagent-p0-cross-project-send-20260908` | `bug-6369e0-cross-project-send-20260908` | blocked | waiting, present, identity valid | preserve; blocked task needs owner decision |
| RouteCodex | `routecodex-1` | `v3-agent-memory-foundation-20260904` | blocked | working, present, identity valid | preserve V3 owner state |
| RouteCodex | `routecodex-2` | `v4-cordis-governance-master-takeover-0906` | blocked | waiting, present, identity valid | preserve V4 owner state |
| RouteCodex | `routecodex-3` | `v3-combined-0907` | delivered | unknown, present, identity valid | preserve; outcome is not synthesized |
| RouteCodex | `routecodex-subagent-counter-eperm-0906-yolo` | `task-m1788789987107-39` | assigned | lost, absent, identity invalid | `needs_operator`; owner must be resolved |
| RouteCodex | `routecodex-subagent-p0-gcm-probe-0909` | `task-m1788961699258-445` | assigned | lost, absent, identity invalid | `needs_operator`; owner must be resolved |
| RouteCodex | `routecodex-subagent-v3-cooldown-sse-0906` | `v3-build-admission-repair-0907` | delivered | lost, absent, identity invalid | `needs_operator`; delivery/cleanup must be resolved |
| RouteCodex | `routecodex-subagent-v4-governance-audit-yolo` | `task-m1788742378277-235` | working | lost, absent, identity invalid | `needs_operator`; owner must be resolved |
| RouteCodex | `routecodex-subagent-v4-governance-owner-0907` | `task-m1788792436878-82` | verifying | lost, absent, identity invalid | `needs_operator`; owner must be resolved |
| RouteCodex | `routecodex-subagent-v4-host-socket-0906` | `task-m1788742173770-222` | reviewed | lost, absent, identity invalid | `needs_operator`; review/owner relation must be resolved |
| RouteCodex | `routecodex-subagent-v4-runtime-bin-0906` | `task-m1788742664848-8` | blocked | lost, absent, identity invalid | `needs_operator`; blocked owner must be resolved |
| RouteCodex | `routecodex-subagent-v4-runtime-owner-0907` | `task-m1788793100660-141` | verifying | lost, absent, identity invalid | `needs_operator`; owner must be resolved |

The active AppSDK bug tasks and RouteCodex V3/V4 tasks are source facts to
preserve. They are not permission to reset a root or to treat a delivered,
reviewed, or historical receipt as a migration PASS. The lost/unknown rows
cannot be silently assigned to a new identity.

## codexapp endpoint bootstrap

`/Users/fanzhang/Documents/github/codexapp` is a non-Git Node transport
prototype with no `.agent-collab` directory. Its only observed history is the
external journal above. The journal has 54 valid JSON objects with the safe
top-level key names `address`, `agent`, `at`, `error`, `evidence`, `expiresAt`,
`messageId`, `protocol`, `scope`, `source`, `state`, `status`, and `type`.
No external events or mailbox projection was observed.

The repair external socket observation was made at `2026-09-09T19:54:18Z`; the
raw output is indexed at `capture-09`, lines 783-800:

| Field | Observed value |
|---|---|
| journal mtime | `2026-09-08T23:19:46-0700` |
| socket | `/Users/fanzhang/.codex-communication/sockets/commd.sock` |
| socket type/mode | Unix socket, `srwxr-xr-x` |
| socket mtime | `2026-09-08T23:12:59-0700` |
| native `commd`/`codexapp` process | none observed at the check time |
| read-only `node src/cli.js status --socket ...` | failed with `connect ECONNREFUSED /Users/fanzhang/.codex-communication/sockets/commd.sock` |

The source contains an `app-server-adapter.js`, bridge helpers, and
`MockAppServerAdapter` fixtures. Those source references and tests do not
prove a native listener, current writer ownership, native initialize
capabilities, or a live endpoint generation. Classify codexapp as
`needs_operator` for a new endpoint bootstrap. If an operation attempts to
import its external journal as active Collab history before that bootstrap and
reconciliation, classify that attempt as `reset_required`.

## Migration admission and controller gaps

### Current disposition

| Project | Classification | Reason and required next action |
|---|---|---|
| Collab | `reset_required` | dirty v2 root, source events changed during inspection, all four registered workers are lost, no durable runtime/binding/generation/scope projection, and 20 host-level `collab serve` processes disprove one-writer admission; preserve source and obtain a clean candidate plus an operator-controlled cutover plan |
| AppSDK | `reset_required` | unresolved Git conflicts, mutable journal/mailbox, three active bug tasks, and mixed valid/lost worker identities; preserve AppSDK quality records and reconcile on a clean source candidate |
| RouteCodex | `reset_required` | dirty mixed V3/V4 root, 23 lost workers with active task records, mutable events/mailbox, and no durable runtime/binding/generation/scope projection; preserve V3 and V4 ownership separately |
| codexapp | `needs_operator` | external socket has no listener and no writer ownership; bootstrap a native endpoint, prove initialize/capability/identity binding, and archive/reconcile its journal before any import |

Use `needs_operator` only where an explicit owner can repair the relation and
recheck it. Use `reset_required` for dirty or stale candidates, source drift,
multiple writers, missing owner relations, unresolved conflicts, ambiguous
history, or an attempted active import without a verified endpoint. Never
synthesize accepted, delivered, executed, replied, or read outcomes when the
current record cannot distinguish them.

### Controller contract gap

The v1 `MigrationRecord` in `src/server/state.rs` currently carries only local
version/phase/freeze data, one local snapshot hash, worker/task/message
counts, an operator and issue strings. The v1 handlers implement local
`inspect -> plan -> apply -> verify`, admission freeze, and local state replay.
They do not own the following facts required for governance-history migration:

- source project/schema/repository/branch/HEAD/tree and immutable source archive
  reference/digest;
- target epoch, source/target revision binding, monotonic target sequence, or
  per-record source ID/type/digest to target entity/sequence mapping;
- mapping class/status (`direct`, `adapt`, `reset`, `unknown`) and exact first
  failed boundary/error;
- mailbox, claims, worktrees, branches, evidence, waits, notification state,
  and AppSDK record references with relation checks;
- idempotency key
  `(source_project_id, source_record_id, source_record_digest, target_epoch)`;
- runtime ID, binding ID, endpoint generation, and identity rebind receipt;
- rollback fencing token/CAS expectations and post-boundary reconciliation
  receipt;
- source archive verification, projection rebuild from committed facts, or
  cross-project one-writer admission.

The v2 `plan_legacy_beta_migration` path reads one `legacy-state.json` and
understands only top-level identities and tasks. Its `ApplyMigration` path
requires an empty v2 state and copies the limited plan into a separate v2
reducer/journal. It does not replay the v1 journal or preserve mailbox,
claims, worktrees, branches, evidence, waits, notification state, or AppSDK
references. Wiring that path into v1 history would create a second migration
semantic owner and a second writer.

The separate `codex/v1-migration-archive-20260909` line at `895ef7f` contains
only unintegrated `src/migration_archive.rs`; it is not wired into `main.rs`
or `src/server` and is not runtime or migration evidence. The current
candidate also contains no exact, reproducible reviewed R1-R5 candidate/tree
receipts. The goal contract therefore leaves admission at
`runtime_prerequisite`; it must not be reimplemented inside this audit.

The required gate remains:

```text
S1 inspect/classify/snapshot
→ S2 archive/map/rebuild
→ S3 independent review/no-write rehearsal
→ S4 separately approved production cutover
```

This refresh reached S1 evidence only. No archive, map, rebuild, rehearsal
write, or cutover was performed.

## Explicit non-actions

- No live-root source-truth file was edited, including journal, events, PID,
  lock, socket, identity, claim, task, or evidence files. Two authorized
  `collab_sendmessage` deliveries appended this audit and its correction to
  the AppSDK mailbox (`m1788983066183-9` and `m1788983149319-10`); those
  deliveries are called out above and are not treated as migration evidence.
- No daemon was started, stopped, restarted, or migrated.
- No task, bug, worker, claim, mailbox message, or notification was closed,
  acknowledged, reassigned, or repaired.
- No identity token was printed, copied, or re-bound.
- No source digest was overwritten after drift.
- No reset, archive, replay, install, push, merge, global replacement, or
  production operation was performed.
