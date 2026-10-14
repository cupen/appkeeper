## MODIFIED Requirements

### Requirement: service install 生成并注册 systemd unit

`ak service install` SHALL 在 `/etc/systemd/system/ak.service` 生成 unit 文件，并执行 `systemctl daemon-reload` 与 `systemctl enable`，使服务开机自启。unit 文件 MUST 满足：

- `ExecStart` 为当前 appkeeper 可执行文件的绝对路径，后接 `daemon --config <daemon 配置文件绝对路径>`（daemon 配置路径按 `-c/--config` 解析后的路径；未指定时用平台默认路径）。
- `Restart=` 策略与 `RestartSec`，崩溃退出后由 systemd 重新拉起。`Restart=` 缺省 `always`，可用 `--restart <POLICY>` 覆盖，取值为 systemd 的 restart 策略（`no`/`on-success`/`on-failure`/`on-abnormal`/`on-watchdog`/`on-abort`/`always`），并在 `--help` 中列出全部可取值。
- 优雅停机走 SIGTERM（与 appkeeper 自身信号处理一致），并配置 `TimeoutStopSec` 不小于配置解析后的最长停止等待。
- 声明 `RuntimeDirectory=appkeeper` 使控制 socket 的父目录 `/run/appkeeper` 由 systemd 在服务启动时创建（并随服务停止清理），MUST NOT 依赖该目录事先存在。

控制 socket 的默认路径 `/run/appkeeper/<uid>/ak.sock` 按运行账户 uid 分层：unit 以 `User=<name>` 非 root 运行时，守护进程 SHALL 在 `RuntimeDirectory` 之下创建该 uid 的 `0700` 子目录；systemd SHALL NOT 以 `ProtectSystem=strict` 等方式使 `/run/appkeeper` 不可写（生成 unit 时 MUST NOT 引入阻断 socket 创建的沙箱指令）。

升级兼容：既有 unit 的 `ExecStart` 若仍是旧入口形态（`run --config`），在新二进制下 systemd 拉起 SHALL 被守护形态拦截逻辑拒绝（退出码 2）；拦截错误信息 SHALL 提示以 `ak service install --force --now` 重新生成 unit。`ak service install` 自身的 `--force` 覆盖语义保持不变。

既有 unit 未含 `RuntimeDirectory` 时，服务 SHALL 仍能启动：守护进程在缺失时自建 `/run/appkeeper`（`0755`）与 uid 子目录（`0700`）。

#### Scenario: 安装成功并开机自启

- **WHEN** 用户以 root 执行 `ak service install`
- **THEN** `/etc/systemd/system/ak.service` 存在且 ExecStart 指向当前可执行文件与解析后的 daemon 配置路径（`daemon --config <路径>` 形态），`systemctl is-enabled appkeeper` 返回 enabled

#### Scenario: --start 立即启动

- **WHEN** 用户执行 `ak service install --start`（`--now` 为等价别名）
- **THEN** 注册完成后执行 `systemctl start appkeeper`，服务进入 running 状态

#### Scenario: --restart 自定义 systemd 重启策略

- **WHEN** 用户执行 `ak service install --restart on-failure`
- **THEN** 生成的 unit 含 `Restart=on-failure`；未指定 `--restart` 时 unit 含 `Restart=always`

#### Scenario: 旧 unit 升级后拉起失败并给自救提示

- **WHEN** 既有 unit 的 ExecStart 为旧入口形态 `run --config <路径>`，二进制升级为移除守护形态 `run` 的版本后 systemd 拉起服务
- **THEN** 服务启动失败（拦截退出码 2），错误信息提示改用 `ak daemon` 并以 `ak service install --force --now` 重新生成 unit

#### Scenario: unit 声明 RuntimeDirectory

- **WHEN** 用户执行 `ak service install` 后查看生成的 unit
- **THEN** unit 含 `RuntimeDirectory=appkeeper`；服务启动后 `/run/appkeeper` 存在且服务停止后被清理

#### Scenario: 非 root unit 仍可创建 socket

- **WHEN** 以 `User=akuser` 运行服务，守护进程按 uid 分层创建控制 socket
- **THEN** socket 创建于 `/run/appkeeper/<akuser 的 uid>/ak.sock`，目录权限为 `0700`，服务启动成功

#### Scenario: 缺 RuntimeDirectory 的既有 unit 仍可启动

- **WHEN** 既有 unit 未含 `RuntimeDirectory` 且 `/run/appkeeper` 不存在时启动服务
- **THEN** 守护进程自建该目录及 uid 子目录后成功启动并伺服控制 socket
