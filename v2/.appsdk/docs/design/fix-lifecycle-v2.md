# Fix Lifecycle v2

## Required order

```text
goal admitted
  -> clean isolated worktree from approved base
  -> baseline reproduction
  -> formal fix in the unique owner
  -> fix candidate verification
  -> development whitebox PASS
  -> build / install / restart
  -> deployed public-entrypoint blackbox PASS
  -> pre-review validation PASS
  -> architecture review PASS
  -> unchanged-candidate effectiveness replay PASS
  -> merge queue admission when parallel development is enabled
  -> tested integration identity verification
  -> local and remote mainline receipt
  -> compile / Active / Protected / Freeze
```

Playground is the logical experiment lifecycle. The clean Git worktree is the physical execution boundary. Absolute local worktree paths stay in `.appsdk-control/`; committed records bind IDs, commits, tree/diff hashes, scope, producer, and timestamps.

## Invalidation

- Source, tree, diff, scope, owner, design, or map change after architecture review invalidates the review and effectiveness evidence.
- Effectiveness failure returns the lifecycle to `fix_candidate`.
- Candidate commit missing from the tested integration blocks promotion. In single-worker mode, the merged tree must equal the reviewed/effectiveness-tested candidate tree. In parallel mode, the final main tree may contain other accepted work, but the merged commit/tree must exactly equal IntegrationRecord and remain reachable from both local and recorded remote main refs.
- Compile and publish never consume Playground or an unmerged candidate branch.

## Record graph

```text
WorktreeRecord
  -> ReproductionRecord
  -> EvidenceRecord[]
  -> FixCandidateRecord
  -> PreReviewValidationRecord (whitebox + evidence-backed install/restart + deployed blackbox)
  -> Architecture ReviewRecord
  -> EffectivenessRecord
  -> CollaborationRecord (parallel)
  -> MergeQueueRecord (parallel)
  -> IntegrationRecord (parallel)
  -> MainlineReceiptRecord (parallel)
  -> MergeRecord
  -> PromotionRecord
  -> RegressionReport
  -> FreezeRecord
```

Required evidence phases are `baseline_reproduction`, `fix_candidate`, `positive_intervention`, `negative_intervention`, and `post_architecture_effectiveness`. Every referenced evidence ID must resolve to a distinct record.

The pre-review gate additionally requires `development_whitebox`, `deployment_install`, `deployment_restart`, and `deployed_blackbox` EvidenceRecords for the exact candidate. It enforces whitebox ≤ install ≤ restart ≤ deployed blackbox ≤ validation time. Issue lifecycle states such as `pre_review_validated` are derived projections of validated records; they are not module promotion stages.

## Compatibility

Fix Lifecycle v2 began with the v0.1.3 single-worker graph. AppSDK v0.1.4 adds the atomic `multi_worker_collaboration` + `multi_worktree_merge_queue` scenario pair and keeps the graph fail-closed. AppSDK v0.1.5 adds pre-review whitebox/deployed-blackbox validation and evidence-backed install/restart receipts. A project already at `architecture_stable` must keep its retained binary until an explicit 0.1.5 migration can produce genuine PreReviewValidationRecord evidence bound to the historical deployed artifact, environment, receipts, and entrypoint. The binary checks `.appsdk/project.json` and rejects a different SDK version with `PROJECT_SDK_VERSION_PIN_MISMATCH` before interpreting records. Do not synthesize PASS records when historical evidence is unavailable.
