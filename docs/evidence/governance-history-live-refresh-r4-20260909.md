# Governance history live refresh r4

This document is the fresh, read-only field record for the planned migration of
the Collab, AppSDK, RouteCodex and codexapp governance histories to the Collab
v1 host-wide daemon. It is an evidence input for a later operator-controlled
migration. It is not a migration receipt and it does not authorize a reset.

No live archive, reset, replay, rebind, daemon restart, installation, branch
merge, push, worktree cleanup, task reassignment or notification was performed
for this refresh.

## Capture boundary and evidence validity

The main collection window was `2026-09-09T22:38:01Z` through
`2026-09-09T22:38:10Z`. A clean process/socket recheck ran at
`2026-09-09T22:40:42Z`. The raw command transcript is:

`/private/tmp/governance-history-live-refresh-r3-20260909-raw.log`

Its observed digest is
`3b850c77172bdbd3b4e7befe968a5607943d66f4dc7b5d52d2c6328a3b7b568b`;
the file has 1,253 lines and 98,968 bytes. The capture policy and start time
are at raw lines 1-2. The project captures are at raw lines 3-120 (Collab),
121-437 (AppSDK), 438-1143 (RouteCodex) and 1144-1166 (codexapp). The
initial process/socket probe is at raw lines 1167-1224; the clean recheck is at
raw lines 1225-1253. The exact source counts, digests and JSONL framing results
are retained in those ranges.

The proposed evidence expiry is `2026-09-09T23:10:42Z`. Source JSONL, process
ownership, sockets, goal projections, branches and worktrees are mutable. A
new read-only inspect is mandatory immediately before any lease, freeze or
write. A digest below proves only the bytes read during this bounded capture;
it does not prove that the source is safe to replay.

The status CLI families were deliberately not called. `collab status`,
`collab who`, `collab master status` and AppSDK status commands append request
records to the live event journal in this installation. The observations below
therefore use direct Git/filesystem/JQ projections and process/socket
inspection. Worker, master and goal rows derived from JSONL are historical
projections, not live liveness or authorization proof.

## Project-level migration disposition

Project disposition answers whether the project can currently enter an active
migration attempt. It is separate from the classification of each preserved
history record.

| Project | Canonical source | Project disposition | Evidence and reason | Next gate |
| --- | --- | --- | --- | --- |
| Collab | `/Volumes/extension/code/collab` | `reset_required` | Dirty v2 root; 151 worktrees; 135 branches not merged to local `main`; the current bounded projection contains one `SubagentUpdated` row (`closed`) and four `KeepaliveUpdated` rows (`absent`); host-wide one-writer admission is unproven; no verified v1 scope, runtime binding, endpoint generation or epoch. | Fresh inspect, operator authorization, one migration lease, legacy writer freeze, one target writer/reducer, immutable archive, then fresh epoch and explicit identity rebind. |
| AppSDK | `/Users/fanzhang/Documents/github/appsdk` | `reset_required` | Dirty checkout with five unresolved paths; 101 worktrees; 42 branches not merged; mixed worker projections; active blocked bug/task facts; goal control is recovery-required with exact subscription/reconciliation error; cross-window source differences require a new frozen snapshot. | Preserve AppSDK quality and bug references; freeze a new source snapshot; reconcile only confirmed owners/runtimes/worktrees in the target epoch. |
| RouteCodex | `/Users/fanzhang/Documents/github/routecodex` | `reset_required` | Dirty V3/V4 recovery root; 74 worktrees; 378 branches not merged; current keepalive projection is 22 `absent`, four `idle`, one `unknown` and one `working`; orphan task edges; cross-window source differences; project-local legacy goal state is not global migration proof. | Preserve V3/V4 histories separately; classify orphan work archive-only; import only facts with fresh owner, scope, runtime and evidence. |
| codexapp | `/Users/fanzhang/Documents/github/codexapp` plus `/Users/fanzhang/.codex-communication` | `needs_operator` | No Git checkout, no Collab identity/runtime binding, only an external transport journal, and the communication socket had no successful listener/capability/initialize receipt. | Operator must bootstrap and verify the native AppServer endpoint, archive the external journal, and explicitly register a new project identity. Desktop cannot register a goal subscription. |

`reset_required` means safe active replay is not proven. It does not mean a
reset has occurred, old facts were deleted, or a target epoch is valid.
`needs_operator` means an external authorization or runtime bootstrap is
missing. Neither disposition is a completion state.

## Source and Git observations

The following rows are exact read-only observations from each canonical cwd.
`status_entries` is the number of rows emitted by `git status --porcelain=v1`;
it includes tracked paths with staged or unstaged changes and untracked paths.
`staged_paths` is the count from `git diff --name-only --cached`, while
`unstaged_paths` is the count from `git diff --name-only`; a path may appear in
both because the index and worktree differ. `untracked_paths` is the count
from `git ls-files --others --exclude-standard`, and is also represented in
the porcelain status rows. `unresolved_paths` is the count of unique paths
from `git ls-files -u`. These counters therefore are not additive. Branch and
worktree counts are inventory facts, not merge decisions.

| Project | Branch / HEAD | `origin/main` | Status rows | Staged | Unstaged | Untracked | Unresolved | Worktrees | Branches not merged to local `main` |
| --- | --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| Collab | `codex/v2-cordis-architecture` / `064824c375a720450c43f4829679bb8e45d4a1d1` | `d7ac749dbc6c7af752c889c67ec4dce662a0fc68d` | 38 | 1 | 4 | 667 | 0 | 151 | 135 |
| AppSDK | `chore/project-memory-snapshot` / `2d14efed9d7f6454d119cb7a1aea24a384e966e0` | `8fa6be2412b9f4e84fcd296d66c96e8849d8723b` | 7 | 7 | 9 | 0 | 5 | 101 | 42 |
| RouteCodex | `codex/root-dirty-recovery-0908` / `d876adea1d5c57a73cf643f5c8d89b56bd3d42c4` | `6e86a7e606cae8d3e0915ce9b788d6ef2bdaf72b` | 49 | 38 | 19 | 4 | 0 | 74 | 378 |
| codexapp | no Git checkout | n/a | n/a | n/a | n/a | n/a | n/a | n/a | n/a |

The corresponding raw output is Collab lines 4-15, AppSDK lines 122-133,
RouteCodex lines 439-450, and codexapp lines 1145-1147. The three Git roots
are dirty and the AppSDK root has five unresolved paths. No branch was chosen
for integration. Every branch/worktree must later be classified by exact
candidate, base, tree, owner, task, review, delivery and merge evidence.

### Durable source digests

All three Collab JSONL pairs and the codexapp external journal passed the
framing probe. Framing success does not establish typed-schema validity,
sequence continuity, complete owner relations, scope, runtime binding, or a
safe replay prefix.

| Project | Source | Lines / bytes | SHA-256 observed |
| --- | --- | ---: | --- |
| Collab | `.agent-collab/server/journal.jsonl` | 72 / 20,871 | `c47e0f95b388809274ae2c43b8a23289e3d92e6afa6a61ea21033a7491fffc83` |
| Collab | `.agent-collab/server/events.jsonl` | 476 / 76,979 | `457104632645eb5e44b56a4390ec1a7e2b5912bedf1e02636286a4ff1da709ad` |
| AppSDK | `.agent-collab/server/journal.jsonl` | 46,777 / 14,061,215 | `316c9f5878e782a2931f4618d0f4087de4d9951d51a15961aa1ae8132f5b5d25` |
| AppSDK | `.agent-collab/server/events.jsonl` | 5,244 / 1,354,142 | `cf8c5b1d535f15a8cdb71212b195677dfce1b37bdb8a6e4a4876edb1466d4dce` |
| RouteCodex | `.agent-collab/server/journal.jsonl` | 39,431 / 8,608,934 | `9dd93d730fa221aa445ebbc5e79e5c8884ebc877b180ff704bcbe8249c941273` |
| RouteCodex | `.agent-collab/server/events.jsonl` | 26,251 / 7,780,335 | `7ae997f821c5820fc681e6ac3f3ebba914b6af80757c91d48e398aedf04c1a00` |
| codexapp | external `/Users/fanzhang/.codex-communication/journal.jsonl` | 54 / 61,245 | `c3d995d870cd45a3887c1f3eddba4e2a214db6b60158f3b037ed4fc826f485a4` |

Observed source paths and framing results are at raw lines 16-21, 134-139,
451-456 and 1148-1151. Comparisons with earlier evidence windows show
cross-window differences in the AppSDK journal, RouteCodex events and Collab
events. The nine-second main capture does not establish that those files
changed during that window. A future apply must freeze a new source snapshot
before computing the migration manifest.

Additional source counts were:

| Project | Mailbox files | Claims | Panes | Runs |
| --- | ---: | ---: | ---: | ---: |
| Collab | 7 | 2 | 3 | 29 |
| AppSDK | 552 | 3 | 13 | 98 |
| RouteCodex | 3,660 | 37 | 25 | 519 |

These are filesystem counts at raw lines 22-25, 140-143 and 457-460. They are
not proof that each entry has an active owner or a deliverable result.

## Runtime, owner and goal observations

### Collab

The current bounded direct projection contains one `SubagentUpdated` row:
`approval-replay-gcm-0908` with status `closed`. It also contains four
`KeepaliveUpdated` rows, each with observed state `absent`. This is the only
Collab worker/keepalive result sampled in this round; it is a JSONL projection,
not live liveness or a master authorization result. The worker and migration
projections are at raw lines 26-120. A previous evidence round recorded four
registered/lost workers and no live master; that earlier result is retained as
a prior-round clue in
[`governance-history-live-refresh-r3-20260909.md`](./governance-history-live-refresh-r3-20260909.md),
not as a direct conclusion of this refresh. A legacy `MigrationUpdated` row
reports `phase=verified` for `v1-low-intervention`, but that row is historical
and does not prove current global v1 ownership, one writer, scope, runtime
binding or endpoint generation.

### AppSDK

The journal projection contains five closed worker rows and three idle rows in
the latest `SubagentUpdated` view. The keepalive projection contains seven
`absent`, four `idle` and one `working` latest observations. The rows are
historical journal projections, not live liveness. The `collab-safe-close-0908`
history includes a duplicate-session error; `gcm-timeout-0908` and
`p0-cross-project-send-20260908` remain represented by idle projections.

The local goal control file says:

```text
active=false
desired=recovery_required
remote_state=unknown
observed=unknown
error=GOAL_STATUS_SUBSCRIPTION_ID_MISSING; GOAL_STATUS_SUBJECT_RECONCILIATION_FAILED:GOAL_RECONCILE_COLLAB_FAILED:exit=1
```

The same projection contains `collab_subscribed=true` and an armed local
subscription, but that stale/local field conflicts with `active=false` and
the exact reconciliation error. It is not proof of a successful subscription.
The exact row is at raw line 436; worker and keepalive projections occupy raw
lines 144-434.

### RouteCodex

The current keepalive projection has 22 `absent`, four `idle`, one `unknown`
and one `working` observation. These are historical projections, not current
process liveness. A previous evidence round recorded 23 `lost` workers and
two `unknown` keepalive observations; that is a prior-round clue, retained in
[`governance-history-live-refresh-r3-20260909.md`](./governance-history-live-refresh-r3-20260909.md),
not the current-round result.
The goal control file says `active=true`, `desired=subscribed`,
`remote_state=armed`, `observed=subscribed`, `repeat_count=100`, and points to
the V4 plan. This is project-local legacy control state, not proof of a valid
global migration goal. The exact goal row is raw line 1133; historical
migration rows are raw lines 1135-1143.

### codexapp

There is no Git checkout and no Collab identity, runtime binding, owner,
endpoint generation or project scope evidence. The external journal contains
14 `agent.refreshed`, 7 `agent.registered`, 3 `message.failed`, 22
`message.state`, 6 `scope.registered` and 2 `session.status` rows. Three
historical failures are exact `reply_timeout` failures, retained at raw lines
1159-1162. The external socket exists with mode `srwxr-xr-x`, but a Unix
connection probe exited 1 and no native initialize/capability receipt was
observed (raw lines 1163-1166).

### Host process census

The clean process recheck found 21 matching `collab serve` processes, including
the canonical-looking PIDs `34612` (Collab), `56952` (AppSDK) and `57862`
(RouteCodex). The initial process census is at raw lines 1167-1224; the clean
recheck starts at raw line 1225 and ends at raw line 1253, with the canonical
PID rows at 1247-1250. The 21 count is the 21 process rows at raw lines
1226-1246. Multiple matching processes make single-writer admission unproven.
This must be stated as “multiple processes; one writer not proven”, never as
“21 writers”. PID, socket and empty lock files do not prove ownership. No
process was stopped or modified.

## Record-level mapping policy

Record mapping is independent of project disposition. A project may be
`reset_required` while some historical records are retained as `direct` or
`adapt`; it may also preserve records as `archive-only` without repairing every
old owner first.

The target manifest uses the schema in
[`migration-v1-history-manifest.schema.json`](../migration-v1-history-manifest.schema.json).
That schema represents `direct`, `adapt`, `reset` and `unknown`. `archive-only`
is an import disposition for a preserved record whose active import is blocked;
it is not a mapping class and does not replace the class. A missing-owner or
lost-runtime record therefore keeps `mapping_class=reset`, while an ambiguous
side effect keeps `mapping_class=unknown`. At manifest stage S1 (`planned`),
`raw_archive_ref` and `archive_ref` remain null until the immutable archive
operation has produced its receipt; after that boundary an archive-only record
must carry the archive pointer, exact error and `first_failed_boundary`.

| Record condition | Mapping class | Record disposition | Required evidence |
| --- | --- | --- | --- |
| Complete typed prefix with valid ID, timestamp, scope, owner, task/worktree/evidence edge, single-writer interval and deterministic outcome | `direct` | `mapped` only after target append and receipt | Source digest, target epoch/sequence/entity, owner/runtime/binding and replay receipt |
| Meaning recoverable from a typed legacy row whose shape lacks v1 controls | `adapt` | `needs_reconciliation` until current owner, scope, runtime and evidence are checked | Raw archive pointer, discarded-field list, adaptation reason and new v1 fact receipt |
| Missing owner, lost runtime, stale/broken worktree or claim, unresolved merge, unknown delivery, dirty historical candidate, conflicting ID, missing scope, wait cycle, or historical PASS without exact artifact/environment/entrypoint/producer evidence | `reset` | `archive-only` with active import blocked | At S1: exact error and `first_failed_boundary`; after archive: source digest and immutable archive pointer |
| Timeout, socket close or ambiguous side effect whose outcome cannot be queried deterministically | `unknown` | `archive-only` and operator decision required | At S1: exact error and `first_failed_boundary`; after archive: operation/idempotency ID and immutable archive pointer |

The following are record-level classifications, not project-level dispositions:

- Missing owner, lost runtime, stale worktree, unresolved merge conflict and
  unknown delivery are `archive-only` import dispositions while retaining
  `mapping_class=reset`; they do not require repairing every old owner before a
  preservation archive.
- An ambiguous side effect is `archive-only` while retaining
  `mapping_class=unknown`. It remains unknown after one authoritative lookup;
  `archive-only` never converts it to `reset` or success.
- Unknown side effects remain unknown after one authoritative lookup. A retry
  cannot turn an unknown result into a success or create a second daemon,
  message, task or goal.
- Historical PASS, review, delivery, goal, wake or master receipts are
  archive evidence only until their candidate/tree, owner, runtime, scope and
  current epoch edges are revalidated.
- Old session IDs, pane titles and process names are query clues. They are not
  identity authorization and they do not survive an epoch boundary.

## Migration admission and operation order

Hard migration admission requires all of the following in the same attempt:

1. Explicit operator authorization naming the projects, source roots, target
   build, archive destination and allowed operation.
2. One authenticated migration lease bound to the frozen source digest, source
   epoch, target build, canonical cwd and fencing token.
3. All legacy writers frozen with explicit acknowledgements; one target writer
   and one reducer proven.
4. An exact reviewed target build with candidate, artifact, environment,
   entrypoint and producer evidence.
5. Frozen source and archive receipt with byte-for-byte digest equality and a
   verified immutable archive reload.
6. A new target epoch, old-epoch fencing, and native identity/runtime/scope/
   endpoint-generation rebind for every active entity.

The correct order is:

```text
fresh inspect
→ operator authorization
→ acquire lease
→ freeze legacy writers
→ immutable archive
→ old epoch fencing
→ fresh epoch
→ direct/adapt/reset/unknown mapping and reconciliation
→ native identity/runtime/scope rebind
→ projection rebuild
→ verify
→ resume
```

The target epoch must not inherit old PASS, review, delivery, freeze, master
grant, notification or binding state. `agent_id` is the stable logical
identity; a new process receives a new `runtime_id`, the native AppServer gets
a `binding_id`, and reconnect increments `endpoint_generation`. TUI may use a
verified tmux pane as wake evidence. Desktop uses its native AppServer endpoint
and never invents a tmux session. Desktop/codexapp does not register a goal
subscription.

Scopes are checked independently during rebind:

| AppServer relation | cwd relation | Scope result |
| --- | --- | --- |
| same | same | same app and project scope |
| same | different | same app scope, different project scope |
| different | same | same project, different communication scope |
| different | different | different app and project scope |

Active records are admitted only after the graph
`agent_id ↔ runtime_id ↔ binding_id ↔ endpoint_generation` and its task,
worktree, claim, app scope, project scope and canonical cwd edges are proven.

## Reconciliation rules for current facts

### Bugs

Query the owning AppSDK/git-bug system at migration time and preserve its
authoritative immutable ID. Active bugs become inputs to the master priority
queue. P0 blocks its project and generates one urgent latest-state notice;
normal bug notices join the two-minute aggregate. Cross-project work is
registered through the bug system before the target master is notified. A
historical mailbox or notification row never creates a duplicate bug.

### Tasks, worktrees and branches

Import an open task only when one current owner, one worktree/branch, one
project scope and a recoverable evidence path are all proven. Tasks tied to a
lost identity, dirty candidate, unresolved merge, unknown delivery or broken
claim/worktree relation remain archive-only. The fresh master may redispatch
only after a fresh runtime accepts a new reservation; the old ID is not
silently reassigned. No branch is deleted, reset, squashed, merged or cleaned
by migration. An authorized integration pass is a separate operation with its
own candidate, review, merge and delivery receipts.

### Goals

Import a goal only after its current owner, plan file, project scope, runtime
binding and target epoch are confirmed. A local `active` or `subscribed` field
does not prove remote registration. A failed or missing subscription ID is a
blocked fact with its exact error. Desktop does not call `goal subscribe`.

## Projection and notification rebuild

The target journal is the source for rebuildable projections. Mailbox JSONL is
append-only raw history; each record includes event ID, time, project/app
scope, sender, recipient, entity, title, priority, state/process/result kind,
operation ID and source/archive reference. The visible notification view is a
latest-state projection keyed by `(from, to, entity)` and presents only the
latest title, time, priority and JSONL pointer. `sendmessage` remains
immediate. Idle, progress, delivery, bug and worker-idle notices aggregate and
send at most once per two minutes; P0 interrupts immediately. Master-idle
probes are event-driven and stop after three reminders without a `working`
transition. Workers do not receive idle wakeups.

Projection failure does not convert a committed fact into a failed operation
and does not mark a notice delivered. Record `repair_needed`, rebuild from the
journal and block the affected admission or close gate. Verify that every
visible item points to a committed target fact, every latest-state key is
unique, raw JSONL counts/digests match the manifest, and no notice is mistaken
for execution, delivery or read acknowledgement.

## Error, reset and rollback handling

The migration controller has four outcomes: `success`, `failure`, `unknown`
and `unsupported`.

- `failure` is an authoritative reducer rejection. Preserve the exact error;
  repair uses a new attempt ID and expected revision.
- `unknown` includes timeout, socket close, daemon loss, ambiguous append,
  missing owner or unverified side effect. Look up the original operation once
  by its idempotency key. Do not blindly resend.
- `unsupported` requires an explicit native “method unavailable” result. A
  timeout is not unsupported and cannot trigger a fallback.
- Journal open/append/sync/replay failure poisons the writer. No wake,
  admission, projection or close follows until the first failed boundary is
  reconciled.

If replay cannot safely prove a verified prefix, the reset/rebuild route is:

```text
fresh read-only inspect
→ operator admission
→ one migration lease
→ freeze every legacy writer
→ immutable archive and byte/digest verification
→ mark old epoch archived/blocked
→ create fresh target epoch and one writer
→ replay only verified direct/adapt records
→ retain reset/unknown records archive-only with exact error and boundary
→ reconcile confirmed bugs, tasks and goals
→ rebind native runtimes, scopes and endpoint generations
→ rebuild mailbox JSONL and latest-state notification view
→ verify counts, sequence, scope, permissions, P0 and one-writer invariants
→ controlled resume
```

For rollback, freeze the target epoch, mark it `superseded/aborted`, revoke
its bindings and stop projections. Reconcile every fact after the epoch
boundary and compare-and-set the active epoch using the source epoch, target
epoch, expected active revision, fencing token, command/operation ID and
reconciliation digest. A changed epoch, revision or token rejects the switch.
Unknown external effects keep admission stopped for operator decision.

## Project-specific plan

**Collab:** preserve the dirty v2 root, all unmerged worktrees and legacy
records. Resolve duplicate `collab serve` writers only after the lease and
freeze. The current round provides one closed worker projection and four
absent keepalive projections; it does not establish a live master or current
worker liveness. Keep old tasks/claims archive-only until current native
endpoints are re-registered and a new user-approved master grant is obtained.

**AppSDK:** preserve the five unresolved paths, quality records, bug IDs and
the failed goal reconciliation. The AppSDK owner resolves conflicts before
admitting affected candidates or tasks, but does not need to repair them before
an authorized preservation archive. AppSDK quality records remain under
AppSDK ownership; Collab receives immutable references.

**RouteCodex:** keep V3 production evidence separate from V4 design/refactor
experiments. Preserve the dirty root and 378 unmerged branches until each is
classified. The current keepalive projection is 22 absent, four idle, one
unknown and one working; task edges remain archive-only until a current
owner/runtime is proven. The earlier 23-lost/two-unknown result is a
prior-round clue only. RouteCodex quality reset is a separate authorized
operation, never a Collab migration shortcut.

**codexapp:** treat the project as a transport bootstrap. Prove native
AppServer initialize/capability handshake, a successful communication socket
listener, endpoint generation, project cwd and explicit registration before
any active import. Archive the 54-line external journal first. Do not import
mock registries, mock adapters, role/scheduler state or in-memory receipts.

## Unresolved P1 prerequisites

The following remain open and block an active migration claim:

- M1 journal/replay candidate and deterministic replay evidence.
- Reachable, integrated migration contract in the reviewed target build.
- One-writer fencing across the 21-process host census.
- Frozen source receipt and immutable archive receipt with byte equality.
- Archive reload and typed manifest validation.
- Native AppServer initialize/capability/runtime/binding rebind for codexapp,
  TUI and Desktop where applicable.
- Fresh operator-approved master grants and active bug/task/goal ownership
  reconciliation.

These are runtime and admission blockers. A historical missing owner, stale
worktree, lost runtime, unresolved merge or unknown delivery is instead
recorded at record level as archive-only with an exact error and boundary.

## No-action record for this refresh

This refresh did not:

- call status CLIs that append live request events;
- stop or start any daemon or process;
- acquire a migration lease or freeze a writer;
- write, truncate, rename, delete or hand-edit any live journal, event,
  mailbox, claim, identity, task, goal, PID, lock or socket;
- archive, reset, replay, rebind or migrate any record;
- create, close, reprioritize or reassign a bug/task/goal;
- merge, push, reset, clean or delete a branch/worktree;
- send, retry, ACK or consume a notification.

The next operation, if authorized, starts with a fresh inspect. It must not
reuse this evidence after the stated expiry or after any source, process,
branch, worktree, build, socket or goal revision changes.
