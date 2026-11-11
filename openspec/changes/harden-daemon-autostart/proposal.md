# Proposal: harden-daemon-autostart

## Why

`ak daemon install` 已能把 daemon 注册为 systemd unit / Windows SCM 服务并开机自启，但「开机启动」这条链路上的若干行为与设计意图相悖：systemd 缺省 `KillMode=control-group` 会在停服时绕过 daemon 的逆序优雅停止；`Restart=always` 让 `ak daemon shutdown` 变成 3 秒后自动复活；`RuntimeDirectory=appkeeper` 停服时递归删除共享的 `/run/appkeeper`，会打断同机其他用户的实例；默认 `log_dir=/tmp` 使服务形态的日志与 `unless-stopped` 用户停止记录随重启丢失；服务形态不继承登录环境、不等待网络就绪；Windows 服务以 LocalSystem 运行时默认命名管道与安装者的命令行对不上；`install` 对缺失/损坏的 daemon 配置不报错，装完却空跑。这些问题都落在「规范没写」的空白里，需要先补规范、再按规范修实现。

## What Changes

- **systemd unit 硬化**（`ak daemon install` 生成的 unit）：
  - 新增 `KillMode=mixed`：停服时 SIGTERM 只发给 daemon 主进程，由 daemon 按逆序、按各程序 `stop_timeout` 优雅停止；超时后 systemd 才 SIGKILL 整个 cgroup。
  - `RuntimeDirectory` 由共享根 `appkeeper`（0755）改为按运行 uid 分层 `appkeeper/<uid>`（安装期解析的字面 uid；`%U` 在 system unit 中恒展开为 0，不可用），模式 `0700`；停服只清理本实例目录，不影响同机其他用户的 `/run/appkeeper/<uid>`。
  - 新增 `StateDirectory=appkeeper/<uid>`（`0750`，字面 uid 同上）：把「用户停止」记录从 `log_dir` 迁到 systemd 的 State 类目录 `/var/lib/appkeeper/<uid>`，跨重启保留、停服不删、卸载不删；daemon 启动时优先采用 `$STATE_DIRECTORY`。
  - 新增确定性的 `Environment=PATH=...`（标准系统路径），消除不同发行版/systemd 版本对服务 PATH 的差异。
  - 新增 `Wants=network-online.target` + `After=network-online.target`，配合既有 `After=network.target`，降低开机早期依赖网络的程序被判失败的窗口。
  - `[Unit]` 新增 `StartLimitIntervalSec` / `StartLimitBurst`，显式声明失败重启上限。
- **持久状态与日志解耦**：新增可选 `[daemon] state_dir`（绝对路径，`config set/get/delete` 白名单同步）；未配置时按 `$STATE_DIRECTORY` → 平台 State 默认（unix root `/var/lib/appkeeper/<uid>`、非 root `$XDG_STATE_HOME/appkeeper`、Windows 服务 `%ProgramData%\appkeeper\state`、控制台 `%LOCALAPPDATA%\appkeeper\state`）推导；均不可写时告警回退 `log_dir`。既有 `<log_dir>/state.json` 一次性迁移到新位置。
- **`install` 安装期预检**：daemon 配置缺失或校验失败时拒绝安装（提示 `ak config --init` 或 `-c`）；注册 app 中存在不可解析项时逐条告警但不阻断；解析后的 `log_dir` 或生效 `state_dir` 位于 `/tmp`（或 Windows `%TEMP%`）时给出显式告警（日志丢失／停止记录丢失各自说明）。
- **服务托管识别与 `shutdown` 语义**：daemon 启动时判定自己是否由 systemd/SCM 托管（Linux `INVOCATION_ID` 环境变量，Windows `--service` 运行模式），并在状态投影中暴露托管来源；托管时 `POST /v1/shutdown` / `ak daemon shutdown` SHALL 拒绝并提示改用 `systemctl stop <unit>` / `sc stop <name>`，避免被 `Restart=always` 复活。
- **BREAKING** Windows 配置与端点：
  - daemon 默认配置路径由 `%APPDATA%\appkeeper\daemon.toml` 改为 `%ProgramData%\appkeeper\daemon.toml`（机器级，服务账号可读）；`%APPDATA%` 下的旧文件作为兼容回退仍会被读取，`install` 在回退命中时提示迁移。
  - Windows `install` 在生效 `socket` 为默认 IPC 管道、而服务运行账号与安装者不一致时告警，并给出「显式写 `socket`（固定管道名或 `tcp://`）」的修复指引。
- **验收补齐**：unit 生成物断言（KillMode / RuntimeDirectory / Environment / network-online / StartLimit）、`shutdown` 拒绝路径、以及「模拟开机」e2e（以服务形态等价入口重跑 daemon，断言按策略拉起与 `unless-stopped` 记录跨重启保持）；真机 systemd 冒烟以环境变量门控、缺省跳过。

## Capabilities

### New Capabilities

（无——全部并入既有能力规范）

### Modified Capabilities

- `service-registration`: unit 模板硬化（KillMode / RuntimeDirectory 硬化 / StateDirectory / Environment / network-online / StartLimit）、install 预检与告警、服务托管识别。
- `control-plane`: 状态投影新增托管来源字段；托管时 `/v1/shutdown` 与 `ak daemon shutdown` 拒绝并给出服务管理器命令。
- `configuration`: 新增 `[daemon] state_dir`（持久状态的落位与推导、`log_dir` 解耦）；Windows daemon 配置默认路径改为 `%ProgramData%\appkeeper\daemon.toml`（含旧路径兼容回退）。
- `process-management`: 明确「用户停止」记录 MUST 落在持久状态目录（State 类），不得落在日志或运行时目录，并在服务卸载后保留。

## Impact

- **代码**：`src/service.rs`（unit 渲染、install 预检、Windows 配置路径与管道告警）、`src/main.rs`（Windows 默认配置路径、托管识别的启动接线）、`src/config.rs`（`state_dir` 解析/校验/白名单与模板）、`src/supervisor.rs`（状态目录迁移与读写）、`src/server.rs`（状态投影与 shutdown 拒绝）、`src/client.rs`（409 错误呈现）。
- **测试**：`src/service.rs` / `src/config.rs` / `src/main.rs` 单元测试更新；`xtask/src/e2e.rs` 新增 unit 断言、状态目录迁移与跨重启保持、shutdown 拒绝与「模拟开机」段；真机 systemd 段以 `AK_E2E_SYSTEMD=1` 门控。
- **文档**：README 命令速查/升级速览（服务形态用 `systemctl stop`、`log_dir` 与 `state_dir` 的持久性、状态目录迁移、Windows 配置路径迁移）。
- **不影响**：Linux/Windows 的控制面协议其余部分、webui、bench、注册表与 apply 语义。Windows 真机路径仍受限于本仓库既有的「无 Windows 实机」验收缺口，本变更只保证纯逻辑单测与条件编译通过。
