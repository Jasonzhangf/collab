# Governance history inventory evidence

Snapshot time: `2026-09-09T16:12:32Z` (UTC)

This is a read-only inventory captured before any migration lease. It records
the exact roots, revisions, files, counts and SHA-256 values observed during
the audit. It is evidence for planning and archive selection, not a current
liveness claim. Every value with an expiry must be refreshed by a new
read-only inspect immediately before a lease, archive or writer transition.

## Collection contract

The inspection read `git status --short --branch`, `git rev-parse HEAD`,
`git rev-parse HEAD^{tree}`, source files and configured data roots. JSONL
counts came from line counting without rewriting the files; digests are full
SHA-256 values of the observed bytes. Socket checks only establish that a path
exists. They do not establish a live owner. No daemon was started or stopped,
no journal/mailbox was edited, and no project reset was run.

## Collab

| Field | Observed value |
|---|---|
| checkout | `/Volumes/extension/code/collab` |
| v1 integration branch/HEAD | `codex/v1-collab-refactor-main-20260909` / `ac54d09dcab14ec35d5a5291207af9edda00fcf7` |
| source journal | `.agent-collab/server/journal.jsonl`, 72 lines, SHA-256 `c47e0f95b388809274ae2c43b8a23289e3d92e6afa6a61ea21033a7491fffc83` |
| event projection | `.agent-collab/server/events.jsonl`, 442 lines, SHA-256 `08b6514230e4f5980bf6727583cd6d8f317f17572a324026c4a22de8b5799b1f` |
| prior migration fact | `MigrationUpdated`, `from_version=v1-legacy`, `to_version=v1-low-intervention`, `phase=verified` |
| v2 root observation | branch `codex/v2-cordis-architecture`; dirty with untracked governance/evidence files |
| playground count | 98 directories observed under the root |
| writer/liveness | `.agent-collab/server/server.pid` and `daemon.lock` were empty; daemon liveness and single-writer ownership are `unknown` |

The complete hash-valid v1 prefix is a possible direct input only after a
fresh replay and owner check. The prior local migration fact is historical
evidence and does not prove global-daemon ownership.

## AppSDK

| Field | Observed value |
|---|---|
| checkout | `/Users/fanzhang/Documents/github/appsdk` |
| branch/HEAD | `chore/project-memory-snapshot` / `2d14efed9d7f6454d119cb7a1aea24a384e966e0` |
| working tree | unresolved `UU`/`DU` paths in `contracts/maps/function-map.json`, `contracts/maps/verification-map.json`, `contracts/migrations/sdk-0.1.5-to-0.1.6.json`, `docs/design/appsdk-project-integration.md`, `docs/design/rust-binary-delivery.md`, `rust/src/main.rs`, `rust/tests/cli_smoke.rs` |
| AppSDK control projection | `.appsdk-control/long-task-goal.json`, SHA-256 `b7f09b518a250ca3d120429487462034ffe1f8ec2019b3c50d5479a5bb817386`; `active=false`, `desired=recovery_required`, `remote_state=unknown`, `GOAL_STATUS_SUBSCRIPTION_ID_MISSING`, `GOAL_SUBJECT_RECONCILIATION_FAILED` |
| current `.appsdk` directory | absent in this checkout; this absence does not erase other contract or collaboration history |
| migration contract digest | `contracts/migrations/sdk-0.1.5-to-0.1.6.json`, SHA-256 `9e670f391f189c5e10718ac7d5b79bb9db238a3b5b9de42d65bcbd537ebb790e` |
| collaboration counts | 98 run files, 541 mailbox files, 999 review files, 3 claims, 2 handoffs |

The dirty root is not an active quality candidate. AppSDK owns its contracts,
quality records, Active and Protected history. Collab may retain immutable
references and digests only after the AppSDK owner resolves the conflicts.

## RouteCodex

| Field | Observed value |
|---|---|
| checkout | `/Users/fanzhang/Documents/github/routecodex` |
| branch/HEAD | `codex/root-dirty-recovery-0908` / `d876adea1d5c57a73cf643f5c8d89b56bd3d42c4` |
| working tree | extensive staged and unstaged changes across `.appsdk`, V3 and UI files |
| source journal | `.agent-collab/server/journal.jsonl`, 39,190 lines, SHA-256 `905e96bef054d16344d89fe49eff1df71cde8b23848768aef14097786c108aca` |
| event projection | `.agent-collab/server/events.jsonl`, 26,086 lines, SHA-256 `00a35bf5efaecd275e8bb627dc1eb761686b4bbf5495c1544005a8b58178edd6` |
| server log | `.agent-collab/server/log.txt`, 70,558 lines |
| writer/liveness | `.agent-collab/server/server.pid` and `daemon.lock` were empty; daemon liveness and single-writer ownership are `unknown` |
| AppSDK project digest | `.appsdk/project.json`, SHA-256 `af8b4ee97f694e0250b51808a461245b940e87e4222651cabd4e35720f45e465` |
| AppSDK migration digest | `.appsdk/migrations/0.1.5-to-0.1.6/record.json`, SHA-256 `0d72c82cec57620c2e88448324df7d5aa5cfc68f2323db09675b1a4184d557e6` |
| collaboration counts | 517 run files, 3,636 mailbox files, 280 review files, 37 claims, 20 handoffs |

V3 production evidence and V4 architecture/refactor history remain separate.
The dirty root, V4 playgrounds and unbound worktrees are archive/adapt inputs,
not active runtime state. The AppSDK `reset-governance --discard-legacy`
operation remains a separate owner-controlled operation.

## codexapp

| Field | Observed value |
|---|---|
| source directory | `/Users/fanzhang/Documents/github/codexapp` |
| source control | no `.git`; no `.agent-collab` |
| source shape | Node transport prototype (`src/bridge.js`, `src/app-server-adapter.js`, `src/ws-jsonrpc.js`, JSONL helper and two test files) |
| external journal | `/Users/fanzhang/.codex-communication/journal.jsonl`, 54 lines, SHA-256 `c3d995d870cd45a3887c1f3eddba4e2a214db6b60158f3b037ed4fc826f485a4` |
| external socket | `/Users/fanzhang/.codex-communication/sockets/commd.sock` exists; writer ownership and process liveness are `unknown` |
| configurable paths | `src/cli.js` permits `--journal` and `--socket`; the effective paths must be read again before archive |
| test boundary | tests use `MockAppServerAdapter`; the six-test run proves only an in-memory bridge projection |
| invalid test invocation | `npm test -- --runInBand` failed with Node `bad option: --runInBand`; this is not test evidence |

The external journal and socket are mandatory archive and writer-inspection
inputs. Only the native AppServer/WebSocket transport seam may be adapted;
Node registry, role, message, scheduler and mock receipts are not Collab
control state.

## Unknowns and refresh rules

The following values were intentionally left `unknown`: empty PID/lock files,
socket writer ownership, daemon liveness, current branch/worktree state after
the snapshot, and any source record whose owner, outcome or canonical cwd
cannot be proved. `unknown` is not equivalent to stopped, clean, delivered or
safe to replay.

Before migration starts, the controller must repeat the read-only inspection,
compare all source digests and revisions, identify every writer through the
supported lifecycle, and stop on any drift. A changed digest is a new source
snapshot or a reconciliation conflict; it is never overwritten in place.
