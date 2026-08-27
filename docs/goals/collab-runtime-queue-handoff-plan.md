# Collab runtime queue and handoff implementation plan

## 目标与验收标准

完善 v1 Collab 的运行时感知、heartbeat、消息队列和 handoff 闭环：tmux/Herdr 只能在各自 runtime 内通信；daemon 只在目标 agent idle 时 heartbeat 或 idle 投递；重复积压消息合并后一次投递；worker deliver 后能继续选择任务；master 收到 deliver 后得到 merge 与任务列表更新提醒。

验收必须包括：代码、单元测试、真实 tmux session、真实 Herdr workspace、主分支合并、运行版本与源码一致、全局安装后的命令验证。

## 范围与边界

In scope：`/Users/fanzhang/code/collab` v1 Rust binary、runtime registration、tmux/Herdr input delivery、message journal/state、heartbeat、task deliver/dispatch/handoff、docs/README、全局 cargo 安装。

Out of scope：App Server、lock 修复、v2 Cordis、wrai.th、跨 cwd 通信、跨 runtime fallback、修改无关项目业务代码。

## 设计原则

- mailbox/journal 是唯一消息真源，控制语义不进入业务 payload。
- runtime 是项目注册边界：首个 worker 固定 tmux 或 Herdr，之后不允许混用。
- immediate 与 idle 是显式投递策略；不允许静默降级。
- dedupe 只合并相同目标、方向、类型和业务正文的未投递消息；不同消息不得丢失。
- heartbeat 只能投递给已注册且可确认 idle 的 pane；working、blocked、unknown、missing 均不发送。
- deliver、merge、close、claim 必须遵守现有角色权限和任务状态机。
- master 转移、stale worker 清理、任务 handoff 都必须写 journal，可重启恢复。

## 技术方案与文件清单

- `src/server/state.rs`：消息投递模式/队列索引、去重和 journal event。
- `src/proto.rs`、`src/main.rs`：`--delivery immediate|idle`、状态查询与 handoff 输出。
- `src/server/knock.rs`：tmux/Herdr pane alive/idle 检查及输入投递。
- `src/server/timers.rs`：idle-only heartbeat、idle queue flush、正反状态判断。
- `src/server/mod.rs`：发送策略、合并投递、Delivered 收口、master merge/task-list 提醒。
- `src/scope.rs`、`README.md`、`docs/collab.md`：生命周期、runtime、队列和 handoff 规则。

## 风险与规避

- tmux/Herdr 状态误判：使用真实 pane/session smoke，并覆盖 unknown/working/blocked/idle。
- 并发重复发送：使用 daemon mutex + journal event 原子提交；不得靠客户端去重。
- 重启丢队列：队列模式必须进入 journal，重放后仍可 flush。
- Herdr session/pane 变化：注册时保存 socket 和 pane，发送前重新查询；失败显式记录，不投递到默认 session。
- handoff 误关闭任务：只有合法状态转换才允许 close/merge；任务列表必须由 daemon 真状态生成。

## 测试计划

- 单元：runtime 隔离、消息 dedupe、immediate/idle、queue flush、idle heartbeat、非 idle 反向拒绝、deliver/claim/merge/close 状态机。
- 构建：`cargo fmt --check`、`cargo test`、release build。
- tmux smoke：独立 session，两个 pane，验证注册、immediate、idle、heartbeat 和 master transfer。
- Herdr smoke：独立 named session/workspace，两个 pane，验证 agent 状态、输入投递、idle queue 和同 runtime 通信。
- 反向 smoke：跨 runtime 注册/发送、无 pane 注册、working heartbeat、重复消息都必须失败或只投递一次。
- 安装：`cargo install --path /Users/fanzhang/code/collab --force`，确认 `command -v collab` 和 `collab --version`。

## 实施步骤

1. 读取现有 resource/function/mainline/verification 文档和当前 run notes，建立本次 change set。
2. 实现消息投递模式、journal queue、去重和 idle flush。
3. 接入 tmux/Herdr idle 检测与 runtime-specific 输入投递。
4. 修改 heartbeat，仅允许 idle 目标，并补正反测试。
5. 完善 worker deliver 返回 available task list 和 master merge/update 提醒。
6. 同步文档，运行单元测试、构建和真实 tmux/Herdr smoke。
7. 在主 tree 复验安装和在线命令；通过 AGY Review MCP 后再提交。
8. 只提交本 change set，合并到 `main`，确认 HEAD 与运行 binary 一致，再执行全局安装和最终 smoke。

## 完成定义（DoD）

- 所有测试和两种 runtime 的真实 smoke 通过，正反向证据齐全。
- 没有 App Server 或 v2 运行路径。
- queue、heartbeat、handoff 状态可重启恢复且无重复投递。
- AGY Review PASS；只提交声明路径；已合并 `main`。
- `/Users/fanzhang/.cargo/bin/collab` 来自最新 main，版本和源码一致。
