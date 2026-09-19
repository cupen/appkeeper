# Proposal: add-windows-service-registration

## Why

Linux 侧 `xkeeper service install` 已能把 daemon 注册为 systemd 服务（开机自启、崩溃拉起），而 Windows 上同一命令直接失败「暂不支持 Windows 服务注册」。Windows 是本项目的一等平台（Job Object 进程树清理、Ctrl+C 处理均已适配），服务注册是 Windows 开机自启的标准途径，需要补齐平台对等能力。

## What Changes

- **Windows 落地 `service install`**：通过 SCM 注册 `xkeeper` 为 Windows 服务 —— `SERVICE_AUTO_START`（开机自启）+ `SERVICE_CONFIG_FAILURE_ACTIONS`（首次失败 3 秒后重启，对齐 Linux `Restart=always` / `RestartSec=3`）。
- **`xkeeper run` 增加 Windows 服务入口**：经 `windows-service` crate 的 dispatcher 进入 service_main，注册控制处理器；`SERVICE_CONTROL_STOP` 映射到与 SIGTERM/Ctrl+C 相同的优雅停机路径；按配置解析的最长停止等待上报 wait hint。
- **Windows 落地 `service uninstall`**：停止（失败仅 warn）并删除服务，幂等（不存在时成功提示未安装）。
- **CLI 新增 flag**：`--name <服务名>`（仅 Windows，缺省 `xkeeper`）、`--desc <文本>`（跨平台；Windows 写 SCM 服务描述，Linux 写 unit 的 `Description=`，缺省均为 `xkeeper process keeper`）。`--unit-file` 维持仅 Linux；跨平台误用对方 flag 时明确报错。`--force` / `--now` 跨平台保留。
- **`--user` 的 Windows 语义**：接受免密内置账号 `LocalSystem` / `LocalService` / `NetworkService`，其他账号名报错，缺省 `LocalSystem`。
- **已存在语义对齐 Linux `decide_write`**：读回 SCM 配置快照（binpath / start_type / failure actions / description）比对 —— 一致则幂等成功（`--now` 仍启动），不一致则拒绝并提示 `--force`，`--force` 修正配置（服务运行中提示下次启动生效）。
- **权限**：install / uninstall 需管理员权限（非管理员失败、零 SCM 变更，提示用管理员运行），对齐 Linux root 检查。
- **移除** spec 中「Windows 平台明确不支持」的失败要求。
- **e2e**：xtask 增加 Windows 服务 roundtrip 段（install → 读回 SCM 验证 → `--now` 起服务 → uninstall 验证注销；含 `--name` / `--desc` 场景），非管理员环境自动跳过并 warn。

## Capabilities

### New Capabilities

（无 —— 能力并入既有 service-registration 规范）

### Modified Capabilities

- `service-registration`: Windows 从「明确不支持」改为完整的 SCM 注册/卸载能力（自启、拉起、幂等、--force、权限、--name、--desc、--user 内置账号）；Linux 侧 unit 渲染支持 `--desc` 覆盖 Description。

## Impact

- **代码**：`src/service.rs`（Windows 分支实现 + 纯逻辑扩展）、`src/main.rs`（CLI flag、dispatch、Windows 服务分发入口）、`Cargo.toml`（新增 `windows-service` 依赖）。
- **测试**：`src/service.rs` 单元测试（binpath 渲染、存在性决策、内置账号/服务名校验）、`xtask/src/e2e.rs` Windows 服务段。
- **不影响**：Linux 路径行为、控制面 API、webui、bench。
