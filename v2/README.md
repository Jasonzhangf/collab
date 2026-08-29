# Collab v2

Collab v2 is an isolated redesign. The v1 source tree and runtime state are
not imported, mutated, or shared by v2.

## Frozen boundary

- Rust owns identity, authorization, journal/replay, task/claim lifecycle,
  resource truth, mailbox state, and the single reducer.
- Cordis (original Node.js package) owns plugin activation, dependency
  wiring, event subscriptions, disposal, and hot replacement.
- Codis/container orchestration owns process/container lifecycle and health;
  it never writes Collab state directly.
- `Arc` is process-local shared read state. Cross-process communication uses
  versioned commands/events.
- Tmux text delivery has one Rust transport owner; agents never send raw
  tmux input.

The design is frozen as `v2-design-freeze-1`. Changes to these boundaries or
protocols require a new freeze revision before implementation continues.
