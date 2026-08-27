# Collab resource map

| resource_id | owner | truth store | allowed operations |
|---|---|---|---|
| worker-identity | `identity::load_or_create` | `.agent-collab/runs/by-pane/*.json` | resolve pane identity, register |
| herdr-session-identity | `identity::load_or_create` | Herdr socket/session path + `pane get` terminal_id | derive a unique session/pane key |
| collaboration-journal | `server::Server` | `.agent-collab/server/journal.jsonl` | append/replay worker, task, message events |
