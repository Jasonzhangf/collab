---
name: collab
description: >
  Coordinate registered peers only with reusable finite direct-message leases,
  one-shot event subscriptions, task/worktree ownership, resource waits,
  controlled daemon maintenance, and explicit user-approved master promotion
  when no live master exists. Guidance: (1) recovery: inspect durable status,
  verify the real live registered route, re-register/rebind only the named peer,
  then send one recovery report request; (2) failure: preserve the exact error,
  keep journal/mailbox truth, do not retry or claim delivery, and escalate with
  root cause and evidence; (3) reset: never delete journal/mailbox, copy tokens,
  reset bindings, or start a second daemon; use explicit down/up or migration
  only; (4) regression recognition: distinguish durable send, transport delivery,
  agent response, ACK/consume, task close, and cleanup evidence. Ordinary peer
  notices use one direct command with no discovery or retry step. Codex root
  is not Collab master.
---

# Collab

Durable truth lives in the global Collab state root. App Server is the only
registered transport and is selected by the server. Production projects use
the globally installed Collab v1.

## Current-version baseline

Use only the current globally installed `collab` and `collab-mcp`. The
`Cargo.toml` version is the semantic source baseline; every official release
compile uses `scripts/build-collab.sh`, which increments the host-global
`~/.collab/build-version` counter under one lock. The installed binary's
`collab --version` reports `0.2.NNNN` as the runtime build version. Direct
release builds fail with an instruction to use the official entry. An upgrade
targets the current reviewed source and does not migrate, replay, or interpret
older local versions.

The canonical install sequence from the reviewed source is:

```sh
scripts/install-global-collab.sh
```

The installer performs one release build and installs those exact candidate
bytes; it does not run a second build with another auto-incremented version.
The canonical pair is `$CARGO_HOME/bin/collab` and
`$CARGO_HOME/bin/collab-mcp` (default `$HOME/.cargo/bin`). The sequence invokes
the exact newly installed binary to refresh the embedded Skill, so an older
PATH entry cannot write a stale Skill. The install does not remove business
source, Git history, `~/.collab/`, project-local `.agent-collab/`, AppSDK
state, run notes, or shared evidence.

Legacy user-local copies are not removed automatically by this sequence.
If an exact old copy must be retired, first prove it is Collab by running its
own `--version` (and for MCP, its `initialize` response), then remove only the
verified pair. A path that cannot prove that identity is a collision: preserve
it and report the exact path. Never delete `~/.local/bin/collab*` or
`~/.local/lib/collab/*` merely because the pathname matches.

Installing a new binary does not replace a running daemon. The global daemon
may be serving other projects, so do not run `collab down` or `collab up`
merely because the binary was upgraded. Keep the existing daemon running until
an explicitly authorized maintenance window. In that window, use the
controlled lifecycle and preserve PID/socket/identity/journal/mailbox evidence;
never use a broad process kill or a second daemon.

Before changing the installed binary, inspect the current source version,
installed version, canonical paths, and daemon PID/socket:

```sh
cargo metadata --no-deps --format-version 1
collab --version
command -v collab
command -v collab-mcp
collab status --all
```

If the version or command path is stale after installation, fix PATH or refresh
the shell command cache (`rehash` in zsh, `hash -r` in bash), then verify that
`command -v collab` and `command -v collab-mcp` resolve to the exact
`$CARGO_HOME/bin` pair.
Do not hand-copy binaries, leave a second managed entry, or select an older
binary as a fallback.

## One lifecycle loop

Every master goal, bug report/fix, master-to-subagent assignment, and peer task
uses one authoritative `Trigger -> Work -> Gate -> State -> Stop` loop:

1. Discover current durable truth, ownership, dependencies, and evidence.
2. Persist durable dispatch intent before attempting notification.
3. Hand off one scoped assignment with delivery and test conditions.
4. Verify the real result; unknown, timeout, and failure remain explicit.
5. Persist the verified state, evidence, blocker, or failure.
6. Schedule the next eligible work, or stop at the loop's terminal condition.

No ACK, fallback, retry, snapshot, or notification may fabricate success or
replace a missing gate. Bug reports and fixes enter the AppSDK or git-bug
backlog with investigation evidence. P0 is highest priority and blocks the
affected project.

## Recovery, Failure, Reset, Regression

### 1. Recovery

Use recovery only after a daemon restart, transport replacement, identity
mismatch, or an explicitly reported delivery failure:

```sh
collab status --all
collab worker status <peer>
collab context
```

Verify the recorded App Server thread is live and belongs to the same peer.
If the selected transport is stale, use the explicit peer-scoped
re-registration/rebind path, then send one registration/report request. Do not
inject `collab init` into a foreign thread, guess among multiple threads, or replay
an old message batch. A recovery request is a maintenance action, not a normal
notification.

### 2. Failure

Treat each claim separately:

```text
durable=true -> mailbox journal accepted
notification=accepted -> selected transport accepted the preview
transport evidence -> App Server queued it
recv response -> peer consumed the message
task close receipt -> lifecycle ended
```

An error, timeout, `subscribed-not-sent`, `thread-lost`, `identity-mismatch`,
`unknown`, or absent Agent is not success. Preserve the exact error and
durable IDs; do not retry automatically, ACK for another identity, or mark a
task delivered/closed without its required evidence. A worker reports the
root cause and proposed fix to the live master. The master takes ownership by
fixing, re-dispatching, or force-closing with an auditable reason.

### 3. Reset

Use reset only when the operator explicitly authorizes discarding the named
legacy Collab control plane. Reset is offline, transactional, and starts a
new current baseline; it is not migration and does not preserve history:

```text
collab down/up -> controlled daemon restart; journal/mailbox survive
collab migrate -> authenticated migration and identity rebind
collab reset --discard-legacy --approval "<user text>" -> retire and rebuild
```

The exact reset sequence is:

```sh
collab down
collab reset --discard-legacy --approval "<explicit user authorization>"
collab up
collab init
```

`collab reset` takes the same host writer lock as the daemon, requires the
daemon to be down, archives the exact `.agent-collab/` and
`.agent-collab-v2/` bytes under `~/.collab/archives/`, verifies the archive,
removes only those Collab-owned project control roots plus stale host routes,
and rebuilds the current empty scaffold. It is idempotent, repairs a missing
baseline, ignores legacy history, and never imports old PASS or delivery
claims. It records `delivery_verified: false`; reset alone is not delivery,
review, install, restart, or live-communication evidence.

`.appsdk/` and `.appsdk-control/` are AppSDK-owned and are not removed by
`collab reset`. The host-wide runtime truth remains `~/.collab/`
(`server.sock`, `events.jsonl`, `log.txt`, and route state); project-local
`.agent-collab/` is reducer input and local durable data, not the global truth.
For a new project or an explicitly authorized clean epoch, remove old
project-local governance only through the owner's canonical reset/migration
command. Never manually remove `.agent-collab/server/journal.jsonl`, mailbox
files, identity tokens, task records, or bindings to make status look clean.
Never start a second daemon or use broad process kills. `collab ack` remains a
compatibility operation; it is not a substitute for task close or identity
recovery.

An AppSDK `reset-governance` or `appsdk init --fresh --discard-legacy` is not
Collab reset/migration and must not remove `.agent-collab/`. AppSDK's reset owner
explicitly treats `.agent-collab/` as a reserved root. If the project also
needs to move or retire Collab state, follow
[Migration and Daemon Maintenance](references/migration-daemon.md); the two
operations have separate transactions and separate completion evidence.

### 4. Regression recognition

After a fix, verify the same user path again and classify the first divergence:

- `send` durable but no transport acceptance: inspect the selected transport,
  subscription, ownership, Agent state, and daemon log.
- App Server queue acceptance appears but no worker result: inspect the
  native thread and worker state; do not call that a reply.
- `recv` returns messages: the read is consumed atomically; no follow-up ACK is
  required. `msg`, `inbox`, and `context` remain read-only.
- task remains open: inspect owner identity, master responsibility, cleanup
  receipt, and notification supersession.

Record the tested source commit, binary digest, daemon PID/socket, exact
commands, and live replay result in the bug system. A test pass without
latest-main merge, installed-binary verification, or applicable live replay
does not close the bug.

## Automatic multi-worker collaboration

Keep Collab enabled. At multi-worker startup, run official `collab init` once
in the inherited live peer environment unless AppSDK already initialized it.
This registers the peer and default finite direct-message subscription.
Registration also returns `role_brief`. Read it as the active operating
contract:

- `master`: dispatch and allocate resources, keep workers loaded, own blockers,
  and drive verify/merge/cleanup/close. Implementation is not the primary job.
- `worker`: complete the independently owned task end to end, evaluate master
  collaboration requests against current ownership/capacity, and explicitly
  accept or negotiate rather than ignore.
- `managed-subagent`: execute the assigned scoped task, obey master/parent for
  that assignment, and return root-cause/evidence rather than build a global
  schedule.

The default identity is peer. Master authority is explicit and
user-authorized: initial promotion requires the user's approval, and
delegation is accepted only from the current live master and records that
handoff. Registration, a process, inferred `/goal`, or `role_brief` never
silently creates master authority.

On trouble, every non-master investigates first and reports the live master:
root cause, attempted actions, proposed fix, and exact decision needed. A role
change via `master promote` or `master delegate` returns the new master brief;
the old worker brief no longer governs that peer.
Once task scope and the independent worktree are known, automatically follow
[task/worktree registration](references/task-worktree-lifecycle.md): bind the
task, feature/resource and owned file scope before concurrent product edits.
Use [resource coordination](references/resource-waits.md) for overlaps; never
share a worktree or overwrite another peer's files.

Registration, necessary coordination and subscriptions require no repeated
user confirmation within an authorized multi-worker task. Send only messages
authorized by the user's collaboration request; no unrelated external notices.
Do not require a serial merge queue for communication or read-only work.

If initialization fails, report collaboration unavailable and preserve its
error. Shared writes and dependent coordination wait for reliable ownership;
independent isolated work and AppSDK quality checks can continue. No invented
peer, local substitute claim or fake successful registration.

## Send an ordinary message

Run exactly one command:

```sh
collab sendmessage --to <peer> --subject <short-topic> "<original message>"
```

`--to`, a non-empty short `--subject`, and the original body are required.

Do not first run `notify methods`, `notify subscribe --help`, `whoami`, or
choose `mailbox-only`. There is no separate mailbox-only send mode.
`sendmessage` always commits the full subject/body to the durable mailbox.
With a matching live subscription, the first pending message opens a fixed
120-second window by default. `~/.appsdk/config.toml` can select immediate or
batched delivery globally or per project; `appsdk config` shows effective
policy. All eligible unsent messages for that recipient are combined
into the selected transport's bounded delivery (up to 3 previews per knock,
with overflow retained in the inbox). App Server explicit notifications use
`turn/start`, which starts a new turn or steers the recipient's current turn.
`thread/queue/add` is reserved for daemon-generated wakeup/long-horizon
notifications that require a safe waiting/idle agent; actively working agents
defer those wakeups without burning attempts so in-flight tasks are not
polluted. Explicit `collab sendmessage` is immediate and follows the
explicit-message adapter gate, including while the recipient is working.
If delivered-but-unconsumed notifications reach the throttle threshold (default
3), further push knocks pause until `collab recv` consumes them, preventing
terminal pollution and storms. Each batch has one attempt; the default window
is 120 seconds. Policy changes require controlled daemon restart, not task
reset.

Explicit `collab sendmessage` is immediate. Idle, progress, delivery, bug, and
worker-idle notices are auto-merged by the daemon in the 120-second batch
window; they are not repeated as heartbeat storms.

Do not retry a failed send automatically. Return its exact error and durable
status. Never call a transport command directly; the server owns transport
selection and sends only through the selected adapter.

## Common command card

For user-requested persistent subagents, run `appsdk subagent start --id <id>`.
That starts Codex with the configured profile. `--runtime codex` is accepted
for compatibility. Then `status`,
`send <id> --subject <topic> "<task>"`, and explicit `close <id>`.
`collab-mcp` is the shared Collab MCP for every agent. Use `collab_*`
tools when this session lists them. The `collab` CLI is also valid.
If MCP is missing, unsupported, aborted, or unknown, run the same
actions with the CLI in the inherited project cwd:
`collab init`, `collab recv`, `collab ack <id>` / `collab ack --all`,
`collab msg <id>`,
`collab inbox`, `collab worker status [id]`, `collab subagent ready|working <id>`,
`collab sendmessage --to <parent> --subject <topic> "<body>"`.
The CLI is a complete protocol path. Missing MCP is not a blocker and
does not justify skipping receive or waiting. Do not repeat `collab init`
after it already succeeded. Child results go to the parent with
`collab sendmessage`, not the parent-only `subagent send` action.
No ACK loops, automatic respawn or redispatch.

The live master is the sole scheduler assignment owner. Dispatch through the
durable scheduler path with a stable request ID:

```sh
collab subagent dispatch --request-id <id> --subject <topic> "<assignment>"
```

The scheduler reserves one message/task pair for an eligible peer, binds the
peer's active direct-message lease, records the admission audit, persists its
succeeded or failed status, and only then attempts one bounded notification on
success. An ordinary assigned peer must authenticate
with its own worker token and run `collab task accept <task-id>`; that command
atomically records `assigned -> working` and is the only receive entry for this
assignment. `collab task update --status working` is rejected for an assigned
task. A managed child accepts through `collab subagent working <id>`. Reusing a
request ID after an audit interruption recovers the existing reservation and
must never create a second task or message. The legacy `collab task dispatch`
and `collab task claim` commands remain deprecated and fail explicitly.

Consume notifications promptly with `collab recv`. A successful receive delivers
and acknowledges the batch atomically. After 3 delivered-but-unconsumed
notifications, push knocks pause automatically to prevent notification storms
and prompt pollution; `collab inbox` is read-only and does not resume delivery.
Use explicit `collab ack` only for legacy clients or recovery of an already
delivered message. Inspect peer/worker health, identity validity, and throttle
status at any time with `collab worker status [id]` or `collab who`.

Worker wake model: only master has long-horizon wake; workers are not
long-horizon wake targets and are not automatically woken from idle. A worker
acts on an explicit dispatch, a bounded direct-message lease, or its own open
task state;
it does not need periodic activation to make progress. Unknown/absent produces
no transport input. On each `working` -> `idle` transition, a worker sends one
idempotent worker-idle fact to the live master and then stops; it does not keep
knocking. Idle, progress, delivery, bug, and worker-idle notices are
auto-merged; explicit `collab sendmessage` remains immediate. Master idle
reminders are level-triggered: within the same master idle episode, each
reminder attempt consumes the shared episode-local budget, up to three
attempts. An observed change to `working` ends that episode; after three
consecutive attempts without an observed `working` change, the episode stops.
Worker-idle facts do not count toward this budget, and there is no
scheduling-turn counter or automatic rearm.

Managed subagents do not get child-targeted periodic liveness ACK loops. Their
state is persisted by the daemon; a `working` -> `idle` transition contributes
one durable `subagent-status` fact to the live master. The master, not the
child, owns the outcome and decides whether to re-dispatch, force-close, or
leave the child idle. A subagent with an unfinished task is not repeatedly
woken just because its task is not closed. `subagent status` includes tasks,
parent mailbox, counters and notification/ACK history.
`subagent snapshot <id> --lines 40` reads the screen only on request.

## Cross-project master communication

Collab communication across projects is master-only and explicit. Only a live
master may send to the live master of another initialized project; non-master
peers and managed subagents are rejected before any message is persisted:

```sh
collab master send --project /abs/path/to/target --to <target-master> \
  --subject <short-topic> "<original message>"
```

`--project` must be the exact target project root with `.agent-collab`, and
`--to` must be that project's live master. The target daemon also verifies the
sender-side `assigned_by` / `approval` / `assigned_ms` from the local master
status before accepting the message.

Task liveness is an obligation, not an ACK ceremony. A worker owns its assigned
tasks and drives them to verified cleanup/close during its working cycle. This
is a task-bound inspect obligation, not a transport activation schedule: it
does not wake idle workers and does not generate worker transport input. If an actionable
task is open, continue it; if it is blocked, find a concrete solution first,
then report it to the live master in the same activation. Do not leave a task
at `assigned`, `working`, `blocked`, `waiting`, `delivered`, or
`cleanup_pending` merely because the last direct message was acknowledged.
After delivery or merge, perform the real cleanup and close the task; a
reminder does not create a second task or a duplicate dispatch.

Escalation routing is explicit:

- A managed subagent and an ordinary worker both report blockers to the live
  Collab master immediately. Do not wait for the next liveness cycle or for
  master to invent the fix. First find a concrete solution (root cause,
  proposed change, authorization needed); escalate that, not a symptom.
  Lazy thinking is forbidden: do not dump "I'm blocked" and idle. If the
  assignment's delivery or test conditions are ambiguous, do not guess;
  propose the missing conditions and send them to master. A subagent must
  also copy its parent when parent is not the master, and may not decline a
  master collaboration request. Independent peers may temporarily decline a
  master collaboration invite to protect their own current task. If no live
  master exists, report to the collaborator that initiated the task. Include
  the task ID, exact blocker, proposed solution, attempted actions, and
  requested decision.
- A peer may promote itself to master only when no live master exists and the
  user explicitly approves that exact peer for that exact project. Record the
  approval with `collab master promote --approval "<user text>"` and verify the
  promoted peer has a live registered identity/transport before treating it as
  master. If a live master already exists, do not promote; only that master
  may `collab master delegate <peer>`. `appsdk init` alone never proves master
  ownership. Only `collab master status` proves whether a live master exists:
  `master` with `endpoint_live=true` means a live master exists; only
  `master: null` (with no `recorded_unusable` entry) means none exists. A
  missing worktree-local `.agent-collab/`, a failed `collab context`, a token
  mismatch, or a missing `who.master` field never proves there is no live
  master and never authorizes promotion. Codex root is not Collab master.
- If a blocker or wait cannot be executed locally after a real solution is
  found, report that solution to the live master immediately instead of
  silently waiting. Keep the durable wait/task state, continue any
  independent work, and re-escalate on the next direct master communication or
  when the situation changes; do not wait for a periodic worker wake.

## Master owns the outcome, not the excuse

Collab master is accountable for the final result of every assigned task
in the project. Once master accepts a dispatch, the master -- not the
worker -- is the escalation target, and the master cannot hide behind the
worker's blocker. Concretely:

- A live master must close any task that cannot otherwise be closed,
  including stuck or merged-but-unclean tasks, with `collab task close
  <id> --force --reason "<text>"`. The reason is recorded in the cleanup
  receipt so the manual close is auditable; notification obligations for that
  task owner are superseded.
- When a worker reports a blocker the master must take over ownership of
  the resolution: re-dispatch, close manually, or revise the assignment
  conditions. Master is not allowed to send an "I'm waiting on you"
  reply, mark the task blocked, and idle. If master cannot unblock the worker
  promptly, master force-closes the task with a reason so the loop stops and
  the worker's identity stays clean.
- An ordinary peer that cannot reach a live master within one escalation
  cycle may self-close its own task with `collab task close <id> --force
  --reason "<text>"`. If a task owner's registered transport identity is lost and no live
  master exists, a registered peer may close that orphaned task with the
  same `--force --reason` command. These are the only allowed fallbacks;
  the reason and cleanup receipt are mandatory so the daemon can show who
  closed what and why.
- Master may not delegate its accountability by passing the task back to
  the worker and waiting. Master either solves, re-dispatches, or force-
  closes. Doing nothing on a stuck task is a master failure, not a
  worker failure.


## Master splits and assigns

The human remains the only final authority for goals, money, irreversible
risk, and version promotion. Collab master is the user-approved project
dispatcher, not the human 主脑 and not Codex root. Master compiles
the goal into a task graph, then assigns; it does not take another peer's
task or worktree.

**Master is an architect and dispatcher, never an everyday code-author.**
Master authoring business diffs is an anti-pattern and a failure of division
of labor. Master's scarce capacity belongs to task graph compilation, strict
dependency boundaries, unblocking workers, and driving overall throughput.
Master operates under two prime directives:
1. **Exemplary Task Decomposition**: Slice goals along clear dependencies and
   non-overlapping file ownership. Every dispatched task must be closed-loop
   by design: define explicit done-iff (DoD), artifacts, forbidden edits, exact
   test commands, expected results, and evidence location. Every assignment must
   anticipate failure and define an exception resolution path: **fallbacks,
   silent downgrades, or masking errors are strictly prohibited**. A blocked
   worker must produce root-cause evidence and proposed fixes for master
   arbitration; master must actively close the lifecycle rather than patch
   output symptoms.
2. **Worker Capacity Saturation**: Master must keep the entire worker fleet
   fully saturated without idle time or serial bottlenecks.

**Sovereignty and Backlog Priority**:
- **No autonomous technical debt refactoring**: When assigned tasks complete
  and the fleet becomes idle, Master is strictly forbidden from autonomously
  launching long-range technical debt refactors, speculative architectural
  rewrites, or unapproved work. Master must formulate a structured proposal
  for the human user and pause. The human is the ultimate decision-maker;
  unbounded autonomous runs risk destabilizing user intent.
- **Autonomous Bug Tracking Backlog Resolution**: If pre-existing issues or
  requirements are logged in the bug system (`appsdk bug list --status open`),
  these represent authorized project work. Master autonomously pulls and
  dispatches open bugs in strict priority order (P0 > P1 > P2) to keep
  worker capacity saturated before suggesting closure.

**Worker->Master idle fact and master long-horizon wake**:
When a worker transitions from `working` to `idle`, it emits one idempotent
worker-idle fact to the live master. The master has long-horizon wake; the
worker does not. On an idle fact, master may:
1. Check the active task graph for unblocked downstream tasks and dispatch;
2. If the main graph is clear, pull the highest-priority open issue from the
   bug backlog (`appsdk bug list --status open`), with P0 first; P0 blocks the
   affected project;
3. If all tasks and bugs are closed, report completion and propose next
   steps to the user.

Master idle reminders are level-triggered. Within the same master idle episode,
each reminder attempt consumes the shared episode-local budget, up to three
attempts. An observed change to `working` ends that episode; after three
consecutive attempts without an observed `working` change, the episode stops.
Worker-idle facts do not count toward this budget, and there is no
scheduling-turn counter.
Master must not expect workers to be woken periodically.

**Unacknowledged Workers & Snapshot Diagnostic Closure**:
If a worker fails to acknowledge notifications or remains unresponsive across
repeated dispatches, never blindly loop sends or expect the model to self-correct.
Execute diagnostic closure immediately:
```sh
collab subagent snapshot <id> --lines 40
```
Inspect ground-truth terminal output to distinguish between interactive prompt
waits, process crashes, or infinite loops. Base all recovery decisions on
concrete snapshot evidence—adjusting instructions, force-closing dead tasks,
or restarting App Server threads—closing the loop deterministically.

Master keeps architecture, dispatch, integration, critical repair, and
final acceptance. Bulk implementation does not stay on the master's own
chain. Start a managed subagent with `appsdk subagent start --id <id>`
(optional `--runtime codex`), then `send <id> --subject <topic>
"<assignment>"`, or `collab sendmessage --to <peer>`. Give each child its
own worktree and file scope. Subagents must obey master and parent;
independent peers may decline an invite to protect their current task.
Wait for evidence summaries, then integrate. Chat tone is not completion.

A worker or subagent executes only the approved assignment, owns that
task's full lifecycle, and returns evidence. It has no global schedule.
On a blocker: find a concrete solution first, then report it to the live
master immediately. Do not wait. Do not lazy-think (symptoms without a
fix, or idle hoping master will design it). Copy parent if parent is not
master. If delivery or test conditions are unclear, propose the missing
conditions instead of guessing. Last owned `collab task close` cancels this
peer's direct-message auto-notify. To cancel a specific owner-scoped lease,
use `collab notify unsubscribe <subscription-id>`. After the task is done,
the task close lifecycle stops the owner's auto-notify; there is no separate
`collab notify close` command. Do not unsubscribe another peer's lease. Next collaboration re-arms with
`collab init` or `collab notify subscribe --event direct-message`.

AGY review is not used for Collab v1 lifecycle gates. Ordinary review uses an
independent review path when review is required; a milestone may use Astra when
required. Missing AGY is not a blocker because it is excluded; missing a
declared review gate is a failure.

Without an available registered transport, initialization and observer queries
report no notification channel. Use local `appsdk subagent list/status/snapshot`
without fake registration; check the mailbox in status yourself. No automatic
completion notification can reach this observer. Screen text is diagnostic,
never task/control truth.

| Intent | Command |
|---|---|
| Notify a peer now | `collab sendmessage --to <peer> --subject <short-topic> "<original message>"` |
| Receive and consume notifications | `collab recv` |
| Read one notification without consuming | `collab msg <notification-id>` |
| List unread messages | `collab inbox` |
| Recover an already-delivered notification | `collab ack <id>` or `collab ack --all` |
| Inspect worker health and notification status | `collab worker status [id]` |
| Read own authoritative context | `collab context` |
| List peers | `collab who` |
| Check own subscriptions | `collab notify status` |
| Inspect live master | `collab master status` |
| Promote this peer when no live master exists | `collab master promote --approval "<user text>"` |
| Delegate live master to another peer | `collab master delegate <peer>` |
| Split work to a managed subagent | `appsdk subagent start --id <id>` then `send <id> --subject <topic> "<assignment with delivery and test conditions>"` |
| Report a blocker to live master | `collab sendmessage --to <master> --subject blocker "<task_id; cause; proposed fix; decision needed>"` |
| Cancel one of your own notification leases | `collab notify unsubscribe <subscription-id>` |

After a transport preview, use its notification ID and abbreviated subject to weigh
urgency against the current task. When selecting the notice, run
`collab msg <notification-id>`, read durable detail, and execute the actionable
request inside this Agent's scope. Do not stop at ACK or waiting; mailbox truth
persists.

## Initialize once

For an AppSDK-governed project, the only bootstrap command is:

```sh
appsdk init .
```

In a live App Server Agent this runs official `collab init`,
starts/reuses the daemon, registers the current peer through the server-selected
transport, and creates/refreshes the finite reusable default `direct-message`
lease. Do not run a second `collab init`,
`collab whoami`, or manual ordinary-message subscription.

Only a standalone non-AppSDK project uses explicit `collab init`.

## Worktree identity

A Git worktree normally has no local `.agent-collab/`; that is not evidence
that the peer is unregistered or that no live master exists. Run
`collab context` from the worktree. The server resolves the canonical project
route from the global Collab state by the same Codex sessionID/App Server
thread, and the returned `project_root` is the canonical project root.

Use `collab master status` for the authoritative live-master answer; `collab
who` only lists registered peers. This query resolves the canonical route from
the global Collab route state and does not require a worktree-local
`.agent-collab/` or a new registration. Do not run `appsdk init`, `collab
init`, `collab worker recover`, or master promotion from a worktree, and do
not report "no master" because `.agent-collab/`, `collab context`, or a
`who.master` field is absent or failed. If `collab context` fails with
`token mismatch`, `PROJECT_SCOPE_UNKNOWN`, or another exact error, preserve
that error, run `collab master status` separately, and report the
registration problem to the live master. Do not infer "no master", recover by
copying or editing identity/token state, or reset the project. Only
`master status` returning `master: null` with no `recorded_unusable` entry
means no live master; then follow the explicit user-approved promotion
protocol.

### Thread-backed route resolution

When `CODEX_THREAD_ID` is present, the daemon is the sole route selector. The
CLI sends only the native App Server thread ID through the context-free
`RouteResolve` request. The daemon first finds the one global identity for that
thread, then matches that identity's current runtime binding; the global
identity's current binding is the route selector. Historical routes are not
candidates, even when they contain the same thread, binding ID, or generation.
`routes.jsonl` is only the host route admission/storage index, never a selector
or a fallback.

`collab route resolve` exposes this read-only lookup; it defaults to
`CODEX_THREAD_ID` and accepts `--native-thread-id <id>` for diagnostics.
Thread-backed `collab context`, `collab master status`, and normal scoped
commands must not use the current cwd or read `routes.jsonl` to guess a route.
The daemon returns exactly one route, `ROUTE_RESOLVE_NOT_FOUND` for zero
matches, and `ROUTE_RESOLVE_AMBIGUOUS` when multiple current global identities
are bound to the thread or the selected identity's binding still maps to
multiple routes. An invalid or malformed thread ID is
`ROUTE_RESOLVE_INVALID`. The resolver is read-only and returns no token;
identity/token loading remains a separate authentication step after the route
is selected.

## Subscribe to a future event

Use subscriptions only when this Agent wants a later event to wake it:

```sh
collab notify subscribe --event resource-released --subject <resource-id> \
  --ttl-seconds <bounded>
collab notify subscribe --event async-result --subject <operation-id> \
  --ttl-seconds <bounded>
collab notify subscribe --event deadline --subject <timer-id> \
  --ttl-seconds <bounded>
```

The sender never inspects or configures the recipient's subscription.
For subscription semantics or delivery diagnosis, read
[references/notifications.md](references/notifications.md).

Desktop does not register goal subscribe. Goal subscribe is master-only
long-horizon scheduling; workers and Desktop clients must not infer or
register it.

## Hard boundaries

- Never attach production work to v2 or `.agent-collab-v2`.
- Project scope comes from the exact process cwd. Never choose/search/hardcode
  a path. MCP and child commands inherit the same environment.
- Identities are equal peers by default. There is no implicit master from
  first registration, automatic process recovery, or inferred `/goal`. Collab
  master is explicit, user-approved project arbitration; it is not Codex root,
  and it does not take ownership of another peer's task. If a
  live registered master exists, other peers cannot promote and only that
  master may delegate. If no live master exists, a peer may promote itself
  only with explicit user approval and a live registered transport. Independent peers may
  decline a master collaboration invite; managed subagents must obey the
  master. Master splits by dependency then unique write scope, assigns
  subagents with unambiguous delivery and test conditions, and keeps
  architecture/integration/acceptance; it does not take another peer's
  task. Workers and subagents find a solution first, then report blockers
  to master immediately; they do not wait or dump symptoms. Last owned
  task close cancels that owner's auto-notify; use
  `collab notify unsubscribe <subscription-id>` for a specific leftover
  lease. AGY review is not a Collab v1 gate; ordinary review is independent,
  and a milestone may use Astra when required. Explicit managed subagent tasks
  use the task-bound inspect obligation above; it is not a free-form task queue
  and does not create worker transport input.
- Each peer owns its complete task/worktree/integration/resource/cleanup
  lifecycle. Never mutate or close another peer's work.
- Send only explicit notices, shared-resource coordination, or subscribed async
  results—not routine progress, heartbeat, ACK, review, or completion reports.
- A wake is only a signal. It cannot change task/resource truth, fabricate
  success, authorize maintenance, or create an ACK loop.
- `absent` or `unknown` Agent state produces no transport input. If the selected
  App Server thread is dead, reassigned, unowned, or mismatched, the
  subscription enters its explicit unavailable state to prevent storms.
  Each due batch is reserved durably once; failed or uncertain attempts are
  never automatically replayed, including after restart. Details remain
  readable in the inbox.

## Load details only when needed

- Task/worktree registration, delivery, close, cleanup:
  [references/task-worktree-lifecycle.md](references/task-worktree-lifecycle.md)
- Resource conflicts and bounded waits:
  [references/resource-waits.md](references/resource-waits.md)
- Migration, daemon stop/start, deprecated commands:
  [references/migration-daemon.md](references/migration-daemon.md)
- Source/release/install/restart verification:
  [references/verification.md](references/verification.md)

Do not load references for ordinary `sendmessage`, `msg`, or `inbox`.

The current v1 contract has no 15-minute periodic worker liveness. Worker and
master liveness wake only on a supported timer/wake, direct message, or real
external event. If any installed copy still contains the old wording, replace
it from this SKILL and its references.
