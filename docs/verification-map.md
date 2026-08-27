# Collab verification map

| feature_id | positive gate | negative gate |
|---|---|---|
| identity.herdr_session | session socket + pane produces stable identity | another session with a reused pane ID does not collide |
| identity.registration | repeated commands in one Herdr pane reuse identity | another Herdr session with a reused pane ID does not collide |
| runtime.queue | immediate/idle delivery reaches same-runtime pane | cross-runtime delivery and non-idle heartbeat are rejected |
