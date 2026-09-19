# service-registration Specification Delta

## REMOVED Requirements

### Requirement: Windows 平台明确不支持

**Reason**: Windows 服务注册已由本变更完整实现（SCM 注册/卸载/开机自启/崩溃拉起），「执行即失败」的语义被下列 Windows 支持需求取代。

**Migration**: 无需迁移。此前在 Windows 上失败的两条命令现在直接可用：`xkeeper service install`（注册开机自启）与 `xkeeper service uninstall`（对称注销）。

## ADDED Requirements

### Requirement: Windows service install 注册 SCM 服务（开机自启 + 崩溃拉起）

`xkeeper service install` 在 Windows 上 SHALL 通过服务控制管理器（SCM）注册 Windows 服务，且 MUST 满足：

- 服务命令行（binpath）为当前 xkeeper 可执行文件的绝对路径，后接 `run --config <daemon 配置文件绝对路径>`（与 Linux unit 的 ExecStart 同规则）。
- 启动类型为自动（开机自启）。
- 配置失败动作（failure actions）：服务异常退出后 3 秒自动重启，对齐 Linux unit 的 `Restart=always` / `RestartSec=3`（仅对异常退出触发，服务正常停机不重启）。
- 服务描述缺省为 `xkeeper process keeper`。

#### Scenario: 安装成功并开机自启

- **WHEN** 用户以管理员执行 `xkeeper service install`
- **THEN** SCM 中存在该服务，binpath 指向当前可执行文件与解析后的 daemon 配置绝对路径，启动类型为自动，且配置了「失败 3 秒后重启」的动作

#### Scenario: --now 立即启动

- **WHEN** 用户执行 `xkeeper service install --now`
- **THEN** 注册完成后服务立即启动，SCM 报告其进入运行状态

### Requirement: 以服务方式运行的 xkeeper 响应 SCM 停机控制

在 Windows 上由 SCM 启动的 `xkeeper run` SHALL 经服务控制调度器上报运行状态；收到 `SERVICE_CONTROL_STOP` 时 MUST 走与 SIGTERM / Ctrl+C 相同的优雅停机路径（按各程序 `stop_timeout` 等待子进程退出），停机过程中按剩余等待上报 wait hint，最终上报已停止状态。

#### Scenario: 停止服务触发优雅停机

- **WHEN** 服务运行中收到停止控制（`service uninstall` 或服务管理器手动停止）
- **THEN** xkeeper 等待子进程按 `stop_timeout` 优雅退出后自身停止，SCM 状态流转为已停止，进程树不被强杀

### Requirement: --name 设置 Windows 服务名

`service install` 与 `service uninstall` SHALL 支持 `--name <服务名>`（仅 Windows 生效，缺省 `xkeeper`）。服务名 MUST 非空且不含 `/` 与 `\`；非法名称 MUST 在任何 SCM 变更前失败报错。Linux 上传递 `--name`、Windows 上传递 `--unit-file` 时，MUST 以明确报错拒绝且不产生任何副作用。

#### Scenario: 自定义服务名安装与卸载

- **WHEN** 用户执行 `xkeeper service install --name xk-prod` 随后 `xkeeper service uninstall --name xk-prod`
- **THEN** 安装注册名为 xk-prod 的服务，卸载删除该服务且不影响其他服务

#### Scenario: 非法服务名

- **WHEN** `--name` 为空或包含 `/` / `\`
- **THEN** 安装在产生任何 SCM 变更前失败并提示非法名称

#### Scenario: 跨平台误用平台专属 flag

- **WHEN** 在 Linux 上执行 `service install --name xk`，或在 Windows 上执行 `service install --unit-file x.service`
- **THEN** 命令失败并提示该 flag 仅支持另一平台，无文件写入、无 SCM 变更

### Requirement: --desc 跨平台设置服务描述

`service install` SHALL 支持 `--desc <文本>`（缺省 `xkeeper process keeper`）：Windows 上写入 SCM 服务描述，Linux 上写入 unit 文件的 `Description=`。

#### Scenario: 自定义描述

- **WHEN** 用户执行 `xkeeper service install --desc "prod keeper"`
- **THEN** Windows 上该服务的 SCM 描述为 prod keeper；Linux 上 unit 文件包含 `Description=prod keeper`

#### Scenario: 缺省描述

- **WHEN** 用户执行 `xkeeper service install` 未带 `--desc`
- **THEN** 服务描述为 `xkeeper process keeper`（与变更前行为一致）

### Requirement: Windows 安装需要管理员权限

`service install` 与 `service uninstall` 在 Windows 上 MUST 要求管理员权限（elevated）；非管理员执行时 MUST 以非零退出码失败、提示需要管理员运行，且 MUST NOT 产生任何 SCM 变更。

#### Scenario: 非管理员执行安装

- **WHEN** 非管理员用户执行 `xkeeper service install`
- **THEN** 命令失败并提示需要管理员权限，SCM 中不出现该服务

### Requirement: 同名 Windows 服务已存在时幂等或拒绝覆盖

当 `--name` 指定的服务已存在时，`service install` MUST 读回其配置快照（binpath、启动类型、失败动作、服务描述、运行账号）并与目标比对：完全一致则幂等成功（不改写配置，提示已是最新，`--now` 仍启动服务）；不一致则默认拒绝并提示 `--force`；`--force` 时修正配置，服务运行中 MUST 提示更改将于下次启动生效。

#### Scenario: 重复安装

- **WHEN** 同名服务已存在且配置快照一致时再次执行 `xkeeper service install`
- **THEN** 命令成功退出且不改写任何配置

#### Scenario: 配置漂移拒绝与 --force 修正

- **WHEN** 同名服务已存在但快照字段不同且未加 `--force`
- **THEN** 安装失败并提示使用 `--force`；加 `--force` 后配置被修正为目标值，若服务运行中则提示下次启动生效

### Requirement: Windows service uninstall 对称注销

`xkeeper service uninstall` 在 Windows 上 SHALL 停止（若正在运行；停止失败仅告警、不阻断删除）并删除 `--name` 指定的服务。服务不存在时 MUST 幂等成功并提示未安装。

#### Scenario: 卸载已安装的服务

- **WHEN** 用户以管理员执行 `xkeeper service uninstall`
- **THEN** 服务先停止再删除，SCM 中不再存在该服务

#### Scenario: 卸载未安装的服务

- **WHEN** `--name` 指定的服务不存在时执行 `xkeeper service uninstall`
- **THEN** 命令成功退出并提示服务未安装

### Requirement: --user 的 Windows 内置账号语义

`--user` 在 Windows 上 SHALL 仅接受免密内置账号 `LocalSystem`、`LocalService`、`NetworkService`（大小写不敏感）；其他账号名 MUST 在安装产生任何 SCM 变更前失败报错。未指定时服务以 `LocalSystem` 运行。

#### Scenario: 内置账号成功

- **WHEN** 用户执行 `xkeeper service install --user NetworkService`
- **THEN** 服务注册为以 NetworkService 账号运行

#### Scenario: 非内置账号报错

- **WHEN** 用户执行 `xkeeper service install --user svc-xk`
- **THEN** 安装失败并提示仅支持 LocalSystem / LocalService / NetworkService，无 SCM 变更
