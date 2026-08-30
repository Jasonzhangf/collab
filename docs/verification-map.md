# Collab v1 verification map

| feature_id | positive gate | negative gate |
|---|---|---|
| identity.peer-register | first and later tmux sessions register as equal peers | no `master` role, promotion, transfer, or inferred authority appears |
| task.self-lifecycle | one peer completes register→working→verifying→reviewed→delivered→merged→closed and cleanup | reviewed cannot bypass successful delivery; another peer cannot mutate/close; no central dispatch/offer |
| resource.p2p-conflict | conflict returns holder synchronously; subscribed exact release changes waiter to blocked and clears wait | no automatic holder/waiter message; no wake without matching subscription |
| wait.liveness | wait records waiter, blocker owner, deadline, resume, escalation and release | direct/two-peer/three-peer cycle, missing owner/deadline, terminal wait rejected; timeout creates no unsolicited message |
| notification.subscription | methods/register/status/unsubscribe replay exact owner/event/subject/TTL; success consumes one-shot | invalid event/TTL/subject/owner rejected; expired/cancelled/consumed registration cannot wake |
| notification.tmux-wake | direct message/resource release/deadline use short id and one tmux sequence; failure stops permanently at three | no registration, absent, unknown, working, expired, mismatched subject, fourth attempt, body injection, periodic continuation all produce zero input |
| notification.context | inbox/context/status return durable body and control projection | context does not consume or ACK notification; no unrelated peer supervision |
| daemon.project-scope | inherited `TMUX_PANE` exact pane cwd resolves one local daemon; non-tmux operator uses exact process cwd | MCP/Agent path override absent; invalid pane/path, ancestor capture, and sibling sharing fail closed |
| migration.peer-v1 | inspect→plan→apply→restart/rebind→verify preserves task/mailbox/journal and removes legacy declared roles | malformed/manual journal, second writer, changed snapshot, missing/inactive/unrelated wait holder fail closed |
| daemon.operator | controlled down→up uses installed binary and journal replay | mailbox text or peer role cannot authorize maintenance; duplicate daemon rejected |

Required integration gate: real isolated tmux peers complete independent
worktrees and explicit sendmessage/subscription/restart notification without `/goal`.
