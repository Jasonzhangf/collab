# v2 architecture — role-free runtime

```text
Codis containers -> Cordis orchestrator -> versioned IPC/events -> Rust Core
                                                         |
                              Rust extension plugins ---+
```

## Immutable core

Rust `CoreState` is the only owner of equal-peer identity/session mapping,
owner-scoped task transitions, resource notices, bounded waits, exact
subscriptions, wake leases, journal replay, mailbox truth, idempotency, and
restart recovery. There is no role, dispatch, claim, heartbeat, ACK, or
inferred continuation state.

## Replaceable extensions

The Node/Cordis layer is a typed adapter and read-only projection. The tmux
adapter is the only live notification channel and emits only
`COLLAB_NOTIFY <message-id>` plus `Enter`. Agent state is probed before an
explicit wake; absent, unknown, and working state produce zero input.

## Hot replacement

Daemon lifecycle is explicit and operator-owned. No agent identity, message,
subscription, timer, or peer event may start or restart it. A stopped marker
must remain effective until the operator explicitly clears it.

## Commit and replay contract

The core reducer is the only sequence allocator. It checks `command_id`
idempotency before reducing, appends the command durably before returning, and
replays only contiguous sequences. Wake leases have an expiry and require an
explicit recovery command; recovery never sends tmux, increments attempts, or
resets the immutable three-attempt lifetime cap.

Runtime observations are control-side inputs only. They cannot mutate business
truth and are never copied into business payloads.
