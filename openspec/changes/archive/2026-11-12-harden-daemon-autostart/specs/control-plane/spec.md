# control-plane Specification Delta

## ADDED Requirements

### Requirement: 服务托管时的停机命令语义

当守护进程由系统服务管理器托管（见 `service-registration` 的「服务托管识别」）时：

- `POST /v1/shutdown` SHALL 返回 409，MUST NOT 停止任何程序、MUST NOT 让守护进程退出；响应 SHALL 说明守护进程由服务管理器托管，并给出对应的停机命令（Linux `systemctl stop <unit>`，Windows `sc stop <name>` 或服务管理器）。
- `ak daemon shutdown` SHALL 以非零退出码呈现该拒绝与提示，且 MUST NOT 产生副作用。

Linux 的 `<unit>` SHALL 取自守护进程自身的 cgroup 归属（`/proc/self/cgroup` 中的 unit 名）；无法判定时 SHALL 以「当前 unit」表述并仍给出 `systemctl stop` 形式。非托管形态（`ak daemon start` 或 `ak daemon start --foreground` 手工启动）下 `POST /v1/shutdown` 与 `ak daemon shutdown` 行为不变：停止全部程序后退出。

#### Scenario: systemd 托管下 shutdown 被拒绝

- **WHEN** 守护进程由 systemd 拉起运行，发送 `POST /v1/shutdown` 或执行 `ak daemon shutdown`
- **THEN** 请求返回 409（CLI 非零退出），消息包含 `systemctl stop <unit>`；程序状态与守护进程均不受影响

#### Scenario: Windows 服务托管下 shutdown 被拒绝

- **WHEN** 守护进程由 SCM 以 `--service` 模式拉起，发送 `POST /v1/shutdown`
- **THEN** 返回 409，消息包含 `sc stop <name>`；服务继续运行

#### Scenario: 手工启动 shutdown 正常退出

- **WHEN** 用户手工执行 `ak daemon start` 后执行 `ak daemon shutdown`
- **THEN** 守护进程停止全部程序后退出，退出码为 0

### Requirement: 状态投影暴露服务托管来源

`GET /v1/status` 的守护进程信息 SHALL 新增可空字段 `managed_by`，取值为 `systemd`、`scm` 或 `null`（非托管）。既有字段名与语义 MUST NOT 改变，忽略该字段的既有消费者 SHALL 不受影响。

#### Scenario: 托管时字段取值

- **WHEN** 守护进程由 systemd 拉起，请求 `GET /v1/status`
- **THEN** 响应中的 `managed_by` 为 `systemd`（SCM 拉起时为 `scm`）

#### Scenario: 手工启动为 null

- **WHEN** 守护进程由用户手工启动，请求 `GET /v1/status`
- **THEN** 响应中的 `managed_by` 为 `null`，其余字段与既有投影一致
