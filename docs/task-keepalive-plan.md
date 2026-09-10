# Task-bound inspect obligation

Scope: the Collab task/message journal is the sole owner. Managed sends create
an assigned task, child registration claims it, and there is no second task
list. Existing claimed tasks participate under this task-bound inspect
obligation; blocked/waiting tasks do not get continuation wakes.

Wake model:

- Only master has long-horizon wake. Workers are not long-horizon wake targets
  and are not automatically woken from idle.
- A worker acts on an explicit dispatch, a bounded direct-message lease, or its
  own open task state. It inspects and progresses owned tasks during its
  working cycle; this is a task-bound inspect obligation, not a tmux activation
  schedule, and it does not produce worker tmux input.
- Explicit `collab sendmessage` is immediate. Idle, progress, delivery, bug,
  and worker-idle notices are auto-merged by the daemon in the 120-second batch
  window; they are not repeated as heartbeat storms.
- On each worker `working` -> `idle` transition, the worker sends one idempotent
  worker-idle fact to the live master, then stops. Unknown/absent never causes
  input.
- Master stops autonomous scheduling after three consecutive idle facts with no
  working change; no automatic rearm.

Failure and evidence:

- Unknown, timeout, and failure stay explicit; no ACK, fallback, or retry may
  fabricate success.
- No ACK-to-ACK response, automatic process respawn, task redispatch, or
  automatic subagent rearm; an explicit operator rearm remains a separate
  command.
- Bugs enter the AppSDK or git-bug backlog with investigation evidence; P0
  blocks the affected project.
- Tests use injected time/probes/sender, never production panes.

Legacy note: earlier drafts that described periodic worker activation keepalive
timers are deprecated and non-authoritative; this file does not restore them.

Policy lives only in `~/.appsdk/config.toml`.
