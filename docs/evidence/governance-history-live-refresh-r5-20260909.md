# Governance history live refresh r5

This document is the fresh, read-only field record for a possible migration of
the Collab, AppSDK, RouteCodex and codexapp governance histories to the Collab
v1 host-wide daemon. It is an evidence input for a later operator-controlled
migration. It is not an archive receipt, reset receipt, replay receipt,
identity rebind, or migration admission.

No live archive, reset, replay, rebind, daemon restart, installation, branch
merge, push, worktree cleanup, task reassignment, goal operation, or
notification was performed for this refresh.

## Capture boundary and evidence validity

The main collection window was `2026-09-09T23:30:19Z` through
`2026-09-09T23:30:21Z`. The exact read-only transcript is retained at
`/private/tmp/unmerged-branch-inventory-r2-fresh-inspect-20260909-raw.log`.
It has 184 lines, 8,637 bytes, and SHA-256
`f89b816552ed32c765fc235196d40e3cd0328396145a1c3183940f09dbca8f91`.
The candidate identity is in raw lines 1-5, Git observations are in lines
6-37, source JSONL counts and digests are in lines 38-55, mailbox and claims
inventory is in lines 56-68, PID/socket observations are in lines 69-76,
goal projections are in lines 77-134, and the codexapp protocol projection is
in lines 135-183.

The raw transcript is a record of commands and their output, not a frozen
source snapshot. The source files were mutable while the broader inspection
was being prepared: AppSDK journal advanced from 48,156 to 48,429 and then
48,447 lines; AppSDK events advanced from 5,244 to 5,300; RouteCodex journal
advanced from 39,431 to 39,495; RouteCodex events advanced from 26,251 to
26,277; Collab events advanced from 476 to 478; and mailbox inventories also
changed between enumerations. The values below are the final bounded capture,
not a claim of a stable prefix. A new inspect and operator-controlled freeze
are mandatory immediately before any archive, lease, writer fencing, or
replay.

## Project-level migration disposition

Project disposition answers whether a project can currently enter active
migration. It is separate from the classification of an individual preserved
record.

| Project | Canonical source | Disposition | Evidence and reason | Next gate |
| --- | --- | --- | --- | --- |
| Collab | `/Volumes/extension/code/collab` | `reset_required` | Dirty v2 root; 161 worktrees; 145 branches not merged to local `main`; source projections drifted; 16 matching `collab serve` processes mean one global writer is unproven; no complete current identity/runtime/binding/endpoint-generation graph. | Fresh inspect, operator authorization, one migration lease, legacy writer freeze, immutable archive, fresh epoch, and explicit native identity rebind. |
| AppSDK | `/Users/fanzhang/Documents/github/appsdk` | `reset_required` | Dirty checkout with five unresolved Git paths; 102 worktrees; 43 branches not merged; active blocked P0 bugs and a working scheduler task remain; goal control is recovery-required with an exact subscription/reconciliation error; source projections drifted. | Preserve quality and bug references, freeze a new source snapshot, then reconcile only confirmed owner/runtime/worktree edges in a new epoch. |
| RouteCodex | `/Users/fanzhang/Documents/github/routecodex` | `reset_required` | Dirty recovery root; 75 worktrees; 378 branches not merged; many blocked/verifying task groups; source projections drifted; local legacy goal state is not global migration proof. | Preserve V3/V4 histories separately, classify stale or orphan records archive-only, then import only fresh owner/scope/runtime/evidence edges. |
| codexapp | `/Users/fanzhang/Documents/github/codexapp` plus `/Users/fanzhang/.codex-communication` | `needs_operator` | No Git checkout, no verified Collab identity/runtime binding, only an external transport journal, and no successful native listener/capability/initialize receipt. | Operator must bootstrap and verify the native AppServer endpoint, archive the external journal, and explicitly register a new project identity. Desktop does not register a goal subscription. |

`reset_required` means safe active replay is not proven. It does not mean a
reset occurred, old facts were deleted, or a target epoch exists.
`needs_operator` means an external authorization or runtime bootstrap is
missing. Neither disposition is a completion state.

## Source and Git observations

These are direct read-only observations from each canonical cwd. `status_rows`
is the number of `git status --porcelain=v1` rows. The staged and unstaged
counts come from the corresponding path lists; a path may occur in both.
`unresolved` counts unique unmerged paths. Worktree and branch counts are
inventory facts, not merge decisions.

| Project | Branch / HEAD | Tree | `origin/main` | Status rows | Staged | Unstaged | Untracked | Unresolved | Worktrees | Branches | Unmerged to local `main` |
| --- | --- | --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| Collab | `codex/v2-cordis-architecture` / `064824c375a720450c43f4829679bb8e45d4a1d1` | `7f0bfc31628af30604a3b10e13098ac166bcd236` | `d7ac749dbc6c7af752c889c67ec4dce662a0fc68` | 38 | 1 | 4 | 667 | 0 | 161 | 162 | 145 |
| AppSDK | `chore/project-memory-snapshot` / `2d14efed9d7f6454d119cb7a1aea24a384e966e0` | `728960b0e100201045317a28e740d0e58e0558d4` | `8fa6be2412b9f4e84fcd296d66c96e8849d8723b` | 7 | 7 | 9 | 0 | 5 | 102 | 120 | 43 |
| RouteCodex | `codex/root-dirty-recovery-0908` / `d876adea1d5c57a73cf643f5c8d89b56bd3d42c4` | `88edaa554f16ff17cd4e28bc49ac52c5fc2a84ae` | `6e86a7e606cae8d3e0915ce9b788d6ef2bdaf72b` | 49 | 38 | 19 | 4 | 0 | 75 | 620 | 378 |
| codexapp | no Git checkout | n/a | n/a | n/a | n/a | n/a | n/a | n/a | n/a | n/a | n/a |

The candidate worktree itself is included in the Collab worktree count. The
candidate was created from integration HEAD `247bfdb38010ba22144a14bb95a1ec3bf2797feb`;
its initial tree was `0e392dc62a5673e3c082775720eee24c364a85e6`.

### Durable source digests

All inspected JSONL files passed the framing/parsing probe. A valid JSONL
frame does not establish typed schema validity, sequence continuity,
complete owner relations, scope, runtime binding, or a safe replay prefix.

| Project | Source | Lines | Bytes | SHA-256 |
| --- | --- | ---: | ---: | --- |
| Collab | `.agent-collab/server/journal.jsonl` | 72 | 20,871 | `c47e0f95b388809274ae2c43b8a23289e3d92e6afa6a61ea21033a7491fffc83` |
| Collab | `.agent-collab/server/events.jsonl` | 478 | 77,301 | `501cb177fbae3e822d046d6c3e0f897612721b1b9bb4ceb3f50c19f4f59cf9d1` |
| AppSDK | `.agent-collab/server/journal.jsonl` | 48,447 | 14,571,977 | `ca1bde6588224eaf8706050d9e95ccac095de14d248bfd2d6a6b8b2019bca16d` |
| AppSDK | `.agent-collab/server/events.jsonl` | 5,300 | 1,373,464 | `0623b8fdb7b4583e810d0c78bb245da3009e68a123a1f8e347f9357fd6e02f40` |
| RouteCodex | `.agent-collab/server/journal.jsonl` | 39,495 | 8,623,815 | `d38626897916682a6d491367de2564eada6119eccc1f99fe13c985cd91469390` |
| RouteCodex | `.agent-collab/server/events.jsonl` | 26,277 | 7,787,438 | `264aca654e42a754e5662b06936be933a3aa29d0209ea9032d32332da78b1521` |
| codexapp | `/Users/fanzhang/.codex-communication/journal.jsonl` | 54 | 61,245 | `c3d995d870cd45a3887c1f3eddba4e2a214db6b60158f3b037ed4fc826f485a4` |

Mailbox and claims were captured as sorted per-file digest lists. They are
inventory evidence, not immutable archive receipts:

| Project | Mailbox rows | Mailbox digest-list SHA-256 | Claims rows | Claims digest-list SHA-256 |
| --- | ---: | --- | ---: | --- |
| Collab | 7 | `6fe9bba474b61d0571dec0747f795c2395dbbf832f70f63c4e4638f2f6b1dcb7` | 2 | `856e9e5fa38f767a2d0a38201899c0d815ba2d4bd57e01236d2eb6595003472c` |
| AppSDK | 559 | `e6135c72113e850435e75d6d6811f52726bab1d18bf44c1c6f1a8c5eef24e093` | 3 | `e43815a502c9f85e99f1d0f8f71db3b3473e05b998fa674fa9200b03d7721` |
| RouteCodex | 3,665 | `49d79aaab2d32b2da2e20a1b1aa8cc1e263593fcae2c8dc9047478925dcd6b5c` | 37 | `803eb6e0234f0270c9914316e0fa6fa2daa0a6959b939c9a73d575808b0583ca` |

The AppSDK mailbox count was 559 in the final digest capture; an earlier
quick enumeration saw 556. This is direct evidence of source mutability.

### Journal and event projections

The bounded journal projections contained these event counts. They describe
historical rows and do not establish current liveness or authorization.

| Event type | Collab | AppSDK | RouteCodex |
| --- | ---: | ---: | ---: |
| `Registered` | 19 | 1,286 | 10,387 |
| `SubagentUpdated` | 14 | 14,461 | 521 |
| `KeepaliveUpdated` | 9 | 15,615 | 7,928 |
| `TaskCreated` | 2 | 17 | 110 |
| `TaskUpdated` | 2 | 59 | 552 |
| `MasterWakeSignal` | 3 | 13,851 | 122 |
| `WakeAttempted` | 0 | 394 | 3,352 |
| `WakeBound` | 3 | 504 | 3,552 |
| `Sent` | 4 | 552 | 3,650 |
| `Delivered` | 2 | 703 | 3,897 |
| `Acked` | 2 | 319 | 2,586 |
| `NotificationConsumed` | 0 | 29 | 115 |
| `NotificationSubscribed` | 3 | 20 | 72 |
| `NotificationStatus` | 3 | 14 | 76 |
| `WorkerClosed` | 0 | 4 | 3 |
| `SchedulerAdmission` | 0 | 1 | 0 |
| `SchedulerAdmissionStatus` | 0 | 1 | 0 |
| `TaskLifecycleUpdated` | 0 | 1 | 0 |
| `Superseded` | 0 | 3 | 2 |
| `CleanupVerified` | 1 | 14 | 35 |
| `DeliveryMode` | 4 | 392 | 2,515 |
| `MigrationUpdated` | 1 | 0 | 9 |

The event projection `kind` counts were: Collab `request=295`,
`daemon_down_requested=46`, `daemon_restart_requested=47`, `daemon_start=45`,
`daemon_up_requested=45`; AppSDK `request=3,772`,
`mailbox_projection_error=1,095`, `daemon_down_requested=110`,
`daemon_restart_requested=104`, `daemon_start=101`,
`daemon_up_requested=116`, `protocol_error=1`, `scheduler_admission=1`; and
RouteCodex `request=25,981`, `daemon_down_requested=58`,
`daemon_restart_requested=86`, `daemon_start=66`, `daemon_up_requested=70`,
`protocol_error=14`, `scheduler_admission=2`. The projection errors and
protocol errors are preserved blockers, not evidence of successful recovery.

## Runtime, identity and scope observations

The project projections mainly carry worker ID, pane, cwd, session,
parent/peer, and occasionally profile/runtime labels. They do not establish a
complete typed relation:

```text
agent_id ↔ runtime_id ↔ binding_id ↔ endpoint_generation ↔ scope
```

No reliable current `runtime_id`, `binding_id`, or `endpoint_generation` was
found in the governance projections. Session IDs and pane values are
historical binding clues and cannot be reused as migration authorization.
Every active entity must register a fresh target identity and endpoint
generation. Fork, compaction, reconnect, and native AppServer replacement
must advance runtime/binding/generation records rather than silently reuse a
session ID.

The exact PID files and matching processes were:

| Project | PID | Process | PID-file SHA-256 |
| --- | ---: | --- | --- |
| Collab | 34612 | `/Users/fanzhang/.cargo/bin/collab serve` | `0847553f2f56600478dcaf636bf04647f6f6d96fe6d6f8e6987be2e34b06ae30` |
| AppSDK | 56952 | `/Users/fanzhang/.cargo/bin/collab serve` | `0db511f8d2b0a75615027758ac8df327761ca0d8dc4f4dfc296cfa31bb0fc062` |
| RouteCodex | 57862 | `/Users/fanzhang/.cargo/bin/collab serve` | `1fa85b00cf11529c3ac9838ca3934cb4d3520ea3fe8bfc4c9ec9f8b8da053915` |

The exact process-path census found 16 matching `collab serve` processes. It
does not prove 16 writers: process identity and writer ownership are separate
facts. It does prove that one host-wide writer is not established by the
current state.

Canonical sockets were present with these observations:

| Project | Socket | Mode | Mtime |
| --- | --- | --- | --- |
| Collab | `/Volumes/extension/code/collab/.agent-collab/server/server.sock` | `srw-------` | `Sep 8 18:33:18 2026` |
| AppSDK | `/Users/fanzhang/Documents/github/appsdk/.agent-collab/server/server.sock` | `srw-------` | `Sep 9 08:31:37 2026` |
| RouteCodex | `/Users/fanzhang/Documents/github/routecodex/.agent-collab/server/server.sock` | `srw-------` | `Sep 9 06:50:09 2026` |
| codexapp | `/Users/fanzhang/.codex-communication/sockets/commd.sock` | `srwxr-xr-x` | `Sep 8 23:12:59 2026` |

Socket presence, PID files, and lock files do not prove a valid owner,
listener capability, or one-writer fence.

## Current tasks and goals

The latest Collab task projection contains only closed tasks:
`post-restart-smoke` and `task-m1788917856798-2`. Latest keepalive
projections are absent/offline for the observed historical workers.

AppSDK has three non-closed task projections relevant to migration:

- `bug-6369e0-cross-project-send-20260908`: P0, blocked, owner
  `appsdk-subagent-p0-cross-project-send-20260908`, worktree
  `./playground/xsend-0908`, branch `codex/p0-cross-project-send-20260908`.
- `bug-ead2041-gcm-timeout-20260908`: P0, blocked, owner
  `appsdk-subagent-gcm-timeout-0908`, worktree
  `./playground/gcm-timeout-0908`, branch `codex/gcm-timeout-0908`.
- `task-scheduler-appsdk-90a4cd5-complete-producer-r3-0909`: P0, working,
  owner `appsdk-1`, with no worktree recorded.

RouteCodex has non-closed task groups with these priority/state counts:

| Priority/state | Count |
| --- | ---: |
| P0 blocked | 9 |
| P0 cancelled | 2 |
| P0 delivered | 2 |
| P0 merged | 1 |
| P0 verifying | 1 |
| P1 accepted | 3 |
| P1 blocked | 9 |
| P1 verifying | 22 |
| P1 working | 2 |
| P2 assigned | 4 |
| P2 blocked | 8 |
| P2 delivered | 2 |
| P2 merged | 1 |
| P2 reviewed | 1 |
| P2 verifying | 1 |
| P2 working | 7 |

Representative blocked P0 tasks are `routecodex-appsdk-collab-unify`,
`v3-counter-permission-0906`, `v3-goaichat400-root-0906`,
`v3-live-session502-0906`, `v3-multimodal-vr-regression-20260904`,
`v3-thinking-effort-0830`, `v3-toolreason-live-0830`,
`v4-cordis-governance-master-takeover-0906`, and
`v4-cordis-m1-governance-records-0906`.

The AppSDK goal control file
`/Users/fanzhang/Documents/github/appsdk/.appsdk-control/long-task-goal.json`
has SHA-256
`b7f09b518a250ca3d120429487462034ffe1f8ec2019b3c50d5479a5bb817386` and
reports `active=false`, `desired=recovery_required`, `observed=unknown`,
`remote_state=unknown`, interval `10m`, revision `5`, and goal ID
`sha256:27ef0e42dbe1551b4d54ea68751b1d595d4f88f164c8702ccf4e9938c44ca762`.
The exact error is:

```text
GOAL_STATUS_SUBSCRIPTION_ID_MISSING; GOAL_STATUS_SUBJECT_RECONCILIATION_FAILED:GOAL_RECONCILE_COLLAB_FAILED:exit=1
```

The local subscription-shaped fields are inconsistent with `active=false` and
the reconciliation error; they are not proof of a successful remote
subscription. The RouteCodex goal file
`/Users/fanzhang/Documents/github/routecodex/.appsdk/goal.json` has SHA-256
`2800abc344229a04fbb7ec764837e1c38b0ccff9ed80867d0ab92f776b48cba5`, reports
`goal-routecodex-appsdk-migration` as `confirmed` by `Jason` at
`2026-08-13T00:00:00Z`, and remains project-local legacy state. Collab has no
goal file; `.agent-collab/collab.json` contains only
`{"continuation_minutes":1}`. Desktop/codexapp must not register a goal
subscription.

The external codexapp journal has these counts: `scope.registered=6`,
`agent.registered=7`, `agent.refreshed=14`, `session.status=2`,
`message.state=22`, and `message.failed=3`. Historical failures preserve
`reply_timeout`, including message IDs `msg-16d32a1c-23b3-4071-9da6-8fd9808a98fd`
and `msg-88a1b0a0-0dd8-475a-bf1d-8a1ecc302185`. No native initialize or
capability receipt was verified, so codexapp remains `needs_operator`.

## Record mapping and notification policy

The four accompanying `manifest-r5.json` files are S1 planned drafts. They
use the schema in `docs/migration-v1-history-manifest.schema.json`, keep every
target sequence/entity/archive field null, and never claim `mapped`,
`verified`, archived, replayed, or imported. The schema has no
`source_disposition` field; the archive-only disposition is therefore
expressed by the planned status plus the exact blocker and first failed
boundary, rather than by adding an invalid property.

For the eventual operation:

- complete typed records with a verified owner, scope, runtime, worktree and
  deterministic outcome may become `direct` only after a target append receipt;
- shape-compatible but control-incomplete records may become `adapt` only
  after reconciliation;
- missing owner/runtime, dirty candidate, unresolved merge, stale claim,
  unknown delivery, or unverified historical PASS remains `reset` and
  archive-only;
- ambiguous timeout/socket/side-effect outcomes remain `unknown` and
  archive-only until one authoritative idempotency lookup and operator
  decision.

Mailbox is append-only JSONL fact history. Visible notifications are a
latest-state projection keyed by `(from, to, entity)` and show only the latest
title, time, priority and JSONL pointer. `sendmessage` is immediate. Idle,
progress, delivery, bug and worker-idle notices aggregate at most once per two
minutes; P0 interrupts immediately. Master-idle probes are event-driven and
stop after three reminders without a `working` transition. Workers do not
receive idle wakeups. Projection failure records `repair_needed`; it does not
turn a committed fact into a delivered notice or a successful operation.

## Migration and reset plan

Active migration requires, in one attempt:

1. Explicit operator authorization naming projects, source roots, target build,
   archive destination and allowed operation.
2. One migration lease bound to the frozen source digest, source epoch, target
   build, canonical cwd and fencing token.
3. Legacy writers frozen with acknowledgements, one target writer, and one
   reducer.
4. Reviewed candidate, artifact, environment, entrypoint and producer
   evidence for the target build.
5. Immutable archive receipt with byte-for-byte digest equality and verified
   reload.
6. A fresh target epoch with old-epoch fencing and native identity/runtime/
   scope/endpoint-generation rebinds.

The operation order is:

```text
fresh inspect
→ operator authorization
→ acquire lease
→ freeze legacy writers
→ immutable archive
→ fence old epoch
→ create fresh epoch
→ map direct/adapt/reset/unknown records
→ rebind native identities, runtimes, scopes and endpoint generations
→ rebuild projections
→ verify counts, permissions, P0 and one-writer invariants
→ controlled resume
```

If a safe verified prefix cannot be proven, use the reset/rebuild route:
freeze every writer, archive the old epoch, create one fresh writer and fresh
epoch, replay only verified direct/adapt records, retain reset/unknown records
archive-only with exact errors, reconcile confirmed bugs/tasks/goals, rebind
native endpoints, rebuild JSONL/latest-state views, and verify before resume.
Rollback freezes the target epoch, marks it superseded/aborted, revokes its
bindings, and compare-and-sets the active epoch using the expected revision,
fencing token, operation ID and reconciliation digest. A changed revision,
epoch or token rejects the switch.

## Comparison with prior r4

The project classifications are unchanged: Collab, AppSDK and RouteCodex
remain `reset_required`; codexapp remains `needs_operator`. The new facts are
more restrictive because the source moved during inspection and the roots
remain dirty.

| Project | r4 worktrees | r5 worktrees | Change | r4 unmerged | r5 unmerged | Change |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Collab | 151 | 161 | +10 | 135 | 145 | +10 |
| AppSDK | 101 | 102 | +1 | 42 | 43 | +1 |
| RouteCodex | 74 | 75 | +1 | 378 | 378 | 0 |
| codexapp | no Git | no Git | unchanged | n/a | n/a | n/a |

The r4 process recheck described 21 matching processes; this r5 refresh uses
the corrected exact path filter and found 16. The earlier count was a shell
search artifact and is not reused. The r5 source digests are new observations;
none is an immutable archive receipt.

## Blocking prerequisites

The following block any active migration claim:

- a frozen source snapshot and immutable archive receipt with byte equality;
- one-writer fencing across the exact process census;
- a reachable reviewed migration contract and deterministic replay gate;
- archive reload and schema validation on copied input;
- fresh native AppServer initialize/capability/runtime/binding evidence for
  codexapp, TUI and Desktop where applicable;
- fresh operator-approved master grants and bug/task/goal ownership
  reconciliation;
- exact candidate, branch, worktree, review, delivery and merge evidence for
  any task considered for active import.

## No-action record

This refresh did not call status CLIs that append live request events; stop or
start a daemon; acquire a lease; freeze a writer; write, truncate, rename or
delete any live journal, event, mailbox, claim, identity, task, goal, PID,
lock or socket; archive, reset, replay, rebind or migrate a record; create,
close, reprioritize or reassign a bug/task/goal; merge, push, reset, clean or
delete a branch/worktree; or send, retry, ACK or consume a notification.

The next authorized operation must begin with a new read-only inspect and must
not reuse this evidence after source, process, branch, worktree, build,
socket, goal, or revision changes.
