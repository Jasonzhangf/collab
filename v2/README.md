# Collab v2

Collab v2 is an isolated redesign. The v1 source tree and runtime state are
not imported, mutated, or shared by v2.

## Candidate boundary

- Rust owns equal-peer identity, owner task lifecycle, bounded waits, exact
  subscriptions, durable notices, wake leases, journal/replay, migration, and
  the single reducer.
- Node/Cordis is a typed command adapter and read-only projection. It does not
  retain semantic maps or write journal truth.
- tmux is the only live notification channel. It sends only a short message id
  after an Agent explicitly subscribes and the target is observed `waiting`.
- `unknown`, `absent`, and `working` produce zero tmux input; no timer or
  restart infers continuation. v2 remains isolated and is not production.

The runtime is still a candidate. Canonical AppSDK compile, installation,
restart, real tmux black-box evidence, review, and freeze are required before
any production deployment decision.

## Explicit daemon lifecycle

The candidate daemon is controlled only from the exact project cwd:

```sh
collab up
collab status
collab down
```

`up` owns one deterministic short Unix socket, one PID file, and one lock.
`down` writes the environment-owned `DOWN` marker before stopping that exact
PID; ordinary CLI/MCP operations cannot clear it or auto-start a daemon.
`status` is read-only. A stale socket, PID, or lock is an explicit error and
is never guessed or replaced by an Agent identity or message.
