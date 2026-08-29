# v2 design baseline checklist

- [x] v2 has an independent directory and branch.
- [x] v1 source and `.agent-collab` state are not shared.
- [x] Core owner and immutable responsibilities are defined.
- [x] Cordis orchestration boundary is defined.
- [x] Codis/container boundary is defined.
- [x] Arc is limited to process-local shared state.
- [ ] Versioned event envelope is implemented with typed control/business separation.
- [ ] Event sequence, idempotency, durability, snapshot and replay gates pass.
- [ ] Hot replacement fencing, epochs, drain timeout and late-result rejection pass.
- [ ] Initial three-container deployment is implemented and live-tested.

This is a design-baseline draft. It is not an implementation freeze. The
implementation freeze requires source, contracts, maps, tests, artifact,
installation, restart, live replay, review and regression evidence.
