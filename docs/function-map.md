# Collab v1 function map

| feature_id | owner | entry symbols | required gate |
|---|---|---|---|
| identity.peer-register | `identity` + `server` | `load_or_create`, `handle_register` | every registration is peer; no first-worker promotion |
| task.self-lifecycle | task owner peer | `handle_task_register`, `handle_task_update`, `handle_task_deliver`, `handle_task_close` | only owner mutates; full worktree→test→integrate→close lifecycle |
| resource.p2p-conflict | `server` | `task_conflicts`, `handle_task_wait`, close release projection | durable occupancy/release notices target waiter and holder; release exits waiting and clears edge |
| wait.liveness | `server` | `handle_task_wait`, `wait_cycle`, `timers::tick` | bounded acyclic wait, responsible blocker owner, deadline wake |
| continuation.local-wake | `timers` + `knock` | `timers::tick`, `queue_system_knock` | waiting-agent-only wake; durable attempt lease; pending retry; success-only delivery |
| continuation.context | `server` | `handle_context` | authenticated peer snapshot consumes only the caller's local continuation |
| migration.peer-v1 | `server` | migration inspect/plan/apply/verify handlers, `replay` | legacy role fields are discarded; snapshot/replay preserves lifecycle |
| daemon.operator | CLI + `server::run` | `collab down`, `collab up`, `replay` | explicit operator path, one socket writer, no role-derived authority |

Central dispatch, permanent master, transfer-master, master recovery, and
automatic idle-worker offers have no owner in the target architecture and must
be physically removed after migration compatibility is covered.
