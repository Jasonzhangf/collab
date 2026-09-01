# v2 role-free runtime checklist

- [x] v2 has an independent directory and branch.
- [x] v1 source and `.agent-collab` state are not shared.
- [x] Rust is the sole semantic owner; Node/Cordis is a typed adapter.
- [x] Equal-peer, role-free identity and owner task lifecycle are defined.
- [x] Typed control-side commands are separated from durable business notice bodies.
- [x] Event sequence, idempotency, durability, snapshot and replay gates pass.
- [x] Bounded waits, exact subscriptions, and three-attempt wake cap are tested.
- [x] Expired wake lease recovery is explicit and does not send tmux.
- [ ] Explicit daemon up/down/status lifecycle is implemented and black-box tested.
- [ ] Isolated release install, real tmux, AppSDK compile/review/freeze are verified.

This remains a candidate checklist. It is not a production freeze. Production
v1 stays installed for zterm, OneStop, RouteCodex, and dsh-plugins until the
candidate has source, contracts, maps, tests, artifact, installation, restart,
live replay, review, regression, and canonical AppSDK freeze evidence.
