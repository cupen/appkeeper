# configuration Specification Delta

## MODIFIED Requirements

### Requirement: daemon 根配置

daemon 配置全局唯一，SHALL 只包含全局内容：`[daemon]`（控制端点、日志、巡检间隔、`app_dir` 应用注册目录）、可选的 `[app-default]`（全部应用共享的默认值表，字段名与程序字段一致）与可选的 `[webui]`（内嵌控制台段：`listen` 监听地址；段存在即启用控制台，`listen` 缺省 `127.0.0.1:9877`）。daemon 配置 MUST NOT 承载任何应用专属内容：应用条目（如 `[[app]]`）、应用名、配置路径等 SHALL 被校验直接拒绝。`app_dir` 缺省为 daemon 配置文件同级的 `app.d` 目录（生产默认落位 `/etc/appkeeper/app.d`）。`daemon.log_dir` 缺省为 `/tmp/appkeeper/logs`（绝对路径，不随 daemon 配置文件位置漂移）；配置的 `log_dir` MUST 为绝对路径，相对路径 SHALL 被校验拒绝（守护进程拒绝启动）。daemon 配置默认路径 SHALL 为 Linux `/etc/appkeeper/daemon.toml`、Windows `%ProgramData%\appkeeper\daemon.toml`（机器级路径，服务账号可读），并 SHALL 可用 `-c/--config` 覆盖。Windows 上当 `%ProgramData%` 路径不存在而旧默认路径 `%APPDATA%\appkeeper\daemon.toml` 存在时，SHALL 回退读取旧路径以兼容既有安装。daemon 配置文件不存在时守护进程 SHALL 以内置默认值空启动，首次写入时创建。

`daemon.socket` SHALL 为非空字符串，缺省为 `/run/appkeeper/<uid>/ak.sock`（unix）与 `\\.\pipe\<用户名>.sock`（Windows）。该值 URL 形态自识别：`tcp://host:port` 前缀声明 TCP 形态控制端点，其它非空值为本地 IPC 路径（语义见 `control-plane` 能力规范）。`daemon.auth_token` SHALL 为条件键：`socket` 为 `tcp://` 形态时必配（非空字符串），`socket` 为路径形态时 MUST NOT 被设置。`[daemon]` 表 SHALL NOT 包含 `host`、`port` 两个键：出现即 SHALL 被校验拒绝并指明该键已移除（端点统一由 `socket` 的 URL 表达）。

`daemon.state_dir` SHALL 为可选键，声明守护进程持久状态的目录，配置时必须为绝对路径（相对路径 SHALL 被校验拒绝）。未配置时按以下顺序推导生效值：进程环境中的 `STATE_DIRECTORY`（systemd 由 unit 的 `StateDirectory=` 注入）→ 平台 State 缺省：unix 上有效 uid 为 0 时 `/var/lib/appkeeper/<uid>`、非 root 时 `$XDG_STATE_HOME/appkeeper`（未设置则 `~/.local/state/appkeeper`）；Windows 上服务模式（`--service`）为 `%ProgramData%\appkeeper\state`、控制台形态为 `%LOCALAPPDATA%\appkeeper\state`。生效值 SHALL 与 `log_dir` 相互独立：改动 `log_dir` MUST NOT 改变状态的落位与内容。目录无法创建或不可写时，守护进程 SHALL 告警并回退使用生效 `log_dir`，MUST NOT 因此拒绝启动。

#### Scenario: 全新机器空启动

- **WHEN** 在没有任何 daemon 配置文件的机器上执行 `ak daemon start`
- **THEN** 守护进程以默认设置启动，受管程序为空，控制平面正常可用，控制台不伺服

#### Scenario: daemon 配置中的应用条目被拒绝

- **WHEN** daemon 配置中出现 `[[app]]` 等应用专属条目
- **THEN** 校验失败并报未知字段，守护进程拒绝以该配置启动

#### Scenario: 缺省日志目录固定

- **WHEN** daemon 配置未声明 `log_dir`，守护进程拉起任一程序并产生输出
- **THEN** 程序日志写入 `/tmp/appkeeper/logs`，与 daemon 配置文件所在目录无关

#### Scenario: app_dir 缺省名为 app.d

- **WHEN** daemon 配置未声明 `app_dir`，执行 `ak compose ./proc --name abc` 完成注册
- **THEN** 注册记录写入 daemon 配置文件同级的 `app.d/abc.toml`（Linux 生产默认 `/etc/appkeeper/app.d/abc.toml`）

#### Scenario: 显式 app_dir 覆盖默认名

- **WHEN** daemon 配置显式声明 `app_dir = "apps"`
- **THEN** 注册记录写入该 `apps` 目录，缺省名 `app.d` 不生效

#### Scenario: 缺省 socket 路径含 uid 分层

- **WHEN** daemon 配置未声明 `socket`，以 uid 1000 启动守护进程
- **THEN** 控制端点为 `/run/appkeeper/1000/ak.sock`，`/run/appkeeper` 存在且权限为 `0755`

#### Scenario: tcp 形态声明控制端点

- **WHEN** daemon 配置声明 `socket = "tcp://127.0.0.1:7310"` 与非空 `auth_token`
- **THEN** 守护进程以 TCP 形态启动并监听该端点，不创建 socket 文件

#### Scenario: 已移除的键导致校验失败

- **WHEN** daemon 配置中残留 `host` 或 `port` 任一键，执行 `ak validate` 或 `ak daemon start`
- **THEN** 校验失败并指明该键已移除及其替代（端点由 `socket` 的 URL 表达），守护进程拒绝以该配置启动

#### Scenario: IPC 形态下 auth_token 被拒绝

- **WHEN** daemon 配置声明缺省或路径形态的 `socket`，同时设置了 `auth_token`，执行 `ak validate` 或 `ak daemon start`
- **THEN** 校验失败并说明 `auth_token` 仅属于 `tcp://` 形态，守护进程拒绝以该配置启动

#### Scenario: Windows 默认配置路径为机器级

- **WHEN** 在 Windows 上未传 `-c/--config` 执行任何 CLI 命令，且 `%ProgramData%\appkeeper\daemon.toml` 存在
- **THEN** 使用该机器级路径作为 daemon 配置（Linux 仍为 `/etc/appkeeper/daemon.toml`）

#### Scenario: 旧 Windows 配置路径兼容回退

- **WHEN** 在 Windows 上 `%ProgramData%\appkeeper\daemon.toml` 不存在，而 `%APPDATA%\appkeeper\daemon.toml` 存在
- **THEN** 读写该旧路径的配置，命令行为与升级前一致

#### Scenario: state_dir 显式配置优先

- **WHEN** daemon 配置声明 `state_dir = "/srv/appkeeper/state"`，且进程中存在 `STATE_DIRECTORY`
- **THEN** 守护进程的持久状态写入该显式路径，`STATE_DIRECTORY` 被忽略

#### Scenario: state_dir 采用 systemd 注入目录

- **WHEN** daemon 配置未声明 `state_dir`，进程环境含 `STATE_DIRECTORY=/var/lib/appkeeper/0`
- **THEN** 守护进程的持久状态写入 `/var/lib/appkeeper/0`

#### Scenario: state_dir 相对路径被拒绝

- **WHEN** daemon 配置声明 `state_dir = "state"`
- **THEN** 校验失败并说明该键必须为绝对路径，守护进程拒绝以该配置启动

#### Scenario: state_dir 不可写时回退日志目录

- **WHEN** 生效 `state_dir` 无法创建或不可写
- **THEN** 守护进程告警并改用生效 `log_dir` 保存状态，正常启动并继续守护程序

### Requirement: config --set 写入

`ak config --set key=value` SHALL 仅接受 `[daemon]` 表的已知键白名单：`log_level`、`log_dir`、`monitor_interval`、`log_buffer_lines`、`app_dir`、`socket`、`auth_token`、`state_dir`，值按目标字段类型解析（`monitor_interval` 为正浮点、`log_buffer_lines` 为非负整数、`log_level` 限定既有日志级别集合、`log_dir`/`app_dir`/`socket`/`state_dir` 为非空字符串、`auth_token` 为非空字符串）；未知键或类型不符 MUST 拒绝并以非零码退出，不落盘。已移除的 `host`、`port` SHALL 与其它未知键同等对待：以非零码拒绝，并在错误信息中指明该键已移除及其替代（端点由 `socket` 的 URL 表达），MUST NOT 接受写入。写入 SHALL 采用读-改-写：文件中未被本次触碰的键、顺序与注释 SHALL 保持原样；写后 SHALL 对整个配置做整体校验（含端点形态与凭据的组合约束、`state_dir` 绝对路径约束），校验失败 MUST 回滚为原文件内容（磁盘上不留半成品）并以非零码退出。daemon 配置文件不存在时 set MUST 拒绝并提示先执行 `--init`。`--set` SHALL 可重复以一次写入多个键。成功 SHALL 打印写入的键与新值；目标 daemon 在线时 SHALL 提示需 reload 生效。

#### Scenario: 强类型拒绝

- **WHEN** 执行 `ak config --set log_buffer_lines=abc` 或 `ak config --set foo=1`
- **THEN** 非零码退出并说明类型不符/未知键，配置文件内容不变

#### Scenario: 已移除的键拒绝写入

- **WHEN** 执行 `ak config --set port=7311`
- **THEN** 非零码退出并说明 `port` 已移除、端点由 `socket` 表达，配置文件内容不变

#### Scenario: 校验失败回滚

- **WHEN** 对 `socket = "tcp://127.0.0.1:7310"` 的配置执行 `ak config --delete auth_token`
- **THEN** 命令非零退出（整体校验不通过），文件内容保持写入前的原样（含注释）

#### Scenario: 未触碰内容保持

- **WHEN** 对一份带手工注释与自定义键序的合法配置执行 `--set socket=tcp://127.0.0.1:7310`（随后补写 `auth_token` 通过整体校验）
- **THEN** 仅被触碰的键改变，其余键的顺序、值与全部注释保持原样

#### Scenario: 缺失文件拒绝

- **WHEN** daemon 配置文件不存在，执行 `ak config --set log_level=debug`
- **THEN** 非零码退出，提示先执行 `ak config --init`

#### Scenario: state_dir 写入与校验

- **WHEN** 执行 `ak config --set state_dir=/srv/appkeeper/state`，随后执行 `ak config --set state_dir=state`
- **THEN** 第一次写入成功并落盘；第二次因非绝对路径被整体校验拒绝，文件保持第一次写入后的内容

### Requirement: config --get 读取

`ak config --get <key>` SHALL 接受与 set 相同的白名单裸键名，打印该键的生效值：配置文件中显式设置了该键 SHALL 打印文件值，未设置 SHALL 打印内置默认值（如 `socket` 未配置打印按 uid / 用户名推导的缺省路径，`log_dir` 未配置打印平台缺省路径，`state_dir` 未配置打印当前进程环境下的平台 State 缺省路径——服务形态的实际取值由 systemd 注入，可能与 CLI 打印结果不同；`auth_token` 未配置打印空串）。输出 SHALL 仅为值本身（便于脚本消费）。未知键 MUST 以非零码拒绝；已移除的 `host`、`port` SHALL 被拒绝并指明其已移除。配置文件不存在时 SHALL 等价于全默认配置（所有键返回内置默认值）。`--get` SHALL 可重复以一次读取多个键（每键一行 `key=value` 形式；单键时仅打印值）。

#### Scenario: 回退内置默认

- **WHEN** 配置文件存在但未写 `log_buffer_lines`，执行 `ak config --get log_buffer_lines`
- **THEN** 打印 `1000`，以 0 退出

#### Scenario: 文件值优先

- **WHEN** 配置文件写了 `log_level = "debug"`，执行 `ak config --get log_level`
- **THEN** 打印 `debug`

#### Scenario: socket 缺省按 uid 推导

- **WHEN** 配置文件未写 `socket`，以 uid 1000 执行 `ak config --get socket`
- **THEN** 打印 `/run/appkeeper/1000/ak.sock`

#### Scenario: auth_token 缺省打印空串

- **WHEN** 配置文件未写 `auth_token`，执行 `ak config --get auth_token`
- **THEN** 打印空行，以 0 退出

#### Scenario: 已移除的键拒绝读取

- **WHEN** 执行 `ak config --get port`
- **THEN** 非零码退出并说明该键已移除、端点由 `socket` 表达

#### Scenario: 文件缺失全默认

- **WHEN** daemon 配置文件不存在，执行 `ak config --get log_dir`
- **THEN** 打印平台缺省日志目录

#### Scenario: state_dir 未配置打印平台缺省

- **WHEN** 配置文件未写 `state_dir`，以非 root 用户执行 `ak config --get state_dir` 且环境无 `STATE_DIRECTORY`
- **THEN** 打印 `$XDG_STATE_HOME/appkeeper`（未设置时 `~/.local/state/appkeeper`），以 0 退出

### Requirement: config --delete 删除

`ak config --delete <key>` SHALL 按 TOML 点路径解析删除目标：省略表名的裸键 SHALL 视为 `[daemon]` 表内叶键的简写；`表.键` 形式 SHALL 删除指定表内的叶键；点路径解析命中已知表名时 SHALL 删除整张表。可删除目标 SHALL 受与 set/get 相同的已知键白名单约束（本能力当前仅 `[daemon]` 8 键：含 `socket`、`auth_token` 与 `state_dir`，不含已移除的 `host` / `port`）。删除 SHALL 使目标回到「未配置」态：有内置默认值的键随删除回退默认值；未被触碰的键、顺序与注释 SHALL 保持原样；写后 SHALL 整体校验并原子落盘（校验失败回滚不落盘），daemon 在线时 SHALL 提示需 reload 生效。目标在文件中本不存在时 SHALL 幂等成功（文件不变，输出说明无变化）；未知键或未知表 MUST 以非零码拒绝且文件不变。daemon 配置文件不存在时 delete MUST 拒绝并提示先执行 `--init`。`--delete` SHALL 可重复以一次删除多个目标。

#### Scenario: 删键回退内置默认

- **WHEN** 配置文件写了 `log_level = "debug"`，执行 `ak config --delete log_level`
- **THEN** 文件中 `log_level` 行被移除、其余内容（含注释与键序）原样，`config --get log_level` 此后打印默认值 `info`

#### Scenario: 目标不存在幂等成功

- **WHEN** 配置文件未设置 `socket`，执行 `ak config --delete socket`
- **THEN** 以 0 退出，文件内容不变，输出说明该键本未设置

#### Scenario: 未知键拒绝

- **WHEN** 执行 `ak config --delete foo`
- **THEN** 非零码退出并说明未知键，配置文件内容不变

#### Scenario: 删除与回滚

- **WHEN** 一次 `--delete` 使整体配置非法（如 `tcp://` 形态下删除 `auth_token`）
- **THEN** 命令非零退出，文件内容保持写入前的原样（含注释）

#### Scenario: 删除 state_dir 回退平台缺省

- **WHEN** 配置文件写了 `state_dir = "/srv/appkeeper/state"`，执行 `ak config --delete state_dir`
- **THEN** 该行被移除，`config --get state_dir` 此后打印平台 State 缺省路径
