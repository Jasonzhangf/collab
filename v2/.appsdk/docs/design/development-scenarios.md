# Development Scenario Contracts

AppSDK provides composable development constraints. A project declares enabled scenarios in `.appsdk/project.json`; the SDK validates scenario dependencies, records, and gates before promotion.

## Parallel development pair

`multi_worker_collaboration` and `multi_worktree_merge_queue` use `all_or_none` activation. Enabling either without the other fails verification.

Each worker owns one semantic claim and one clean isolated worktree. Workers never write main. A single merge owner serializes accepted candidates through a merge queue.

The unit of collaboration is a small, independently verifiable milestone—not an open-ended task. One milestone owns one semantic claim, branch, and clean worktree. As soon as that milestone passes candidate verification, architecture review, and effectiveness replay, it must be committed and admitted to the merge queue. A worker may not accumulate a second milestone in the same worktree or continue dependent work while the completed milestone exists only on its branch.

For a later milestone in the same parent task, create a new claim and a new clean worktree from main after the predecessor's live remote receipt. The predecessor collaboration and receipt IDs are explicit machine evidence. This makes “small-step delivery” a sequence of independently merged mainline states, rather than a stack of unmerged worktree commits.

```text
CollaborationRecord
  -> WorktreeRecord
  -> Reproduction / Candidate / Review / Effectiveness
  -> MergeQueueRecord
  -> IntegrationRecord
  -> MainlineReceiptRecord
  -> MergeRecord
  -> Promotion / Freeze / Cleanup
```

The candidate tree is not required to equal the final main tree. Other accepted work may advance main. The exact tested integration tree must equal the merged integration commit tree. Local reachability is checked from the declared local main ref; remote reachability is checked with `git ls-remote` against the receipt's named remote/ref.

Conflict resolution never occurs inside the merge queue. A conflict returns the task to its owning worker and worktree, producing a new candidate and new review/effectiveness evidence.

Cleanup is forbidden until the remote main receipt passes.
