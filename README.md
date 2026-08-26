# collab

Project-local coordination daemon + CLI for multi-agent work inside one working
tree. Rust, single static binary, no network — a unix socket under
`.agent-collab/` is the whole transport.

## Why

Multiple LLM agents (each an LLM reasoning loop) work in the same repo. Every
wake-up costs tokens, so coordination must be event-driven, not polled by the
agents. `collab` gives them:

- a mailbox with long-poll `recv` (wait costs zero tokens)
- task registry: one active owner per task; feature/worktree conflicts are
  checked at registration
- tmux knock: when a message lands, the server types `[MAIL] ...` into the
  target worker's tmux pane, waking the agent immediately
- server-side timers: task-scoped heartbeats (45 minutes by default) and request
  escalation, all journaled
- request filtering: one live request per sender/recipient direction during a
  5-minute cooldown; duplicate replies are superseded so only the newest one is
  delivered

## Install

```sh
cargo build --release
install -m 755 target/release/collab ~/.local/bin/collab
```

## Usage

```sh
cd <project-root>
collab init                  # create .agent-collab skeleton
                             # also writes docs/collab.md when absent
collab up                    # start daemon (idempotent)
collab whoami                # print your identity (auto-announces your tmux pane)
collab status                # server summary

collab send --to <worker> --type request "can I take build?"  # -> msg_id
collab recv --timeout 600    # long-poll; returns when mail arrives
collab inbox                 # unread messages
collab ack <msg_id>          # mark read (clears nudges)
collab msg <msg_id>          # delivery state + nudge count for a sent request

collab role                  # master if first registered, otherwise worker
collab config                # show .agent-collab/collab.json
collab config --heartbeat-minutes 45

# Master creates a dispatchable task. It stays owned by master and available.
collab task register my-task \
  --feature zterm.feature-id \
  --worktree ./playground/my-task-<run-id> \
  --branch codex/my-task \
  --base-commit <sha> \
  --priority p1              # p0 highest through p4 lowest; defaults to p2

# A worker claims it without asking permission. Claim atomically transfers
# ownership to the worker and starts the task.
collab task claim my-task

# Master can register another registered worker as owner.
collab task register peer-task --owner pane-552 --feature other.feature-id

collab task status [task-id]
collab task update my-task --status verifying --next "run integration gate"
collab task deliver my-task --evidence "commit=<sha>; gates=pass" # worker completion
collab task update my-task --status merged      # master only
collab task dispatch                            # master only: assign available to idle
collab task close my-task                       # master closes merged work and cleans resources
```

## Identity

Identity is keyed to your tmux pane: the first `collab whoami` in a pane
creates `.agent-collab/runs/by-pane/<pane>.json` and registers worker
`pane-<pane>` with a secret token. Later calls in the same pane reuse the same
identity automatically. Token proves mailbox ownership; `recv`/`ack` require it
and refuse messages not addressed to you.

Outside tmux you can pass `--worker <id>` to act as a specific identity.

## Roles and tasks

- The first registered identity becomes `master`; every later identity becomes
  `worker`. There is never an automatic second master or automatic takeover.
- Master owns progress decisions, merge authority, rework/close/cancel, and
  final worktree cleanup. A worker owns one task, including implementation,
  tests, evidence, review, delivery report, and bounded safe continuation when
  master is temporarily unavailable.
- Only master may register tasks. A worker must claim an existing `available`
  task; registration by a worker is rejected.
- The fixed task record is `id / owner / feature_id / worktree_path / branch /
  base_commit / priority / status`. Valid statuses are exactly `available`,
  `working`, `verifying`, `reviewed`, `delivered`, `rework`, `merged`,
  `closed`, and `cancelled`.
- Available tasks are returned in priority order (`p0` through `p4`), then by
  creation time.
- An `available` task is held by master as a dispatch placeholder. Worker claim
  atomically transfers ownership, sets status to `working`, and starts its
  heartbeat. A worker cannot hold more than one active task.
- A registered task is the resource owner. Registration fails when another
  active task already owns the same `feature_id` or `worktree_path`. Inspect
  `collab task status` before choosing independent parallel work.
- Worker lifecycle: claim turns `available` into `working`; the worker may move
  its own claim through `working -> verifying -> reviewed`; `collab task
  deliver <id> --evidence ...` atomically sets `delivered`, notifies master,
  returns this worker's identity, and returns the available board in priority
  order. The worker should immediately claim from that response instead of
  sending another completion report.
- Master rejection is sent as one message; after the worker consumes it, it
  moves the same task back to `working`. `delivered` stops heartbeat but keeps
  resource conflict until master closes it.
- After reviewing and merging, master runs `collab task close <id>`. Close
  refuses a worker, an unclaimed/available task, or a task not in
  `delivered|merged`. It verifies the branch is merged into HEAD, removes a
  clean declared worktree under `./playground/`, safe-deletes the merged branch
  with Git's own safety checks, marks the task `closed`, and notifies the former
  owner. Close then reconciles the board in the same state transaction:
  available tasks are dispatched in priority order (`p0` first) to idle
  workers with no active claim; each assignment becomes `working`, records the
  worker as owner, writes a mailbox message, and submits a tmux prompt. When
  all workers are busy, the remaining board stays `available` and no duplicate
  message is sent. Any dirty, missing, outside-playground, or unmerged resource
  fails closed before state mutation.
- Master can also run `collab task dispatch` after registering a new batch. It
  performs the same available-to-idle assignment without waiting for another
  close, so the merge → decompose → register → dispatch loop is immediate.
- Only master may set `merged`, `cancelled`, or `closed`.
- Task heartbeat runs every 45 minutes only while status is
  `working/verifying/reviewed`. A worker with no active claim receives no
  heartbeat. Each heartbeat tells the worker that it has an active claim and to
  run `collab role` and `collab task status <task-id>` before continuing. It
  sends once per interval and does not resend while pending. Any owner/master
  task update clears pending state; `delivered`, `merged`, `cancelled`, or
  `closed` unregisters it.
- Project configuration lives at `.agent-collab/collab.json`. Read it with
  `collab config`; update it with `collab config --heartbeat-minutes 45`.
  The daemon reads this file on each scheduler tick, so changes take effect
  without a rebuild or restart. Values must be whole minutes >= 1.
- Heartbeat asks the owner to inspect state and continue. It never asks for an
  ACK. Report to master only for a state change, blocker, ETA change, decision,
  verification result, or handoff. `collab who` exposes each worker's active
  task/status so master can distinguish working workers from available workers
  without messaging them.
- Workers remain registered after task closure and can receive another task.
- Master's post-merge workflow is deterministic: after each merge, run
  `collab task close <id>`, then immediately register the next decomposed
  tasks with `--next`, then run `collab task dispatch`. If a worker is idle,
  dispatch assigns an `available` task to that worker automatically. If every
  worker is busy, leave the new tasks `available`; workers claim them from the
  returned board.
- Master task registration is the task decomposition contract: every task must
  carry a stable `id`, `feature_id`, worktree path, branch, base commit,
  priority, and next-step text. The daemon uses that fixed format in the
  automatic assignment prompt.
- After delivery, a worker immediately checks the board and claims the next
  available task without asking permission. Rework on a previously delivered
  task takes precedence over the interrupted task, especially when it blocks
  another worker or merge. For a new dispatch during active work, finish the
  current task unless the new task has explicitly higher priority or blocks the
  critical path.
## Protocol / invariants

- Single writer: all shared state lives inside the daemon. Agents never write
  each other's directories.
- Journal: every mutation is an append-only event (`server/journal.jsonl`);
  restart = replay. Atomicity of reads is guaranteed by the daemon.
- Messaging: `notify` (read and continue silently when no action is needed),
  `request` (exactly one substantive reply expected), `reply`
  (`--in-reply-to`, send only when meaningful). New requests from the same sender to the same
  recipient are rejected during a 5-minute cooldown while an earlier request is
  still unanswered. The recipient receives one nudge after 5 minutes; the
  sender receives an escalation notice after 15 minutes. If multiple replies
  are sent, earlier ones are marked `superseded` and only the newest is
  delivered.
- Every send commits to the mailbox, then submits a tmux notification using the
  verified zterm v1 sequence: literal text first
  (`tmux send-keys -t <pane> -l -- <text>`), then exactly one tmux Enter key
  (`tmux send-keys -t <pane> Enter`). Short content (up to 500 characters) is
  sent once inline. Longer content is stored under
  `.agent-collab/messages/<message-id>.md`, while tmux receives only a short
  `body-ref=<path>` reference. A timed-out blocking `recv` also produces one
  submitted timeout reminder. Knock failures are logged when panes no longer
  exist; mailbox state remains authoritative.
- A tmux delivery is a reasoning prompt carrying `from`, the full short body or
  actionable long-body reference, and a continuation anchor. Bare ACK/mailbox-
  ID prompts are invalid for real messages. System notices, nudges, and
  wait-timeout reminders use the same full prompt path.
- On `[MAIL]`, read the referenced mailbox body first, decide whether to
  collaborate, defer, or reject. Send one substantive result/evidence/next-step
  reply only when required; for an informational notify/reply with no action,
  continue silently without ACK-only noise. Then execute the current run's
  next product/verification step. A reply is work input, not a reason to idle;
  if no next step is recorded, derive one from the active task and
  execute it.
- Receive state machine: `MAIL_RECEIVED -> READ_BODY -> DECIDE ->
  REPLY_IF_REQUESTED -> ACK -> RESUME_OWN_TASK -> REPORT`. `ACK` is not a
  terminal state. "Received", "unread 0", "monitor", and "await next request"
  are not substantive completion reports. If the pane is busy, consume the
  pending prompt at the next turn before unrelated work.
- Blocking waits are bounded at five minutes for foreground work. On timeout,
  inspect the current run/task next step and continue immediately if a safe
  action exists; report blocked with evidence only if no safe action remains.

## Out of scope (explicit)

- Cross-directory/cross-host coordination (one server per `.agent-collab`).
- Auto-takeover of high-risk resources; escalation is always to a human/controller.
