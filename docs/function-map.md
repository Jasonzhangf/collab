# Collab v1 function map

| feature_id | owner | entry symbols | required gate |
|---|---|---|---|
| identity.peer-register | `identity` + `server` | `load_or_create`, `handle_register` | every registration is peer; no first-worker promotion |
| task.self-lifecycle | task owner peer | `handle_task_register`, `handle_task_update`, `handle_task_deliver`, `handle_task_close` | only owner mutates; full worktree→test→integrate→close lifecycle |
| resource.p2p-conflict | `server` | `task_conflicts`, `handle_task_wait`, close release projection | task operations return synchronously; exact subscribed release exits waiting and clears edge |
| wait.liveness | `server` | `handle_task_wait`, `wait_cycle`, `timers::tick` | bounded acyclic wait, responsible blocker owner, deadline state transition without unsolicited message |
| notification.subscription | `server::state` + `server` | `handle_notification_subscribe`, `matching_subscription`, `handle_notification_unsubscribe` | Agent-owned exact event/subject/TTL, one-shot, replayable control truth |
| notification.tmux-wake | `timers` + `knock` | `attempt_notification_with`, `queue_system_knock` | registered-only; short id; absent/unknown/working reject; hard max three; success consumes |
| notification.context | `server` | `handle_context`, inbox/status handlers | authenticated read-only projection; durable body fetched through CLI/MCP |
| daemon.project-scope | `scope` + CLI init | `project_root`, `init`, `Scope::resolve` | inherited `TMUX_PANE` resolves exact pane cwd; non-tmux operator uses exact process cwd; MCP cannot select either |
| migration.peer-v1 | `server` | migration inspect/plan/apply/verify handlers, `replay` | legacy role fields are discarded; snapshot/replay preserves lifecycle |
| daemon.operator | CLI + `server::run` | `collab down`, `collab up`, `replay` | explicit operator path, one socket writer, no role-derived authority |

Central dispatch, permanent master, transfer-master, master recovery, and
automatic idle-worker offers have no owner in the target architecture and must
be physically removed after migration compatibility is covered.
