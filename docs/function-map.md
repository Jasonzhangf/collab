# Collab function map

| feature_id | owner | entry symbol | required gate |
|---|---|---|---|
| identity.register | `identity` | `load_or_create` | `cargo test`, tmux registration smoke |
| runtime.queue | `server` | `handle_send`, `timers::tick` | queue and runtime isolation tests |
| task.lifecycle | `server` | `handle_task_claim`, `handle_task_deliver`, `handle_task_close`, `handle_task_update` | lifecycle transition and claim-release tests |
| task.wait-liveness | `server` | `handle_task_wait`, `wait_cycle`, `timers::tick` | cycle, deadline, escalation, terminal cleanup tests |
| continuation.context | `server` | `handle_context` | authenticated snapshot and tmux blackbox |
