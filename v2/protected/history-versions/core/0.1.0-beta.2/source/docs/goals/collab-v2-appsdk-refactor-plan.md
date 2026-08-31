# Collab v2 AppSDK 0.1.5 Refactor Plan

## Objective and acceptance

Implement the frozen v2 design as an independently governed project. Rust owns semantic truth and authorization; the original Cordis package orchestrates plugin lifecycle; Codis composes fixed containers; typed versioned events cross process boundaries; Arc is process-local only. The v1 tree and runtime state are outside this change.

The change is accepted only when the AppSDK gates, Rust tests/build, clean-worktree checks, install/restart smoke test, live black-box replay, architecture review, promotion records, and regression suite all pass. Evidence must identify the exact commit and runtime artifact used.

## Scope and boundaries

In scope: `v2/crates/**`, `v2/protocol/**`, `v2/orchestration/**`, `v2/containers/**`, `v2/tests/**`, `v2/docs/**`, and `v2/.appsdk/**`.

Out of scope and read-only: v1 `src/**`, v1 runtime state under `.agent-collab/**`, `playground/**` except isolated experiments explicitly recorded by AppSDK, `generated/**`, `active/lib/**`, and `protected/**`.

Never put routing, retry, health, debug, snapshot, provider, or stop/control fields in business payloads. No fallback, silent strip, duplicate truth owner, or `cordis-rs` production dependency.

## Frozen architecture

1. Rust core: journal, identity, role authorization, task/claim state machine, resource truth, and error chain.
2. Cordis adapter: discover/validate/start/ready/route-switch/drain/dispose plugin lifecycle; no direct writes to Rust truth.
3. Codis containers: fixed orchestration graphs and health wiring; no business-state mutation.
4. Arc bus: in-process event sharing only.
5. Versioned protocol: command, event, and health contracts in `protocol/`; control and payload types are separate.
6. Plugin lifecycle: `discover -> validate -> start -> ready -> route switch -> drain -> dispose`, with failure recorded on the error/event chain.

## Implementation units

- `crates/core`: Rust domain state, persistence ports, authorization, task/claim transitions, and typed errors.
- `crates/protocol`: builders/parsers for the frozen command/event/health contracts.
- `crates/plugins`: Rust plugin ABI and validation; deterministic replacement state machine.
- `orchestration/cordis`: thin original-Cordis adapter and lifecycle coordinator.
- `containers/codis`: fixed container definitions and wiring only.
- `tests/`: contract, lifecycle, authorization, replacement, payload-isolation, positive/negative, and live-replay harnesses.

Each unit must be registered in `.appsdk/maps/module-registry.json`, `.appsdk/maps/function-map.json`, and `.appsdk/maps/mainline-call-map.json` before implementation. Every source file has exactly one owner module.

## Ordered execution

1. Run `appsdk prepare` and confirm the existing `.appsdk-prepare.json`; run `appsdk init` only for the v2 root.
2. Update project/goal/module/resource/function/call/verification maps and run `appsdk verify`.
3. Create a clean owner worktree under `v2/playground/<short-slug>/`; record worktree, run, claim, and evidence records. Do not edit the main tree.
4. Write failing contract and lifecycle tests first, including positive and negative transitions and control/payload separation.
5. Implement the Rust core and protocol builders, then the Cordis adapter and Codis containers. Keep conversions adjacent to their contract nodes.
6. Run white-box tests, Rust formatting/lint/build, AppSDK verification, and architecture-boundary gates in the clean worktree.
7. Build the deterministic artifact, install it into the v2 runtime, restart the daemon/runtime, and run live black-box replay using the same artifact/commit. Capture evidence.
8. Run `appsdk verify --review-admission <project> --module <module>` for each changed module, then run the required architecture review. A review cannot replace failed verification or live validation.
9. Write handoff/merge-queue records, merge only the declared change set, and repeat affected tests/build/install/restart/live checks in the main tree.
10. Run regression, compile/publish the active immutable library, record promotion, and freeze only after a clean tree and all hashes/reviews match.
11. Remove the merged experiment worktree and branch only after clean/unmerged checks and remote parity are proven.

## Verification matrix

- Schema/map: JSON parse, AppSDK verify, owner/path coverage, symbol and adjacent-edge checks.
- Core: Rust unit/property tests for all lifecycle states, authorization, restart/recovery, claim close, and error paths.
- Protocol: round-trip command/event/health tests and rejection of unknown or mixed control fields.
- Orchestration: Cordis lifecycle ordering, Codis graph wiring, plugin replacement and drain behavior.
- Isolation: tests proving v1 paths and runtime state are not read or written.
- Runtime: install, restart, socket/health check, live task replay, replacement replay, and artifact hash capture.
- Review: AppSDK review admission plus architecture review after all runtime evidence.

## Risks and mitigations

- Boundary drift: fail AppSDK owner/path gates before code review.
- Event/payload contamination: typed separate contracts plus negative serialization tests.
- Replacement race: one Rust state-machine owner and deterministic event ordering.
- Runtime artifact mismatch: record commit and artifact hash, then replay after restart.
- v1 regression: read-only boundary checks and v1 smoke baseline.

## Definition of done

The v2 implementation, maps, contracts, tests, evidence, review, promotion, and freeze records are internally consistent; all required gates pass on the installed artifact; the main tree and remote branch contain only the declared v2 change set; v1 remains unchanged; the experiment worktree is cleaned up.
