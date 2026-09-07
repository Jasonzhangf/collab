# Notifications

Read this only for event subscriptions or notification delivery diagnosis.
Ordinary peer notification is always:

```sh
collab sendmessage --to <peer> --subject <short-topic> "<original message>"
```

## Subscription commands

```sh
collab notify subscribe --event direct-message --ttl-seconds <bounded>
collab notify subscribe --event resource-released --subject <resource-id> \
  --ttl-seconds <bounded>
collab notify subscribe --event deadline --subject <timer-id> \
  --ttl-seconds <bounded>
collab notify subscribe --event async-result --subject <operation-id> \
  --ttl-seconds <bounded>
collab notify status
collab notify unsubscribe <subscription-id>
collab notify close
```

- AppSDK project initialization creates/refreshes the seven-day reusable default
  `direct-message` lease through official `collab init`. An explicit owner
  unsubscribe of that lease stays cancelled; later `register` / `context` /
  `ack` must not silently re-arm it. Last owned `collab task close` cancels
  the owner's direct-message auto-notify. `collab notify close` cancels all
  own armed subscriptions. After a finished task, a leftover keepalive can
  be stopped the same way. `collab init` or
  `collab notify subscribe --event direct-message` re-arms it for the next
  collaboration.
- Direct-message leases are owner-scoped and reusable until expiry. Resource,
  deadline, and async-result subscriptions are exact-subject and one-shot.
- Success consumes only a one-shot event subscription. Expiry or unsubscribe
  ends any subscription; one attempted batch exhausts only its messages on a
  reusable direct-message lease.
- Before every attempt, the daemon revalidates owner, event, subject, TTL,
  pane liveness, pane ownership, worker registration match, Agent presence, and
  Agent state. If the pane is dead, unowned, or mismatched (`identity-mismatch`),
  or the agent is `absent`, the subscription immediately transitions to `pane-lost`
  to prevent notification storms. `absent` and `unknown` produce zero tmux input.
- Timer ticks, restart, replay, re-registration, or delivery mode cannot reset
  the one-attempt lifetime cap. Delivery requires the agent to be in a prompt/idle
  `waiting` state; actively `working` agents defer delivery without burning
  attempts, protecting active execution from pollution.
- Unacknowledged notification throttling (Backpressure): To prevent notification
  storms and terminal pollution, push knocks pause when unacknowledged notifications
  reach `max_unacked` (default 3, range 1-5). Run `collab ack <id>` or `collab ack --all`
  to resume push delivery. Messages remain safely buffered in the durable mailbox.
- First pending message starts a fixed 60-second window. At dispatch, include
  eligible unsent messages for the recipient, capped at 3 previews per batch knock.
  Excess messages remain in the inbox with `[+N more pending in inbox]`. When unacked
  notifications reach the throttle cap, the batch preview appends an
  `[ACK REQUIRED: ...]` notice. Combine previews into one line with one final Enter;
  at most one batch attempt per recipient per minute. Reserve attempts before sending;
  failed, absent, unknown, or uncertain delivery does not cause later automatic replay.
- One safe preview contains notification ID, abbreviated subject, and one-line
  original body. Control characters are escaped. Cursor delivery is two tmux
  processes: literal payload, then `C-m` after 250ms, so bracketed paste cannot
  swallow Enter. Codex keeps `paste-buffer -p` and `C-m` in one tmux queue so
  the paste is submitted. Dual tmux sessions must prove both mailbox and pane Enter.
- Full subject/body remains in the mailbox without a matching subscription.
  This outcome is not a sender-selected `mailbox-only` mode.
- A failed/lost/delayed/duplicate wake never rolls back mailbox truth or counts
  as lifecycle evidence.
- The daemon never infers continuation from task state, pane title, heartbeat,
  progress, ACK, or elapsed time, and must not create a `CONTINUE_TASK` message.
  The 15-minute task-liveness rule is an Agent/skill obligation: when a
  legitimate keepalive or scheduled check arrives, the owner inspects durable
  task state and continues or escalates it. It is not a second queue or a
  synthetic periodic task.

## Worker inspection and acknowledgment

```sh
collab ack <notification-id>
collab ack --all
collab worker status [worker-id]
collab who
```

- `collab ack <id>` acknowledges a single notification.
- `collab ack --all` acknowledges all unread or pending delivered notifications in one step.
- `collab recv` reads and consumes the returned message batch atomically; a
  successful receive writes `Delivered` and `Acked` together, so a follow-up
  ACK is not required. `collab msg`, `collab inbox`, and `collab context` are
  read-only and do not consume messages. Keep `ack` for legacy clients or
  explicit recovery of already-delivered messages.
- `collab worker status [worker-id]` inspects real-time worker health, including
  `endpoint_live`, `identity_valid`, `agent_state`, `unacked_notifications`,
  `notifications_paused`, `suspected_offline`, and `active_task`.

After a preview, compare its ID/subject, urgency, current task, and interruption
cost. Read durable details only when appropriate:

```sh
collab msg <notification-id>
collab inbox
collab context
```
