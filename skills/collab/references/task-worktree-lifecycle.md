# Task and Worktree Lifecycle

Use this lifecycle automatically for multi-worker tasks once scope and worktree
are known. Bind the semantic feature/resource and owned file paths in the task
scope; check overlapping ownership before shared changes. Registration is not
a prerequisite for unrelated single-worker quality checks.

Engineering delivery and Collab resource close are separate. Verified delivery
can be reported while a worktree is retained with owner and purpose recorded.
Keep the Collab cleanup obligation open until its actual removal requirements
are met; never fake `closed`, remove an unmerged tree, or demand deletion merely
to declare tested work delivered.

```text
latest main
  -> clean declared <project-main>/playground/<short-slug> worktree
  -> register owned task
  -> implement and verify
  -> candidate commit
  -> sync latest main and verify exact integration candidate
  -> acquire short integration/resource lease
  -> integrate, verify main, push exact main HEAD
  -> mark merged
  -> cleanup_pending
  -> cleanup_verified receipt
  -> closed
```

States:

```text
working -> verifying -> reviewed -> delivered -> merged -> closed
blocked -> waiting -> blocked|working
reviewed|delivered -> rework -> working
* -> cancelled
```

Common commands:

```sh
collab context
collab task status [task-id]
collab task conflicts --feature <feature-id>
collab task register <task-id> --feature <feature-id> \
  --worktree ./playground/<short-slug> \
  --branch codex/<short-slug> --base-commit <sha> --priority p2
collab task update <task-id> --status verifying --next "<next evidence gate>"
collab task update <task-id> --status reviewed
collab task deliver <task-id> --evidence "commit=<sha>; gates=pass" \
  --worktree ./playground/<short-slug>
collab task update <task-id> --status merged --next "main verified and pushed"
collab task close <task-id>
```

Last owned close cancels this peer's direct-message auto-notify. A leftover
keepalive after the task is done is closed with `collab notify close`.

- One issue owns one clean worktree under
  `<project-main>/playground/<short-slug>`; the project ignores `playground/`.
- Declare task ID, owner, feature/resource ID, worktree, branch, base commit,
  priority, status, and next step before product edits.
- Never share/reuse a worktree. Never depend on dirty main.
- Only the owner may update, deliver, mark merged, cancel, or close.
- Force close exceptions are explicit and audited: a live master may close
  any task; with no live master the owner may self-close, or a registered
  peer may close a task whose owner tmux identity is lost.
- A bound worktree creates a mandatory cleanup obligation. Keep its exact path
  and branch bound until close.
- `delivered`/`merged` are not cleanup. Close requires a clean, removed,
  verified-absent worktree plus durable cleanup receipt.
- Close rejects unmerged commits, dirty paths, path escape, wrong branch,
  unique unmerged history, or cleanup failure.
- Cancellation cannot bypass cleanup. Dirty/missing/unproven paths are never
  force-removed. Terminal state without a receipt is an audit failure.

## Task liveness and escalation

An assigned task remains live until its actual cleanup receipt and `closed`
state exist. At least once every 15 minutes, the owner must inspect its task,
worktree, mailbox, and wait/block state, then take the next actionable step.
The owner must continue execution when work is available, resolve a blocker
when it can, and record/escalate when it cannot. ACKing a wake is not progress
and does not satisfy liveness.

Route escalation by worker type:

- A managed subagent and an ordinary worker both report blockers to the live
  Collab master immediately. First find a concrete solution (root cause,
  proposed change, authorization needed); send that, not a symptom. Do not
  wait. A subagent copies its parent when parent is not the master, and may
  not decline a master collaboration request. Independent peers may
  temporarily decline a master collaboration invite to protect their own
  task. If no live master exists, report to the collaborator that initiated
  the task.
- A peer becomes master only after explicit user approval for that peer and
  project, with a live registered identity/pane verified. If a live master
  exists, only that master may `collab master delegate`; if none exists, the
  peer may `collab master promote --approval` itself. `appsdk init` proves
  initialization of the current peer, not master ownership. A dead recorded
  pane is not a live master. Codex/Cursor root is not Collab master.
- Master compiles the goal into a dependency graph, then parallel unique-write
  scopes. It assigns managed subagents with `appsdk subagent start` and
  `send` only when delivery conditions (done-iff, artifacts, in/out of
  scope, forbidden edits) and test conditions (exact commands, expected
  results, evidence location) are unambiguous. Do not equally split a large
  goal or share a write scope. Workers execute only the approved assignment
  and return evidence. On a blocker they find a solution first, then report
  master immediately.

For every escalation include `task_id`, current durable status, exact blocker or
wait target, proposed solution, attempted resolution, and the decision/action
needed. Keep the task blocked or waiting truth durable; do not create a
duplicate task, silently release a claim, or wait without a next check and
escalation path.

## Lifecycle recovery

Read project rules, maps, own notes/task/worktree records, journal tail,
mailbox, migration state, socket, and exact daemon PID ownership. If
`.agent-collab/server/DOWN` exists, keep it stopped. Reconstruct only the
calling peer's work with `collab context`; other peer state is read-only except
for resolving a shared-resource notice.

Match daemon ownership by exact project socket and cwd, never process name.
