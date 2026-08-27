# Collab function map

| feature_id | owner | entry symbol | required gate |
|---|---|---|---|
| identity.register | `identity` | `load_or_create` | `cargo test`, tmux/Herdr registration smoke |
| identity.herdr_session | `identity` | `load_or_create` | repeated pane registration and reused pane-ID tests |
| runtime.queue | `server` | `handle_send`, `timers::tick` | queue and runtime isolation tests |
