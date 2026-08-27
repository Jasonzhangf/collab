# Collab function map

| feature_id | owner | entry symbol | required gate |
|---|---|---|---|
| identity.register | `identity` | `load_or_create` | `cargo test`, tmux/Herdr registration smoke |
| identity.herdr_terminal | `identity` | `herdr_terminal_id` / `parse_terminal_id` | positive and missing-field tests |
| runtime.queue | `server` | `handle_send`, `timers::tick` | queue and runtime isolation tests |
