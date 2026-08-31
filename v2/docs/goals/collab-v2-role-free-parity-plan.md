# Collab v2 Role-Free Low-Intervention Parity Plan

## Objective and acceptance

Bring Collab v2 to semantic parity with the verified role-free v1 runtime while
preserving the v2 ownership boundary: Rust is the only semantic truth owner,
Cordis owns plugin lifecycle and wiring, and Codis owns process/container
lifecycle and health. tmux remains the only live agent notification channel;
the journal, mailbox, task records, wait records, migration records, and Rust
state are durable truth.

The change is accepted only when v2 has no declared or inferred
`master`/`worker` role, no central dispatch/offer/claim path, no normal peer
progress/heartbeat/ACK traffic, and no App Server notification path. Every peer
must own its complete task, worktree, verification, integration, push, and
cleanup lifecycle. Durable resource coordination, bounded waits, explicit
finite notification subscriptions, deterministic replay, formal migration, AppSDK governance,
installed-runtime black-box validation, and AGY Review must all pass on the
same candidate artifact.

## Current gap baseline

Baseline commit: `8e0afa1cc0527010e44373a7f2724dfd325c94be`.

Current green tests prove the role-based beta implementation is internally
consistent, not that it has the required semantics:

- Node: 33 tests pass, including tests that require the first registration to
  become `master`, later registrations to become `worker`, master-only
  governance, task claim, heartbeat, mailbox ACK, and App Server delivery.
- Rust: 2 tests pass, both explicitly enforcing `Master`/`Worker`, master task
  creation, `Available` tasks, and claim.
- Build: the current manifest check passes but only validates ten declared
  plugins and a small control-field list; it does not bind source ownership,
  imports, lifecycle semantics, or role-free behavior.

The implementation gap is structural:

| Surface | Current v2 | Required parity |
|---|---|---|
| Identity | First identity is `master`; later identities are `worker` | Equal peer identity; no serialized, inferred, promoted, recovered, or transferred role |
| Task admission | Master creates `available` task; worker claims it | Owner self-registers its own task directly as `working`; `/goal` recognition remains deferred |
| Authorization | Role permission arrays and master override | Task owner, scoped resource lease, and explicit daemon operator capability only |
| Lifecycle | `available -> working -> verifying -> reviewing -> delivered -> merged -> closed` | `working -> verifying -> reviewed -> delivered -> merged -> closed`, plus explicit blocked/waiting/rework/cancel paths and no delivery bypass |
| Coordination | General peer send, heartbeat, ACK, assignment and transfer | Durable P2P only for `RESOURCE_OCCUPIED` and `RESOURCE_RELEASED`; no routine reports |
| Waits | No durable wait graph | Waiter, blocker, responsible actor, reason, deadline, resume events, escalation, cycle rejection, and release cleanup |
| Wake | tmux sends arbitrary payload directly; App Server can also start turns | tmux-only literal short wake after durable commit; shell/offline/working panes fail closed |
| Retry | No durable wake-attempt lease or lost-wake recovery | Journaled attempt time, immutable lifetime cap of three, replay that cannot reset the count, and terminal consume/expiry/exhaustion |
| Continuation | Mailbox receive/ACK lifecycle | No inferred or periodic continuation; after an opt-in short wake the caller explicitly reads durable context/mailbox without an ACK loop |
| Truth owner | Node maps duplicate workers/tasks/messages while Rust has a smaller second state | Rust owns all semantic state and transitions; Node/Cordis are typed projections/adapters only |
| Persistence | Node snapshot plus Rust JSON file; no contiguous journal contract | One Rust journal/reducer, deterministic snapshot/replay, idempotency, contiguous sequence checks |
| Migration | `--upgrade` creates a side-by-side control plane and reports a role | Formal role-based-v2-beta migration transaction with freeze, snapshot, rebind, verify, and resume |
| Governance maps | Design-draft maps still register role authorization, claim, heartbeat/probe edges | Active machine maps bind real role-free symbols, owned paths, imports, adjacent edges, and gates |
| Tests | Existing tests lock obsolete roleful behavior | Positive/negative parity tests, real tmux black-box, restart replay, migration preservation, and installed artifact evidence |

There are existing v2 worktrees, including a dirty `v2-core-fix` worktree.
They are independent resources. The implementation owner must inspect their
status and durable handoff evidence, coordinate any real resource conflict,
and must not copy, reset, clean, merge, or overwrite another peer's changes.

## Scope and boundaries

### In scope

- `v2/crates/core/**`: peer identity, task/resource/wait/message state,
  reducer, journal, replay, migration, typed errors, and daemon protocol.
- `v2/protocol/**` and `v2/contracts/**`: role-free command/event/state and
  migration contracts with physical control/business-payload separation.
- `v2/src/**`: thin Rust client, Cordis composition, projections, persistence
  port, wake scheduling, and transport adapters.
- `v2/bin/**`: role-free CLI/MCP surface, context recovery, migration, and
  explicit daemon lifecycle.
- `v2/docs/**`, `v2/.appsdk/maps/**`, and governance manifests: active
  resources, modules, functions, call edges, verification gates, lifecycle,
  migration, and operator guidance.
- `v2/tests/**`: unit, contract, state-machine, migration, restart, transport,
  payload-boundary, and real-runtime tests.
- The global Collab skill after the v2 implementation and runtime evidence are
  complete, with explicit v1/v2 command and migration guidance.

### Out of scope

- `/goal` parsing, task delegation, or interactive intent recognition.
- Importing v1 task/message/journal truth into v2. v1-to-v2 data migration
  requires a separately versioned import contract; this change only migrates
  existing role-based v2 beta state in place.
- Replacing or stopping the verified production v1 daemon before the isolated
  v2 artifact passes every gate and an explicit deployment step is reached.
- Starting, attaching, installing, or exposing a v2 daemon to zterm, OneStop,
  RouteCodex, or dsh-plugins before full v2 production admission. Those four
  projects remain v1-only throughout this milestone.
- Editing v1 `src/**`, v1 `.agent-collab/**`, another peer's worktree, or
  protected/frozen AppSDK artifacts by hand.
- Adding a second notification channel or using App Server as a tmux fallback.

## Design principles

1. Rust is the single semantic owner. Cordis, Codis, CLI, MCP, transports, and
   dashboards submit typed commands or consume projections; none reconstruct
   or mutate business truth.
2. Every identity is an equal peer. Capability checks are scoped operations,
   never persistent social roles.
3. Each task is created by its owner directly in `working`; no unowned
   `available` queue, dispatch, offer, or claim exists.
4. Task transitions are adjacent and owner-controlled. `reviewed -> merged`
   and every other delivery bypass fail closed.
5. Resource coordination is the only cross-peer messaging purpose. Durable
   state commits before a tmux wake is attempted.
6. A wait edge is a typed Rust resource with finite responsibility and cannot
   be inferred from idle/presence observations.
7. tmux carries only `COLLAB_NOTIFY <message-id>` plus Enter in one command
   sequence. Mailbox and journal content are never pasted into panes.
8. Wake authority is opt-in: an Agent registers an owner-scoped, exact-event,
   exact-subject, finite-TTL, one-shot subscription. Absent, unknown, and
   working panes produce zero attempts. A message has at most three lifetime
   attempts, and replay or re-registration cannot reset that count.
9. Errors stay on the typed error/control chain. No fallback, silent strip,
   duplicate writer, or success projection from failure is permitted.
10. Migration is a journaled transaction, not startup compatibility logic.

## Target resources and lifecycle

The resource registry must make these Rust-owned truth resources explicit:

- `peer-identity`
- `task-lifecycle`
- `resource-lease`
- `wait-graph`
- `message-mailbox`
- `notification-subscription`
- `wake-attempt-ledger`
- `journal-sequence`
- `migration-transaction`
- `daemon-operator-capability`

Required task states:

```text
working -> verifying -> reviewed -> delivered -> merged -> closed
working|blocked -> waiting -> blocked|working
reviewed|delivered -> rework -> working
working|blocked|waiting|verifying|reviewed|delivered|rework -> cancelled
```

Every wait must contain:

```text
waiter
waiting_for
responsible_actor
reason = resource_conflict
deadline_ms
resume_on = resource_released | blocker_rework | blocker_cancelled
escalation
```

The reducer must reject self-wait, missing or inactive blocker, unrelated
resource, terminal/delivered wait, missing deadline/resume path, and direct or
transitive cycles. Holder close/rework/cancel must clear the obsolete wait
edge, move the waiter to explicit `blocked`, and persist release truth. It may
create and schedule a `RESOURCE_RELEASED` notification only for an exact live
subscription; otherwise it creates no message and no wake.

## Implementation units and expected files

Prefer modifying existing owners. Add a new file only when the current file
cannot represent one typed owner cleanly.

- Rust domain owner: `v2/crates/core/src/lib.rs` and
  `v2/crates/core/src/bin/core-daemon.rs`.
- Protocol owner: `v2/protocol/command-v1.json`,
  `v2/protocol/event-v1.json`, and versioned additions only when an existing
  consumed schema cannot change safely.
- Node projection/adapters: `v2/src/collab-core.mjs`,
  `v2/src/rust-core-client.mjs`, `v2/src/communication.mjs`, and
  `v2/src/persistence.mjs`. Node must not retain duplicate semantic maps after
  Rust exposes the required projection.
- Wake transport: `v2/src/transports/tmux.mjs`. App Server and mailbox may
  expose durable/control facts, but cannot be selected as live agent wake
  transports.
- CLI/MCP/migration: `v2/bin/collab.mjs`, `v2/bin/collab-mcp.mjs`,
  `v2/bin/collab-v2-init.mjs`, and `v2/bin/collab-v2.mjs`.
- Machine truth: `v2/docs/resource-map.json`,
  `v2/docs/function-map.json`, `v2/docs/mainline-call-map.json`,
  `v2/docs/verification-map.json`, `v2/docs/plugin-registry.json`, and
  `v2/contracts/collab-v2-runtime.manifest.json`.
- Human guidance: `v2/README.md`, `v2/docs/architecture.md`, a formal v2 beta
  migration guide, and the global Collab skill.
- Tests: existing `v2/tests/**`, Rust unit tests, and an isolated real-tmux
  black-box harness owned by the verification map.

Before product edits, update maps from `design-baseline-draft` to active only
for symbols and edges that actually exist. Design/pending entries remain
explicitly pending. Gates must parse real imports/calls and prove source files
are owned exactly once; document-only consistency is insufficient.

## Migration design

Existing role-based v2 beta projects use one journaled transaction:

```text
inspect -> plan -> admission freeze -> deterministic snapshot
        -> install exact reviewed artifact -> controlled runtime restart
        -> peer identity rebind -> verify -> resume
```

Migration requirements:

- Inventory serialized roles, master-only capabilities, available/unowned
  tasks, claims, waits, peer bindings, journal sequence, mailbox, evidence,
  active runtime writer, and exact artifact version.
- Strip no fields at request boundaries. Legacy role fields are accepted only
  by the migration parser and projected into equal peer identity plus scoped
  task/resource ownership according to deterministic rules.
- Unowned `available` tasks, ambiguous master-owned operations, or waits with
  no real responsible actor stop the plan for explicit operator resolution.
- Freeze all business mutations after snapshot. Permit only reads, existing
  identity rebind, migration verify, and explicit daemon lifecycle.
- Verify exact counts and hashes for journal, tasks, mailbox, messages,
  evidence, resource ownership, and snapshot before resuming.
- Duplicate runtime writers, malformed journal, count/hash mismatch, or stale
  artifact keep the project frozen with a typed error.
- Remove the legacy migration parser after supported beta states have been
  migrated and its physical-deletion gate passes.

## Verification plan

### Architecture and contract gates

- Machine maps parse and bind every source file to exactly one owner.
- Real imports/calls match registered adjacent edges; undeclared crossings
  fail.
- `master`, role-based `worker`, role transfer, dispatch, offer, claim,
  available queue, routine progress, heartbeat traffic, and explicit ACK loop
  are absent from active runtime contracts and public CLI/MCP surfaces.
- Control fields cannot serialize into business payload, and business payload
  cannot rebuild control state.
- Cordis/Codis/App Server/tmux/mailbox cannot write Rust truth or journal
  directly.

### Rust and Node tests

- Equal peer registration and same-session rebind; duplicate identity/session
  rejection.
- Owner self-registration directly into `working`; another peer cannot mutate,
  deliver, merge, close, cancel, or clean it.
- Positive and negative coverage for every adjacent lifecycle transition,
  including delivery and review bypass rejection.
- Valid wait, self-wait, missing blocker, unrelated resource, inactive blocker,
  direct cycle, transitive cycle, deadline, release, rework, cancellation, and
  terminal cleanup.
- Success, failure, still-running, and already-terminal resource/wake cases.
- Shell/offline/working-pane rejection; confirmed waiting-agent acceptance.
- No subscription produces zero wake attempts. Failed tmux wake remains
  pending only while the exact subscription is armed; attempts stop forever at
  three. Immediate/timer races produce at most one delivered wake.
- Attempt time is persisted; restart replay is clock-independent and snapshot
  hashes are stable.
- Successful delivery consumes the one-shot subscription. `context` is
  read-only and never infers, consumes, acknowledges, or schedules work.
- Rust accepts message ids only when they are 1..=128 ASCII bytes, start with
  an alphanumeric byte, and continue with alphanumeric, `-`, `_`, `.`, or `:`.
  Empty, overlong, control-character, non-ASCII, and non-canonical ids return
  typed `InvalidMessageId` before message, journal, or snapshot mutation.
- The tmux adapter independently rejects terminal C0/C1 and DEL framing
  controls before process invocation. A valid id produces exactly one
  `tmux send-keys -t <pane> -- "COLLAB_NOTIFY <id>" Enter` invocation.
- App Server cannot become a live wake route; tmux failure has no fallback.

### Migration and runtime black-box

- Inspect/plan/apply/verify a realistic role-based v2 beta fixture.
- Preserve task/resource/message/mailbox/evidence counts and hashes.
- Reject duplicate daemon/runtime without corrupting authoritative PID/socket.
- Build the exact release artifact and install it only into an isolated v2
  runtime first.
- Run a real multi-pane tmux black-box covering two equal peers, independent
  task lifecycles, resource conflict/release, bounded wait, lost wake,
  subscription consumption/exhaustion, explicit context query, migration
  restart, and deterministic replay.
- After isolated success, merge the reviewed candidate into latest main,
  repeat affected gates and installed-runtime smoke, then deploy v2 only at the
  explicit deployment stage. Keep the verified v1 daemon untouched until then.

### Required commands and review order

At minimum:

```text
cargo fmt --check
cargo test
cargo build --release
npm test
npm run build
appsdk verify
appsdk compile
appsdk freeze
git diff --check
```

Run AppSDK review admission and AGY Review only after source tests, build,
canonical compile/freeze, isolated installation/restart, and live black-box
evidence pass. Any post-review code/test/build/runtime change invalidates the
review and affected evidence.

## Ordered implementation

1. Refresh latest main, active v2 worktrees, durable handoffs, AppSDK state,
   protected staging, installed artifacts, and runtime processes. Resolve real
   resource conflicts without touching another peer's dirty worktree.
2. Record the role-based baseline and failing parity tests. Update resource,
   module, function, call, lifecycle, and verification maps before runtime
   edits.
3. Replace Rust role/claim/available semantics with equal peer identity,
   owner-created task lifecycle, scoped operator/resource capabilities, wait
   graph, notification subscriptions, wake ledger, and formal migration.
4. Reduce Node/Cordis state to typed commands and projections; remove duplicate
   task/identity/message truth and master override behavior.
5. Restrict cross-peer communication to explicit durable resource notices.
   Make tmux the only live wake transport, gated by exact finite subscriptions,
   confirmed waiting state, persisted leases, and the three-attempt hard cap.
6. Update CLI/MCP/init/migration surfaces and make obsolete role/dispatch/
   claim/ACK commands fail fast.
7. Run unit, state-machine, contract, architecture, replay, migration, and
   payload-isolation gates in the clean worktree.
8. Generate the canonical AppSDK artifact, clear only transaction-owned
   recoverable staging through formal governance operations, and prove compile
   and freeze records/hash references are consistent. Never hand-edit
   Protected artifacts.
9. Install the exact candidate into an isolated v2 runtime and complete the
   real tmux/migration/restart black-box.
10. Run review admission and AGY Review. Fix findings through a fresh complete
    verification cycle.
11. Synchronize latest main into the candidate, verify the exact integrated
    tree, acquire the short integration resource, merge and push exact main,
    then repeat main/runtime verification.
12. After remote parity, clean only the declared merged worktree and branch;
    update the Collab skill with v1/v2 migration and operation guidance.

## Risks and controls

- Duplicate truth during Rust migration: delete Node semantic ownership as
  Rust projection becomes available; a gate rejects duplicate mutable maps.
- Role fields surviving in compatibility paths: migration-only parser lives
  behind freeze and is invisible to runtime projections; source scan and red
  tests reject active role semantics.
- tmux command execution in shell panes: require confirmed waiting-agent
  evidence; shell and working panes fail closed.
- Lost or duplicate wake: durable pending state, persisted attempt lease,
  immutable three-attempt lifetime cap, and one delivered transition after a
  successful single-command `COLLAB_NOTIFY <message-id>` plus Enter.
- AppSDK Protected staging contamination: use canonical transaction ownership,
  compile, and freeze operations; never delete arbitrary archives or patch
  hashes by hand.
- Active v2 worktree conflict: wait or coordinate using durable resource
  occupancy/release; never reset, stash, copy, or clean another peer's files.
- Production regression: v2 remains isolated until exact-artifact black-box and
  review pass; zterm, OneStop, RouteCodex, and dsh-plugins remain on verified
  v1, and no persistent v2 process may exist before explicit v2 deployment.

## Definition of done

- v2 exposes equal peers and no declared/inferred role, master, central
  dispatch, offer, task claim, available queue, routine report, heartbeat
  traffic, or explicit ACK loop.
- Each peer owns the complete task/worktree/verification/integration/push/
  cleanup lifecycle and cannot mutate another peer's task.
- Only durable resource occupancy/release crosses peers; tmux is the sole live
  wake channel; no inferred or periodic continuation exists.
- Wait graph, deadline, release, escalation, finite notification subscription,
  three-attempt lease, deterministic replay, and formal v2 beta migration are
  Rust-owned and verified positively and negatively.
- Maps, manifests, docs, CLI, MCP, tests, AppSDK compile/freeze records, and the
  global Collab skill describe the same active semantics.
- Unit/build/governance gates, real tmux black-box, migration restart,
  installed-runtime replay, review admission, and AGY Review pass on one exact
  commit/artifact.
- Exact main is pushed and verified remotely; only the clean merged owner
  worktree/branch is removed; unrelated dirty worktrees and the running v1
  daemon remain untouched until explicit v2 deployment.
