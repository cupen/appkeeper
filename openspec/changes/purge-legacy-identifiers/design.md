# Design: purge-legacy-identifiers

## Context

`rename-to-appkeeper` 已确立「全名 `appkeeper` / 简称 `ak`」契约并完成主标识面
（CLI、环境变量、配置路径、webui 标签）改名，但清查不彻底。当前活文件残留约
100 处，按性质分五类：

1. `#[cfg(test)]` 临时目录/标识前缀 `xk-<场景>-`（`src/*.rs`、`tests/`、
   `xtask/`，40 余种散乱后缀形态）；
2. bench 运行期契约前缀 `xk-bench-`——`appkeeper-bench/src/topology.rs`
   生成临时 workspace，`xtask/src/e2e.rs` 与
   `appkeeper-bench/tests/integration.rs` 按该前缀扫描断言无残留；
3. Web 控制台 logo 字样 `xk`（`webui/src/app-shell.ts`）；
4. GitHub 仓库引用 `cupen/xkeeper`（README 下载链接 4 处、
   `scripts/verify-release-workflow.sh` 3 处——远端真名当前仍是旧名）；
5. 测试夹具中性数据与 `branding`/`service-registration` 规范文本中的旧名
   提法（`branding` 的「零残留」requirement 自身含旧名字面，自证违例）。

见 proposal.md - Why。归档 `openspec/changes/archive/**` 按契约豁免，不动。

## Goals / Non-Goals

**Goals:**

- 仓库内（归档外）旧名全名与其缩写字面清零，零残留门禁可按 branding 新
  scenario 字面执行；
- 测试/工具临时标识归并到统一前缀模式，终结命名泛滥；
- bench 清理契约前缀更名且三处联动一致。

**Non-Goals:**

- 不重写归档与 git 历史；
- 不提供任何旧名兼容层或迁移逻辑（含旧 `xk-bench-*` 临时目录的清理——
  交由 OS 临时目录机制兜底）；
- 不执行 GitHub 仓库名实际重命名与 `git remote set-url`（用户侧操作，
  本设计仅假设其将完成）；
- 不改任何运行期行为逻辑（纯标识、文案与测试夹具）。

## Decisions

### D1 临时标识统一前缀模式（归并而非直换）

- `src/*.rs` 与 `tests/*.rs`：统一 `ak-test-<场景>-<pid>`（无 pid 的测试
  用例补 pid 或保持既有 tag 组合，保证并发唯一性不弱于现状）。
- `xtask/`：统一 `ak-xtask-<场景>`。
- `appkeeper-bench/`：契约前缀 `xk-bench-` → `ak-bench-`，`<pid>-<nanos>`
  结构不变。

备选「最小直换」（`xk-` → `ak-` 各自后缀不动）被否：保留 40+ 种散名，
违背用户「统一前缀、避免命名泛滥」的决策。备选「全域单一前缀
`ak-tmp-`」被否：bench 前缀承担清理断言扫描契约，必须与测试临时标识
可区分；xtask 场景与单测场景的域分离也便于 e2e 残留归因。

场景名归并规则：同一测试内语义不同的后缀（如 metrics 与 metrics-panic）
保留区分，统一挂到 `ak-test-` 下；无语义价值的后缀变体（如同一 helper 的
`-f`/`-n`/`-r` 单字母）保留原样——归并目标是前缀统一，不是重命名全部
用例。

### D2 bench 前缀三处联动 + 旧残留语义

前缀是运行期清理契约：生成侧（`topology.rs` 的 `spawn_site`）与两处扫描侧
（`xtask/src/e2e.rs` bench 冒烟后的残留断言、
`appkeeper-bench/tests/integration.rs` 的清理契约断言）必须同一次变更改齐，
否则断言扫不到自己刚生成的 workspace 之外的真实残留或误报。更名后，
历史上旧版本跑剩的 `xk-bench-*` 临时目录不再被断言覆盖——可接受（见
Non-Goals），如需处理由用户一次性手工清理。

### D3 branding 旧名代称化与权威指派

`branding` 规范正文的旧名提法全部改写为代称「旧名」，字面值以归档变更
`2026-10-14-rename-to-appkeeper` 与 git 历史为权威指派。规范不携带旧名
字面，零残留 scenario 才能字面自洽；可测试性由验证任务硬编码 grep 模式
（`xkeeper` 全名与 `\bxk\b` 缩写、大小写不敏感）保证。delta 覆盖三个
requirement（分工/完整改名/零残留），其中「分工」新增统一前缀约束与
scenario，「完整改名」覆盖面补入 GitHub 仓库引用与页面可见标识字样，
「零残留」排除面修正为 `openspec/changes/`（归档与进行中工件——本变更
工件自身合法引用旧名）、`.git/`、`webui/node_modules/`、`target/`。

规范 Purpose 行的旧名提法不属 delta 管辖（openspec 约定 Purpose 直接改
主规范），与 `service-registration` 场景示例 `--name ak-prod` 一并作为
主规范直接编辑任务执行。

备选「branding 加豁免条款保留旧名字面」被否（用户拍板连字面一并清零）；
备选「删除 rename 归档」被否（归档是历史记录，且删除不解决活规范问题）。

### D4 GitHub 引用改指新仓库名

README 4 处下载链接与校验脚本 3 处 `cupen/xkeeper` 一律改
`cupen/appkeeper`。校验脚本中的 `GITHUB_REPOSITORY` / `--repo` 是自洽
测试夹具（模拟值+回显断言），改名不影响其校验逻辑；README 链接是事实性
下载地址，依赖「远端仓库名将同步更改」的假设（记录于 proposal Impact）。

## Risks / Trade-offs

- [远端仓库名未及时更改，README 链接与发布流程指向 404] → 实施完成后
  提醒用户立即在 GitHub 改名并 `git remote set-url`；GitHub 对旧仓库名
  的重定向可兜底 git 操作，但 Release 资产链接以新名为准。
- [旧 `xk-bench-*` 临时目录残留无人断言] → 接受；必要时用户一次性清理
  `/tmp` 下旧前缀目录。
- [webui logo 改动忘记重嵌入] → 遵守既有约束：`pnpm exec tsc --noEmit
  && pnpm test && pnpm build` 后再 `cargo build`（AGENTS.md 已载）。
- [归并前缀时测试唯一性被破坏（并发冲突）] → 归并只动前缀部分，pid/tag
  组合保持；以 `cargo test --workspace` 全绿为准。

## Migration Plan

无部署迁移：CLI、配置、协议、API 零变化。同机已运行的 daemon 与本次改名
互不影响（bench 每次运行自建隔离环境）。回滚即 git revert。
