# Design: rename-to-appkeeper

## Context

命名决策已定：全名 `appkeeper`、简称 `ak`、编译产物与命令 `ak`（见
proposal.md「What Changes」与 `branding` 规范）。本设计只回答"怎么落地"。

现状约束（探查所得，决定了工作量与顺序）：

- 旧名散布约 200 个受版本控制的文件、约 1200 处；密集区为 `README.md`（150）、
  `webui/src/styles/tokens.css`（100）、`src/main.rs`（88）、
  `webui/src/styles/shared.ts`（86）、`xtask/src/e2e.rs`（67）。
- 名字以多种形态出现，必须分别处理：普通标识（`xkeeper`）、crate/目录
  （`xkeeper-bench`）、文件名（`xkeeper.toml`）、路径
  （`/etc/xkeeper`、`/tmp/xkeeper/logs`、`%APPDATA%\xkeeper`）、unit 名
  （`xkeeper.service`）、环境变量（`XKEEPER_*`）、CSS 变量（`--xkeeper-*`）、
  自定义元素（`xkeeper-*`）、localStorage 键（`xkeeper:theme`、
  `xkeeper.refresh_ms`）、生成内容里的提示串（脚手架注释、错误信息）。
- 有一处**活跃**变更 `openspec/changes/add-config-subcommand`（12/13 任务）
  的规划工件仍用旧名，且其未完成任务会改写 `README.md`/`AGENTS.md`。
- `webui/dist/` 与 `target/` 被 gitignore；`webui/dist` 是 rust-embed 的嵌入源。

## Goals / Non-Goals

**Goals:**

- 一次改净，使"产品面零旧名残留"可机械校验（搜索即断言）。
- 改名不改变任何运行时行为契约：进程管理、控制面、配置分层、webui 协议
  语义逐字节不变，只是名字变了。
- 构建、测试、e2e、bench 在两条平台路径上全绿。

**Non-Goals:**

- 不提供用户侧自动迁移工具（旧配置/旧 unit 的搬迁由部署人手工完成）。
- 不改 `openspec/changes/archive/**` 的历史变更记录（历史即历史）。
- 不改 git 历史、不改远端仓库与本地目录名（仓库外动作，见 Open Questions）。
- 不重构任何被顺手带到的代码（纯改名，避免与后续变更纠缠）。

## Decisions

### D1 分层命名规则（全名 vs 简称）

`ak` = 紧凑/机器标识面；`appkeeper` = 完整/人类阅读面。逐面归属：

| 标识面 | 采用 | 理由 |
|---|---|---|
| cargo package / bench crate / webui 包名 | `appkeeper` / `appkeeper-bench` / `appkeeper-webui` | 包名是完整词，也是 `cargo install`/检索标识 |
| 二进制与 CLI 命令名、clap `name`、帮助/错误/提示符 | `ak` | 用户高频敲击，短 |
| 环境变量前缀 | `AK_` | 环境变量是机器接口，求短 |
| CSS 自定义属性 / 自定义元素 / localStorage 键 | `--ak-*` / `ak-*` / `ak:*` | 机器标识，且控制台自称应与命令一致 |
| systemd unit 文件名 | `ak.service`（`Description` 用 `appkeeper`） | unit 名要被 `systemctl` 敲；描述面向人 |
| app 配置文件名 / daemon 配置路径 / 默认日志目录 | `appkeeper.toml`、`/etc/appkeeper/`、`/tmp/appkeeper/logs` | 部署人手工书写的路径，可读优先 |
| Ansible role 目录与变量前缀 | `roles/appkeeper`、`appkeeper_*` | 与配置文件、路径同层，保持一套词 |
| 文档/规范正文 | `appkeeper`；命令示例 `ak …` | 可读性 |

**备选与否决**：①"ak 贯穿一切（含 `ak.toml`、`/etc/ak`）"——否决，文件路径
是低频书写、需要自解释，缩写会牺牲可读性；②"仅二进制叫 ak，其余全
`appkeeper`"——否决，会让 env/CSS/组件出现长前缀，且控制台与命令名不一致。

### D2 不留任何兼容别名

不提供旧命令名 shim、旧环境变量回退读取、旧文件名双读、旧 unit 名兼容。
理由：本工具尚在 0.1，无外部兼容负担；兼容层会让"零残留"无法机械校验，且
旧名会长期存活。**备选**：加一次性 `XKEEPER_*` 回退读取——否决，收益仅覆盖
少数构建脚本，代价是永久双路径。

### D3 规范中的产品名按"符号引用"就地改写，不产出 MODIFIED

`openspec/specs/**` 16 个能力规范里约 60 处出现旧名（含路径、unit 名、命令名）。
这些是引用，不是行为：改名后要求的"行为"完全不变。因此本变更**不**为
`configuration`/`service-registration` 等写 MODIFIED delta（否则归档时会用
等同内容重写正式规范、制造"行为变更"假象）；改由实施阶段在正式规范里就地
替换这些符号引用。行为契约的唯一新增/变更点集中在新能力 `branding`。

### D4 控制台存储键直接更名，不做一次性迁移

`xkeeper:theme` / `xkeeper.refresh_ms` → `ak:theme` / `ak.refresh_ms`，不写
旧键读取回退。代价是升级后主题与刷新偏好回落到默认一次。**备选**：双读旧键
再写新键——否决，为一个可再设的偏好引入长期兼容分支不值。

### D5 bench crate 连目录改名

`xkeeper-bench/` → `appkeeper-bench/`（含 crate 名、`xtask` 里的
`-p xkeeper-bench` 调用与产物路径常量、`BENCH_PREFIX = "ak-bench-"`、报告里的
二进制名字段）。理由：crate 名与目录名必须一致，留着旧目录会让 `cargo -p`
与路径发现逻辑长期错位。

### D6 与活跃变更 `add-config-subcommand` 的协调

该变更未完成任务会在 README/AGENTS 写入命令示例。若先合入它、再改名，示例会
被重写；若先改名、再合入它，它会把旧名写回 README。**决策**：本变更的实施
范围包含把该活跃变更的规划工件（proposal/design/tasks/specs 里的命令与路径
引用）一并改为新名，使"非 archive 面零旧名"成立。**备选**：把
`openspec/changes/**` 整体排除出零残留规则——否决，会放过 README 被写回旧名
的路径。

### D7 改名顺序按依赖与验证成本排序

manifest → `src/` → `build.rs` → bench crate（先改 crate 名，避免 workspace
解析失败）→ `xtask` → webui → 部署样例/Ansible → 文档与规范 → 全量校验。
每层改完即可编译/测试该层，失败能定位到层，而不是最后面对一次性大爆炸。

## Risks / Trade-offs

- [crate 改名后 workspace 解析与 `-p`/路径常量错位] → 先改 `Cargo.toml`
  members 与 bench `Cargo.toml`，`cargo metadata` + `cargo build --workspace`
  立即验证；`xtask`/`e2e` 里发现 bench 产物的路径常量同批核对。
- [隐藏残留：生成物、注释、测试夹具里的字面量] → 以大小写不敏感搜索为验收
  断言（排除 `archive/`、`.git/`、`target/`、`webui/dist/`、`node_modules/`），
  并在 `webui/dist` 重建后再查一次。
- [webui 改名后前端类型检查/测试失败（标签与 CSS 变量成对出现）] → 按
  AGENTS.md 约束，前端必须 `tsc --noEmit && test && build` 后再 `cargo build`，
  否则二进制嵌的是旧前端。
- [Windows 路径与 `%APPDATA%`、PATHEXT 行为只在一侧验证] → 平台默认路径常量
  有既有单测覆盖；按仓库约束两条平台路径都过一遍（CI 矩阵含 windows）。
- [活跃变更 `add-config-subcommand` 把旧名写回文档] → 见 D6，同批改名。
- [用户侧可感知破坏：命令名、env、配置文件名、unit 名、默认路径全变] → 属
  proposal 已声明的 BREAKING；无自动迁移，需在 README 给出迁移清单。
- [service install 写入的 `ExecStart` 指向 `ak`] → 既有单测以
  `/usr/local/bin/xkeeper` 等夹具断言，须同步改夹具，否则测试假绿。

## Migration Plan

1. 按 D7 顺序落地改名，每层编译/测试。
2. 前端重建（`pnpm build`）后 `cargo build --release` 重新嵌入，确认产物为
   `target/release/ak`。
3. 全量校验：`cargo test --workspace`、`cargo run -p xtask -- e2e`、
   bench 冒烟；零残留搜索断言。
4. 文档补"从 xkeeper 迁移"清单（命令名、env、`appkeeper.toml`、
   `/etc/appkeeper/daemon.toml`、`ak.service`）。
5. 回滚：单次改名提交，`git revert` 即可；无数据迁移、无状态变更。

## Open Questions

- 远端仓库与本地目录是否随之改名（`git@github.com:cupen/xkeeper`、
  `repos-ai/xkeeper`）：属仓库外动作，可延后；不影响本变更的规范与任务分解。
