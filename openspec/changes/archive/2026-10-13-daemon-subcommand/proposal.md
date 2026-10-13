## Why

裸 `ak` 当前直接进入守护循环（`main.rs` 中无子命令即 `run_daemon`）：误敲一个 `ak` 就在前台挂起终端或意外拉起守护进程，且与「`run` 是客户端动作」的心智相悖。`run` 一词身兼守护入口与客户端动词，语义含混；对齐 `docker run` 式心智——客户端动词负责向 daemon 发指令，守护进程由显式子命令 `ak daemon` 运行。

## What Changes

- **BREAKING** 裸 `ak`（无子命令）不再启动守护进程，改为输出完整 help 到 stdout 并以 0 退出。
- **BREAKING** 新增 `ak daemon` 子命令：唯一守护入口，前台运行；`--detach` 跟随该子命令（`ak daemon --detach` 后台拉起，沿用既有 background-daemon 拉起协议：就绪探测、日志路径报告、端口冲突报错）。
- **BREAKING** 移除 `run` 的守护形态：裸 `ak run` 与 `ak run --detach` 被拦截（clap 前拦截，沿用已移除 `start` 动词的迁移提示模式），退出码 2，按形态给提示——`--detach` 形态提示 `ak daemon --detach`；仅 `--config` 形态（systemd ExecStart 特征）额外提示用 `ak service install --force --now` 重新生成 unit。
- `run` 收敛为纯客户端动词：`run <name>` / `run --app <app>` / `run -- <cmd...>` / `run --cmd "<string>"` 行为不变；新语义下裸 `ak run`（无任何操作数）报用法错误并列出三种客户端形态，退出码 2。
- **BREAKING** `service install` 生成的 systemd unit `ExecStart` 由 `<bin> run --config <path>` 改为 `<bin> daemon --config <path>`。已装机机器升级二进制后旧 unit 拉起会被拦截失败，须重新生成 unit（手动 `ak service install --force --now` 或 ansible 设 `appkeeper_force_overwrite: true` 重跑）；升级路径写入 `.ansible/README.md` 与发布说明。
- 内部自动后台拉起（daemon 离线时 `add --apply` / run ad-hoc 的 re-exec）改传 `daemon` 子命令，拉起行为不变。
- 同步面：`client.rs` 守护不可达提示文案、README / AGENTS.md、xtask e2e 与 stress、appkeeper-bench topology 中以 `run` 起 daemon 的调用点全部改为 `daemon`。

## Capabilities

### New Capabilities

（无——守护行为本身不变，变的是入口动词面，全部落在既有能力域内）

### Modified Capabilities

- `control-plane`: 动词面重构——`run` 的守护形态（裸 run / `run --detach`）从动词表中移除并拦截；新增 `daemon` 子命令为唯一守护入口；裸 `ak` 输出 help（stdout，exit 0）。
- `background-daemon`: 后台拉起的入口动词由 `ak run --detach` 改为 `ak daemon --detach`；裸 `run` 前台守护语义由 `ak daemon` 承接；自动拉起 re-exec 的子命令同步。
- `service-registration`: 生成的 unit `ExecStart` 改用 `daemon --config <path>`；拦截错误对旧 ExecStart 给出 unit 重新生成的自救提示。
- `webui-api`: 规范文中以 `ak run` 指代守护启动的措辞改为 `ak daemon`（行为不变，仅入口动词同步）。
- `configuration`: 规范场景中以 `ak run` 触发守护启动的示例动词改为 `ak daemon`（行为不变，仅入口动词同步）。

## Impact

- 代码：`src/main.rs`（Cli/Cmd 结构、拦截函数、dispatch）、`src/daemonize.rs`（re-exec 参数）、`src/service.rs`（ExecStart 渲染）、`src/client.rs`（提示文案）。
- 测试与工具：`xtask` e2e/stress、`appkeeper-bench` topology 的 daemon 启动调用点；main.rs 内 run 解析相关单测。
- 文档：README、AGENTS.md、`.ansible/README.md`（升级路径：`appkeeper_force_overwrite: true`）。
- 运维影响：已装机 systemd unit 必须重新生成，否则升级后服务拉起失败（拦截错误含自救提示）；项目刚完成 xkeeper→appkeeper breaking rename，本变更落在同一 breaking 窗口。
- 无新增依赖；supervisor / server / webui 运行时行为零改动。
