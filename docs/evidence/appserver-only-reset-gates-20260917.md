# App Server-only Collab reset gate evidence

Candidate date: `2026-09-17`

This receipt binds the reviewed source candidate to executable gate results.
It proves only the gates listed below. It is not a delivery, merge, install,
restart, or live communication receipt.

## Candidate identity

| Field | Observed value |
| --- | --- |
| worktree | `/Volumes/extension/code/collab/playground/reset-canonical-20260917` |
| branch | `codex/reset-canonical-20260917` |
| candidate base | `6eafb208dcbb07ad6118cbabed6b27b16d7e14db` |
| candidate commit | `13641a09bf49a56c8c51a63cd8c004daccd28390` |
| candidate tree | `bc3abfcb49d64cfa44cc4d37b5fa391a9b2c9de2` |
| candidate branch push | `origin/codex/reset-canonical-20260917` |
| release binary | `target/release/collab` |
| release SHA-256 | `9544c56a92f3a33b1568241b9f70ec9281e50c10036fcbc56c2268e0669dfba5` |
| reset owner | `src/reset.rs` (`fe3e07de957f0dfe0aac9fbcddccf04f9c3e86c031757b5c2654c805473bf869`) |
| App Server blackbox | `tests/appserver_blackbox.mjs` (`2d1c24ed053a329f4d94a1b2771059e7df32e83149e887484db9efa1e90b42f5`) |
| App Server A/B E2E | `tests/appserver_e2e.mjs` (`fa1a09bf6b64275c628a4d58dff3a061f2311873ec0c7b85ed9d3f2c59100a35`) |
| reset live replay | `tests/reset_live_replay.mjs` (`de61551132da888afae7d576eb205fe71afb1543df97fcf41691e2d8677313dd`) |

The reviewed candidate is committed and pushed. The review ran against the
uncommitted tree at base `6eafb208dcbb07ad6118cbabed6b27b16d7e14db`; the
committed candidate preserves that reviewed tree. This evidence contains no
secret or copied control token.

## Gate results

| Gate | Command | Result |
| --- | --- | --- |
| formatting | `env -u TMUX -u TMUX_PANE cargo fmt --check` | PASS |
| full tests | `env -u TMUX -u TMUX_PANE cargo test --all-targets --no-fail-fast -- --test-threads=1` | PASS: Collab 466 passed, MCP 8 passed, 0 failed |
| release build | `env -u TMUX -u TMUX_PANE cargo build --release` | PASS; warnings only |
| migration replay | `env -u TMUX -u TMUX_PANE sh tests/migration_history_rehearsal.sh` | PASS: Collab, AppSDK, RouteCodex, and codexapp fixtures |
| isolated App Server blackbox | `env -u TMUX -u TMUX_PANE COLLAB_APPSERVER_BLACKBOX_SOCKET=<isolated-socket> node tests/appserver_blackbox.mjs` | PASS: two distinct native threads; `thread/queue/add` returned `queuedSubmission`; both `thread/read` statuses were `idle`; `thread/items/list` was probed as an optional diagnostic capability |
| isolated A/B App Server E2E | `env -u TMUX -u TMUX_PANE COLLAB_APPSERVER_E2E_COLLAB=<release-binary> node tests/appserver_e2e.mjs` | PASS: two real project roots and native threads selected App Server; `context`/`worker status` reported `live=true`, `presence=present`, `endpoint_live=true`, `identity_valid=true`; cross-project master send was `durable=true`, `cross_project=true`, `notification=sent`; target `inbox` and `recv` consumed the marker; restart preserved identity, route, and one unconsumed message before `recv` |
| full task lifecycle | same A/B E2E | PASS: owner task reached `working -> verifying -> reviewed -> delivered -> accepted -> merged -> closed`; delivery evidence, independent review evidence, exact `refs/heads/main` commit, and cleanup receipt `verified` were asserted |
| reset live replay | `env -u TMUX -u TMUX_PANE COLLAB_RESET_REPLAY_COLLAB=<release-binary> node tests/reset_live_replay.mjs` | PASS: archived and removed `.agent-collab` and `.agent-collab-v2`; preserved an unrelated initialized route; removed stale routes; rebuilt the current empty baseline and guidance; `delivery_verified=false`; second reset was idempotent with `already_reset=true`; daemon `up/status/down` passed |

## Review admission

| Field | Observed value |
| --- | --- |
| review task | `reset-canonical-20260917-appserver-only-r11` |
| mode | uncommitted candidate at `6eafb208dcbb07ad6118cbabed6b27b16d7e14db` |
| verdict | PASS |
| outcome | `controller_no_blocking_findings` |
| receipt | `.agent-collab/review/reset-canonical-20260917-appserver-only-r11/status.json` |

## App Server blackbox boundary

The blackbox starts an isolated `codex app-server --listen
unix://<temporary-socket>` process. It does not inspect or mutate the live
`~/.codex/app-server-control` daemon and does not use tmux. It verifies:

1. native WebSocket `initialize`;
2. `thread/start` creates two distinct threads;
3. `thread/queue/add` returns transport acceptance (`queuedSubmission`);
4. `thread/read` preserves the exact thread identity and reports native status;
5. `thread/items/list` is probed as an optional diagnostic capability and
   method-not-found does not block registration.

`thread/queue/add` acceptance is not execution, read, reply, delivery, or
consumption. This gate therefore does not fabricate a communication result.
The candidate implementation must not interpret this receipt as delivery
verification.

## Candidate scope checked

- `src/reset.rs` is the single transactional reset owner.
- Reset archives and retires `.agent-collab` / `.agent-collab-v2`, rewrites
  stale host routes, rebuilds the current empty baseline, and records
  `delivery_verified: false`.
- Reset rejects symlinks anywhere below a retired control root and treats
  non-`NotFound` control-root inspection errors as explicit failures before
  archiving or mutation.
- Reset does not own or mutate `.appsdk-control`, project editor/MCP
  configuration, business source, runtime data, `active/`, or `protected/`.
- Production transport code has no tmux/TMax selection path. Remaining
  references are the explicit environment removal in `src/subagent.rs`, the
  retired-config migration in `src/config.rs`, and historical evidence text.
- `collab init` resolves project scope from the exact process cwd and uses the
  server-selected App Server transport when the registered native thread is
  live.

## Delivery layers outside this receipt

- final main merge, push, and post-merge rebuild;
- installation of the candidate binary;
- controlled daemon restart against the installed global binary;
- replay against the installed binary in the user's real projects;
- final cleanup of this candidate worktree.

Those layers remain separate gates and must be reported independently.
