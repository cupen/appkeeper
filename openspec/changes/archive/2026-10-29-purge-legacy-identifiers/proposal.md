# Proposal: purge-legacy-identifiers

## Why

`rename-to-appkeeper` 变更换名不彻底：仓库内（归档外）仍散落约 100 处旧标识
`xkeeper`/`xk`——测试临时目录前缀、bench 运行期清理契约前缀、Web 控制台
logo 字样、README 下载链接等，`branding` 规范的「产品面零残留」requirement
从未兑现；且该 requirement 文本自身以旧名作符号引用，按其字面自证违例，
门禁无法落地。

## What Changes

- **测试临时标识归并统一**：`src/*.rs` 与 `tests/*.rs` 中 40 余种
  `xk-<场景>-` 临时目录前缀归并为 `ak-test-<场景>-<pid>` 单一模式
  （同模块重复场景名一并归并）；`xtask/` 归并为 `ak-xtask-<场景>`。
- **bench 运行期前缀改名**：临时 workspace 契约前缀 `xk-bench-` →
  `ak-bench-`（结构不变），三处联动——`appkeeper-bench/src/topology.rs`
  生成、`xtask/src/e2e.rs` 残留扫描、`appkeeper-bench/tests/integration.rs`
  清理断言。旧版跑剩的 `xk-bench-*` 临时目录不再被新版断言覆盖（可接受，
  由 OS 临时目录清理兜底）。
- **Web 控制台 logo**：`webui/src/app-shell.ts` 的标识字样 `xk` → `ak`。
- **GitHub 引用改指向新仓库名**：README 下载链接（4 处）与
  `scripts/verify-release-workflow.sh`（3 处）中 `cupen/xkeeper` →
  `cupen/appkeeper`（假设远端仓库名将同步更改，见 Impact）。
- **测试夹具中性数据**：`svc-xk` → `svc-ak`、`xk-prod_1.service` →
  `ak-prod_1.service`、`/var/log/xk` → `/var/log/ak`、`C:\xk\logs` →
  `C:\ak\logs`、`xk-definitely-not-an-editor` →
  `ak-definitely-not-an-editor` 等。
- **规范文本**：`openspec/specs/branding/spec.md` 正文 11 处旧名符号引用
  改写为代称（旧名字面值以归档 `rename-to-appkeeper` 与 git 历史为权威
  指派），「零残留」scenario 的排除面明确为 `openspec/changes/`（归档与
  进行中变更工件）与 `.git/`，并补记临时标识的统一前缀约束；
  `service-registration` 规范场景示例 `--name xk-prod` → `--name ak-prod`
  （符号引用随命名契约更新，不构成 requirement 语义变更）。
- **归档不动**：`openspec/changes/archive/**`（含 `add-xkeeper-bench`
  目录名）是历史记录，本次不重写。
- 无兼容层：不提供任何旧名回退、双读或别名（测试临时标识每次运行新建，
  bench 前缀只影响新生成的 workspace，均无兼容面）。

## Capabilities

### New Capabilities

（无。）

### Modified Capabilities

- `branding`: 「旧名不残留且无兼容别名」requirement 的旧名提法代称化、
  排除面修正为 `openspec/changes/` 与 `.git/`；「全名与简称的分工」
  requirement 细化机器识别用临时标识的统一前缀模式
  （`ak-test-*` / `ak-xtask-*` / `ak-bench-*`）。

## Impact

- Rust 测试代码：`src/{action,client,config,daemonize,main,metrics,program,
  pump,registry,server,service,supervisor,transport,web}.rs` 的
  `#[cfg(test)]` 临时目录/标识（约 60 处）、`tests/revamp_integration.rs`、
  `xtask/src/{e2e,stress}.rs`、`appkeeper-bench/src/{api,cases,dcfg,
  integrity,report,topology}.rs` 与 `appkeeper-bench/tests/integration.rs`。
- 前端：`webui/src/app-shell.ts`（logo mark，需重跑 tsc/test/build 并重新
  嵌入）。
- 文档与脚本：`README.md`、`scripts/verify-release-workflow.sh`。
- 规范：`openspec/specs/branding/spec.md`、
  `openspec/specs/service-registration/spec.md`。
- 假设（记录在案）：GitHub 远端仓库名将由 `cupen/xkeeper` 同步改为
  `cupen/appkeeper`，否则 README 链接与校验脚本指向不存在的地址；git
  remote 本地 URL 的更新（`git remote set-url`）是用户侧操作，不在本次
  代码范围。
- 用户可感知影响：Web 控制台 logo 字样变化；无 CLI/配置/协议层面的
  行为变化。
