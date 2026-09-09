# Governance history inventory refresh evidence

Snapshot time: `2026-09-09T18:35:57Z` (UTC).

This is a read-only planning snapshot for the four source roots named by the
governance-history migration inventory. It records only values observed during
this refresh. It does not authorize a migration lease, archive, reset,
replay, writer transition, identity rebind or runtime change.

The evidence window expires at `2026-09-09T19:05:57Z` (`expires_at`). A new
read-only inspect is required at or before `2026-09-09T18:55:57Z`
(`refresh_before_lease`) and immediately before any lease, archive or writer
transition, even if the expiry time has not been reached. AppSDK and
RouteCodex had observed processes/cwds and their journal/event files changed
during earlier probes in this collection, so the values below are bounded
observations rather than an immutable source snapshot.

## Candidate provenance

The candidate was created from the integration commit in the Collab
repository, not from the dirty AppSDK checkout:

| Field | Observed value |
|---|---|
| source repository | `/Volumes/extension/code/collab` |
| exact base commit | `abd33f437fd679ce3f6fde503f84e4c86aeb10f3` |
| candidate worktree | `/Volumes/extension/code/collab/playground/governance-history-inventory-refresh-20260909` |
| candidate checkout | detached HEAD at `abd33f437fd679ce3f6fde503f84e4c86aeb10f3` |
| candidate HEAD tree before this file | `411bb73eb8d9cf667c04726bb2b4c61e52df74d4` |
| candidate state | clean before the new file; after the edit, one untracked file at the requested path |
| allowed write | `docs/evidence/governance-history-inventory-refresh-20260909.md` only |

The live Collab root used for observation is
`/Volumes/extension/code/collab`; it is not the candidate and was already
dirty. No live-root file, daemon, journal, events file, mailbox, socket or lock
was edited.

## Collection contract

Git values came from `git branch --show-current`, `git rev-parse HEAD`,
`git rev-parse HEAD^{tree}`, and read-only status queries. `status_entries`
counts all porcelain entries, `staged_entries` and `unstaged_entries` count
the corresponding changed-path projections, `untracked_files` counts
untracked files, and `unresolved_paths` counts unique paths returned by
`git ls-files -u`.

For regular JSONL files, `lines` is `wc -l` and `sha256` is the full-file
SHA-256 from `shasum -a 256`. A mailbox is a directory projection: its row
count is the total `wc -l` count of all regular files, and its aggregate hash
is the SHA-256 of the concatenated bytes of those files in C-locale sorted
absolute-path order. This is a content observation of the mailbox projection;
it is not a claim that every file is a current or consumable message.

`server.pid` and `daemon.lock` were inspected as filesystem artifacts. A
socket path or a PID value, including an empty PID value, does not prove
liveness or single-writer ownership. For Collab, AppSDK and RouteCodex,
`ps` observed a `collab serve` process and `lsof -nP -a -p <pid> -d cwd`
observed its cwd during this refresh. That is a bounded process/cwd
observation with the same expiry; socket ownership and lock ownership remain
unproven. No socket-owner inference is made from socket existence.

## Git roots and working-tree state

| Project | Canonical cwd | Branch | HEAD | HEAD tree | Dirty/unresolved observation |
|---|---|---|---|---|---|
| Collab | `/Volumes/extension/code/collab` | `codex/v2-cordis-architecture` | `064824c375a720450c43f4829679bb8e45d4a1d1` | `7f0bfc31628af30604a3b10e13098ac166bcd236` | dirty: 671 status entries, 1 staged, 4 unstaged, 667 untracked; 0 unresolved paths |
| AppSDK | `/Users/fanzhang/Documents/github/appsdk` | `chore/project-memory-snapshot` | `2d14efed9d7f6454d119cb7a1aea24a384e966e0` | `728960b0e100201045317a28e740d0e58e0558d4` | dirty: 7 status entries, 7 staged, 9 unstaged, 0 untracked; 5 unresolved paths |
| RouteCodex | `/Users/fanzhang/Documents/github/routecodex` | `codex/root-dirty-recovery-0908` | `d876adea1d5c57a73cf643f5c8d89b56bd3d42c4` | `88edaa554f16ff17cd4e28bc49ac52c5fc2a84ae` | dirty: 50 status entries, 38 staged, 19 unstaged, 4 untracked; 0 unresolved paths |
| codexapp | `/Users/fanzhang/Documents/github/codexapp` | no Git checkout observed | no Git HEAD observed | no Git tree observed | source directory has no `.git`; Git dirty/unresolved state is not applicable |

The five AppSDK unresolved paths observed in the index were:
`contracts/maps/function-map.json`,
`contracts/migrations/sdk-0.1.5-to-0.1.6.json`,
`docs/design/appsdk-project-integration.md`, `rust/src/main.rs`, and
`rust/tests/cli_smoke.rs`. This is an unresolved source checkout, not an
active quality candidate.

## Journal, events and mailbox projections

| Project | Journal rows and SHA-256 | Events rows and SHA-256 | Mailbox rows and SHA-256 |
|---|---|---|---|
| Collab | `.agent-collab/server/journal.jsonl`: 72 lines; `c47e0f95b388809274ae2c43b8a23289e3d92e6afa6a61ea21033a7491fffc83` | `.agent-collab/server/events.jsonl`: 445 lines; `6d9eb601ac4ea27db78b8cf8a1ecfd39d97b1f4d05572abcf16393239511395a` | `.agent-collab/mailbox`: 7 regular files, 58 lines; `67ab5e8977150060574bfce98360eea8be9cc628e6fc8a006e536b59e8c01b7e` |
| AppSDK | `.agent-collab/server/journal.jsonl`: 40,600 lines; `b9025a6a6c7011deb7a567ee0abe252f4af59cb27d840752f68a6bd72fcd3fde` | `.agent-collab/server/events.jsonl`: 5,194 lines; `b1a3747fc2d1172b1262678819de4a3951d7f2919a07145c3843a163a4105973` | `.agent-collab/mailbox`: 545 regular files, 7,119 lines; `e437c35e4c89ffdc290a8b75c7fd72ff51399ea86f11f5aeebdd9df1d69bcf32` |
| RouteCodex | `.agent-collab/server/journal.jsonl`: 39,431 lines; `9dd93d730fa221aa445ebbc5e79e5c8884ebc877b180ff704bcbe8249c941273` | `.agent-collab/server/events.jsonl`: 26,231 lines; `8f2bebb12eb55418a2bc32649914e4599737d7444d52cbcd5ed51a1225ffd842` | `.agent-collab/mailbox`: 3,660 regular files, 45,663 lines; `dd56eb28b50f4bbf8dd484ea88ba675cbfe4c98e9306cbe80961b2104bb2a941` |
| codexapp | external `/Users/fanzhang/.codex-communication/journal.jsonl`: 54 lines; `c3d995d870cd45a3887c1f3eddba4e2a214db6b60158f3b037ed4fc826f485a4` | external `/Users/fanzhang/.codex-communication/events.jsonl`: absent | external `/Users/fanzhang/.codex-communication/mailbox`: absent |

The codexapp source has no `.agent-collab` directory in the inspected root.
Its external journal is the only observed JSONL history input; no events or
mailbox projection was invented for the missing paths.

## PID, cwd and socket/lock types

| Project | PID artifact and process | `lsof` cwd observation | Lock artifact | Socket artifact |
|---|---|---|---|---|
| Collab | `.agent-collab/server/server.pid`: regular file, mode `-rw-r--r--`, 5 bytes, no line terminator, value `34612`; `ps` showed `/Users/fanzhang/.cargo/bin/collab serve` | PID `34612` cwd was `/Volumes/extension/code/collab` | `.agent-collab/server/daemon.lock`: regular file, mode `-rw-r--r--`, 0 bytes, empty; owner/held state unknown | `.agent-collab/server/server.sock`: Unix `Socket`, mode `srw-------`; existence does not prove liveness/owner |
| AppSDK | `.agent-collab/server/server.pid`: regular file, mode `-rw-r--r--`, 5 bytes, no line terminator, value `56952`; `ps` showed `/Users/fanzhang/.cargo/bin/collab serve` | PID `56952` cwd was `/Users/fanzhang/Documents/github/appsdk` | `.agent-collab/server/daemon.lock`: regular file, mode `-rw-r--r--`, 0 bytes, empty; owner/held state unknown | `.agent-collab/server/server.sock`: Unix `Socket`, mode `srw-------`; existence does not prove liveness/owner |
| RouteCodex | `.agent-collab/server/server.pid`: regular file, mode `-rw-r--r--`, 5 bytes, no line terminator, value `57862`; `ps` showed `/Users/fanzhang/.cargo/bin/collab serve` | PID `57862` cwd was `/Users/fanzhang/Documents/github/routecodex` | `.agent-collab/server/daemon.lock`: regular file, mode `-rw-r--r--`, 0 bytes, empty; owner/held state unknown | `.agent-collab/server/server.sock`: Unix `Socket`, mode `srw-------`; existence does not prove liveness/owner |
| codexapp | No PID file was observed under `/Users/fanzhang/.codex-communication`; no process/cwd claim made | unknown/not applicable; no numeric PID source was observed | No lock file was observed under the inspected external root; lock state unknown | `/Users/fanzhang/.codex-communication/sockets/commd.sock`: Unix `Socket`, mode `srwxr-xr-x`; existence does not prove liveness/owner |

The refresh records PID plus `ps` and cwd `lsof` only for the three
`.agent-collab` roots. No bounded socket-owner `lsof` result was obtained;
therefore no socket is attributed to any PID. Empty or filled PID files,
regular empty lock files and socket types remain filesystem observations.

## Classification changes since the prior inventory

The comparison point is the prior inventory in
[`governance-history-inventory-20260909.md`](./governance-history-inventory-20260909.md),
captured at `2026-09-09T16:12:32Z`. The comparison changes classification for
planning only; it does not create migration records or mark anything mapped.

| Project | Prior classification and state | Refresh change | Current planning classification |
|---|---|---|---|
| Collab | The v1 branch at `ac54d09dcab14ec35d5a5291207af9edda00fcf7` was described as a possible direct input after replay and owner checks; journal was 72 lines/events 442 lines; PID and lock were empty; 98 playground directories were observed. | The live root is now branch `codex/v2-cordis-architecture` at `064824c375a720450c43f4829679bb8e45d4a1d1`, dirty with 671 entries and 110 playground directories. The journal remains 72 lines with the same digest, while events are 445 lines with a new digest. PID `34612` and its cwd were observed, but socket/lock ownership is still unproven. | Withdraw the prior direct-input presumption for the live root. Treat the live root as `adapt/needs_reconciliation` and archive-only planning input. The clean `abd33f4…` candidate is a separate source tree and has not been replayed or leased. |
| AppSDK | The root was already classified as dirty/unresolved and not an active quality candidate; HEAD was `2d14ef…`; `.appsdk` was absent. | The same branch, HEAD and unresolved five-path condition remain. Journal/events changed to 40,600/5,194 rows and new digests; mailbox is 545 files/7,119 rows. PID `56952` and cwd were observed, without proving candidate quality or migration safety. | Classification unchanged: `adapt/needs_reconciliation`, reference-only for migration planning; never import the dirty root as an active quality candidate. |
| RouteCodex | The dirty `codex/root-dirty-recovery-0908` root at `d876ade…` was an archive/adapt input, not active runtime state; journal/events were 39,190/26,086 rows; mailbox had 3,636 files; PID and lock were empty. | HEAD/tree and dirty/no-unresolved shape remain, while journal/events moved to 39,431/26,231 rows and mailbox to 3,660 files/45,663 rows. PID `57862` and cwd were observed; socket/lock ownership remains unproven. | Classification remains `adapt/needs_reconciliation` and archive-only until a clean, owner-bound candidate and fresh source replay exist. V3/V4 or AppSDK quality status is not inferred from these coordination projections. |
| codexapp | The non-Git Node transport prototype was classified as a new endpoint bootstrap; external journal had 54 lines with the same digest; socket owner was unknown; the invalid `npm test -- --runInBand` invocation was not evidence. | External journal row count/hash and socket type remain unchanged. Events and mailbox are still absent; no Git branch/head/tree or PID/lock source was observed. | Classification unchanged: transport bootstrap requiring adapter/capability inspection and reconciliation; no direct Collab history import from mock or in-memory state. |

The AppSDK and RouteCodex journal/event changes observed between earlier
probes and this final refresh confirm that these are mutable live projections.
The earlier probe values were AppSDK `40,528/5,192` and RouteCodex
`39,428/26,228` journal/events rows; the final values above were
`40,600/5,194` and `39,431/26,231` respectively.
A changed digest is source drift requiring a new inspect or reconciliation; it
is never overwritten in place. No source is marked
`direct/mapped`, `verified`, `reset` or `replayed` by this document.

## Explicit non-actions and next gate

- No daemon was started, stopped, restarted or migrated.
- No live-root journal, events file, mailbox, PID file, lock or socket was
  edited.
- No old inventory was changed.
- No lease, archive, reset, replay, identity rebind, task import or writer
  transition was performed.
- The next gate is a fresh read-only inspect after the controller binds an
  exact source root and candidate, then comparison of all source digests,
  owner/cwd bindings, journal replay and single-writer evidence. A socket or
  empty PID/lock artifact cannot satisfy that gate.
