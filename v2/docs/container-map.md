# v2 container map — v2-design-freeze-1

| container | implementation | responsibility | forbidden |
|---|---|---|---|
| `collab-core` | Rust | reducer, journal, identity, authorization, task/claim | plugin hot policy, direct tmux |
| `collab-extensions` | Rust | transport, probe, scheduler, resource policy | direct journal/task mutation |
| `cordis-orchestrator` | Node.js/TypeScript | plugin lifecycle and event wiring | task/claim truth |

Codis starts, stops, connects, and health-checks these containers. The first
release keeps extensions in one Rust process; a plugin becomes a separate
container only when isolation is an observed requirement.
