# Governance history live refresh r3

This is a read-only refresh for the planned migration of Collab, AppSDK,
RouteCodex and codexapp to one Collab v1 host daemon. It records observations
only. It does not acquire a migration lease, freeze a writer, archive or reset
history, replay records, rebind identities, restart a daemon, install a
binary, merge or push a branch, or clean a worktree.

## Capture and validity

The refresh ran in a bounded window beginning at `2026-09-09T22:05:32Z` and
ending at `2026-09-09T22:15:41Z`. The JSONL files, process table, socket and
goal projections are mutable; this evidence expires at
`2026-09-09T22:45:41Z` and must be refreshed again immediately before any
operator-authorized migration operation. A digest in this document is an
observation of the bytes read by the corresponding command, not an admission
that those bytes are safe to replay.

## Git and worktree observations

All three Git roots were dirty during the refresh. Counts are read-only
projections from the exact canonical cwd and do not select a merge candidate.

| Project | Canonical cwd | Branch / HEAD | `origin/main` | Status rows | Worktrees | Branches not merged to local `main` |
|---|---|---|---|---:|---:|---:|
| Collab | `/Volumes/extension/code/collab` | `codex/v2-cordis-architecture` / `064824c375a720450c43f4829679bb8e45d4a1d1` | `d7ac749dbc6c7af752c889c67ec4dce662a0fc68d` | 38 | 146 | 130 |
| AppSDK | `/Users/fanzhang/Documents/github/appsdk` | `chore/project-memory-snapshot` / `2d14efed9d7f6454d119cb7a1aea24a384e966e0` | `8fa6be2412b9f4e84fcd296d66c96e8849d8723b` | 7 | 101 | 42 |
| RouteCodex | `/Users/fanzhang/Documents/github/routecodex` | `codex/root-dirty-recovery-0908` / `d876adea1d5c57a73cf643f5c8d89b56bd3d42c4` | `6e86a7e606cae8d3e0915ce9b788d6ef2bdaf72b` | 49 | 74 | 378 |
| codexapp | `/Users/fanzhang/Documents/github/codexapp` | no Git checkout | n/a | n/a | n/a | n/a |

The branch/worktree inventory remains a separate evidence source. The latest
inventory candidate records duplicate heads, stale task/worktree edges and
branches that are checked out in more than one place. No branch is considered
mergeable merely because it is clean or ahead of a local ref; the candidate,
base, review, delivery, owner and exact integration edge must all be proven.

## Durable source observations

| Project | Source | Lines / bytes | SHA-256 observed in this refresh |
|---|---|---:|---|
| Collab | `.agent-collab/server/journal.jsonl` | 72 / 20,871 | `c47e0f95b388809274ae2c43b8a23289e3d92e6afa6a61ea21033a7491fffc83` |
| Collab | `.agent-collab/server/events.jsonl` | 472 / 76,341 | `5ed04323af470f5514a656b324d130af65e5f49a2213f12c8e4c37af479c07ef` |
| AppSDK | `.agent-collab/server/journal.jsonl` | 46,090 / 13,849,049 | `d397bb010d407486ce28487d1e39135222d1e767ce2a06e14e68599535c1eded` |
| AppSDK | `.agent-collab/server/events.jsonl` | 5,240 / 1,353,504 | `6b74a39c8b72a77a5b409f7984731389c7bc9d3f26530569e827f0c028d2946b` |
| RouteCodex | `.agent-collab/server/journal.jsonl` | 39,431 / 8,608,934 | `9dd93d730fa221aa445ebbc5e79e5c8884ebc877b180ff704bcbe8249c941273` |
| RouteCodex | `.agent-collab/server/events.jsonl` | 26,247 / 7,779,697 | `e1b6bcf449451c230c4ca7b16eabc64a5468025efc874d1fe50490aa167e50b5` |
| codexapp | external `/Users/fanzhang/.codex-communication/journal.jsonl` | 54 / 61,245 | `c3d995d870cd45a3887c1f3eddba4e2a214db6b60158f3b037ed4fc826f485a4` |

The AppSDK journal grew from 45,868 to 46,090 lines during the bounded
refresh, and RouteCodex events grew from 26,243 to 26,247. Collab events also
grew from 468 to 472. This drift means there is no stable replay boundary in
the live sources. A fresh frozen digest is mandatory for a future apply.

All three Collab JSONL pairs were readable by the framing probes. Framing
success does not prove typed-schema validity, sequence continuity, complete
owner relations, scope, runtime binding, or a safe replay prefix.

## Runtime and identity observations

| Project | Workers and owner state | Goal / endpoint state |
|---|---|---|
| Collab | 4 registered workers: 4 `lost`; no live master | `collab master status` returned `master=null` |
| AppSDK | 8 workers: 3 `lost`, 4 `waiting`, 1 `working`; two active tasks were `blocked` (`bug-ead2041-gcm-timeout-20260908`, `bug-6369e0-cross-project-send-20260908`) | live master `appsdk-2`; goal control is `active=false`, `desired=recovery_required`, `remote_state=unknown`, with `GOAL_STATUS_SUBSCRIPTION_ID_MISSING` and `GOAL_STATUS_SUBJECT_RECONCILIATION_FAILED:GOAL_RECONCILE_COLLAB_FAILED:exit=1` |
| RouteCodex | 27 workers: 23 `lost`, 2 `unknown`, 1 `waiting`, 1 `working`; active tasks remain attached to lost or unknown identities | live master `routecodex-2`; legacy goal control reports `active=true`, `desired=subscribed`, `remote_state=armed`, `repeat_count=100`; this is not proof of a global migration goal |
| codexapp | no Collab identity, runtime binding, owner, or project scope | external `commd.sock` exists, but the Unix connection probe exited 1; no native initialize/capability/endpoint-generation receipt was observed |

The process census found 20 `collab serve` processes on the host, including
the canonical PIDs `34612` (Collab), `56952` (AppSDK) and `57862`
(RouteCodex). This disproves one-writer admission for the current host. PID,
socket and lock files do not prove ownership; no process was stopped or
modified during this refresh.

## Active bug and goal clues

The AppSDK bug listing contains the existing migration issue `cd864e8` and
related open records for master-idle wake, notification batching and
idempotency, GCM probe timeout, cross-project send, scheduler saturation,
worker close safety, goal subscription identity, and the 0.1.6 migration
contract. These records are the source of priority input for reconciliation;
this refresh did not create, close, reopen or reprioritize any bug.

The migration issue is therefore an existing active fact. A later migration
must query it again, retain its immutable ID and current status, and import it
only after the target project, owner, worktree, runtime and evidence edges are
revalidated. A historical notification or goal receipt is never replayed as a
new wake.

## Classification for the next migration plan

| Project | Current disposition | Evidence-based reason | Required next action |
|---|---|---|---|
| Collab | `reset_required` | dirty v2 root; 146 worktrees and 130 unmerged branches; all registered workers lost; no live master; 20 host writers; no proven v1 scope/binding/generation/epoch | freeze only after a fresh inspect and operator lease; preserve the old epoch immutably, then create a fresh target epoch and rebind identities |
| AppSDK | `reset_required` | dirty/unresolved checkout; journal drift during inspection; mixed live/lost workers; active blocked bugs; goal state is recovery-required | preserve AppSDK quality records and bug IDs as references; reconcile only from a clean, frozen source and a fresh target epoch |
| RouteCodex | `reset_required` | dirty V3/V4 recovery root; 74 worktrees and 378 unmerged branches; 23 lost and 2 unknown workers; orphaned active tasks; event drift; legacy goal is project-local | retain V3/V4 history separately; classify orphan tasks archive-only; import only explicitly confirmed active facts |
| codexapp | `needs_operator` | no Git/Collab source; only an external transport journal; socket has no successful listener; no native capability or identity binding | operator must bootstrap and verify the native AppServer endpoint, archive the external journal, then register a new peer binding; Desktop cannot register a goal |

`reset_required` means safe active replay cannot currently be proved. It does
not mean reset has happened. Missing owners, lost runtimes, dirty worktrees,
unknown delivery and merge conflicts remain preserved as `archive-only` facts;
they do not justify inventing an owner or deleting the source.

## Reset/rebuild route

If a future frozen inspect still cannot prove a verified replay prefix, the
approved rebuild route is:

```text
fresh read-only inspect
→ explicit operator admission
→ acquire one migration lease
→ freeze every legacy writer
→ immutable archive and byte/digest verification
→ mark old epoch archived/blocked
→ create a fresh target epoch and one writer
→ replay only verified direct/adapt records
→ keep unknown/reset records archive-only with exact error and first boundary
→ reconcile confirmed active bugs, tasks and goal with new owner/runtime/scope
→ rebind TUI/Desktop runtime and endpoint generations
→ rebuild mailbox JSONL and latest-state notifications
→ verify counts, sequence, scope, permissions, P0 policy and one writer
→ resume admission
```

The new epoch does not inherit old PASS, review, delivery, freeze, master
grant, notification or binding state. A timeout, socket close, ambiguous write,
missing owner or digest drift stops the operation as `unknown` or
`needs_operator`; it never authorizes a blind retry. No live archive/reset,
replay, rebind, daemon cutover, installation, restart or cleanup was performed
by this refresh.
