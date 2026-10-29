# service-registration Specification

## Purpose

让用户用一条命令把 appkeeper 自身注册为系统服务（Linux systemd unit / Windows SCM 服务，开机自启、崩溃自动拉起），并提供对称的卸载命令与自定义服务名、描述、运行账号。

## Requirements

### Requirement: daemon install 生成并注册 systemd unit

`ak daemon install` SHALL 在 `/etc/systemd/system/ak.service` 生成 unit 文件，并执行 `systemctl daemon-reload` 与 `systemctl enable`，使服务开机自启。unit 文件 MUST 满足：

- `ExecStart` 为当前 appkeeper 可执行文件的绝对路径，后接 `daemon start --foreground --config <daemon 配置文件绝对路径>`（daemon 配置路径按 `-c/--config` 解析后的路径；未指定时用平台默认路径）。
- `Restart=` 策略与 `RestartSec`，崩溃退出后由 systemd 重新拉起。`Restart=` 缺省 `always`，可用 `--restart <POLICY>` 覆盖，取值为 systemd 的 restart 策略（`no`/`on-success`/`on-failure`/`on-abnormal`/`on-watchdog`/`on-abort`/`always`），并在 `--help` 中列出全部可取值。
- 优雅停机走 SIGTERM（与 appkeeper 自身信号处理一致），并配置 `TimeoutStopSec` 不小于配置解析后的最长停止等待。
- 声明 `RuntimeDirectory=appkeeper` 使控制 socket 的父目录 `/run/appkeeper` 由 systemd 在服务启动时创建（并随服务停止清理），MUST NOT 依赖该目录事先存在。

控制 socket 的默认路径 `/run/appkeeper/<uid>/ak.sock` 按运行账户 uid 分层：unit 以 `User=<name>` 非 root 运行时，守护进程 SHALL 在 `RuntimeDirectory` 之下创建该 uid 的 `0700` 子目录；systemd SHALL NOT 以 `ProtectSystem=strict` 等方式使 `/run/appkeeper` 不可写（生成 unit 时 MUST NOT 引入阻断 socket 创建的沙箱指令）。

升级兼容：既有 unit 的 `ExecStart` 若为旧入口形态——`run --config`（守护形态拦截，退出码 2）或裸 `daemon --config`（本变更后裸 `ak daemon` 为 usage 错误，退出码 2）——在新二进制下 systemd 拉起 SHALL 失败；两种形态的报错信息均 SHALL 提示以 `ak daemon install --force --now` 重新生成 unit。`ak daemon install` 自身的 `--force` 覆盖语义保持不变。

既有 unit 未含 `RuntimeDirectory` 时，服务 SHALL 仍能启动：守护进程在缺失时自建 `/run/appkeeper`（`0755`）与 uid 子目录（`0700`）。

#### Scenario: 安装成功并开机自启

- **WHEN** 用户以 root 执行 `ak daemon install`
- **THEN** `/etc/systemd/system/ak.service` 存在且 ExecStart 指向当前可执行文件与解析后的 daemon 配置路径（`daemon start --foreground --config <路径>` 形态），`systemctl is-enabled appkeeper` 返回 enabled

#### Scenario: --start 立即启动

- **WHEN** 用户执行 `ak daemon install --start`（`--now` 为等价别名）
- **THEN** 注册完成后执行 `systemctl start appkeeper`，服务进入 running 状态

#### Scenario: --restart 自定义 systemd 重启策略

- **WHEN** 用户执行 `ak daemon install --restart on-failure`
- **THEN** 生成的 unit 含 `Restart=on-failure`；未指定 `--restart` 时 unit 含 `Restart=always`

#### Scenario: 旧 unit 升级后拉起失败并给自救提示

- **WHEN** 既有 unit 的 ExecStart 为旧入口形态 `run --config <路径>` 或 `daemon --config <路径>`，二进制升级为本变更后的版本后 systemd 拉起服务
- **THEN** 服务启动失败（退出码 2），错误信息提示以 `ak daemon install --force --now` 重新生成 unit

#### Scenario: unit 声明 RuntimeDirectory

- **WHEN** 用户执行 `ak daemon install` 后查看生成的 unit
- **THEN** unit 含 `RuntimeDirectory=appkeeper`；服务启动后 `/run/appkeeper` 存在且服务停止后被清理

#### Scenario: 非 root unit 仍可创建 socket

- **WHEN** 以 `User=akuser` 运行服务，守护进程按 uid 分层创建控制 socket
- **THEN** socket 创建于 `/run/appkeeper/<akuser 的 uid>/ak.sock`，目录权限为 `0700`，服务启动成功

#### Scenario: 缺 RuntimeDirectory 的既有 unit 仍可启动

- **WHEN** 既有 unit 未含 `RuntimeDirectory` 且 `/run/appkeeper` 不存在时启动服务
- **THEN** 守护进程自建该目录及 uid 子目录后成功启动并伺服控制 socket
### Requirement: 安装需要 root 权限

`daemon install` 与 `daemon uninstall` MUST 在非 root（有效 uid 非 0）时失败，错误信息说明需要 root，且 MUST NOT 产生任何文件或 systemd 状态变更。

#### Scenario: 非 root 执行安装

- **WHEN** 非 root 用户执行 `ak daemon install`
- **THEN** 命令以非零退出码失败并提示需要 root，unit 文件不被创建
### Requirement: 同名 unit 已存在时拒绝覆盖

当目标 unit 文件已存在且内容与将生成的不同时，`daemon install` MUST 默认拒绝并提示使用 `--force`；内容完全相同时 MUST 幂等成功（不重复报错）。

#### Scenario: 重复安装

- **WHEN** unit 文件已存在且再次执行 `ak daemon install`
- **THEN** 若内容一致则成功且无副作用；若内容不同则失败并提示 `--force`

#### Scenario: --force 覆盖

- **WHEN** 用户执行 `ak daemon install --force` 且 unit 已存在
- **THEN** unit 文件被覆盖并重新 `daemon-reload` + `enable`
### Requirement: daemon uninstall 对称注销

`ak daemon uninstall` SHALL 停止（若在运行）、禁用服务，删除 unit 文件并执行 `daemon-reload`。unit 文件不存在时 MUST 幂等成功并提示未安装。

#### Scenario: 卸载已安装的服务

- **WHEN** 用户以 root 执行 `ak daemon uninstall`
- **THEN** 服务被 stop + disable，unit 文件被删除，`systemctl is-enabled appkeeper` 失败

#### Scenario: 卸载未安装的服务

- **WHEN** unit 文件不存在时执行 `ak daemon uninstall`
- **THEN** 命令成功退出并提示服务未安装
### Requirement: 自定义 unit 名与运行用户

`daemon install` SHALL 支持 `--name <unit>`（默认 `appkeeper`，仅限合法 systemd unit 名字符）与 `--user <name>`（unit 中 `User=` 字段，影响子进程运行用户），卸载按同名 unit 反向清理。

#### Scenario: 自定义 unit 名安装与卸载

- **WHEN** 用户执行 `ak daemon install --name ak-prod` 随后 `ak daemon uninstall --name ak-prod`
- **THEN** 安装生成 `/etc/systemd/system/ak-prod.service`，卸载删除该文件且不影响其他 unit

#### Scenario: 非法 unit 名

- **WHEN** `--name` 包含非法字符（如 `/` 或空白）
- **THEN** 安装在写任何文件前失败并提示非法名称

### Requirement: Windows daemon install 注册 SCM 服务（开机自启 + 崩溃拉起）

`ak daemon install` 在 Windows 上 SHALL 通过服务控制管理器（SCM）注册 Windows 服务，且 MUST 满足：

- 服务命令行（binpath）为当前 appkeeper 可执行文件的绝对路径（含空格时整段加双引号），后接 `daemon start --service <NAME> --config <daemon 配置文件绝对路径>`（配置路径同样按 SCM 规则整段加双引号；`--service` 是安装器生成的运行模式标记，见 `background-daemon`）。
- 启动类型为自动（开机自启）。
- 配置失败动作（failure actions）：服务异常退出后 3 秒自动重启，对齐 Linux unit 的 `Restart=always` / `RestartSec=3`（仅对异常退出触发，服务正常停机不重启）。
- 服务描述缺省为 `appkeeper process keeper`。

#### Scenario: 安装成功并开机自启

- **WHEN** 用户以管理员执行 `ak daemon install`
- **THEN** SCM 中存在该服务，binpath 指向当前可执行文件与解析后的 daemon 配置绝对路径（`daemon start --service <NAME> --config <路径>` 形态），启动类型为自动，且配置了「失败 3 秒后重启」的动作

#### Scenario: --start 立即启动

- **WHEN** 用户执行 `ak daemon install --start`（`--now` 为等价别名）
- **THEN** 注册完成后服务立即启动，SCM 报告其进入运行状态
### Requirement: 以服务方式运行的 daemon 响应 SCM 停机控制

在 Windows 上由 SCM 启动的 `ak daemon start --service <NAME>` SHALL 经服务控制调度器上报运行状态；收到 `SERVICE_CONTROL_STOP` 时 MUST 走与 SIGTERM / Ctrl+C 相同的优雅停机路径（按各程序 `stop_timeout` 等待子进程退出），停机过程中按剩余等待上报 wait hint，最终上报已停止状态。

#### Scenario: 停止服务触发优雅停机

- **WHEN** 服务运行中收到停止控制（`ak daemon uninstall` 或服务管理器手动停止）
- **THEN** appkeeper 等待子进程按 `stop_timeout` 优雅退出后自身停止，SCM 状态流转为已停止，进程树不被强杀
### Requirement: --name 设置 Windows 服务名

`daemon install` 与 `daemon uninstall` SHALL 支持 `--name <服务名>`（仅 Windows 生效，缺省 `ak`）。服务名 MUST 非空且不含 `/` 与 `\`；非法名称 MUST 在任何 SCM 变更前失败报错。Linux 上传递 `--name`、Windows 上传递 `--unit-file` 时，MUST 以明确报错拒绝且不产生任何副作用。

#### Scenario: 自定义服务名安装与卸载

- **WHEN** 用户执行 `ak daemon install --name ak-prod` 随后 `ak daemon uninstall --name ak-prod`
- **THEN** 安装注册名为 ak-prod 的服务，卸载删除该服务且不影响其他服务

#### Scenario: 非法服务名

- **WHEN** `--name` 为空或包含 `/` / `\`
- **THEN** 安装在产生任何 SCM 变更前失败并提示非法名称

#### Scenario: 跨平台误用平台专属 flag

- **WHEN** 在 Linux 上执行 `ak daemon install --name ak`，或在 Windows 上执行 `ak daemon install --unit-file ak.service`
- **THEN** 命令失败并提示该 flag 仅支持另一平台，无文件写入、无 SCM 变更
### Requirement: --desc 跨平台设置服务描述

`daemon install` SHALL 支持 `--desc <文本>`（缺省 `appkeeper process keeper`）：Windows 上写入 SCM 服务描述，Linux 上写入 unit 文件的 `Description=`。

#### Scenario: 自定义描述

- **WHEN** 用户执行 `ak daemon install --desc "prod keeper"`
- **THEN** Windows 上该服务的 SCM 描述为 prod keeper；Linux 上 unit 文件包含 `Description=prod keeper`

#### Scenario: 缺省描述

- **WHEN** 用户执行 `ak daemon install` 未带 `--desc`
- **THEN** 服务描述为 `appkeeper process keeper`
### Requirement: Windows 安装需要管理员权限

`daemon install` 与 `daemon uninstall` 在 Windows 上 MUST 要求管理员权限（elevated）；非管理员执行时 MUST 以非零退出码失败、提示需要管理员运行，且 MUST NOT 产生任何 SCM 变更。

#### Scenario: 非管理员执行安装

- **WHEN** 非管理员用户执行 `ak daemon install`
- **THEN** 命令失败并提示需要管理员权限，SCM 中不出现该服务
### Requirement: 同名 Windows 服务已存在时幂等或拒绝覆盖

当 `--name` 指定的服务已存在时，`daemon install` MUST 读回其配置快照（binpath、启动类型、失败动作、服务描述、运行账号）并与目标比对：完全一致则幂等成功（不改写配置，提示已是最新，`--start` 仍启动服务）；不一致则默认拒绝并提示 `--force`；`--force` 时修正配置，服务运行中 MUST 提示更改将于下次启动生效。

#### Scenario: 重复安装

- **WHEN** 同名服务已存在且配置快照一致时再次执行 `ak daemon install`
- **THEN** 命令成功退出且不改写任何配置

#### Scenario: 配置漂移拒绝与 --force 修正

- **WHEN** 同名服务已存在但快照字段不同且未加 `--force`
- **THEN** 安装失败并提示使用 `--force`；加 `--force` 后配置被修正为目标值，若服务运行中则提示下次启动生效
### Requirement: Windows daemon uninstall 对称注销

`ak daemon uninstall` 在 Windows 上 SHALL 停止（若正在运行；停止失败仅告警、不阻断删除）并删除 `--name` 指定的服务。服务不存在时 MUST 幂等成功并提示未安装。

#### Scenario: 卸载已安装的服务

- **WHEN** 用户以管理员执行 `ak daemon uninstall`
- **THEN** 服务先停止再删除，SCM 中不再存在该服务

#### Scenario: 卸载未安装的服务

- **WHEN** `--name` 指定的服务不存在时执行 `ak daemon uninstall`
- **THEN** 命令成功退出并提示服务未安装
### Requirement: --user 的 Windows 内置账号语义

`--user` 在 Windows 上 SHALL 仅接受免密内置账号 `LocalSystem`、`LocalService`、`NetworkService`（大小写不敏感）；其他账号名 MUST 在安装产生任何 SCM 变更前失败报错。未指定时服务以 `LocalSystem` 运行。

#### Scenario: 内置账号成功

- **WHEN** 用户执行 `ak daemon install --user NetworkService`
- **THEN** 服务注册为以 NetworkService 账号运行

#### Scenario: 非内置账号报错

- **WHEN** 用户执行 `ak daemon install --user svc-ak`
- **THEN** 安装失败并提示仅支持 LocalSystem / LocalService / NetworkService，无 SCM 变更
