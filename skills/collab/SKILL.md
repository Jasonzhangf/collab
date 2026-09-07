---
name: collab
description: >
  Coordinate independent peers with reusable finite direct-message leases,
  one-shot event subscriptions, task/worktree ownership, resource waits,
  controlled daemon maintenance, and explicit user-approved master promotion
  when no live master exists. Guidance: (1) recovery: inspect durable status,
  verify the real live tmux identity, re-register/rebind only the named peer,
  then send one recovery report request; (2) failure: preserve the exact error,
  keep journal/mailbox truth, do not retry or claim delivery, and escalate with
  root cause and evidence; (3) reset: never delete journal/mailbox, copy tokens,
  reset bindings, or start a second daemon; use explicit down/up or migration
  only; (4) regression recognition: distinguish durable send, tmux delivery,
  agent response, ACK/consume, task close, and cleanup evidence. Ordinary peer
  notices use one direct command with no discovery or retry step. Codex/Cursor
  root is not Collab master.
---

# Collab

Durable truth lives in the project server. tmux carries only a bounded wake
preview. Production projects use the globally installed Collab v1.

## Recovery, Failure, Reset, Regression

### 1. Recovery

Use recovery only after a daemon restart, pane replacement, identity mismatch,
or an explicitly reported delivery failure:

```sh
collab status --all
collab worker status <peer>
collab context
```

Verify the recorded pane is live and owned by the same real tmux identity.
If the pane is stale, use the explicit pane-scoped re-registration/rebind
path for that peer, then send one registration/report request. Do not inject
`collab init` into a foreign pane, guess among multiple panes, or replay an
old message batch. A recovery request is a maintenance action, not a normal
keepalive.

### 2. Failure

Treat each claim separately:

```text
durable=true -> mailbox journal accepted
notification=sent -> tmux command path accepted
pane evidence -> TUI received/submitted the preview
recv response -> peer consumed the message
task close receipt -> lifecycle ended
```

An error, timeout, `subscribed-not-sent`, `pane-lost`, `identity-mismatch`,
`unknown`, or absent Agent is not success. Preserve the exact error and
durable IDs; do not retry automatically, ACK for another identity, or mark a
task delivered/closed without its required evidence. A worker reports the
root cause and proposed fix to the live master. The master takes ownership by
fixing, re-dispatching, or force-closing with an auditable reason.

### 3. Reset

There is no routine destructive reset:

```text
collab down/up -> controlled daemon restart; journal/mailbox survive
collab migrate -> authenticated migration and identity rebind
```

Never remove `.agent-collab/server/journal.jsonl`, mailbox files, identity
tokens, task records, or bindings to make status look clean. Never start a
second daemon or use broad process kills. `collab ack` remains a compatibility
operation; it is not a substitute for task close or identity recovery.

### 4. Regression recognition

After a fix, verify the same user path again and classify the first divergence:

- `send` durable but no tmux preview: inspect subscription, pane liveness,
  ownership, Agent state, and daemon log.
- tmux preview appears but no worker result: inspect the pane snapshot and
  worker state; do not call that a reply.
- `recv` returns messages: the read is consumed atomically; no follow-up ACK is
  required. `msg`, `inbox`, and `context` remain read-only.
- task remains open: inspect owner identity, master responsibility, cleanup
  receipt, and keepalive supersession.

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
60-second window by default. `~/.appsdk/config.toml` can select immediate or
batched delivery globally or per project; `appsdk config` shows effective
policy. All eligible unsent messages for that recipient are combined
into one single-line tmux write and one Enter (up to 3 previews per knock, with
overflow retained in the inbox). Cursor gets literal keys, a 250ms settle, then
`C-m` in a second tmux process. Codex keeps `paste-buffer -p` and `C-m` in the
same tmux queue. Delivery requires the agent to be in safe waiting/idle state;
actively working panes defer delivery without burning attempts so in-flight tasks
are not polluted. If delivered-but-unconsumed notifications reach the throttle
threshold (default 3), further push knocks pause until `collab recv` consumes
them, preventing terminal pollution and storms. Each batch has one attempt;
the default window is one minute. Policy changes require controlled daemon restart,
not task reset.

Do not retry a failed send automatically. Return its exact error and durable
status. Never call `tmux send-keys` directly.

## Common command card

For user-requested persistent subagents, run `appsdk subagent start --id <id>`.
That starts Cursor CLI with `--model auto`. Override with `--runtime cursor|codex`.
Do not look up `agent --help` or start Codex unless config/`--runtime` is `codex`.
Cursor health is `agent status --format json`, not snapshot. Then `status`,
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

Consume notifications promptly with `collab recv`. A successful receive delivers
and acknowledges the batch atomically. After 3 delivered-but-unconsumed
notifications, push knocks pause automatically to prevent notification storms
and prompt pollution; `collab inbox` is read-only and does not resume delivery.
Use explicit `collab ack` only for legacy clients or recovery of an already
delivered message. Inspect peer/worker health, identity validity, and throttle
status at any time with `collab worker status [id]` or `collab who`.

Task keepalive: only unfinished actionable tasks plus explicit idle qualify;
one activation per 15 minutes, grouped per worker. Consume a keepalive with
`collab recv`, then work or record a blocker. Sending a message or positive
working observation also counts as activity. Three unconfirmed attempts stop;
never automatically `subagent rearm` to bypass exhaustion. Unknown stays unknown.

Managed subagents do not get child-targeted keepalive ACK loops. When the
daemon probes a managed subagent and finds it `idle` or `working`, it persists
that state on the subagent and sends one durable `subagent-status` message to
the live master. The master, not the child, owns the outcome and decides
whether to re-dispatch, force-close, or leave the child idle. A subagent with
an unfinished task is not repeatedly woken just because its task is not closed.
`subagent status` includes tasks, parent mailbox, counters and notification/ACK
history. `subagent snapshot <id> --lines 40` reads the screen only on request.

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

Task liveness is an obligation, not an ACK ceremony. Every assigned task that
has not reached verified cleanup/close is checked at least once per 15 minutes.
When the check arrives, continue the task immediately if actionable; if it is
blocked, find a concrete solution first, then report it to the live master
in the same cycle. Do not leave a
task at `assigned`, `working`, `blocked`, `waiting`, `delivered`, or
`cleanup_pending` merely because the last wake was acknowledged. After delivery
or merge, perform the real cleanup and close the task; a reminder does not
create a second task or a duplicate dispatch.

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
  promoted peer has a live registered identity/pane before treating it as
  master. If a live master already exists, do not promote; only that master
  may `collab master delegate <peer>`. `appsdk init` alone never proves master
  ownership; a missing or dead master pane means there is no live master, not
  permission to invent one. Codex/Cursor root is not Collab master.
- If a blocker or wait cannot be executed locally after a real solution is
  found, report that solution to the live master immediately instead of
  silently waiting. Keep the durable wait/task state, continue any
  independent work, and recheck the escalation on the next 15-minute
  liveness cycle.

## Master owns the outcome, not the excuse

Collab master is accountable for the final result of every assigned task
in the project. Once master accepts a dispatch, the master -- not the
worker -- is the escalation target, and the master cannot hide behind the
worker's blocker. Concretely:

- A live master must close any task that cannot otherwise be closed,
  including stuck or merged-but-unclean tasks, with `collab task close
  <id> --force --reason "<text>"`. The reason is recorded in the cleanup
  receipt so the manual close is auditable; keepalives for that task owner
  are superseded and the worker pane stops waking.
- When a worker reports a blocker the master must take over ownership of
  the resolution: re-dispatch, close manually, or revise the assignment
  conditions. Master is not allowed to send an "I'm waiting on you"
  reply, mark the task blocked, and idle. If master cannot unblock the
  worker within the same liveness cycle, master force-closes the task
  with a reason so the wake loop stops and the worker's identity stays
  clean.
- An ordinary peer that cannot reach a live master within one escalation
  cycle may self-close its own task with `collab task close <id> --force
  --reason "<text>"`. If a task owner's tmux identity is lost and no live
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
dispatcher, not the human 主脑 and not Codex/Cursor root. Master compiles
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

**Worker Free Trigger & Light Interruption**:
When a worker transitions from `working` to `idle` (and has no active task),
this state change acts as a primary scheduling trigger. Collab delivers a
lightweight, non-intrusive wake knock to Master. Master must immediately:
1. Check the active task graph for unblocked downstream tasks and dispatch;
2. If the main graph is clear, pull the highest-priority open issue from
   `appsdk bug list --status open`;
3. If all tasks and bugs are closed, report completion and propose next
   steps to the user.

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
or restarting panes—closing the loop deterministically.

Master keeps architecture, dispatch, integration, critical repair, and
final acceptance. Bulk implementation does not stay on the master's own
chain. Start a managed subagent with `appsdk subagent start --id <id>`
(optional `--runtime cursor|codex`), then `send <id> --subject <topic>
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
peer's direct-message auto-notify. Explicit `collab notify close` does the
same at any time. After the task is done, a leftover keepalive or other
notice may be closed that way so tmux stops injecting wakes. Do not
unsubscribe another peer's lease. Next collaboration re-arms with
`collab init` or `collab notify subscribe --event direct-message`.

AGY review is optional. If AGY is unavailable, use Codex review. If neither
exists, the live master reviews. Missing AGY or Codex review is not a
Collab blocker.

Without tmux, initialization and observer queries report no notification channel.
Use local `appsdk subagent list/status/snapshot` without fake registration;
check the mailbox in status yourself. No automatic completion notification can
reach this observer. Screen text is diagnostic, never task/control truth.

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
| Close own notifications after the task is done | `collab notify close` |

After a tmux preview, use its notification ID and abbreviated subject to weigh
urgency against the current task. When selecting the notice, run
`collab msg <notification-id>`, read durable detail, and execute the actionable
request inside this Agent's scope. Do not stop at ACK or waiting; mailbox truth
persists.

## Initialize once

For an AppSDK-governed project, the only bootstrap command is:

```sh
appsdk init .
```

In a live tmux Agent this runs official `collab init`, starts/reuses the daemon,
registers the current peer, and creates/refreshes the finite reusable default
`direct-message` lease. Do not run a second `collab init`,
`collab whoami`, or manual ordinary-message subscription.

Only a standalone non-AppSDK project uses explicit `collab init`.

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

## Hard boundaries

- Never attach production work to v2 or `.agent-collab-v2`.
- Project scope comes only from inherited tmux pane cwd, or exact process cwd
  for an explicit non-tmux operator. Never choose/search/hardcode a path. MCP
  and child commands inherit the same environment.
- Identities are equal peers by default. There is no implicit master from
  first registration, automatic process recovery, or inferred `/goal`. Collab
  master is explicit, user-approved project arbitration; it is not Codex or
  Cursor root, and it does not take ownership of another peer's task. If a
  live registered master exists, other peers cannot promote and only that
  master may delegate. If no live master exists, a peer may promote itself
  only with explicit user approval and a live pane. Independent peers may
  decline a master collaboration invite; managed subagents must obey the
  master. Master splits by dependency then unique write scope, assigns
  subagents with unambiguous delivery and test conditions, and keeps
  architecture/integration/acceptance; it does not take another peer's
  task. Workers and subagents find a solution first, then report blockers
  to master immediately; they do not wait or dump symptoms. Last owned
  task close cancels that owner's auto-notify; `collab notify close` can
  do the same after a leftover wake. AGY review is optional and may
  degrade to Codex review or live master review; missing reviewers are
  not a blocker. Explicit
  managed subagent tasks may use the finite task-bound keepalive above;
  it is not a free-form task queue.
- Each peer owns its complete task/worktree/integration/resource/cleanup
  lifecycle. Never mutate or close another peer's work.
- Send only explicit notices, shared-resource coordination, or subscribed async
  results—not routine progress, heartbeat, ACK, review, or completion reports.
- A wake is only a signal. It cannot change task/resource truth, fabricate
  success, authorize maintenance, or create an ACK loop.
- `absent` or `unknown` Agent state produces no tmux input. If the pane is dead,
  reassigned, unowned, or mismatched, subscriptions transition to `pane-lost` to
  prevent storms. Each due batch is reserved durably once; failed or uncertain
  attempts are never automatically replayed, including after restart. Details remain
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
