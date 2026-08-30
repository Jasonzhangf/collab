# Collab v1 resource map

| resource_id | unique owner | truth store | allowed operations | forbidden relation |
|---|---|---|---|---|
| peer-identity | `identity::load_or_create` + Server registration | journaled `WorkerRec` | bind one tmux session identity to its current pane endpoint | role promotion, inferred master, copied token |
| task-lifecycle | task owner peer | journaled `TaskRec` | self-register, update, verify, integrate, close own task | central dispatch, another peer mutating lifecycle |
| resource-claim | task owner peer through Server | active task feature/worktree claim | claim, hold, wait, release; conflict emits durable p2p notice | silent takeover, automatic release, global arbiter |
| wait-edge | waiter peer + blocking resource owner | `TaskRec.wait` | bounded wait, p2p occupancy notice, deadline escalation, release wake | cycle, missing responsible peer/deadline/resume path |
| collaboration-journal | `server::Server` | `.agent-collab/server/journal.jsonl` | append/replay worker, task, message, migration events | manual JSON edits, silent replay skip |
| durable-mailbox | `server::Server` | `.agent-collab/mailbox/*.json` | persist p2p conflict/release and continuation records | tmux delivery as truth, clearing during migration |
| tmux-wake | `server::knock` + `Event::WakeAttempted` | journaled attempt lease + transient tmux pane input | short wake after durable commit; retry pending wake only for a confirmed waiting agent; success-only delivery | shell input, working-agent interruption, task assignment, ownership, role, normal progress report |
| continuation-context | `server::handle_context` | Server state projection | peer identity, own tasks/waits, conflicts, inbox, next actions; consume own continuation | explicit continuation ACK loop, board management, other-peer progress supervision |
| daemon-maintenance | explicit local operator command | project socket/PID/events | install, controlled down/up, replay verification | deriving authority from peer role or mailbox text |
| migration-transaction | initiating authenticated peer + Server freeze | journaled `MigrationRecord` | inspect, plan, freeze, snapshot, restart, rebind, verify, resume | permanent master requirement, second writer, invented owner |

`/goal` delegation and interactive task recognition are outside this v1 milestone.
