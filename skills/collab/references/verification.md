# Verification

Read this for Collab source changes, release, install, restart, or protocol
verification—not ordinary command use.

Every official release compile uses `scripts/build-collab.sh`, which increments
the host-global `~/.collab/build-version` counter under one lock; there is no
manual patch bump. Direct release builds fail with an instruction to use that
entry. Use `scripts/install-global-collab.sh` so the release candidate is built
once and the exact candidate bytes are installed. Verify the semantic source
baseline, release build version, installed binary versions, canonical command
paths, and refreshed embedded Skill byte-for-byte. Refresh the Skill with the
exact `$CARGO_HOME/bin/collab` binary, not a bare command that may resolve an
older PATH entry. Remove only legacy copies whose binary identity proves they
are matching Collab artifacts, then validate the new baseline only: do not
migrate or replay old local control-plane history. Installing a binary does
not restart the global daemon. A daemon restart is a separate, explicitly
authorized maintenance operation with PID/socket, identity, journal/mailbox,
and live-replay evidence.

Before review, prove the affected subset and every changed invariant:

- architecture/resource/function/verification gates;
- format, unit/state-machine tests, `scripts/build-collab.sh`;
- isolated real two-peer App Server blackbox in a disposable project when an
  App Server capability is available. Never inject a test notice into an existing
  production project, transport, or Agent conversation;
- migration down/up/replay with durable-state preservation;
- duplicate-daemon rejection without PID/socket corruption;
- no wake for shell/absent/unknown; working Agents receive a due batch;
- explicit messages remain durable without active subscription;
- subscriptions are owner-scoped, bounded, exact where required, and one-shot;
- all pending eligible messages coalesce after 60 seconds into one attempt;
- failed wake and daemon restart never replay an attempted batch;
- one ID/subject/original-body delivery preserves a reusable direct-message
  lease and records one accepted delivery event; explicit `sendmessage` proves
  native `turn/start` accepted the bounded preview, while daemon wakeup and
  long-horizon notifications use `thread/queue/add`;
- successful resource/deadline/async-result delivery consumes exactly one
  matching one-shot subscription;
- release clears obsolete wait state and does not wake an unsubscribed Agent;
- no daemon-generated periodic continuation, inferred waiting, progress/ACK
  loop, implicit first-register master, treating Codex root as Collab
  master, dispatch, heartbeat, or `/goal` semantics; skill-level owner checks
  run only on supported timer/wake or direct wake and then continue or escalate
  real unfinished tasks; explicit
  user-approved self-promotion is allowed only when no live master exists, and
  only the live master may delegate; independent peers may decline a master
  invite and managed subagents must obey the master.

Review only after tests and runtime evidence pass. Post-review source/config/test
changes invalidate review and affected runtime evidence. Integrate the reviewed
commit into latest main, rerun main verification, install globally, restart
once, and replay the installed path.
