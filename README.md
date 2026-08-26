# collab

Project-local coordination daemon + CLI for multi-agent work inside one working
tree. Rust, single static binary, no network — a unix socket under
`.agent-collab/` is the whole transport.

## Why

Multiple LLM agents (each an LLM reasoning loop) work in the same repo. Every
wake-up costs tokens, so coordination must be event-driven, not polled by the
agents. `collab` gives them:

- a mailbox with long-poll `recv` (wait costs zero tokens)
- resource claims with leases and FIFO waiting queues
- tmux knock: when a message lands, the server types `[MAIL] ...` into the
  target worker's tmux pane, waking the agent immediately
- server-side timers: lease expiry watch, unanswered-request nudges (one
  recipient reminder after 5 min, sender escalation after 15 min), all journaled
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

collab claim acquire <id> --lease 30 --intent "compiling"
collab claim wait <id> --timeout 1800   # block until free; FIFO queued
collab claim renew <id> --lease 30
collab claim release <id>
collab claim status [id]
```

## Identity

Identity is keyed to your tmux pane: the first `collab whoami` in a pane
creates `.agent-collab/runs/by-pane/<pane>.json` and registers worker
`pane-<pane>` with a secret token. Later calls in the same pane reuse the same
identity automatically. Token proves mailbox ownership; `recv`/`ack` require it
and refuse messages not addressed to you.

Outside tmux you can pass `--worker <id>` to act as a specific identity.

## Protocol / invariants

- Single writer: all shared state lives inside the daemon. Agents never write
  each other's directories.
- Journal: every mutation is an append-only event (`server/journal.jsonl`);
  restart = replay. Atomicity of reads is guaranteed by the daemon.
- Claims: acquire (lease 30m default), renew during long ops, release when
  done. Expiry is a liveness signal; `--force` takeover is logged and the
  previous owner is notified.
- FIFO: waiters are queued on first `claim wait` / failed acquire; on release
  the claim is reserved for the longest-waiter.
- Messaging: `notify` (no reply), `request` (exactly one reply expected),
  `reply` (`--in-reply-to`). New requests from the same sender to the same
  recipient are rejected during a 5-minute cooldown while an earlier request is
  still unanswered. The recipient receives one nudge after 5 minutes; the
  sender receives an escalation notice after 15 minutes. If multiple replies
  are sent, earlier ones are marked `superseded` and only the newest is
  delivered.
- Every send commits to the mailbox, then submits a tmux notification with one
  Codex TUI carriage return (`tmux send-keys -t <pane> C-m`). Short content (up to 500 characters) is sent once inline. Longer
  content is stored under `.agent-collab/messages/<message-id>.md`, while tmux
  receives only a short `body-ref=<path>` reference. A timed-out blocking `recv` or
  `claim wait` also produces one submitted timeout reminder. Knock failures are
  logged when panes no longer exist; mailbox state remains authoritative.
- A tmux delivery is a reasoning prompt carrying `from`, the full short body or
  actionable long-body reference, and a continuation anchor. Bare ACK/mailbox-
  ID prompts are invalid for real messages. System notices, nudges, and
  wait-timeout reminders use the same full prompt path.
- On `[MAIL]`, read the referenced mailbox body first, decide whether to
  collaborate, defer, or reject, send one substantive result/evidence/next-step
  reply when required, ack processed IDs, then execute the current run's next
  product/verification step. A reply is work input, not a reason to idle; if no
  next step is recorded, derive one from the active claim/task and execute it.
- Receive state machine: `MAIL_RECEIVED -> READ_BODY -> DECIDE ->
  REPLY_IF_REQUESTED -> ACK -> RESUME_OWN_TASK -> REPORT`. `ACK` is not a
  terminal state. "Received", "unread 0", "monitor", and "await next request"
  are not substantive completion reports. If the pane is busy, consume the
  pending prompt at the next turn before unrelated work.
- Blocking waits are bounded at five minutes for foreground work. On timeout,
  inspect the current run/claim next step and continue immediately if a safe
  action exists; report blocked with evidence only if no safe action remains.

## Out of scope (explicit)

- Cross-directory/cross-host coordination (one server per `.agent-collab`).
- Auto-takeover of high-risk resources; escalation is always to a human/controller.
