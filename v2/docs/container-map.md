# v2 container map — design-baseline-draft

| container | implementation | responsibility | forbidden |
|---|---|---|---|
| `collab-core` | Rust | reducer, journal, identity, authorization, task/claim | plugin hot policy, direct tmux |
| `collab-extensions` | Rust | transport, probe, scheduler, resource policy | direct journal/task mutation |
| `cordis-orchestrator` | Node.js/TypeScript | plugin lifecycle and event wiring | task/claim truth |

Codis starts, stops, connects, and health-checks these containers. Codis health
observations stay in the runtime-control plane and cannot mutate business
truth. Cordis freezes routes while Rust replays after restart; only an
authorized typed command can resume business transitions.
