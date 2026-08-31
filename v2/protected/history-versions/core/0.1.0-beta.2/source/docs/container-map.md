# v2 container map — role-free runtime

| container | implementation | responsibility | forbidden |
|---|---|---|---|
| `rust-core` | Rust | equal peers, owner task lifecycle, waits, subscriptions, notices, wake leases, journal/replay/migration | roles, dispatch, transport, process lifecycle |
| `rust-core-daemon` | Rust | one state/journal writer and typed stdio command boundary | business payload repair, transport selection, implicit wake |
| `node-adapter` | Node.js | environment-owned CLI/MCP bridge and read-only projection | semantic truth, journal writes, role inference, daemon restart |
| `tmux-transport` | Node.js | one literal short wake sequence after explicit lease | message body, retries, task continuation |

The v2 candidate is isolated from production v1. Operator lifecycle remains an
explicit local control action; an agent cannot start, restart, or stop the
daemon through identity or message data. Health and process observations stay
in the control plane and cannot mutate business truth.
