# service-registration Specification Delta

## MODIFIED Requirements

### Requirement: daemon install 生成并注册 systemd unit

`ak daemon install` SHALL 在 `/etc/systemd/system/ak.service` 生成 unit 文件，并执行 `systemctl daemon-reload` 与 `systemctl enable`，使服务开机自启。unit 文件 MUST 满足：

- `ExecStart` 为当前 appkeeper 可执行文件的绝对路径，后接 `daemon start --foreground --config <daemon 配置文件绝对路径>`（daemon 配置路径按 `-c/--config` 解析后的路径；未指定时用平台默认路径）。
- `Restart=` 策略与 `RestartSec`，崩溃退出后由 systemd 重新拉起。`Restart=` 缺省 `always`，可用 `--restart <POLICY>` 覆盖，取值为 systemd 的 restart 策略（`no`/`on-success`/`on-failure`/`on-abnormal`/`on-watchdog`/`on-abort`/`always`），并在 `--help` 中列出全部可取值。
- 优雅停机走 SIGTERM（与 appkeeper 自身信号处理一致），并配置 `TimeoutStopSec` 不小于配置解析后的最长停止等待。
- `KillMode=mixed`：停服时 SIGTERM 只投递给服务主进程（daemon），由其按逆序、按各程序 `stop_timeout` 优雅停止；`TimeoutStopSec` 到期后 systemd 才 SIGKILL 整个 cgroup 兜底。生成 unit MUST NOT 省略该键——systemd 缺省 `control-group` 会把 SIGTERM 同时投给 cgroup 内全部受管子进程，与 daemon 的逆序优雅停止赛跑。
- `RuntimeDirectory=appkeeper/<uid>` 与 `RuntimeDirectoryMode=0700`：`<uid>` 是安装期解析出的服务运行账号数字 uid（`--user` 指定的账号按 passwd 数据库解析；未指定 `--user` 时为 root 的 `0`；解析失败回退 `0` 并告警），以**字面量**写入 unit。MUST NOT 用 `%U` 占位——systemd.unit(5) 明确 system manager 下 `%U` 恒展开为 `0` 且不受 `User=` 影响，照抄会让所有账号共用 root 的 `appkeeper/0` 叶子。由 systemd 创建本实例运行 uid 的目录 `/run/appkeeper/<uid>`，服务停止时只清理该目录。unit MUST NOT 把共享根 `appkeeper` 声明为 `RuntimeDirectory`——systemd 停服会递归删除它，连带删掉同机其他用户的 `/run/appkeeper/<uid>` 目录。
- `StateDirectory=appkeeper/<uid>` 与 `StateDirectoryMode=0750`：持久状态目录 `/var/lib/appkeeper/<uid>`（`<uid>` 语义同上，字面量），由 systemd 按 `User=` 创建并授权，供守护进程存放「用户停止」记录等持久状态（见 `configuration` 的 `state_dir` 与 `process-management` 的重启策略）；该目录 MUST 跨重启保留、停服不删，`daemon uninstall` MUST NOT 删除它。
- `Environment=PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin`：服务形态 MUST 有确定性的 PATH，不依赖发行版与 systemd 版本的缺省值。服务形态 MUST NOT 假定继承登录 shell 的环境（`HOME` 等变量需由 app 配置显式声明）。
- `Wants=network-online.target` 与 `After=network.target network-online.target`：开机启动 SHALL 等待网络就绪目标，降低依赖网络的程序在开机早期被判失败的窗口。
- `[Unit]` 包含 `StartLimitIntervalSec=60` 与 `StartLimitBurst=5`，显式声明启动失败上限，使反复启动失败的实例进入 failed 状态而不是无限重启。
- 声明运行 uid 层级的 runtime 目录（`appkeeper/<uid>` 字面量）使控制 socket 的父目录由 systemd 在服务启动时创建（并随服务停止清理），MUST NOT 依赖该目录事先存在。

控制 socket 的默认路径 `/run/appkeeper/<uid>/ak.sock` 按运行账户 uid 分层：unit 以 `User=<name>` 非 root 运行时，守护进程 SHALL 在 `RuntimeDirectory` 之下创建该 uid 的 `0700` 子目录；systemd SHALL NOT 以 `ProtectSystem=strict` 等方式使 `/run/appkeeper` 不可写（生成 unit 时 MUST NOT 引入阻断 socket 创建的沙箱指令）。

升级兼容：既有 unit 的 `ExecStart` 若为旧入口形态——`run --config`（守护形态拦截，退出码 2）或裸 `daemon --config`（本变更后裸 `ak daemon` 为 usage 错误，退出码 2）——在新二进制下 systemd 拉起 SHALL 失败；两种形态的报错信息均 SHALL 提示以 `ak daemon install --force --now` 重新生成 unit。既有 unit 缺少本变更新增的硬化键（`KillMode` / `Environment` / `network-online` / `StartLimit*`）时 SHALL 继续可运行，但 SHALL NOT 获得本要求声明的硬化语义；`ak daemon install` 自身的 `--force` 覆盖语义保持不变。

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
- **THEN** unit 含 `RuntimeDirectory=appkeeper/<uid>` 与 `RuntimeDirectoryMode=0700`（`<uid>` 为安装期解析的运行账号数字 uid，root 缺省 `0`）；服务启动后 `/run/appkeeper/<uid>` 存在，服务停止后该 uid 目录被清理

#### Scenario: 停服不误删其他用户的 runtime 目录

- **WHEN** 同机 root 服务与 uid 1000 的守护进程同时存在，以 root 执行 `systemctl stop appkeeper`
- **THEN** `/run/appkeeper/0` 被清理，`/run/appkeeper/1000` 与其 socket 不受影响

#### Scenario: unit 声明 StateDirectory

- **WHEN** 用户执行 `ak daemon install` 后查看生成的 unit
- **THEN** unit 含 `StateDirectory=appkeeper/<uid>` 与 `StateDirectoryMode=0750`（`<uid>` 为安装期解析的运行账号数字 uid，root 缺省 `0`）；服务启动后 `/var/lib/appkeeper/<uid>` 存在，`systemctl stop` 后该目录与其内容仍保留

#### Scenario: 卸载服务保留状态目录

- **WHEN** 服务安装并运行过（状态目录中含「用户停止」记录），用户执行 `ak daemon uninstall`
- **THEN** unit 被删除，`/var/lib/appkeeper/<uid>` 及其内容不被删除

#### Scenario: 停服先走 daemon 自己的逆序优雅停止

- **WHEN** 服务运行中执行 `systemctl stop appkeeper`，受管程序 A、B 的 `stop_timeout` 尚未到期
- **THEN** SIGTERM 只到达 daemon 主进程，daemon 按启动逆序停止 A、B；子进程在超时前不会被 systemd 直接 SIGKILL

#### Scenario: 服务形态 PATH 确定

- **WHEN** 用户执行 `ak daemon install` 后查看生成的 unit
- **THEN** unit 含 `Environment=PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin`

#### Scenario: 开机等待网络就绪

- **WHEN** 用户执行 `ak daemon install` 后查看生成的 unit
- **THEN** `[Unit]` 含 `Wants=network-online.target` 与 `After=network.target network-online.target`

#### Scenario: 启动失败上限显式声明

- **WHEN** 用户执行 `ak daemon install` 后查看生成的 unit
- **THEN** `[Unit]` 含 `StartLimitIntervalSec=60` 与 `StartLimitBurst=5`

#### Scenario: 非 root unit 仍可创建 socket

- **WHEN** 以 `User=akuser` 运行服务，守护进程按 uid 分层创建控制 socket
- **THEN** socket 创建于 `/run/appkeeper/<akuser 的 uid>/ak.sock`，目录权限为 `0700`，服务启动成功

#### Scenario: 缺 RuntimeDirectory 的既有 unit 仍可启动

- **WHEN** 既有 unit 未含 `RuntimeDirectory` 且 `/run/appkeeper` 不存在时启动服务
- **THEN** 守护进程自建该目录及 uid 子目录后成功启动并伺服控制 socket

## ADDED Requirements

### Requirement: daemon install 安装期预检

`ak daemon install` SHALL 在任何注册副作用（写 unit / SCM 变更）之前完成预检，且预检失败 MUST NOT 产生任何文件或服务状态变更：

- daemon 配置文件不存在或校验失败：MUST 以非零退出码拒绝安装，错误信息 SHALL 指明配置路径与补救方式（`ak config --init` 或 `-c/--config`）。
- 注册表中存在不可解析的 app：SHALL 逐条告警（app 名与原因）但 MUST NOT 阻断安装——守护进程启动时会跳过这些 app。
- 解析后的 `log_dir` 位于 `/tmp`（Windows `%TEMP%`）：SHALL 告警，说明日志将随重启丢失，并建议改为持久目录（如 `/var/log/appkeeper`）。
- 生效 `state_dir` 位于 `/tmp`（Windows `%TEMP%`）：SHALL 告警，说明「用户停止」记录将随重启丢失，并给出修复（显式配置持久 `state_dir`，或让服务通过 unit 的 `StateDirectory` 获得状态目录）。
- Windows 上生效 `socket` 为缺省 IPC 管道、且其用户名与 `--user` 指定的服务运行账号不一致：SHALL 告警，并提示在 daemon 配置中显式声明 `socket`（固定管道名或 `tcp://` 端点），否则服务运行后命令行客户端无法连上控制面。

预检 MUST NOT 改变 `--force` 与「同名 unit / 服务已存在」的既有决策语义。

#### Scenario: 配置缺失拒绝安装

- **WHEN** 默认配置路径不存在时以 root 执行 `ak daemon install`
- **THEN** 命令以非零退出码失败，错误信息包含配置路径与 `ak config --init` 提示，unit 文件不被创建

#### Scenario: 配置校验失败拒绝安装

- **WHEN** daemon 配置含非法键（如残留 `host`）时执行 `ak daemon install`
- **THEN** 命令以非零退出码失败并复述校验错误，unit 文件不被创建

#### Scenario: 损坏 app 仅告警

- **WHEN** 注册表中有不可解析的 app 时执行 `ak daemon install`（配置本身合法）
- **THEN** 安装继续完成，stderr 列出该 app 名与解析原因

#### Scenario: 临时日志目录告警

- **WHEN** 生效 `log_dir` 位于 `/tmp` 时执行 `ak daemon install`
- **THEN** 安装完成，stderr 告警说明日志将随重启丢失，并给出持久目录建议

#### Scenario: 临时状态目录告警

- **WHEN** 生效 `state_dir` 位于 `/tmp`（如配置显式指向临时目录，或非服务形态推导到临时位置）时执行 `ak daemon install`
- **THEN** 安装完成，stderr 告警说明「用户停止」记录将随重启丢失，并提示显式配置持久 `state_dir` 或依赖 unit 的 `StateDirectory`

#### Scenario: Windows 服务账号与管道名不匹配告警

- **WHEN** 在 Windows 上以管理员执行 `ak daemon install`，配置未显式声明 `socket`，服务默认以 LocalSystem 运行
- **THEN** 安装完成，stderr 告警说明缺省管道名按安装者用户名推导、与服务账号不一致，并提示显式声明 `socket`

### Requirement: 服务托管识别

守护进程 SHALL 在启动时判定自身是否由系统服务管理器托管，并把结果保存在进程内供状态投影与停机命令使用：Linux 以进程环境中存在 `INVOCATION_ID`（systemd 每次启动注入）为判据；Windows 以 `daemon start --service <NAME>` 运行模式为判据。判定 MUST NOT 改变既有的启动、拉起、停机和日志行为。

#### Scenario: systemd 托管被识别

- **WHEN** systemd 通过生成的 unit 拉起 `ak daemon start --foreground`
- **THEN** 守护进程把托管来源判定为 `systemd`

#### Scenario: 手工启动不被识别为托管

- **WHEN** 用户在普通终端执行 `ak daemon start` 或 `ak daemon start --foreground`
- **THEN** 守护进程的托管来源为空

#### Scenario: Windows 服务模式被识别

- **WHEN** SCM 以 `daemon start --service <NAME>` 拉起守护进程
- **THEN** 守护进程把托管来源判定为 `scm`
