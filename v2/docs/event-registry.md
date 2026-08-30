# v2 event registry — role-free control side-channel

All cross-module control flows use typed commands/results. Durable message
bodies stay in Rust mailbox truth; tmux carries only the message id.

Required envelope fields:

```text
command_id, sequence, operation, typed_control, typed_result, typed_error
```

Initial event families:

```text
Register, RegisterTask, TransitionTask, WaitTask, Subscribe,
SendResourceNotice, BeginWakeAttempt, CompleteWakeAttempt,
RecoverWakeAttempt, PublishSubscriptionEvent, ExpireSubscriptions,
ExpireWaits, and the migration commands.
```

Process, agent-state, retry, lease, wake, snapshot, and stop semantics remain
typed control-side data and never enter a business payload. There is no generic
event transport or fallback channel.
