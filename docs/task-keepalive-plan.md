# Task-bound finite keepalive

Scope: Collab task/message journal is the sole owner. Managed sends create an
assigned task, child registration claims it. No second task list. Existing
claimed tasks participate; blocked/waiting tasks do not get continuation wakes.

An explicit waiting/idle tmux Agent with unfinished actionable tasks may get
one grouped activation per 15 minutes, at most three consecutive unconfirmed
activations. Active sending, a fresh ACK, or positive working observation
resets the idle timer before exhaustion. Unknown/absent never cause input.
Exhaustion is durable and requires explicit owner rearm; restart cannot reset
it. No ACK-to-ACK response, automatic process respawn or task redispatch.

Policy lives only in ~/.appsdk/config.toml. The third attempt gets its full
15-minute response window. A late ACK cannot automatically rearm exhaustion.
Persist reservation before wake; failed/uncertain wakes count toward budget.
No unbounded pending activation queue. Tests use injected time/probes/sender,
never production panes or a real 45-minute wait.

Owner worktree: playground/task-keepalive, base origin/main 23731b8.
Sequence: red tests -> implementation -> full tests -> isolated tmux -> review.
