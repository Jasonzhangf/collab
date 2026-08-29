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

- v2 AppSDK admission uses the compiled module record hash, not the raw binary
  hash. `module.compiled.json.artifact_hash` must bind every pre-review
  validation and deployment evidence record; binary hashes remain in artifact
  entries. Review evidence arrays are phase-strict and deployment receipts
  require one exact producer object across install, restart, and blackbox
  records. After architecture review, effectiveness evidence must be newly
  produced after the review. Verified on 2026-08-29 with candidate `05159fa`
  and governance commit `6b5f60c`.
