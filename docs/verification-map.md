# Collab verification map

| feature_id | positive gate | negative gate |
|---|---|---|
| identity.herdr_terminal | valid `terminal_id` produces stable identity | missing or malformed `terminal_id` fails explicitly |
| identity.registration | repeated commands in one Herdr pane reuse identity | a new Herdr terminal does not collide with a reused pane ID |
| runtime.queue | immediate/idle delivery reaches same-runtime pane | cross-runtime delivery and non-idle heartbeat are rejected |
