# v2 architecture — design-baseline-draft

```text
Codis containers -> Cordis orchestrator -> versioned IPC/events -> Rust Core
                                                         |
                              Rust extension plugins ---+
```

## Immutable core

`collab-core` is the only owner of identity/session mapping, role
authorization, task/claim transitions, resource ownership, journal replay,
mailbox truth, event ordering, idempotency, and restart recovery.

## Replaceable extensions

Transport, terminal probes, heartbeat/scheduler policy, resource policy,
MCP/CLI projections, and observability are plugins. They submit typed
commands or events; they do not mutate core state or journal files.

## Hot replacement

`discover -> validate -> start -> ready -> route switch -> drain -> dispose`.
An old plugin remains the active implementation until the replacement is
ready. A failed replacement is explicit and does not change core truth. Every
command/event carries an authenticated control context plus `plugin_instance_id`
and `route_epoch`; the core rejects stale instances after route switch.

## Commit and replay contract

The core reducer is the only sequence allocator. It checks `command_id`
idempotency before reducing, appends the committed event durably before
publishing it, and replays only contiguous sequences. Snapshots bind
`last_applied_sequence` and `schema_version`; gaps, duplicates, stale epochs,
and unauthorized control contexts fail explicitly.

Runtime health is control truth owned by Codis/Cordis. It may produce a typed
observation, but only an authorized core command may change business truth.
