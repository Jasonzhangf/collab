# Collab v1 verification map

| feature_id | positive gate | negative gate |
|---|---|---|
| identity.peer-register | first and later tmux sessions register as equal peers | no `master` role, promotion, transfer, or inferred authority appears |
| task.self-lifecycle | one peer completes register→working→verifying→reviewed→delivered→merged→closed and cleanup | reviewed cannot bypass successful delivery; another peer cannot mutate/close; no central dispatch/offer |
| task.worktree-cleanup | every worktree-bound task receives a durable cleanup receipt only after exact clean worktree/branch removal and absence verification | merged/closed/cancelled task with a bound worktree, missing receipt, mismatched receipt, existing path, dirty path, unmerged branch, or path escape fails closed |
| resource.p2p-conflict | conflict creates durable holder/waiter notice; release changes waiter to blocked, clears wait, then wakes | no normal progress/report messages; failed tmux wake stays pending and does not change task truth |
| wait.liveness | wait records waiter, blocker owner, deadline, resume, escalation and release | direct/two-peer/three-peer cycle, missing owner/deadline, terminal wait rejected |
| continuation.local-wake | confirmed waiting agent receives one literal-text-plus-Enter wake; one attempt lease and one `Delivered`; lost wake retries | shell/offline/Braille-spinner working panes are not interrupted; immediate/timer race cannot duplicate delivery |
| continuation.context | one query consumes own continuation and returns lifecycle/conflict/inbox/next action | no explicit continuation ACK loop, central board management, or unrelated peer supervision data |
| migration.peer-v1 | inspect→plan→apply→restart/rebind→verify preserves task/mailbox/journal and removes legacy declared roles | malformed/manual journal, second writer, changed snapshot, missing/inactive/unrelated wait holder fail closed |
| daemon.operator | controlled down→up uses installed binary and journal replay | mailbox text or peer role cannot authorize maintenance; duplicate daemon rejected |

Required integration gate: real isolated tmux peers complete independent
worktrees and p2p conflict/release/restart continuation without `/goal`.
