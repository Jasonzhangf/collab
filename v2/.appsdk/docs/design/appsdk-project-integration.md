# AppSDK Project Integration

AppSDK is an external governance implementation. A new project consumes its CLI/compiler and commits only project-specific governance data.

## Repository boundary

```text
external AppSDK installation
  -> versioned Bundle: CLI / compiler / contracts / docs / rules / skills
  -> .appsdk project contract + sdk.lock
  -> compiled manifest and verified artifact
  -> project runtime consumes Active
```

The business project must not copy AppSDK source, compiler, or harness into its own source tree.

## Project layout

```text
project/
  .appsdk/
    project.json
    goal.json
    sdk.lock
    maps/
    architecture/
    verification/
    records/
  .appsdk-control/       # local only; ignored
  playground/experiments/
  active/lib/
  protected/source/
  protected/contracts/
  protected/history/
  generated/              # indexes or project-declared artifact surface
```

## Git policy

Commit:

- `.appsdk/project.json`;
- `.appsdk/goal.json`;
- `.appsdk/sdk.lock`;
- project resource/function/mainline/verification maps;
- Evidence, Review, Promotion, and Freeze records;
- project architecture and verification contracts;
- Active and Protected content when the project uses Git as its artifact/archive store.

Ignore:

```gitignore
.appsdk-control/
```

`.appsdk-control/` may contain local review sessions, worker heartbeat, temporary harness output, and caches. It must not contain project truth, immutable rules, SDK lock, maps, or lifecycle records.

## SDK Bundle and lock

消费者只使用全局安装的 AppSDK Bundle。Bundle 版本必须同时绑定 Rust CLI、machine contracts、文档、规则和 skills；不能让项目自行扫描一个不受版本控制的全局目录。Bundle manifest 是机器真源，`init`/`new` 将其中声明的 Skill、规则和文档安装到项目 `.appsdk/`，并生成 `.appsdk/sdk-resources.json`。

项目中的手动合同（例如 `project.json`、records、maps）由 AI/开发者维护，SDK 只校验 schema、引用、owner、scope 和生命周期关系。`.appsdk/sdk-resources.json` 是自动生成文件，只能由 SDK 重建；`verify` 会校验资源文件摘要和 Bundle digest，发现手工改写立即 fail-fast。

`contracts/sdk-bundle.manifest.json` 声明当前 Bundle 的资源集合和安装位置；初始化后项目内的机器真源位于 `.appsdk/contracts/sdk-bundle.manifest.json`。它和 binary 同版本发布；`pin-lock` 把 binary digest、Bundle digest、manifest digest 和资源集合一起写入锁。

`.appsdk/sdk.lock` binds the project to the external implementation:

```json
{
  "sdk": "appsdk",
  "version": "0.1.5",
  "digest": "sha256:replace-with-compiled-sdk-digest",
  "compiler_digest": "sha256:replace-with-compiler-digest",
  "bundle_digest": "sha256:replace-with-sdk-bundle-digest",
  "bundle_manifest_digest": "sha256:replace-with-bundle-manifest-digest",
  "bundle_resources": {"contracts": [], "docs": [], "rules": [], "skills": []},
  "binary_ref": "project-sdk",
  "contract_schema": 1
}
```

The lock is committed. A template may retain the two documented `replace-with-*` values while the project is `draft`; compile, promotion, and freeze reject those values with `SDK_LOCK_NOT_PINNED`. The external SDK is replaceable only through an explicit versioned migration; runtime does not scan or infer a different SDK.

## Runtime boundary

Runtime may consume only the compiled manifest and verified Active artifact. Runtime must not scan `.appsdk-control/`, Playground, Protected source, or arbitrary instruction files to reconstruct capability.

## Bootstrap

初始化前先创建需求准备文档：

```bash
appsdk prepare ./existing-workspace
```

AI 使用 `.appsdk-prepare.json` 模板向用户确认：这是新项目、模块重构、项目重构还是 debug；新项目根目录是什么；旧代码和 V3 等 legacy roots 是什么；新代码、Protected 和禁止修改边界是什么。用户确认后将 `status` 更新为 `confirmed`，然后才能执行 `init`。没有确认记录时，`init` 会 fail-fast。

已有项目先执行：

```bash
appsdk init ./existing-workspace --project-root new-code
```

`init` 的第一个参数是已有工作区，`--project-root` 是新 AppSDK 项目的相对根目录。这样旧代码可以留在工作区，新代码和治理面进入独立子目录。它是幂等操作：创建 `playground/`、`active/lib/`、`protected/`、`generated/`、`.appsdk/` 和 `.appsdk-control/`，只补齐缺失的治理合同，并向新项目根目录的 `.gitignore` 追加一次 SDK 管理区块。它不覆盖已有项目文件，也不覆盖已有 Git 忽略规则；绝对路径和 `..` 路径会被拒绝。

新项目执行：

```bash
appsdk new ./my-app
appsdk verify ./my-app
```

初始化会自动安装 `.appsdk/docs/`、`.appsdk/rules/` 和 `.appsdk/skills/`。这些是项目治理输入，不是 runtime capability；runtime 只消费编译后的 manifest 和 Active library。

Then fill and confirm `.appsdk/goal.json`, bind project maps and module ownership, and follow the promotion contract. Template creation requires an empty, non-symlinked destination.

## Lifecycle

```text
goal clarification
  -> confirmed/admitted
  -> one semantic claim + one clean isolated worktree per worker
  -> baseline reproduction
  -> committed fix candidate + positive/negative evidence
  -> development whitebox PASS
  -> build + install + restart
  -> deployed public-entrypoint blackbox PASS
  -> PreReviewValidationRecord + `appsdk verify --review-admission` PASS
  -> architecture ReviewRecord PASS
  -> unchanged-source effectiveness replay PASS
  -> one independently verifiable milestone per clean worktree
  -> commit + serial merge queue + tested integration for every milestone
  -> start the next milestone only after the predecessor remote-main receipt
  -> verified local and remote mainline receipt
  -> PromotionRecord
  -> Active library
  -> Protected source/contracts
  -> FreezeRecord
```

The project contract is auditable and committed; the SDK implementation is external and pinned; local control state is disposable and ignored.
