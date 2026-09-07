# Verification

Read this for Collab source changes, release, install, restart, or protocol
verification—not ordinary command use.

Before review, prove the affected subset and every changed invariant:

- architecture/resource/function/verification gates;
- format, unit/state-machine tests, release build;
- isolated real two-peer tmux blackbox in a disposable project with dedicated
  test panes/Agents; never inject a test notice into an existing production
  project, pane, or Agent conversation;
- migration down/up/replay with durable-state preservation;
- duplicate-daemon rejection without PID/socket corruption;
- no wake for shell/absent/unknown; working Agents receive a due batch;
- explicit messages remain durable without active subscription;
- subscriptions are owner-scoped, bounded, exact where required, and one-shot;
- all pending eligible messages coalesce after 60 seconds into one attempt;
- failed wake and daemon restart never replay an attempted batch;
- one ID/subject/original-body tmux delivery submits once: Cursor splits payload from later `C-m`; a working Cursor pane then gets one empty `C-m` to steer; Codex pastes and submits in one queue; two live sessions prove both mailbox and pane Enter,
  preserves a reusable direct-message lease, and records one `Delivered` event;
- successful resource/deadline/async-result delivery consumes exactly one
  matching one-shot subscription;
- release clears obsolete wait state and does not wake an unsubscribed Agent;
- no daemon-generated periodic continuation, inferred waiting, progress/ACK
  loop, implicit first-register master, treating Codex/Cursor root as Collab
  master, dispatch, heartbeat, or `/goal` semantics; skill-level 15-minute
  owner checks continue or escalate real unfinished tasks; explicit
  user-approved self-promotion is allowed only when no live master exists, and
  only the live master may delegate; independent peers may decline a master
  invite and managed subagents must obey the master.

Review only after tests and runtime evidence pass. Post-review source/config/test
changes invalidate review and affected runtime evidence. Integrate the reviewed
commit into latest main, rerun main verification, install globally, restart
once, and replay the installed path.
