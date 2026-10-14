## MODIFIED Requirements

### Requirement: daemon 根配置

daemon 配置全局唯一，SHALL 只包含全局内容：`[daemon]`（控制 socket 端点、日志、巡检间隔、`app_dir` 应用注册目录）、可选的 `[app-default]`（全部应用共享的默认值表，字段名与程序字段一致）与可选的 `[webui]`（内嵌控制台段：`listen` 监听地址；段存在即启用控制台，`listen` 缺省 `127.0.0.1:9877`）。daemon 配置 MUST NOT 承载任何应用专属内容：应用条目（如 `[[app]]`）、应用名、配置路径等 SHALL 被校验直接拒绝。`app_dir` 缺省为 daemon 配置文件同级的 `app.d` 目录（生产默认落位 `/etc/appkeeper/app.d`）。`daemon.log_dir` 缺省为 `/tmp/appkeeper/logs`（绝对路径，不随 daemon 配置文件位置漂移）；配置的 `log_dir` MUST 为绝对路径，相对路径 SHALL 被校验拒绝（守护进程拒绝启动）。daemon 配置默认路径 SHALL 为 Linux `/etc/appkeeper/daemon.toml`、Windows `%APPDATA%\appkeeper\daemon.toml`，并 SHALL 可用 `-c/--config` 覆盖。daemon 配置文件不存在时守护进程 SHALL 以内置默认值空启动，首次写入时创建。

`daemon.socket` SHALL 为非空字符串，缺省为 `/run/appkeeper/<uid>/ak.sock`（unix）与 `\\.\pipe\<用户名>.sock`（Windows）。`[daemon]` 表 SHALL NOT 包含 `host`、`port`、`auth_token` 三个键：出现即 SHALL 被校验拒绝并指明该键已移除，控制面不再监听网络端口、准入不再由应用层 token 决定（见 `control-plane` 能力规范）。

#### Scenario: 全新机器空启动

- **WHEN** 在没有任何 daemon 配置文件的机器上执行 `ak daemon`
- **THEN** 守护进程以默认设置启动，受管程序为空，控制平面正常可用，控制台不伺服

#### Scenario: daemon 配置中的应用条目被拒绝

- **WHEN** daemon 配置中出现 `[[app]]` 等应用专属条目
- **THEN** 校验失败并报未知字段，守护进程拒绝以该配置启动

#### Scenario: 缺省日志目录固定

- **WHEN** daemon 配置未声明 `log_dir`，守护进程拉起任一程序并产生输出
- **THEN** 程序日志写入 `/tmp/appkeeper/logs`，与 daemon 配置文件所在目录无关

#### Scenario: app_dir 缺省名为 app.d

- **WHEN** daemon 配置未声明 `app_dir`，执行 `ak add ./proc --name abc` 完成注册
- **THEN** 注册记录写入 daemon 配置文件同级的 `app.d/abc.toml`（Linux 生产默认 `/etc/appkeeper/app.d/abc.toml`）

#### Scenario: 显式 app_dir 覆盖默认名

- **WHEN** daemon 配置显式声明 `app_dir = "apps"`
- **THEN** 注册记录写入该 `apps` 目录，缺省名 `app.d` 不生效

#### Scenario: 缺省 socket 路径含 uid 分层

- **WHEN** daemon 配置未声明 `socket`，以 uid 1000 启动守护进程
- **THEN** 控制端点为 `/run/appkeeper/1000/ak.sock`，`/run/appkeeper` 存在且权限为 `0755`

#### Scenario: 已移除的键导致校验失败

- **WHEN** daemon 配置中残留 `host`、`port` 或 `auth_token` 任一键，执行 `ak validate` 或 `ak daemon`
- **THEN** 校验失败并指明该键已移除及其替代（控制面改用 socket），守护进程拒绝以该配置启动

### Requirement: 校验规则

配置校验 SHALL 拒绝未知字段（拼写错误报错而非忽略）、空的应用（无任何 `[program.*]`）、超出合法范围的数值字段。应用名、程序名与自定义动作名 SHALL 仅由英文字母、数字、下划线与连字符组成（`[A-Za-z0-9_-]+`），含其它字符的名字（含点号、空格、非 ASCII 字符）SHALL 被校验拒绝（破坏性收紧：此前合法的含点号等名字在升级后校验失败，需改名）。`command` 非空且单行拆分与显式 `args` 不得同时出现；`health_check` 字符串 SHALL 在加载期按协议解析并校验。`daemon.log_dir` SHALL 为绝对路径：相对路径或空值 SHALL 被校验拒绝，守护进程 MUST 拒绝启动并在错误信息中指明字段与修复方式。`daemon.socket` SHALL 为非空字符串，空值 SHALL 被校验拒绝。`webui.listen` SHALL 为非空且可解析的 `host:port`（端口 1–65535），非法值 SHALL 被校验拒绝。程序名 SHALL 在全部已注册应用之间全局唯一：注册与 reload 时检测到跨应用重名 SHALL 拒绝并指明冲突双方。`depends_on` 引用 SHALL 允许跨应用，成环在注册/校验期被拒绝。自定义动作定义 SHALL 随配置解析一并校验：`command` 非空、`timeout` 大于 0、引用的变量名全部已知、动作名不与内置动作保留字（start、stop、restart、signal、reload、shutdown）冲突。

#### Scenario: 跨应用程序名冲突被拒绝

- **WHEN** 注册一个其程序名与已注册程序重名的 app 配置
- **THEN** 注册失败，错误指明重名程序与冲突的双方应用

#### Scenario: 单行 command 与 args 冲突

- **WHEN** 程序同时写 `command = "python -m server"` 与 `args = ["-m", "server"]`
- **THEN** 校验失败，提示二者互斥

#### Scenario: 相对 log_dir 拒绝启动

- **WHEN** daemon 配置声明 `log_dir = "logs"`（相对路径）并执行 `ak daemon` 或 `ak validate`
- **THEN** 校验失败，错误信息指明 `daemon.log_dir` 必须为绝对路径，守护进程不启动

#### Scenario: 非法字符名被拒绝

- **WHEN** app 配置声明 `[program."my.web"]` 或应用名含空格等 `[A-Za-z0-9_-]` 之外的字符
- **THEN** 校验失败，错误指明名字仅允许英文字母、数字、下划线与连字符

#### Scenario: 未知动作变量被拒绝

- **WHEN** 动作 command 引用 `${program.api.hello}`（`hello` 非内置变量字段）
- **THEN** validate 与 reload 校验失败，错误指明未知变量名

#### Scenario: 动作占用保留字被拒绝

- **WHEN** app 配置声明 `[program.api.action.stop]`
- **THEN** 校验失败，错误指明动作名与内置动作保留字冲突

#### Scenario: 空 socket 值被拒绝

- **WHEN** daemon 配置声明 `socket = ""`
- **THEN** 校验失败并指出 `daemon.socket` 必须为非空字符串

#### Scenario: 已移除的 daemon 键被拒绝

- **WHEN** daemon 配置的 `[daemon]` 表出现 `port = 7310`
- **THEN** 校验失败并指明 `port` 已移除、控制面改用 socket 端点；MUST NOT 静默忽略该键

### Requirement: config 子命令

`ak config` SHALL 作为 daemon 配置文件的统一本地管理入口，动作由 flag 指定：`--set`（写入，可重复）、`--get`（读取）、`--delete`（删除，可重复）、`--edit`（编辑器编辑）、`--init`（初始化）；一次调用 SHALL 只执行一类动作（`--set`/`--get`/`--delete` 可各自多键，`--edit` 与 `--init` 不与其他动作同用），无任何动作 flag 时 SHALL 打印用法帮助并以 0 退出。目标文件为解析后的 daemon 配置路径（全局 `-c/--config` 可覆盖，缺省平台位置）。config SHALL 是纯本地文件操作：MUST NOT 依赖运行中的 daemon，MUST NOT 直接改变运行态（写入生效仍经由既有 reload/apply 流程）。daemon 在线且本次动作改写了文件时，SHALL 在输出中提示需要 reload 方可生效；在线检测失败不阻塞动作本身。在线检测 SHALL 经控制 socket 端点进行，MUST NOT 依赖任何 TCP 端口。

#### Scenario: 无动作打印帮助

- **WHEN** 执行 `ak config`
- **THEN** 打印五个动作 flag 的用法说明，以 0 退出

#### Scenario: 离线可用

- **WHEN** daemon 未运行，执行 `ak config --set log_buffer_lines=2000`
- **THEN** 文件被写入并校验通过，输出中说明 daemon 当前离线、下次启动生效

#### Scenario: 动作互斥

- **WHEN** 同时给定 `--init` 与 `--get log_level`
- **THEN** 以非零码退出并说明一次只能执行一类动作

### Requirement: config --set 写入

`ak config --set key=value` SHALL 仅接受 `[daemon]` 表的已知键白名单：`log_level`、`log_dir`、`monitor_interval`、`log_buffer_lines`、`app_dir`、`socket`，值按目标字段类型解析（`monitor_interval` 为正浮点、`log_buffer_lines` 为非负整数、`log_level` 限定既有日志级别集合、`log_dir`/`app_dir`/`socket` 为非空字符串路径、其余为字符串）；未知键或类型不符 MUST 拒绝并以非零码退出，不落盘。已移除的 `host`、`port`、`auth_token` SHALL 与其它未知键同等对待：以非零码拒绝，并在错误信息中指明该键已移除及其替代（`port` → `socket`），MUST NOT 接受写入。写入 SHALL 采用读-改-写：文件中未被本次触碰的键、顺序与注释 SHALL 保持原样；写后 SHALL 对整个配置做整体校验，校验失败 MUST 回滚为原文件内容（磁盘上不留半成品）并以非零码退出。daemon 配置文件不存在时 set MUST 拒绝并提示先执行 `--init`。`--set` SHALL 可重复以一次写入多个键。成功 SHALL 打印写入的键与新值；目标 daemon 在线时 SHALL 提示需 reload 生效。

#### Scenario: 强类型拒绝

- **WHEN** 执行 `ak config --set log_buffer_lines=abc` 或 `ak config --set foo=1`
- **THEN** 非零码退出并说明类型不符/未知键，配置文件内容不变

#### Scenario: 已移除的键拒绝写入

- **WHEN** 执行 `ak config --set port=7311`
- **THEN** 非零码退出并说明 `port` 已移除、应改用 `socket`，配置文件内容不变

#### Scenario: 校验失败回滚

- **WHEN** 一次 `--set` 写入使整体配置非法（如 `--set log_level=verbose`）
- **THEN** 命令非零退出，文件内容保持写入前的原样（含注释）

#### Scenario: 未触碰内容保持

- **WHEN** 对一份带手工注释与自定义键序的合法配置执行 `--set socket=/run/appkeeper/1000/custom.sock`
- **THEN** 仅 socket 值改变，其余键的顺序、值与全部注释保持原样

#### Scenario: 缺失文件拒绝

- **WHEN** daemon 配置文件不存在，执行 `ak config --set log_level=debug`
- **THEN** 非零码退出，提示先执行 `ak config --init`

### Requirement: config --get 读取

`ak config --get <key>` SHALL 接受与 set 相同的白名单裸键名，打印该键的生效值：配置文件中显式设置了该键 SHALL 打印文件值，未设置 SHALL 打印内置默认值（如 `socket` 未配置打印按 uid / 用户名推导的缺省路径，`log_dir` 未配置打印平台缺省路径）。输出 SHALL 仅为值本身（便于脚本消费）。未知键 MUST 以非零码拒绝；已移除的 `host`、`port`、`auth_token` SHALL 被拒绝并指明其已移除。配置文件不存在时 SHALL 等价于全默认配置（所有键返回内置默认值）。`--get` SHALL 可重复以一次读取多个键（每键一行 `key=value` 形式；单键时仅打印值）。

#### Scenario: 回退内置默认

- **WHEN** 配置文件存在但未写 `log_buffer_lines`，执行 `ak config --get log_buffer_lines`
- **THEN** 打印 `1000`，以 0 退出

#### Scenario: 文件值优先

- **WHEN** 配置文件写了 `log_level = "debug"`，执行 `ak config --get log_level`
- **THEN** 打印 `debug`

#### Scenario: socket 缺省按 uid 推导

- **WHEN** 配置文件未写 `socket`，以 uid 1000 执行 `ak config --get socket`
- **THEN** 打印 `/run/appkeeper/1000/ak.sock`

#### Scenario: 文件缺失全默认

- **WHEN** daemon 配置文件不存在，执行 `ak config --get log_dir`
- **THEN** 打印平台缺省日志目录

#### Scenario: 已移除的键拒绝读取

- **WHEN** 执行 `ak config --get port`
- **THEN** 非零码退出并说明该键已移除、应改用 `socket`

### Requirement: config --delete 删除

`ak config --delete <key>` SHALL 按 TOML 点路径解析删除目标：省略表名的裸键 SHALL 视为 `[daemon]` 表内叶键的简写；`表.键` 形式 SHALL 删除指定表内的叶键；点路径解析命中已知表名时 SHALL 删除整张表。可删除目标 SHALL 受与 set/get 相同的已知键白名单约束（本能力当前仅 `[daemon]` 6 键：含新增的 `socket`，不含已移除的 `host` / `port` / `auth_token`）。删除 SHALL 使目标回到「未配置」态：有内置默认值的键随删除回退默认值；未被触碰的键、顺序与注释 SHALL 保持原样；写后 SHALL 整体校验并原子落盘（校验失败回滚不落盘），daemon 在线时 SHALL 提示需 reload 生效。目标在文件中本不存在时 SHALL 幂等成功（文件不变，输出说明无变化）；未知键或未知表 MUST 以非零码拒绝且文件不变。daemon 配置文件不存在时 delete MUST 拒绝并提示先执行 `--init`。`--delete` SHALL 可重复以一次删除多个目标。

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

- **WHEN** 一次 `--delete` 使整体配置非法（如删除必填后无法通过校验的组合场景）
- **THEN** 命令非零退出，文件内容保持写入前的原样（含注释）

### Requirement: webui 配置段

daemon 配置的 `[webui]` 段 SHALL 由 config 子命令按统一键表机制管理：`config --set webui.listen=<addr>` 写入监听地址（写入即开启控制台，reload 后生效或热生效）、`config --get webui.listen` 打印生效值（段未配置时 SHALL 回退内置默认 `127.0.0.1:9877`）、`config --delete webui` 删除整段（即关闭控制台）。键表机制、类型校验、读改写与原子落盘 SHALL 与 `[daemon]` 键一致（见 config 子命令相关 requirement）。`webui.listen` 值 SHALL 为非空的 `host:port` 形式：无法解析或为空 SHALL 被校验拒绝（写入期与加载期一致拒绝）。`[webui]` 段 SHALL 另增 `auth_token` 键：未配置时控制台 `/api/*` 不做应用层鉴权（仅依赖 `webui.listen` 的绑定地址），配置后 SHALL 要求凭据方可访问（鉴权语义见 `webui-api` 能力规范）；该键 SHALL 为非空字符串，空值 SHALL 被校验拒绝。控制台 SHALL 保留 TCP 监听，MUST NOT 因控制面改用 socket 而要求浏览器经本地 IPC 端点访问。`[webui]` 段内未知键 SHALL 被校验拒绝。

#### Scenario: config 开启控制台

- **WHEN** 执行 `ak config --set webui.listen=127.0.0.1:9877`（daemon 离线）
- **THEN** daemon 配置文件新增 `[webui]` 段且整体校验通过，输出说明离线时下次启动生效

#### Scenario: config 关闭控制台

- **WHEN** 配置含 `[webui]` 段，执行 `ak config --delete webui`
- **THEN** 该段整体从文件移除（其余内容含注释保持原样），守护在线时输出需 reload 提示

#### Scenario: 非法 listen 拒绝写入

- **WHEN** 执行 `ak config --set webui.listen=9877`（缺 host）或 `ak config --set webui.listen=`（空值）
- **THEN** 非零码退出并说明值必须是 `host:port`，配置文件内容不变

#### Scenario: 段内未知键校验拒绝

- **WHEN** daemon 配置的 `[webui]` 段内出现未知键（如 `auth = true`），执行 `ak validate` 或 `ak daemon`
- **THEN** 校验失败并指明该未知字段，守护进程拒绝以该配置启动

#### Scenario: 配置控制台 token

- **WHEN** 执行 `ak config --set webui.auth_token=<token>`
- **THEN** `[webui]` 段新增 `auth_token` 键且整体校验通过；控制台 reload 后对 `/api/*` 要求凭据

#### Scenario: 空 webui token 被拒绝

- **WHEN** 执行 `ak config --set webui.auth_token=`
- **THEN** 非零码退出并说明值必须为非空字符串，配置文件内容不变
