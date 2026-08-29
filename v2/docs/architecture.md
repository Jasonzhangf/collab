# v2 architecture — v2-design-freeze-1

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
ready. A failed replacement is explicit and does not change core truth.
