# design: daemon-subcommand

## Context

当前 CLI 分派（`src/main.rs`）：`dispatch` 的 `None => run_daemon` 使裸 `ak` 直接进守护循环；`Cmd::Run` 的 else 分支使裸 `ak run` / `ak run --detach` 同为守护入口。已有先例：`start` 动词移除时采用了 clap 前参数扫描拦截（`reject_removed_start`），带迁移提示与退出码 2。systemd unit 的 `ExecStart` 由 `src/service.rs` 单一来源渲染（ansible 也复用 `ak service install`，不自写模板）；daemon 离线自动拉起在 `src/daemonize.rs` 中以 re-exec 自身并传 `run` 子命令实现。

## Goals / Non-Goals

**Goals:**

- 守护入口收敛为唯一显式子命令 `ak daemon`，`run` 变纯客户端动词。
- 裸 `ak` 成为安全的探索性调用（help + exit 0）。
- 旧守护形态 `run` 的调用得到可自助解决的失败信息（含 systemd 场景）。
- 守护进程运行时行为（supervisor / server / webui / 拉起协议）零改动。

**Non-Goals:**

- 不改 `service install` 的 `--force` 覆盖语义与 root 权限要求。
- 不为旧入口提供任何形式的兼容别名或 deprecation 宽限期。
- 不动 `appkeeper shell` 内的 `run` 动词（本就是客户端语义）。

## Decisions

### D1: 裸 `ak` → 手动打印完整 help 到 stdout，exit 0

在 `dispatch` 的 `None` 分支调用 `Cli::command().print_help()` 后正常返回。不使用 clap 的 `arg_required_else_help = true`（输出走 stderr、退出码 2，与「探索性调用是合法调用」的定位相悖）。help 内容与 `ak --help` 等价，不另维护摘要。

*备选否因*：stderr + exit 2（git 风格）把 help 当错误，脚本捕获与管道行为不友好。

### D2: `run` 守护形态用 clap 前拦截移除，复用 `reject_removed_start` 模式

拦截判定：首子命令为 `run`，且其余 token 只含 `-c/--config <path>`（含 `--config=<path>`）与 `--detach`，无位置参数、无 `--`/`--cmd` —— 即「守护形态特征」。命中则退出码 2，提示按形态分流：含 `--detach` → `use ak daemon --detach`；仅 config 形态 → 追加 `a systemd unit with the old ExecStart must be regenerated: ak service install --force --now`。判定先于 clap，因此 `Cmd::Run` 的 `detach` 字段随之删除（该形态永远到不了 clap）。

*备选否因*：兼容别名（`run` 照跑 daemon）使入口永远双重身份，规范无法收口；deprecation 宽限期需规范两阶段语义，且项目刚过 xkeeper→appkeeper breaking rename 窗口，不差这一次。

### D3: 新增 `Cmd::Daemon { detach }`，复用既有函数

`ak daemon` 前台调用现 `run_daemon`，`ak daemon --detach` 调用现 `run_detach`，两者逻辑不动。`--config` 继续用全局旗标。不新增其他参数。

### D4: 新语义裸 `ak run` 在 dispatch 层报用法错误

拦截（D2）排除后，`Cmd::Run` 无 name/app/adhoc/detach 的空组合到达 dispatch：报「nothing to run」并列出三种客户端形态，exit 2。clap 表达不了「可选位置参数至少一个」的组合约束，放在 dispatch 与现有 ad-hoc 冲突检查同层。

### D5: ExecStart 与内部 re-exec 同步改 `daemon`

`service.rs` 渲染 `ExecStart=<bin> daemon --config <path>`；`daemonize.rs` 的 re-exec 参数从 `run` 改为 `daemon`。`daemonize.rs` 模块注释同步。`client.rs` 不可达提示 `(is \`ak run\` active?)` 改为 `(is \`ak daemon\` active?)`。

### D6: 升级路径文档化，不写运行时迁移逻辑

已装机 unit 升级后首次拉起会被 D2 拦截（提示含自救命令）。不做自动改写 unit 的逻辑（越权且有静默写系统文件风险）。`.ansible/README.md` 写明跨版本升级需 `appkeeper_force_overwrite: true` 重跑（默认流在二进制变化时不带 `--force`，`service install` 会因 unit 内容差异拒绝，任务失败可见）。

## Risks / Trade-offs

- [已装机 systemd unit 升级后服务拉起失败] → 拦截错误内置 `ak service install --force --now` 提示；升级路径写入 `.ansible/README.md` 与发布说明。回滚注意：unit 已改为 `daemon --config` 后换回旧二进制同样起不来（旧二进制无 `daemon` 子命令），回滚须同时用旧二进制 `service install --force` 恢复旧 unit。
- [拦截误伤合法调用] → 守护形态特征（run + 仅 config/detach 旗标）在客户端语义下本就非法（客户端形态必带操作数），无误伤面；`-c`/`--config` 值跳过逻辑沿用 `reject_removed_start` 已验证的实现，`--config=X` 以 `-` 开头自然跳过。
- [调用点漏改（xtask/stress/bench/e2e）] → tasks 逐点列出，`cargo run -p xtask -- e2e` 与 bench firehose 冒烟兜底。
- [规范措辞残留] → 除五个 delta 外，`shell-client` 与 `background-daemon` 主规范的 Purpose 已直改（Purpose 不走 delta）；归档时 `openspec validate --specs` 复核。

## Migration Plan

1. 实现 D1–D5，全量测试（单测 + `cargo run -p xtask -- e2e` + bench 冒烟）。
2. 发布说明与 `.ansible/README.md` 写升级步骤：分发新二进制后执行 `ak service install --force --now`（或 ansible 设 `appkeeper_force_overwrite: true` 重跑）。
3. 回滚：换回旧二进制并以旧二进制执行 `ak service install --force` 恢复旧 unit（两者必须同批操作）。
