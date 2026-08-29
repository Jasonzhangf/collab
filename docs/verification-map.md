# Collab verification map

| feature_id | positive gate | negative gate |
|---|---|---|
| identity.registration | repeated commands in one tmux session+pane reuse identity | another tmux pane does not collide |
| runtime.queue | immediate/idle delivery reaches same-runtime pane | cross-runtime delivery and non-idle heartbeat are rejected |
| task.lifecycle | deliver holds claim; master close releases it and returns available tasks | worker cannot claim another task before close or mutate delivered/closed directly |
