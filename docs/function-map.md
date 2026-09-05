# Collab v1 function map

| feature_id | owner | entry symbols | required gate |
|---|---|---|---|
| identity.peer-register | `identity` + `server` | `load_or_create`, `handle_register` | every registration is peer; no first-worker promotion |
| task.self-lifecycle | task owner peer | `handle_task_register`, `handle_task_update`, `handle_task_deliver`, `handle_task_close` | only owner mutates; full worktree→test→integrate→close lifecycle |
| resource.p2p-conflict | `server` | `task_conflicts`, `handle_task_wait`, close release projection | task operations return synchronously; exact subscribed release exits waiting and clears edge |
| wait.liveness | `server` | `handle_task_wait`, `wait_cycle`, `timers::tick` | bounded acyclic wait, responsible blocker owner, deadline state transition without unsolicited message |
| notification.subscription | `server::state` + `server` | `default_direct_message_events`, `registered_peer_default_events`, `restore_registered_peer_default_leases`, `handle_notification_subscribe`, `matching_subscription`, `handle_notification_unsubscribe` | peer registration owns one deterministic finite default direct-message lease; short explicit leases cannot suppress it; daemon replay restores it only for a matching registered tmux session; resource/deadline/async-result remain one-shot |
| notification.tmux-wake | `server` + `timers` + `knock` | `abbreviated_subject`, `notification_text`, `attempt_notification_with`, `queue_system_knock`, `wake_args` | registered-only; required subject plus safe original-body preview and execute-not-ACK/wait action suffix; one tmux command queue performs unique-buffer bracketed paste then final C-m; direct messages serialize through cooldown; absent/unknown/working reject; hard max three per message |
| notification.context | `server` | `handle_context`, inbox/status handlers | authenticated read-only projection; durable body fetched through CLI/MCP |
| daemon.project-scope | `scope` + CLI init | `project_root`, `init`, `Scope::resolve` | inherited `TMUX_PANE` resolves exact pane cwd; non-tmux operator uses exact process cwd; MCP cannot select either |
| migration.peer-v1 | `server` | migration inspect/plan/apply/verify handlers, `replay` | legacy role fields are discarded; snapshot/replay preserves lifecycle |
| daemon.operator | CLI + `server::run` | `collab down`, `collab up`, `replay` | explicit operator path, one socket writer, no role-derived authority |

Central dispatch, permanent master, transfer-master, master recovery, and
automatic idle-worker offers have no owner in the target architecture and must
be physically removed after migration compatibility is covered.
