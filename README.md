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
- server-side timers: task-scoped heartbeats (15 minutes) and request
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
collab up                    # start daemon (idempotent)
collab whoami                # print your identity (auto-announces your tmux pane)
collab status                # server summary

collab send --to <worker> --type request "can I take build?"  # -> msg_id
collab recv --timeout 600    # long-poll; returns when mail arrives
collab inbox                 # unread messages
collab ack <msg_id>          # mark read (clears nudges)
collab msg <msg_id>          # delivery state + nudge count for a sent request

collab role                  # master if first registered, otherwise worker

# Register your own task; you become its owner.
collab task register my-task \
  --feature zterm.feature-id \
  --worktree ./playground/my-task-<run-id> \
  --branch codex/my-task \
  --base-commit <sha>

# Master can register another registered worker as owner.
collab task register peer-task --owner pane-552 --feature other.feature-id

collab task status [task-id]
collab task update my-task --status verifying --next "run integration gate"
collab task update my-task --status delivered   # worker completion
collab task update my-task --status merged      # master only
collab task update my-task --status closed      # master only
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
- A registered task is the resource owner. Registration fails when another
  active task already owns the same `feature_id` or `worktree_path`. Inspect
  `collab task status` before choosing independent parallel work.
- Worker lifecycle: `working -> verifying -> reviewed -> delivered`, then
  master merges and closes. Master rejection is sent as one message; after the
  worker consumes it, it moves the same task back to `working`. `delivered`
  stops heartbeat but keeps resource conflict until master closes it.
- Only master may set `merged`, `cancelled`, or `closed`.
- Task heartbeat runs every 15 minutes only while status is
  `working/verifying/reviewed`. It sends once per interval and does not resend
  while pending. Any owner/master task update clears pending state;
  `delivered`, `merged`, `cancelled`, or `closed` unregisters it.
- Heartbeat asks the owner to inspect state and continue. It never asks for an
  ACK. Report to master only for a state change, blocker, ETA change, decision,
  verification result, or handoff.
- Workers remain registered after task closure and can receive another task.
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
