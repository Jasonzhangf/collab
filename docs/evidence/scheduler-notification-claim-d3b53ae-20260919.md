# Scheduler notification claim ownership evidence

This receipt binds executable evidence to the scheduler notification claim
ownership fix. It proves only the gates listed below. It is not a delivery,
merge, push, install, daemon restart, or live production replay receipt.

## Candidate identity

| Field | Observed value |
| --- | --- |
| worktree | `/Volumes/extension/code/collab/playground/collab-scheduler-notify-p0-20260919` |
| branch | `codex/collab-scheduler-notify-p0-20260919` |
| base commit | `399abc1b058c2b86e5c3e691e592f2fde9e875f7` |
| candidate commit | `d3b53aea6fa5e9d604a28e298e0483b73ccd9945` |
| candidate tree | `b40edc2abff772b443303bbda505786036b8198b` |
| changed source | `src/server/mod.rs` (`131bfd1898b045c2697429d6305f607d5da7573440910bd6cd5f0bbe050630ef`) |
| release binary | `target/release/collab` (`4902fb07d0e86a816270189eb0147f5297acfed44d830c531a037e41f4df89b5`) |
| release MCP | `target/release/collab-mcp` (`7c3214174facbe058e90753d2020b54755ebc3003a47f919a113f0fc67cc51d4`) |
| release version | `collab 0.2.0004` |
| App Server A/B gate | `tests/appserver_e2e.mjs` (`2fa9c02496cad36cac204619ea9341b4ea674a04e20351acc0afc00551619368`) |

## Change boundary

`attempt_scheduler_notification` captures the timestamp of the `notifying`
claim it owns. Success and failure paths commit only while that exact claim is
still current. A stale attempt cannot clear or overwrite a newer retry claim.
The focused regression test is
`server::scheduler_admission_tests::stale_attempt_cannot_clear_newer_notification_claim`.

## Gate results

| Gate | Command | Result |
| --- | --- | --- |
| formatting | `cargo fmt --all -- --check` | PASS |
| focused stale-claim regression | `cargo test --locked stale_attempt_cannot_clear_newer_notification_claim -- --nocapture` | PASS: 1 passed, 0 failed |
| scheduler dispatch suite | `cargo test --locked scheduler_dispatch_ -- --nocapture` | PASS: 10 passed, 0 failed |
| full test suite | `cargo test --locked` | PASS: 516 main, 8 MCP, 4 context, 3 route tests passed |
| release build | `./scripts/build-collab.sh` | PASS: `collab_build_version=0.2.0004` |
| real A/B App Server E2E | `env -u TMUX -u TMUX_PANE COLLAB_APPSERVER_E2E_COLLAB=<release-binary> node tests/appserver_e2e.mjs` | PASS |

## Real A/B observations

The isolated gate created two real Git project roots and two distinct native
App Server threads. Both `collab init` results selected `appserver`.
`context` and `worker status` reported live, present, identity-valid peers.
Cross-project `master send` returned `durable=true`,
`cross_project=true`, and `notification=sent`; the target observed native
thread activity, and `inbox` plus `recv` consumed the marker.

A second cross-project message was persisted, survived `collab down/up`, and
was consumed after restart with the same App Server identity and thread.
The worktree probe resolved the canonical project root and live master despite
a stale worktree-local route. The task lifecycle reached
`working -> verifying -> reviewed -> delivered -> accepted -> merged -> closed`
with delivery, independent review, exact main commit, and verified cleanup
receipts.

This gate exercises the scheduler notification claim change through the real
App Server transport. It does not prove merge, push, installation, controlled
restart of the host daemon, or production replay.
