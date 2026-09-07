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
  pane, Agent presence, and Agent state. `absent` and `unknown`
  produce zero tmux input.
- Timer ticks, restart, replay, re-registration, or delivery mode cannot reset
  the one-attempt lifetime cap. Working Agents receive notifications directly.
- First pending message starts a fixed 60-second window. At dispatch, include
  all eligible unsent messages for the recipient, including newer arrivals.
  Combine previews into one line with one final Enter; at most one batch
  attempt per recipient per minute. Reserve attempts before sending; failed,
  absent, unknown, or uncertain delivery does not cause later automatic replay.
- One safe preview contains notification ID, abbreviated subject, and one-line
  original body. Control characters are escaped. Cursor delivery is two tmux
  processes: literal payload, then `C-m` after 250ms, so bracketed paste cannot
  swallow Enter. If that Cursor pane is working, one later empty `C-m` steers
  the follow-up into the active run instead of leaving it queued. Codex keeps
  `paste-buffer -p` and `C-m` in one tmux queue so
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

After a preview, compare its ID/subject, urgency, current task, and interruption
cost. Read durable details only when appropriate:

```sh
collab msg <notification-id>
collab inbox
collab context
```
