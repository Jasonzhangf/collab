---
name: appsdk-project-governance
description: Integrate and operate AppSDK governance in a new project without copying the SDK implementation into the project. Use for project bootstrap, goal clarification, Playground changes, evidence/review/promotion, Active publishing, Protected freezing, SDK lock verification, and boundary audits.
---

# AppSDK Project Governance

Use the external AppSDK implementation as the governance engine. Keep only project contracts and lifecycle records in the target repository.

## Boundaries

- External AppSDK: compiler, CLI, schemas, harness, adapters, immutable rules.
- `.appsdk/`: committed project governance contract, maps, goal, records, verification, and `sdk.lock`.
- `.appsdk-control/`: ignored local run state, review cache, temporary harness output, and worker state.
- `playground/`: mutable experiment source.
- `active/lib/`: immutable consumable library.
- `protected/`: frozen source, contracts, and history.
- `generated/` or the project-declared artifact root: compiler output only; never hand-edit.

Never copy the AppSDK source, compiler, or harness into the business project. Never put committed project maps or lifecycle records in `.appsdk-control/`.

## Existing project bootstrap

For an existing project, begin with a preparation record instead of manually creating governance directories:

```bash
appsdk prepare <workspace>
```

The AI must read `.appsdk-prepare.json`, ask the user about change kind, project root, legacy roots, new roots, Protected roots, allowed paths, forbidden paths, and payload/control separation, then update the record to `status: confirmed`. Do not initialize before confirmation.

```bash
appsdk init <workspace> --project-root <relative-path>
```

`init` is idempotent. The workspace may contain legacy code; the configured relative path becomes the new AppSDK project root. It creates the four governance zones and `.appsdk-control/` under that root, fills only missing governance files, and appends one managed `.gitignore` block for local control state, compiled Active libraries, and generated outputs. It preserves existing project files and existing ignore rules. Absolute paths and `..` traversal are rejected. Use `appsdk new` only for an empty destination.

## New project flow

1. Install or reference a pinned external AppSDK.
2. Create the project from a template into an empty, non-symlinked destination.
3. Confirm `.appsdk/project.json`, `.appsdk/goal.json`, module ownership, zone roots, and `sdk.lock`.
4. Run `appsdk verify` before source changes.
5. Clarify the user goal: restate objective, acceptance criteria, non-goals, assumptions, ambiguities, and questions.
6. Do not claim, edit Playground, create a formal red test, compile, or promote while goal status is `received`, `parsed`, or `clarification_pending`.
7. After user confirmation, bind scope, owner, allowed/forbidden paths, and required gates.
8. Treat Playground as a logical lifecycle, not the physical checkout. Create a clean isolated Git worktree from the recorded base commit; both initial and candidate handoff states must be clean. Keep local worktree paths in `.appsdk-control/`, never in committed portable records.
9. Reproduce the issue at the base commit with the same recorded input hashes. Record the first divergence and baseline evidence before implementing the formal fix.
10. Commit the fix candidate and bind its commit, tree hash, diff hash, design ID, owner, scope, changed paths, and positive/negative verification evidence in FixCandidateRecord.
11. Before architecture review or delivery commit, run two distinct gates on the exact candidate: development whitebox verifies internal logic; deployment blackbox builds, installs, restarts, then verifies behavior only through the deployed public entrypoint. Unit tests, source-level CLI invocation, mocks, and relabeled whitebox evidence are not deployment blackbox evidence.
12. Record PreReviewValidationRecord. Bind candidate commit/tree, deployed artifact hash, independent full whitebox producer identity, deployment environment/entrypoint/producer, and disjoint whitebox/blackbox evidence IDs. Install and restart receipt IDs must resolve to unexpired PASS EvidenceRecords for the same candidate, artifact, environment, entrypoint, and deployment producer. The shared machine gate verifies the candidate Git tree, controlled source, rebuilt artifact identity, whitebox producer/artifact, and candidate ≤ whitebox ≤ install ≤ restart ≤ blackbox ≤ validation time; review admission, normal verify, and architecture promotion all call this same gate, so post-admission drift fails closed. Missing deployment capability is a blocker, never a skipped gate. Then require `appsdk verify --review-admission <project> --module <id>` to PASS; an agent must not start review or create the delivery commit before this admission. AppSDK supplies the admission command and contract; the host CI/pre-commit adapter must invoke it when physical Git commit blocking is required.
13. Run architecture review against that exact pre-review-validated candidate. ReviewRecord must reference the PreReviewValidationRecord, contain explicit PASS, AI confidence/rationale, and hashes of the resource, function, mainline-call, and verification maps.
14. After architecture PASS, rerun the original reproduction inputs plus positive, negative, and blackbox checks without changing candidate source. Record EffectivenessRecord. Source/tree/scope changes invalidate pre-review validation, review, and effectiveness.
15. Merge only after effectiveness PASS. For a single worker, MergeRecord proves the candidate commit is an ancestor of the recorded merge commit and that the merge commit remains on the declared mainline ref. For parallel development, follow the atomic scenario pair below.
16. Promote only when the complete worktree → reproduction → candidate → pre-review validation → architecture review → effectiveness → merge record graph is valid and evidence targets the exact module and candidate artifact.
17. Before freeze, run the module's declared regression suite and create a RegressionReport bound to the exact merged source commit, artifact hash, public API hash, scope hash, and input hash. Regression and bug-reproduction evidence must combine whitebox and blackbox coverage; unit and focused tests may be whitebox only.
18. Compile the merged source library, publish the immutable Active artifact, archive source/contracts to Protected, create FreezeRecord with the RegressionReport ID/hash, and verify.
19. After freeze, ordinary full-regression execution for that unchanged module may be disabled to reduce CI load. Keep the suite and report. Any source, contract, public API, artifact, or dependency input change invalidates the report and requires regression re-enablement before a new freeze.
20. Close every experiment with a PlaygroundCleanupRecord; archive evidence to Protected history, then remove the experiment directory under the declared retention policy.

## Parallel development scenario pair

`multi_worker_collaboration` and `multi_worktree_merge_queue` are one atomic capability. Enable both or neither in `.appsdk/project.json`; one-sided activation is invalid.

- Each worker owns one semantic claim, one branch, and one clean isolated worktree. A worker never edits main and never shares a worktree.
- Decompose a parent task into small independently verifiable milestones. One milestone owns exactly one claim, branch, and clean worktree. Commit and queue it immediately after its gates pass; do not stack another milestone in the same worktree or leave a completed milestone only on a worker branch.
- Start a dependent milestone only from a new clean worktree after the predecessor milestone has a live remote-main receipt. Bind the predecessor collaboration and receipt IDs; sequence 1 uses `none` for both.
- After candidate verification, architecture PASS, and unchanged-source effectiveness PASS, the worker emits CollaborationRecord and enters the serial merge queue.
- One merge owner admits one queue entry at a time. Conflict resolution is not allowed in the queue; return the issue to its owner worktree and invalidate stale candidate/review/effectiveness evidence.
- Build an integration commit from the current main base and candidate. IntegrationRecord binds that exact commit/tree and the affected verification gates.
- Merge only the tested integration commit. MainlineReceiptRecord must bind the host VCS producer, remote name/ref, observed commit, and observation time. Verification checks local reachability and queries the remote with `git ls-remote`; local tracking refs and self-declared booleans are not remote truth.
- PromotionRecord references queue, integration, and mainline receipt records. Cleanup and claim release are forbidden until remote receipt passes.

For a frozen module change, run `appsdk begin-version <project> --module <id> --from <current> --to <new>` before formal source edits. The command must bind and preserve the current Active artifact, Protected archive, and record graph; direct edits to the old Active or Protected version are forbidden.

## Debug flow

Clarify goal first. Then use evidence-first debugging: baseline, first divergence, positive intervention, negative intervention, and unique owner. The physical checkout is a clean isolated Git worktree; the logical mutable phase is Playground. Formal order is immutable: reproduce → fix candidate → development whitebox → build/install/restart → deployed-entrypoint blackbox → pre-review validation PASS → architecture review PASS → unchanged-source effectiveness replay → verified mainline merge → promotion/compile/freeze. Review or delivery-commit admission before both whitebox and deployed blackbox PASS is forbidden.

## Required checks

- `appsdk verify <project>`
- `appsdk verify --review-admission <project> --module <id>` before any architecture review or delivery commit
- project tests and required gates
- candidate artifact hash and public API hash
- record graph references, freshness, scope, module, and version relations
- clean isolated worktree identity, candidate Git tree identity, architecture map hashes, post-review unchanged-source effectiveness, merge ancestry, tested integration identity, and local/remote mainline receipt when parallel scenarios are enabled
- RegressionReport whitebox + blackbox coverage, non-zero passing tests, exact input binding, and FreezeRecord report hash
- Protected and Active immutability
- final architecture review with explicit PASS

Do not claim lifecycle completion from unit tests alone. A missing external adapter, review verdict, installation, restart, or online evidence is an explicit remaining gap.

Read `docs/design/appsdk-project-integration.md` for the repository layout and `docs/design/playground-active-promotion.md` for promotion semantics.

Read `references/goal-prompt.md` when Jason gives a new goal and asks you to turn it into an executable `/goal` prompt.
