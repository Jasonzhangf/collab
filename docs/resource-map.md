# Collab v1 resource map

| resource_id | unique owner | truth store | allowed operations | forbidden relation |
|---|---|---|---|---|
| peer-identity | `identity::load_or_create` + Server registration | journaled `WorkerRec` | bind one tmux session identity to its current pane endpoint | role promotion, inferred master, copied token |
| task-lifecycle | task owner peer | journaled `TaskRec` | self-register, update, verify, integrate, close own task | central dispatch, another peer mutating lifecycle |
| resource-claim | task owner peer through Server | active task feature/worktree claim | claim, hold, wait, release; conflict emits durable p2p notice | silent takeover, automatic release, global arbiter |
| wait-edge | waiter peer + blocking resource owner | `TaskRec.wait` | bounded wait, synchronous conflict result, deadline transition, subscribed release event | cycle, missing responsible peer/deadline/resume path |
| collaboration-journal | `server::Server` | `.agent-collab/server/journal.jsonl` | append/replay worker, task, message, migration events | manual JSON edits, silent replay skip |
| durable-mailbox | `server::Server` | `.agent-collab/mailbox/*.json` | persist explicit peer messages and subscribed event results | tmux delivery as truth, clearing during migration |
| notification-subscription | calling Agent through Server | journaled `NotificationSubscription` + control-only wake binding | query methods; register exact event/subject/TTL; status; unsubscribe; consume one-shot | implicit registration, payload-derived state, unbounded lease, another peer mutation |
| tmux-wake | `server::knock` + `Event::WakeAttempted` | journaled attempt ledger + transient tmux short-id input | only active subscription; absent/unknown/working fail closed; lifetime max three; one command sequence | body/prompt injection, automatic continuation, shell input, normal progress report |
| agent-context | `server::handle_context` | read-only Server state projection | peer identity, own tasks/waits, inbox, next actions | consuming notification, board management, other-peer supervision |
| project-control-root | `scope::project_root` + `Scope::resolve` | exact `TMUX_PANE` `#{pane_current_path}`, or exact process cwd for non-tmux operator | inherit Agent environment; initialize and resolve only that root | Agent/MCP path selection, Git or ancestor inference, sibling sharing, invalid pane/path |
| daemon-maintenance | explicit local operator command | project socket/PID/events | install, controlled down/up, replay verification | deriving authority from peer role or mailbox text |
| migration-transaction | initiating authenticated peer + Server freeze | journaled `MigrationRecord` | inspect, plan, freeze, snapshot, restart, rebind, verify, resume | permanent master requirement, second writer, invented owner |

`/goal` delegation and interactive task recognition are outside this v1 milestone.
