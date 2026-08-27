# Collab verification map

| feature_id | positive gate | negative gate |
|---|---|---|
| identity.herdr_terminal | valid `terminal_id` produces stable identity | missing or malformed `terminal_id` fails explicitly |
| identity.registration | repeated commands in one Herdr pane reuse identity | another Herdr session with a reused pane ID does not collide |
| runtime.queue | immediate/idle delivery reaches same-runtime pane | cross-runtime delivery and non-idle heartbeat are rejected |
