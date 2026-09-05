# Collab v1 verification map

| feature_id | positive gate | negative gate |
|---|---|---|
| identity.peer-register | first and later tmux sessions register as equal peers | no `master` role, promotion, transfer, or inferred authority appears |
| task.self-lifecycle | one peer completes register→working→verifying→reviewed→delivered→merged→closed and cleanup | reviewed cannot bypass successful delivery; another peer cannot mutate/close; no central dispatch/offer |
| task.worktree-cleanup | every worktree-bound task receives a durable cleanup receipt only after exact clean worktree/branch removal and absence verification | merged/closed/cancelled task with a bound worktree, missing receipt, mismatched receipt, existing path, dirty path, unmerged branch, or path escape fails closed |
| resource.p2p-conflict | conflict returns holder synchronously; subscribed exact release changes waiter to blocked and clears wait | no automatic holder/waiter message; no wake without matching subscription |
| wait.liveness | wait records waiter, blocker owner, deadline, resume, escalation and release | direct/two-peer/three-peer cycle, missing owner/deadline, terminal wait rejected; timeout creates no unsolicited message |
| notification.subscription | registration creates one deterministic seven-day default direct-message lease; a short explicit lease cannot suppress it; daemon replay restores it for a still-matching registered tmux session; direct-message remains reusable until expiry; resource/deadline/async-result success consumes one-shot | unregistered/stale panes get no default lease; invalid event/TTL/subject/owner rejected; expired/cancelled/consumed registration cannot wake; one failed direct message cannot terminate the peer lease |
| notification.tmux-wake | direct message/resource release/deadline require a subject and use one tmux command queue: unique-buffer bracketed paste of id, abbreviated subject, safe original-body preview, execute-not-ACK/wait action suffix, buffer deletion, then final `C-m`; direct messages serialize and each message stops permanently at three attempts | missing subject, missing action suffix, no registration, absent, unknown, working, expired, mismatched subject, fourth attempt, burst delivery, terminal control injection, multiple tmux client invocations, periodic continuation, or non-bracketed text-plus-submit appears |
| notification.context | inbox/context/status return durable body and control projection | context does not consume or ACK notification; no unrelated peer supervision |
| daemon.project-scope | inherited `TMUX_PANE` exact pane cwd resolves one local daemon; non-tmux operator uses exact process cwd | MCP/Agent path override absent; invalid pane/path, ancestor capture, and sibling sharing fail closed |
| migration.peer-v1 | inspect→plan→apply→restart/rebind→verify preserves task/mailbox/journal and removes legacy declared roles | malformed/manual journal, second writer, changed snapshot, missing/inactive/unrelated wait holder fail closed |
| daemon.operator | controlled down→up uses installed binary and journal replay | mailbox text or peer role cannot authorize maintenance; duplicate daemon rejected |

Required integration gate: real isolated tmux peers complete independent
worktrees and explicit sendmessage/subscription/restart notification without `/goal`.
