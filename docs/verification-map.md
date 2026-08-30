# Collab verification map

| feature_id | positive gate | negative gate |
|---|---|---|
| identity.registration | repeated commands in one tmux session+pane reuse identity | another tmux pane does not collide |
| runtime.queue | immediate/idle delivery reaches same-runtime pane | cross-runtime delivery and non-idle heartbeat are rejected |
| task.lifecycle | deliver holds claim; master close releases it and returns available tasks | worker cannot claim another task before close or mutate delivered/closed directly |
| task.wait-liveness | bounded wait records ownership/deadline and expires to blocker | direct/transitive cycles and terminal waits are rejected; expiry never fabricates success |
| continuation.context | one context query returns identity, liveness, tasks, inbox, next actions | lost tmux wake still recovers from journal/mailbox |
