# Collab project memory

- Daemon-first operation: probe collection, state aggregation, retries,
  journaling, wake-ups, and protocol projection belong in the detached
  daemon. Agent-facing commands must stay minimal and preferably one-shot;
  agents should submit intent and consume structured results, not run probe
  loops or manage daemon internals.
- Master board gate: a wake or ACK is never completion. `task status` exposes
  board counts, `master_action`, and `can_stop`; master continues review,
  decomposition, or dispatch until every published task is `closed` or
  `cancelled`.
