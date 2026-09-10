# Governance history live refresh r2

This is a read-only现场审计 candidate for migrating the four named project
histories to the Collab v1 global daemon. It records bounded observations and
does not authorize or perform a lease, archive, reset, replay, identity
rebind, writer transition, daemon restart, cleanup, merge, push, install, or
cutover.

## Collection and provenance

The collection ran from `2026-09-09T21:18:59Z` through
`2026-09-09T21:29:57Z` (UTC). This evidence is planning-only and expires at
`2026-09-09T22:00:00Z`; refresh again immediately before any migration
operation, even if the expiry has not passed. The live JSONL, mailbox and
process projections are mutable, so this is not an immutable migration
snapshot.

| Field | Observed value |
|---|---|
| source repository | `/Volumes/extension/code/collab` |
| audit worktree | `/Volumes/extension/code/collab/playground/v1-governance-history-refresh-r2-20260909` |
| audit branch | `codex/v1-governance-history-refresh-r2-20260909` |
| audit commit at collection | `721494d6f2d647fd210c693723e215eb96d19cf3` |
| audit tree before evidence | `f3b6bcd137a91aa3813eeba81ceb2fb86d50c4c9` |
| raw output | `/private/tmp/governance-history-live-refresh-r2-20260909-raw.log` |
| raw output final size | 9,597 lines; 746,157 bytes |
| raw output SHA-256 | `eb8144d852e52d439f67fb350afd7ab93b19e1dd7e51d7ca208d69f39a581f86` |
| raw index | file census `171-4539`; schema/kinds `4540-4697`; exact-root CLI `4741-5532`; field probes `5534-7417`; goal/bug probes `7420-7579`; codexapp `7582-7635`; aggregates `7638-7662`; closing snapshot `7664-7692`; daemon ownership `7695-7742`; worktrees/branches `7745-9544`; status counts and bug count `9547-9596` |

The audit worktree was created from the requested integration commit and was
clean before this evidence file was added. The candidate contains this one
new evidence file only; the raw log is outside all live project roots.

## Git and worktree observations

`status_entries` is the number of `git status --porcelain=v1` rows;
`staged`/`unstaged` are path projections and may overlap for unresolved paths;
`unresolved` is the unique path count from `git ls-files -u`. Branch counts are
`git branch --no-merged main`, not proof that any branch is safe to merge.

| Project | Canonical cwd | Branch / HEAD | origin/main | Status entries (staged / unstaged / untracked / unresolved) | Worktrees | Branches not merged to main |
|---|---|---|---|---:|---:|---:|
| Collab | `/Volumes/extension/code/collab` | `codex/v2-cordis-architecture` / `064824c375a720450c43f4829679bb8e45d4a1d1` | `d7ac749dbc6c7af752c889c67ec4dce662a0fc68d` | 671 (1 / 4 / 667 / 0) | 136 | 120 |
| AppSDK | `/Users/fanzhang/Documents/github/appsdk` | `chore/project-memory-snapshot` / `2d14efed9d7f6454d119cb7a1aea24a384e966e0` | `8fa6be2412b9f4e84fcd296d66c96e8849d8723b` | 7 (7 / 9 / 0 / 5) | 101 | 42 |
| RouteCodex | `/Users/fanzhang/Documents/github/routecodex` | `codex/root-dirty-recovery-0908` / `d876adea1d5c57a73cf643f5c8d89b56bd3d42c4` | `6e86a7e606cae8d3e0915ce9b788d6ef2bdaf72b` | 50 (38 / 19 / 4 / 0) | 74 | 378 |
| codexapp | `/Users/fanzhang/Documents/github/codexapp` | no Git checkout / no HEAD | n/a | Git state not applicable | n/a | n/a |

The three Git roots are dirty, and each local `main` also differs from its
`origin/main` (`Collab` local `main` `ebf8f34602b9a969f02fff5bbbc36834749c18ee`,
AppSDK `7a3407f5bbf326a9a283f074c401a2115d072718`, RouteCodex
`d148af56b7ec840e8638ea6c9e9cbcff3c5ae07b`). No branch was selected for
integration by this audit.

## Durable history and runtime projections

The closing file snapshot is the preferred count/digest for planning. Values
changed during the collection, which is itself evidence of live drift.

| Project | Journal | Events | Mailbox / projections |
|---|---|---|---|
| Collab | `.agent-collab/server/journal.jsonl`: 72 lines, 20,871 bytes; `c47e0f95b388809274ae2c43b8a23289e3d92e6afa6a61ea21033a7491fffc83` | `.agent-collab/server/events.jsonl`: 468 lines, 75,697 bytes; `222b07fa516632369c50a9601db944e3b74e0d143afea4a66783860aa1875452` | mailbox 7 files; claims 2; panes 3; runs 29 |
| AppSDK | `.agent-collab/server/journal.jsonl`: 44,821 lines, 13,457,137 bytes; `446895d867170c737a5c96d240d7457534278c3ec73c550e7ebc9d694ebb7f44` | `.agent-collab/server/events.jsonl`: 5,236 lines, 1,352,860 bytes; `a2f5a68507a711a2dd5d71d1af146fca395fcdba7d07538863ca6a3291f231bf` | mailbox 552 files; claims 3; panes 13; runs 98 |
| RouteCodex | `.agent-collab/server/journal.jsonl`: 39,431 lines, 8,608,934 bytes; `9dd93d730fa221aa445ebbc5e79e5c8884ebc877b180ff704bcbe8249c941273` | `.agent-collab/server/events.jsonl`: 26,243 lines, 7,779,053 bytes; `2c57b294a9cde0158bc0dbe7dbbae138d7a104539a586c56afc5d0cefc724798` | mailbox 3,660 files; claims 37; panes 25; runs 519 |
| codexapp | external `/Users/fanzhang/.codex-communication/journal.jsonl`: 54 lines, 61,245 bytes; `c3d995d870cd45a3887c1f3eddba4e2a214db6b60158f3b037ed4fc826f485a4` | external events file absent | external mailbox absent; socket directory contains `sockets/commd.sock` |

All six Collab/AppSDK/RouteCodex JSONL files passed `jq` framing checks during
the collection. A framing pass does not prove the typed v1 event schema,
complete owner relations, sequence continuity, or safe replay. Journal/event
kind census and field-presence probes show legacy projections with missing or
incomplete migration controls; in particular no `scope`, `project`, `binding`,
`generation`, or `epoch` keys were observed in the relevant journal/event
projections. Existing `cwd`, `owner`, `runtime`, `task_id`, `worktree` and
subscription fields are historical clues, not verified v1 bindings.

## Runtime, owner and notification observations

| Project | Daemon artifact and process | Owner/worker status | Master / goal evidence |
|---|---|---|---|
| Collab | `server.pid` contains `34612`; `ps` shows `/Users/fanzhang/.cargo/bin/collab serve`; PID cwd is `/Volumes/extension/code/collab`; socket is Unix `srw-------`; lock is an empty regular file with unknown holder | `collab who`: 4 workers, all `lost`; all have `identity_valid=false`, `endpoint_live=false`, `pane dead or not found`; no active tasks | `collab master status`: `master=null`; `notify status`: `collab identity requires a live tmux pane` |
| AppSDK | `server.pid` contains `56952`; `ps` shows `/Users/fanzhang/.cargo/bin/collab serve`; PID cwd is `/Users/fanzhang/Documents/github/appsdk`; socket is Unix `srw-------`; lock is an empty regular file with unknown holder | `collab who`: 8 workers: 5 `waiting`, 3 `lost`; active bug tasks include `bug-238c3df-master-idle-wakeup-20260908` (`delivered`), `bug-ead2041-gcm-timeout-20260908` (`blocked`), and `bug-6369e0-cross-project-send-20260908` (`blocked`) | master is `appsdk-2`, live and identity-valid; its wake record has `active_goal_revision=null`, `delivery_state=pending`, idle workers and unresponsive workers. `.appsdk-control/long-task-goal.json` is `active=false`, `desired=recovery_required`, `remote_state=unknown`, exact error `GOAL_STATUS_SUBSCRIPTION_ID_MISSING; GOAL_STATUS_SUBJECT_RECONCILIATION_FAILED:GOAL_RECONCILE_COLLAB_FAILED:exit=1` |
| RouteCodex | `server.pid` contains `57862`; `ps` shows `/Users/fanzhang/.cargo/bin/collab serve`; PID cwd is `/Users/fanzhang/Documents/github/routecodex`; socket is Unix `srw-------`; lock is an empty regular file with unknown holder | `collab who`: 27 workers: 23 `lost`, 2 `unknown`, 1 `waiting`, 1 `working`; active tasks include V3/V4 tasks bound to valid and lost identities, including `task-m1788789987107-39`, `task-m1788961699258-445`, `task-m1788742378277-235`, `task-m1788792436878-82`, `task-m1788742173770-222`, and `task-m1788742664848-8` | master is `routecodex-2`, live and identity-valid; wake record has `active_goal_revision=null`, `delivery_state=pending`, and unresponsive workers. Control goal is `active=true`, `desired=subscribed`, `interval=10m`, `repeat_count=100`, `observed=subscribed`, but this does not prove a valid global migration goal |
| codexapp | no PID or lock source observed; `/Users/fanzhang/.codex-communication/sockets/commd.sock` is a Unix socket `srwxr-xr-x`; `nc -U` and verbose connect both exited 1; no listener appeared in the socket/process probe | no Collab identity, owner, runtime binding, or scope evidence | native app-server processes exist, but no codexapp `commd` listener, native initialize/capability receipt, endpoint generation, or identity registration was observed |

The process census found 20 actual `collab serve` processes across the host at
the probe time, including the three canonical roots and additional playground
or duplicate-root processes. The raw count command also matched its own probe
shell, so the reported 20 excludes that probe shell. This disproves one-writer
admission for the current live host state. PID/cwd and socket existence do not
prove lock ownership or single-writer identity.

The external journal has kinds `agent.refreshed` (14), `agent.registered` (7),
`message.failed` (3), `message.state` (22), `scope.registered` (6), and
`session.status` (2). These are transport history clues; no native Collab
history source was observed for codexapp.

## Active bug and goal clues

The read-only AppSDK bug listing returned 32 open records. Relevant active
records include the migration issue `cd864e8`, goal/subscription failures,
master idle wakeup and notification batching, long-poll starvation, worker
status/closure, cross-project send, task rework, and GCM timeout issues. The
full listing is preserved at raw lines `7509-7577`. The audit did not create,
close, reprioritize, or mutate any bug.

The RouteCodex goal control file currently records an armed periodic
subscription with `interval_ms=600000` and `repeat_count=100`; the AppSDK goal
control file records a failed/recovery-required state despite a historical
`collab_subscribed` projection. Neither control file is imported as a Collab
goal during this audit. Desktop/codexapp is not treated as a goal subscriber.

## Classification and migration decision

| Project | Classification | Evidence-based reason | Required next action |
|---|---|---|---|
| Collab | `reset_required` | dirty v2 root; 136 worktrees and 120 unmerged branches; all four registered workers lost; no live master; 20 host `collab serve` processes; no verified scope/binding/generation/epoch projection; events changed during the audit | preserve the source archive; create a clean candidate; resolve duplicate writers and owner history; obtain operator-controlled archive/reset/fresh-epoch plan |
| AppSDK | `reset_required` | unresolved five-path checkout; 101 worktrees and 42 unmerged branches; journal grew from 44,674 to 44,821 lines during the window; mixed valid/lost workers; active blocked bug tasks; goal state is recovery-required with exact reconciliation errors | preserve AppSDK quality records and active bug/task references; reconcile only from a clean source candidate after owner/runtime decisions |
| RouteCodex | `reset_required` | dirty mixed V3/V4 checkout; 74 worktrees and 378 unmerged branches; 23 lost and 2 unknown workers; active tasks remain bound to lost identities; events changed during the window; 20 host writers; goal identity is not a global migration proof | preserve V3 and V4 histories separately; resolve orphan tasks and owners; require clean candidate and fresh replay/reset plan |
| codexapp | `needs_operator` | no Git/Collab source; external journal only; commd socket has no successful connection/listener and no native initialize/capability/identity receipt | operator must bootstrap and verify the native endpoint, establish runtime/scope binding, then archive and reconcile the external journal before any import |

`reset_required` means safe historical replay cannot currently be proven. It
does not mean a reset was performed. No project received `direct`, `adapt`, or
`unknown` as its current disposition. No historical PASS, ACK, PID, socket,
goal subscription, or screen state was used as migration acceptance.

The reset path remains:

```text
fresh read-only inspect
→ operator admission and writer freeze
→ immutable archive of the old epoch
→ archive manifest and source digest verification
→ fresh target epoch
→ verified-prefix direct/adapt replay only where mapping is proven
→ active bug/task/goal reconciliation with owner/runtime/binding evidence
→ projection rebuild and invariant checks
→ master regrant and controlled verify
```

Unknown or conflicting records stay archived with their exact error and first
failed boundary. No line number may be used as a fabricated source sequence;
no missing owner or runtime may be invented. If the controller cannot prove
archive immutability, single writer, or active-fact ownership, it must stop at
`needs_operator` or `reset_required`.

## Drift from prior evidence

Compared with
[`governance-history-live-refresh-20260909.md`](./governance-history-live-refresh-20260909.md),
whose repair window ended at `2026-09-09T19:56:55Z`, this refresh observes:

- Collab events moved from 457 lines (`d580c616…`) to 468 lines
  (`222b07fa…`); the journal stayed at 72 lines and the live root remains a
  dirty v2 checkout.
- AppSDK journal/events moved from 42,470/5,223 lines
  (`5e6243e7…`/`4880d81c…`) to 44,821/5,236 lines with new digests; the
  journal also changed during this collection, so no stable replay boundary
  exists.
- RouteCodex events moved from 26,238 lines (`090b34f2…`) to 26,243 lines
  (`2c57b294…`); its journal stayed at 39,431 lines but the root still has
  mixed V3/V4 dirty work and orphaned worker/task relations.
- codexapp external journal remains 54 lines with the same digest, while the
  current `sockets/commd.sock` probe still has no successful connection.

The older evidence's classifications are therefore retained or made stricter;
no prior digest is overwritten and no old source snapshot is reused for a live
operation.

## Explicit non-actions and next gate

- No live journal, events file, mailbox, identity, claim, task, PID, lock or
  socket was edited.
- No daemon was started, stopped, restarted, migrated or duplicated.
- No task, bug, worker, claim, mailbox message or notification was closed,
  acknowledged, reassigned or repaired.
- No identity token was printed, copied or rebound.
- No archive, reset, replay, install, push, merge, main replacement or
  production replay was performed.
- The initial three CLI probes run accidentally from the audit worktree
  returned the exact local error `collab: no .agent-collab found in exact
  project root /Volumes/extension/code/collab/playground/v1-governance-history-refresh-r2-20260909; run \`collab init\` there first`.
  They are retained in raw lines `4699-4739` as a candidate-root probe and
  are not evidence about any live project root; the subsequent exact-root
  probes at `4741-5532` are authoritative for this refresh.

The next gate is a new read-only inspect immediately before the migration
controller acquires its lease. It must bind exact source roots, immutable
archive destinations, one writer, target epoch, owner/runtime/scope/binding,
active bug/task/goal relations, and rollback fencing. Only then can an
operator-authorized archive/reset/replay begin.
