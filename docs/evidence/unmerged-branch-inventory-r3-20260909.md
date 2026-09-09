# Unmerged branch and worktree inventory (r3)

## Executive decision

This report is the r3 read-only inventory for migrating the existing
governance histories of Collab, AppSDK, RouteCodex and codexapp to the Collab
v1 architecture. It does not authorize a merge, reset, archive, replay,
identity rebind, daemon restart, install, push, worktree cleanup or goal
subscription.

The current evidence does **not** admit any live project to direct history
replay or automatic branch integration. Collab, AppSDK and RouteCodex remain
`reset_required` at the migration level: their histories are mutable or
owner-incomplete, and a verified replay prefix cannot be proved from this
inventory. codexapp remains `needs_operator`: it has no Git/Collab source and
its external communication socket rejects the bounded listener probe.

The correct next step is a per-project, operator-admitted migration attempt:

```text
fresh inspect -> one migration lease -> freeze writers -> immutable archive
-> classify direct/adapt/reset/unknown -> create a fresh epoch
-> rebind confirmed identities -> replay only a verified prefix
-> rebuild projections -> verify -> separate cutover approval
```

The old history is preserved even when the active projection must be reset.
`reset_required` is a migration disposition, not an instruction to delete
JSONL, mailbox, claims, worktrees, branches or identities.

## Capture boundary and candidate provenance

The r3 inventory files were produced in a bounded window from
`2026-09-09T22:34:54Z` through `2026-09-09T22:40:24Z` (the source files were
written at `15:34:54` through `15:40:24` PDT). Git queries were read-only and
were run against the live canonical roots. The source trees and governance
projections may have changed immediately after capture; these values must be
refreshed immediately before an operator-authorized migration lease.

The audit document is authored in this independent worktree:

| Field | Value |
|---|---|
| Repository | `/Volumes/extension/code/collab` |
| Worktree | `/Volumes/extension/code/collab/playground/v1-unmerged-branch-inventory-r3-20260909` |
| Branch | `codex/v1-unmerged-branch-inventory-r3-20260909` |
| Exact base | `e7954e9fc20c3fffa54d580324323506802cdb3b` |
| Base subject | `test(subagent): shorten bounded probe fixtures` |
| Base tree | `cf2cafeb5dff46c71c1fc7ee5d0c9fff7c3162fc` |
| Allowed write | This evidence file only |

The integration base moved after the previous r2 report. Therefore the r2
counts and its integration commit `48fd3e9a...` are historical evidence and
must not be used as the current baseline. The r3 base above is the only
baseline used for this report.

## Git and worktree inventory

The canonical Git roots observed during the collection were:

| Project | Canonical cwd | Branch / HEAD | `origin/main` | Worktrees | Local branches |
|---|---|---|---|---:|---:|
| Collab | `/Volumes/extension/code/collab` | `codex/v2-cordis-architecture` / `064824c375a720450c43f4829679bb8e45d4a1d1` | `d7ac749dbc6c7af752c889c67ec4dce662a0fc68` | 151 | 152 |
| AppSDK | `/Users/fanzhang/Documents/github/appsdk` | `chore/project-memory-snapshot` / `2d14efed9d7f6454d119cb7a1aea24a384e966e0` | `8fa6be2412b9f4e84fcd296d66c96e8849d8723b` | 101 | 119 |
| RouteCodex | `/Users/fanzhang/Documents/github/routecodex` | `codex/root-dirty-recovery-0908` / `d876adea1d5c57a73cf643f5c8d89b56bd3d42c4` | `6e86a7e606cae8d3e0915ce9b788d6ef2bdaf72b` | 74 | 619 |
| codexapp | `/Users/fanzhang/Documents/github/codexapp` | no Git checkout | n/a | n/a | n/a |

Worktree state and local-main reachability are separate facts:

| Project | Clean | Dirty | Unresolved entries | Worktrees merged into local `main` | Worktrees not merged into local `main` | Worktrees merged into `origin/main` | Worktrees not merged into `origin/main` |
|---|---:|---:|---:|---:|---:|---:|---:|
| Collab | 126 | 25 | 0 | 13 | 138 | 46 | 105 |
| AppSDK | 88 | 13 | 26 | 66 | 35 | 68 | 33 |
| RouteCodex | 28 | 46 | 0 | 27 | 47 | 15 | 59 |

The unresolved AppSDK entries are concentrated in the live root
(`/Users/fanzhang/Documents/github/appsdk`, 14 entries) and
`playground/reset-governance-worktree-identity-20260905` (12 entries). An
unresolved index is a hard stop for direct candidate use. A dirty worktree is
also a hard stop because its uncommitted tree is absent from the candidate
commit, even if the HEAD is reachable from a main ref.

The branch-only reachability projection is:

| Project | Branches merged into local `main` | Branches not merged into local `main` | Branches merged into `origin/main` | Branches not merged into `origin/main` |
|---|---:|---:|---:|---:|
| Collab | 17 | 135 | 54 | 98 |
| AppSDK | 77 | 42 | 80 | 39 |
| RouteCodex | 241 | 378 | 232 | 387 |

RouteCodex has 619 local branch refs but only 74 worktrees. The 378 branches
not merged into local `main` are therefore not an automatic merge queue. Many
are branch-only refs with no current owner or worktree. Repeated HEADs are one
candidate fact and must be deduplicated by issue, owner, review, test and
delivery evidence.

Path shape is another boundary. The worktree inventory includes the normal
`playground/` family plus foreign locations: Collab has two `/private/tmp`
review worktrees; AppSDK has two `/private/tmp` worktrees, three
`~/.codex/worktrees` worktrees and the separate `appsdk-bug-integration`
family; RouteCodex has three `~/.codex/worktrees` worktrees. Detached worktrees
and foreign paths are retained as evidence until their owner and intended
target are proven. They are not silently merged or removed.

### Checked-out branch association repair

The original r3 `branches.full.tsv` derivation had a P1 consistency failure:
its `checked_out` field was `NO` for every branch, while the worktree inventory
contained 279 attached branch rows (Collab 132, AppSDK 86 and RouteCodex 61).
The three `attached.raw` files are empty, so they are retained as raw evidence
but cannot be used as an association source.

The association was rebuilt without reading or changing live governance state:
for each project, the attached rows were taken from its captured
`worktrees.full.tsv` where `branch != DETACHED`, the branch names were sorted
and deduplicated, and matched against the captured branch inventory by exact
branch name. This produced 132 + 86 + 61 = **279** row-level checks. Every
attached row had exactly one branch ref, every corrected row is marked
`checked_out=YES`, and each project's unmatched set was empty. The corrected
branch projections contain the expected residual `NO` counts of 20 (Collab),
33 (AppSDK) and 558 (RouteCodex).

This repairs the inventory projection only. It does not prove task ownership,
claim validity, review, delivery, runtime liveness or merge eligibility. A
branch can be correctly marked checked out and still be dirty, stale, foreign,
ownerless or archive-only.

| Project | Attached rows reconstructed | Unique attached branches | Corrected `YES` | Corrected `NO` | Missing branch refs | Association checks |
|---|---:|---:|---:|---:|---:|---:|
| Collab | 132 | 132 | 132 | 20 | 0 | 132 PASS |
| AppSDK | 86 | 86 | 86 | 33 | 0 | 86 PASS |
| RouteCodex | 61 | 61 | 61 | 558 | 0 | 61 PASS |
| **Total** | **279** | **279** | **279** | **611** | **0** | **279 PASS** |

The prior branch digests remain the digest of the unrepaired derived files;
the corrected files and their row-level checks are the inputs for subsequent
review:

| Project | Original branch full TSV (all `NO`) | Original digest | Corrected branch full TSV | Corrected digest | Association checks TSV | Checks digest |
|---|---|---|---|---|---|---|
| Collab | `/private/tmp/unmerged-branch-inventory-r3-collab-20260909.branches.full.tsv` | `862ad38efacc40d5ab3af8146bd263688a98766769919266c6e5ae28f1003b95` | `/private/tmp/unmerged-branch-inventory-r3-collab-20260909.branches.full.checked-out.tsv` | `44c5ef30350e7bbe619a9fb7d0ee1bd841f3a9af7596e7e752103922cf81cf06` | `/private/tmp/unmerged-branch-inventory-r3-collab-20260909.branch-association-checks.tsv` | `a4b6abd3998a9f70deef8d846b59d91de5e6051488413740f9a455a6a1813fe6` |
| AppSDK | `/private/tmp/unmerged-branch-inventory-r3-appsdk-20260909.branches.full.tsv` | `4673ad62b65522d674400fa880e6716eba37cbf622f365ba0fe1084bf1b37c04` | `/private/tmp/unmerged-branch-inventory-r3-appsdk-20260909.branches.full.checked-out.tsv` | `b75b7252e2a5d16dae26c263a0a2a41618a0ad93f517ffd3a13bc84580522c16` | `/private/tmp/unmerged-branch-inventory-r3-appsdk-20260909.branch-association-checks.tsv` | `a8559592259b35ed77ad243cad7a11a30e59ecf9accebc4a7c62763ba60975ad` |
| RouteCodex | `/private/tmp/unmerged-branch-inventory-r3-routecodex-20260909.branches.full.tsv` | `23deb5fd767c57570418cca5fc7908d304fce5a7dbe2ab11562aeb42add9cd50` | `/private/tmp/unmerged-branch-inventory-r3-routecodex-20260909.branches.full.checked-out.tsv` | `f00cb81993506fd75869cc7e53b1b0effee8516a6d241a5d29d0ad85d91c8a35` | `/private/tmp/unmerged-branch-inventory-r3-routecodex-20260909.branch-association-checks.tsv` | `d9568b12cfe184c1372821941892f3991eaabda918d6f6f5172f5224b55f5589` |

The row-level check files contain one record per attached worktree with
`branch_ref_present=YES` and `association_check=PASS`. Their 279 PASS values
are consistency checks between the captured worktree and branch projections;
they are not owner or merge receipts.

## Claim and owner binding audit

The root claim registries were read without mutation. Active-like claims all
have missing worktree paths:

| Project | Active-like claims | Existing claim path | Disposition |
|---|---:|---:|---|
| Collab | 1 `claimed` | 0 | retain the claim fact; operator/daemon reconciliation required |
| AppSDK | 1 `active` | 0 | retain the claim fact; do not infer a live owner |
| RouteCodex | 29 (`24 active`, `5 working`) | 0 | retain all claims as stale/lost until authoritative reconciliation |

Examples are Collab `notification-atomic-send`, AppSDK
`appsdk-beta-codex-wraith`, and all 29 RouteCodex active-like entries in the
r3 claims files. A branch ref surviving after its worktree disappears does not
prove ownership, task liveness, delivery or permission to merge. Claim state,
task state, runtime liveness, worktree state and Git reachability must be
reconciled separately.

## Migration classification rules

The migration controller must classify each worktree, branch, claim and
governance record independently. The following four dispositions are ordered
by evidence strength and operational risk:

| Disposition | Admission rule | Required handling |
|---|---|---|
| `integrate` | Clean attached candidate, no unresolved index, exact issue/task/owner/worktree/path scope, reviewed commit/tree, mapped tests and delivery receipt all proved against the current v1 integration line | Put into the one-at-a-time review and integration queue. This r3 inventory proves no row has all of these admission facts; no row is auto-mergeable. |
| `retain-and-reconcile` | Dirty or unresolved source; active-like claim; a live-looking task with a missing owner; duplicate HEAD; or a project lineage whose target and scope still need reconciliation | Freeze as evidence, preserve exact paths/digests/errors, resolve owner/task/worktree relation through the authoritative daemon, then produce a new clean candidate if work is still wanted. |
| `archive-only` | Detached or foreign worktree, branch-only ref without a confirmed owner, historical V3/V4 experiment, merged ref with no active requirement, or any record whose delivery/review/runtime proof is absent | Preserve immutable source bytes and Git identity. It may be referenced or adapted later, but cannot become active by naming, reachability or cleanup. |
| `needs_operator` | No authoritative source or runtime binding; conflicting writers; unknown append/side effect; unavailable endpoint; missing migration lease; or an explicit user decision is required | Stop the affected project. Record the first failed boundary and exact error. Ask for an operator decision or run a new bounded inspect; never blind-retry or invent a binding. |

`reset_required` is a project-level result when the source cannot currently
prove a safe replay prefix. It expands to `retain-and-reconcile` for the
preserved records plus a fresh target epoch; it does not authorize destructive
Git or journal reset.

## Review queue candidates

These are useful rows for a later controlled review. They are **not** merge
decisions:

| Project | Candidate | HEAD | Git relation | Why it can enter review | Why it cannot be auto-merged |
|---|---|---|---|---|---|
| Collab | `playground/v1-collab-refactor-integration-20260909` / `codex/v1-collab-refactor-integration-20260909` | `e7954e9fc20c3fffa54d580324323506802cdb3b` | local `+106/-0`; origin `+38/-0` | clean attached current v1 integration line | candidate admission, milestone review, and exact integration receipt are still required |
| Collab | `playground/v1-governance-reset-plan-r3-20260909` / `codex/v1-governance-reset-plan-r3-20260909` | `61ab83317e1b53898ca4d0301abf0e7fba720c4a` | local `+104/-0`; origin `+36/-0` | clean migration-plan candidate | a plan is not a migration execution or archive receipt |
| Collab | `playground/v1-host-paths-candidate-r3-20260909` / `codex/v1-host-paths-candidate-r3-20260909` | `8d97abfef0dcff8c1cea3f57436b27b9a9ff0856` | local `+104/-0`; origin `+36/-0` | clean host-path candidate | runtime wiring, one-writer proof and independent review remain open |
| AppSDK | `playground/90a4cd5-complete-producer-0909` / `codex/90a4cd5-complete-producer-0909` | `8fa6be2412b9f4e84fcd296d66c96e8849d8723b` | local `+5/-0`; origin `+0/-0` | clean and at observed `origin/main` | AppSDK root is dirty/unresolved and local governance owner/quality receipt is not reconciled |
| AppSDK | `playground/apps-sdk-lifecycle-map-gate-integration-0908` / `codex/apps-sdk-lifecycle-map-gate-integration-0908` | `cd666d51807218b024913744e6b8ecbff4876bed` | local `+0/-15`; origin `+0/-20` | clean, previously integrated-shaped candidate | reachability is not review, delivery, or current quality acceptance |
| RouteCodex | `playground/acdd3cd-fix-0909` / `codex/acdd3cd-fix-0909` | `e96cc7b3053ea5f1709cef123469298e9a45c586` | local `+1/-6`; origin `+7/-0` | clean, attached V3 fix candidate | mainline drift and 29 lost active-like claims require owner and issue reconciliation |
| RouteCodex | `playground/p0-final-fix-0909` / `codex/p0-final-fix-0909` | `490600631eff8de70404fe5de773717e019bdc5e` | local `+3/-3`; origin `+12/-0` | clean, attached P0-named candidate | the name is not a P0 acceptance receipt; exact bug, review, tests and delivery must be bound |
| RouteCodex | `playground/v4-cordis-replay-7a3407f` / `codex/v4-cordis-replay-7a3407f` | `5cafd81585aac9a1c8867f10166697b455a1fce6` | local `+879/-82`; origin `+879/-70` | clean V4 evidence worktree | it is a large V4 lineage, not a v1 merge candidate; preserve and reconcile separately |

The candidate list intentionally contains both source and integration-shaped
rows so that the next owner can compare them. A clean row only means it can
enter review. It does not prove that the implementation is correct, that the
task remains authorized, or that the branch should replace `main`.

## Project dispositions

### Collab

The live root is on dirty `codex/v2-cordis-architecture`, while the r3 v1
integration line is a separate clean worktree at `e7954e9`. The inventory has
151 worktrees, 152 branches, 25 dirty worktrees, 19 detached worktrees and a
missing-path `notification-atomic-send` claim. The v1 integration line may be
reviewed as a code candidate, but the live governance history is
`reset_required` until a fresh source digest, one migration lease, one writer,
owner reconciliation and a new target epoch are proved. V2 history and
unmerged worktrees remain retained evidence; they are not a shortcut to v1.

### AppSDK

The live checkout is dirty and has 14 unresolved index entries; a separate
reset-governance worktree has 12 more. The 101 worktrees and 119 branches
contain clean candidates, but the unresolved root blocks treating local
governance or quality projections as a safe active source. AppSDK quality
records remain owned by AppSDK. Collab migration may retain immutable
references to issue/review/evidence records after their owner revalidates
them; it must not copy a dirty projection into a new active truth.

### RouteCodex

The live root is dirty on `codex/root-dirty-recovery-0908`; the inventory has
74 worktrees and 619 branch refs, with 46 dirty worktrees, 13 detached
worktrees and 378 branches not merged into local `main`. The V3 production
line and V4 Cordis experiments must remain distinct. V4 rows with hundreds of
commits of divergence are `retain-and-reconcile` or `archive-only`, never a
bulk merge into v1. All 29 active-like claims point to missing worktrees, so
the project is `reset_required` until owners and active bugs/tasks are
revalidated.

### codexapp

`/Users/fanzhang/Documents/github/codexapp` is not a Git repository and has no
`.agent-collab` history. The only observed external source is
`/Users/fanzhang/.codex-communication/journal.jsonl` (54 lines,
SHA-256 `c3d995d870cd45a3887c1f3eddba4e2a214db6b60158f3b037ed4fc826f485a4`).
The socket path
`/Users/fanzhang/.codex-communication/sockets/commd.sock` exists, but a
bounded Unix listener probe failed with `ECONNREFUSED`. There is no proof of a
live native AppServer initialize/capability handshake, endpoint generation,
project registration or TUI/Desktop communication. This is
`needs_operator`, not an importable legacy project.

The operator must first establish and verify the native AppServer adapter and
listener, snapshot the external journal, and decide which typed transport
facts (if any) can be adapted. The Node prototype, mock adapter and README
claims are implementation evidence only; they are not runtime or Collab
history evidence. Desktop must not register a goal subscription.

## Migration and reset/rebuild protocol

### A. Freeze the source boundary

1. Run a new read-only inspect for each selected project immediately before
   migration. Capture cwd, branch, HEAD/tree, journals, events, mailbox,
   claims, tasks, bug IDs, PID/socket/lock observations and process/cwd
   ownership.
2. Compare all digests with the preflight snapshot. Any drift creates a new
   bounded attempt; it does not overwrite the old evidence.
3. Acquire exactly one authenticated migration lease and fence every legacy
   writer. A PID file, socket file, empty lock or daemon name is not a
   single-writer proof.
4. If a writer cannot be fenced, the lease is missing, or an append outcome is
   ambiguous, stop at `needs_operator`/`unknown` and preserve the exact error.

### B. Archive before mapping

1. Create an immutable byte-level archive of every source journal, event,
   mailbox, claim, task, bug reference, identity, branch/worktree inventory
   and codexapp external journal that is in scope.
2. Record source and archive counts, hashes, ordering and the first failed
   boundary. The archive is evidence, not a new active projection.
3. Preserve dirty worktree trees and detached/foreign refs as Git evidence.
   Never make a dirty tree clean by reset, stash, checkout, deletion or a
   guessed commit.

### C. Map each record into a fresh epoch

Each record receives exactly one mapping class:

- `direct`: typed, complete, owner/scope/runtime/generation relation proved;
- `adapt`: an explicitly documented field conversion with source reference;
- `reset`: retained in the old archive and represented by a new target fact
  only after fresh owner/runtime authorization;
- `unknown`: timeout, socket close, ambiguous append, missing owner or
  conflicting source; it remains archive-only until an operator resolves it.

No historical `PASS`, delivery, master grant, notification, ACK or goal receipt
is replayed as current authority. Create a fresh target epoch and fresh
sequence. Re-register stable runtime identities, revalidate cwd/project/app
scope and obtain a new user master grant. Existing active bugs and tasks enter
the new epoch only after their issue, owner, worktree and evidence edges are
confirmed. P0 bugs remain project-blocking until their new resolution receipt
is verified.

### D. Rebuild and verify

1. Replay only the complete verified direct prefix; apply documented adapters
   one record at a time.
2. Keep `reset` and `unknown` records in the archive with their exact error
   and first boundary. They do not create active notifications, wakeups or
   task assignments.
3. Rebuild mailbox JSONL and latest-state notification projections from
   committed target facts. Notification delivery is not evidence of fact
   consumption; raw records remain queryable.
4. Verify unique sequence/entity relations, project and app scope, role
   permissions, runtime binding and endpoint generation, one writer, bug/task
   reconciliation, idempotent notifications, and negative cases for corrupt,
   duplicate, missing-owner and unknown records.
5. Produce a receipt binding migration ID, source/target epoch, lease fencing
   token, command/operation IDs, source/archive/target hashes, reducer
   revision, actor, timestamp and exact result.

### E. Controlled cutover

Cutover is a separate approval after the migration candidate is reviewed and
the no-write rehearsal passes. For one project at a time:

```text
fresh inspect -> operator admission -> lease -> freeze -> archive
-> old-epoch fence -> fresh epoch -> direct/adapt/reset/unknown mapping
-> identity/runtime/scope rebind -> bug/task reconciliation
-> replay/projection rebuild -> invariants and real endpoint verification
-> install reviewed build -> restart one daemon -> TUI/Desktop replay
-> record push and cleanup separately
```

If any source changes, review receipt is missing, target writer is ambiguous,
endpoint is unavailable, rollback CAS changes revision, or external side
effect is unknown, stop the cutover. Never use a fixed retry interval to hide
the failure or create a second daemon.

## Branch/worktree convergence protocol

Governance migration and Git convergence are related but separate. The
integration owner processes one row at a time:

1. Freeze the row with repository, path, branch/detached HEAD, commit/tree,
   base refs, status, owner, task, claim, issue, allowed paths and capture
   digest.
2. Reconcile the authoritative task/worktree/claim relation. Missing or
   conflicting relation means `retain-and-reconcile`/`needs_operator`.
3. Preserve dirty, unresolved, detached, foreign and V3/V4 trees. They do
   not enter the merge queue.
4. For a clean attached row, prove candidate scope, run the mapped tests, run
   independent review and verify the exact commit/tree. A title, branch name,
   clean status or `merged_*` flag is insufficient.
5. Merge one reviewed candidate into the v1 integration line. Record merge
   commit/tree, review, gates and delivery receipt, then rerun affected gates.
6. Refresh all three Git inventories after every merge because main refs,
   worktrees, claims and live state can change during the operation.
7. Only after the refreshed inventory is unambiguous may a separately
   authorized owner replace local `main`, push, rebuild/install the canonical
   binary, restart the single daemon and verify TUI/Desktop communication.
8. Cleanup is last and separate. Do not delete a branch or worktree merely
   because it is clean, old, merged or unclaimed; first preserve its evidence
   and resolve any active-like claim.

## Evidence files and digests

The following files are outside the repository so the live governance roots
were not copied or mutated:

| Project | Worktree full TSV | Original branch full TSV | Corrected branch full TSV | Claims TSV | Worktree digest | Original branch digest | Corrected branch digest | Claims digest |
|---|---|---|---|---|---|---|
|---|---|---|---|---|---|---|---|---|
| Collab | `/private/tmp/unmerged-branch-inventory-r3-collab-20260909.worktrees.full.tsv` | `/private/tmp/unmerged-branch-inventory-r3-collab-20260909.branches.full.tsv` | `/private/tmp/unmerged-branch-inventory-r3-collab-20260909.branches.full.checked-out.tsv` | `/private/tmp/unmerged-branch-inventory-r3-collab-20260909.claims.tsv` | `9272350c80eefe1174466d847519c96420c4c6a0d2295e1b9ee545e991d8dfa4` | `862ad38efacc40d5ab3af8146bd263688a98766769919266c6e5ae28f1003b95` | `44c5ef30350e7bbe619a9fb7d0ee1bd841f3a9af7596e7e752103922cf81cf06` | `ce8cd7865f50797d028f9b7d678ddd78aef1a800bc626653a676cbdf7ecc9ddd` |
| AppSDK | `/private/tmp/unmerged-branch-inventory-r3-appsdk-20260909.worktrees.full.tsv` | `/private/tmp/unmerged-branch-inventory-r3-appsdk-20260909.branches.full.tsv` | `/private/tmp/unmerged-branch-inventory-r3-appsdk-20260909.branches.full.checked-out.tsv` | `/private/tmp/unmerged-branch-inventory-r3-appsdk-20260909.claims.tsv` | `d75a51459fd3a655fbd89201be7b0915cddcd83cd50d1deb40f8cc53c8558399` | `4673ad62b65522d674400fa880e6716eba37cbf622f365ba0fe1084bf1b37c04` | `b75b7252e2a5d16dae26c263a0a2a41618a0ad93f517ffd3a13bc84580522c16` | `46108385dddf0283f0667ef0906c1fab1956bc21b87d03a863de6ed842db8773` |
| RouteCodex | `/private/tmp/unmerged-branch-inventory-r3-routecodex-20260909.worktrees.full.tsv` | `/private/tmp/unmerged-branch-inventory-r3-routecodex-20260909.branches.full.tsv` | `/private/tmp/unmerged-branch-inventory-r3-routecodex-20260909.branches.full.checked-out.tsv` | `/private/tmp/unmerged-branch-inventory-r3-routecodex-20260909.claims.tsv` | `870b37b793554292c5f8b3c4f586e619299f8245b9228151f448a3bbb0d7ccfe` | `23deb5fd767c57570418cca5fc7908d304fce5a7dbe2ab11562aeb42add9cd50` | `f00cb81993506fd75869cc7e53b1b0effee8516a6d241a5d29d0ad85d91c8a35` | `7054c92b186a4a63cba533a99f39c76dff1fb1af018faaba7f46386459103071` |

For reproducibility, the raw inputs are also retained beside each project's
derived TSV with the same `unmerged-branch-inventory-r3-<project>-20260909`
prefix. The original raw/derived branch files are preserved; the corrected
branch files and association checks above are deterministic derivatives of the
captured worktree and branch rows. The derived files include local/origin
ahead-behind, merged flags, dirty/unresolved status, path shape,
`.agent-collab` presence and, in the corrected branch files, the repaired
`checked_out` association.

The earlier historical counts `120/42/378` and r2 counts are retained only as
comparison facts. They cannot be used to select or bulk-merge a branch. The
current r3 counts are the ones above, and even they expire when live sources
drift.

## Non-actions and next gate

This round did not modify any live source, journal, event file, mailbox, claim,
identity, PID, lock or socket. It did not start or stop a daemon, acquire a
migration lease, archive/reset/replay history, rebind an identity, subscribe a
goal, merge/push a branch or clean a worktree.

The next gate is an operator-admitted fresh inspect bound to one exact source
root and one exact candidate. The inspect must re-prove source digests, owner
and cwd binding, one writer, migration lease, replay prefix, target epoch and
the separate AppSDK quality owner. Until that gate passes, the only valid
states are `retain-and-reconcile`, `archive-only`, `needs_operator` or
`reset_required`.
