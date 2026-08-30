# Collab resource map

| resource_id | owner | truth store | allowed operations |
|---|---|---|---|
| worker-identity | `identity::load_or_create` | `.agent-collab/runs/by-pane/*.json` | resolve tmux session+pane identity, register |
| collaboration-journal | `server::Server` | `.agent-collab/server/journal.jsonl` | append/replay worker, task, message events |
| continuation-context | `server::handle_context` | server state projection | one authenticated snapshot for wake/restart continuation |
