## MODIFIED Requirements

### Requirement: daemon 根配置

daemon 配置全局唯一，SHALL 只包含全局内容：`[daemon]`（控制端点、日志、巡检间隔、`app_dir` 应用注册目录）、可选的 `[app-default]`（全部应用共享的默认值表，字段名与程序字段一致）与可选的 `[webui]`（内嵌控制台段：`listen` 监听地址；段存在即启用控制台，`listen` 缺省 `127.0.0.1:9877`）。daemon 配置 MUST NOT 承载任何应用专属内容：应用条目（如 `[[app]]`）、应用名、配置路径等 SHALL 被校验直接拒绝。`app_dir` 缺省为 daemon 配置文件同级的 `app.d` 目录（生产默认落位 `/etc/appkeeper/app.d`）。`daemon.log_dir` 缺省为 `/tmp/appkeeper/logs`（绝对路径，不随 daemon 配置文件位置漂移）；配置的 `log_dir` MUST 为绝对路径，相对路径 SHALL 被校验拒绝（守护进程拒绝启动）。daemon 配置默认路径 SHALL 为 Linux `/etc/appkeeper/daemon.toml`、Windows `%APPDATA%\appkeeper\daemon.toml`，并 SHALL 可用 `-c/--config` 覆盖。daemon 配置文件不存在时守护进程 SHALL 以内置默认值空启动，首次写入时创建。

`daemon.socket` SHALL 为非空字符串，缺省为 `/run/appkeeper/<uid>/ak.sock`（unix）与 `\\.\pipe\<用户名>.sock`（Windows）。该值 URL 形态自识别：`tcp://host:port` 前缀声明 TCP 形态控制端点，其它非空值为本地 IPC 路径（语义见 `control-plane` 能力规范）。`daemon.auth_token` SHALL 为条件键：`socket` 为 `tcp://` 形态时必配（非空字符串），`socket` 为路径形态时 MUST NOT 被设置。`[daemon]` 表 SHALL NOT 包含 `host`、`port` 两个键：出现即 SHALL 被校验拒绝并指明该键已移除（端点统一由 `socket` 的 URL 表达）。

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

#### Scenario: tcp 形态声明控制端点

- **WHEN** daemon 配置声明 `socket = "tcp://127.0.0.1:7310"` 与非空 `auth_token`
- **THEN** 守护进程以 TCP 形态启动并监听该端点，不创建 socket 文件

#### Scenario: 已移除的键导致校验失败

- **WHEN** daemon 配置中残留 `host` 或 `port` 任一键，执行 `ak validate` 或 `ak daemon`
- **THEN** 校验失败并指明该键已移除及其替代（端点由 `socket` 的 URL 表达），守护进程拒绝以该配置启动

#### Scenario: IPC 形态下 auth_token 被拒绝

- **WHEN** daemon 配置声明缺省或路径形态的 `socket`，同时设置了 `auth_token`，执行 `ak validate` 或 `ak daemon`
- **THEN** 校验失败并说明 `auth_token` 仅属于 `tcp://` 形态，守护进程拒绝以该配置启动

### Requirement: 校验规则

配置校验 SHALL 拒绝未知字段（拼写错误报错而非忽略）、空的应用（无任何 `[program.*]`）、超出合法范围的数值字段。应用名、程序名与自定义动作名 SHALL 仅由英文字母、数字、下划线与连字符组成（`[A-Za-z0-9_-]+`），含其它字符的名字（含点号、空格、非 ASCII 字符）SHALL 被校验拒绝（破坏性收紧：此前合法的含点号等名字在升级后校验失败，需改名）。`command` 非空且单行拆分与显式 `args` 不得同时出现；`health_check` 字符串 SHALL 在加载期按协议解析并校验。`daemon.log_dir` SHALL 为绝对路径：相对路径或空值 SHALL 被校验拒绝，守护进程 MUST 拒绝启动并在错误信息中指明字段与修复方式。`daemon.socket` SHALL 为非空字符串，空值 SHALL 被校验拒绝；`tcp://` 形态值 SHALL 可解析（host 非空、端口 1–65535），无法解析 SHALL 被校验拒绝。端点形态与凭据 SHALL 满足组合约束（与 `command`/`args` 互斥同类）：`socket` 为 `tcp://` 且 `auth_token` 为空 SHALL 被校验拒绝；`socket` 为路径形态且设置了 `auth_token` SHALL 被校验拒绝。`webui.listen` SHALL 为非空且可解析的 `host:port`（端口 1–65535），非法值 SHALL 被校验拒绝。程序名 SHALL 在全部已注册应用之间全局唯一：注册与 reload 时检测到跨应用重名 SHALL 拒绝并指明冲突双方。`depends_on` 引用 SHALL 允许跨应用，成环在注册/校验期被拒绝。自定义动作定义 SHALL 随配置解析一并校验：`command` 非空、`timeout` 大于 0、引用的变量名全部已知、动作名不与内置动作保留字（start、stop、restart、signal、reload、shutdown）冲突。

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
- **THEN** 校验失败并指明 `port` 已移除、端点由 `socket` 的 URL 表达；MUST NOT 静默忽略该键

#### Scenario: tcp 缺 token 组合被拒绝

- **WHEN** daemon 配置声明 `socket = "tcp://127.0.0.1:7310"` 且未设置 `auth_token`，执行 `ak validate` 或 `ak daemon`
- **THEN** 校验失败并说明 `tcp://` 形态必须配置 `auth_token`，守护进程不启动

#### Scenario: tcp 端口越界被拒绝

- **WHEN** daemon 配置声明 `socket = "tcp://127.0.0.1:99999"`
- **THEN** 校验失败并说明端口必须为 1–65535

### Requirement: config 子命令

`ak config` SHALL 作为 daemon 配置文件的统一本地管理入口，动作由 flag 指定：`--set`（写入，可重复）、`--get`（读取）、`--delete`（删除，可重复）、`--edit`（编辑器编辑）、`--init`（初始化）；一次调用 SHALL 只执行一类动作（`--set`/`--get`/`--delete` 可各自多键，`--edit` 与 `--init` 不与其他动作同用），无任何动作 flag 时 SHALL 打印用法帮助并以 0 退出。目标文件为解析后的 daemon 配置路径（全局 `-c/--config` 可覆盖，缺省平台位置）。config SHALL 是纯本地文件操作：MUST NOT 依赖运行中的 daemon，MUST NOT 直接改变运行态（写入生效仍经由既有 reload/apply 流程）。daemon 在线且本次动作改写了文件时，SHALL 在输出中提示需要 reload 方可生效；在线检测 SHALL 经控制端点进行，端点形态由目标配置的 `socket` 值决定，MUST NOT 依赖任何硬编码端口。

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

`ak config --set key=value` SHALL 仅接受 `[daemon]` 表的已知键白名单：`log_level`、`log_dir`、`monitor_interval`、`log_buffer_lines`、`app_dir`、`socket`、`auth_token`，值按目标字段类型解析（`monitor_interval` 为正浮点、`log_buffer_lines` 为非负整数、`log_level` 限定既有日志级别集合、`log_dir`/`app_dir`/`socket` 为非空字符串、`auth_token` 为非空字符串）；未知键或类型不符 MUST 拒绝并以非零码退出，不落盘。已移除的 `host`、`port` SHALL 与其它未知键同等对待：以非零码拒绝，并在错误信息中指明该键已移除及其替代（端点由 `socket` 的 URL 表达），MUST NOT 接受写入。写入 SHALL 采用读-改-写：文件中未被本次触碰的键、顺序与注释 SHALL 保持原样；写后 SHALL 对整个配置做整体校验（含端点形态与凭据的组合约束），校验失败 MUST 回滚为原文件内容（磁盘上不留半成品）并以非零码退出。daemon 配置文件不存在时 set MUST 拒绝并提示先执行 `--init`。`--set` SHALL 可重复以一次写入多个键。成功 SHALL 打印写入的键与新值；目标 daemon 在线时 SHALL 提示需 reload 生效。

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

### Requirement: config --get 读取

`ak config --get <key>` SHALL 接受与 set 相同的白名单裸键名，打印该键的生效值：配置文件中显式设置了该键 SHALL 打印文件值，未设置 SHALL 打印内置默认值（如 `socket` 未配置打印按 uid / 用户名推导的缺省路径，`log_dir` 未配置打印平台缺省路径；`auth_token` 未配置打印空串）。输出 SHALL 仅为值本身（便于脚本消费）。未知键 MUST 以非零码拒绝；已移除的 `host`、`port` SHALL 被拒绝并指明其已移除。配置文件不存在时 SHALL 等价于全默认配置（所有键返回内置默认值）。`--get` SHALL 可重复以一次读取多个键（每键一行 `key=value` 形式；单键时仅打印值）。

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

### Requirement: config --delete 删除

`ak config --delete <key>` SHALL 按 TOML 点路径解析删除目标：省略表名的裸键 SHALL 视为 `[daemon]` 表内叶键的简写；`表.键` 形式 SHALL 删除指定表内的叶键；点路径解析命中已知表名时 SHALL 删除整张表。可删除目标 SHALL 受与 set/get 相同的已知键白名单约束（本能力当前仅 `[daemon]` 7 键：含 `socket` 与 `auth_token`，不含已移除的 `host` / `port`）。删除 SHALL 使目标回到「未配置」态：有内置默认值的键随删除回退默认值；未被触碰的键、顺序与注释 SHALL 保持原样；写后 SHALL 整体校验并原子落盘（校验失败回滚不落盘），daemon 在线时 SHALL 提示需 reload 生效。目标在文件中本不存在时 SHALL 幂等成功（文件不变，输出说明无变化）；未知键或未知表 MUST 以非零码拒绝且文件不变。daemon 配置文件不存在时 delete MUST 拒绝并提示先执行 `--init`。`--delete` SHALL 可重复以一次删除多个目标。

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

### Requirement: config --init 初始化

`ak config --init` SHALL 在解析后的 daemon 配置路径生成全新配置文件（父目录不存在时先创建）：渲染 `[daemon]` 表全部已知键（log_level、log_dir、monitor_interval、log_buffer_lines、app_dir、socket）并赋内置默认值，逐键附说明注释；已移除的 `host`、`port` MUST NOT 出现在模板中；`auth_token` SHALL 以注释形式出现在模板中并注明「仅 `socket = "tcp://..."` 形态生效，路径形态下设置将被校验拒绝」；另 SHALL 渲染一段全部注释掉的 `[app-default]` 模板段作为提示；`app_dir` SHALL 渲染相对值 `app.d`（注释说明其相对配置文件所在目录解析）。目标位置已存在任何配置文件时 MUST 拒绝（非零码退出），MUST NOT 覆盖或修改既有文件。init SHALL 连带初始化 app 配置目录：按渲染的 `app_dir` 创建目录，并在其中生成 `example.toml.sample`——内容为全部注释掉的 app 配置模板（`[app]` 表与 `[program.*]` 样例），已存在时 SHALL 跳过且不报错。成功 SHALL 打印生成的配置文件路径、app 配置目录与后续步骤提示（如 `ak config --edit`、`ak add`）。

#### Scenario: 全新机器初始化

- **WHEN** 在既无配置文件也无 app 目录的机器执行 `ak config --init`
- **THEN** 生成含全部默认值与注释的 daemon 配置文件、app 配置目录及 `example.toml.sample`，打印路径与下一步提示，以 0 退出；模板含注释掉的 `auth_token` 行与形态说明，`ak validate` 通过

#### Scenario: 拒绝覆盖既有配置

- **WHEN** daemon 配置文件已存在，执行 `ak config --init`
- **THEN** 非零码退出，说明文件已存在拒绝初始化，既有文件内容不变

#### Scenario: example 样例不参与加载

- **WHEN** init 完成后执行 `ak list`（或触发注册表扫描）
- **THEN** `example.toml.sample` 不作为应用出现（`.sample` 后缀不被注册表扫描）

#### Scenario: 重复 init 幂等安全

- **WHEN** 连续两次执行 `ak config --init`（第二次前已手工删除 daemon 配置文件但保留 app 目录与 example）
- **THEN** 第二次成功重建配置文件，`example.toml.sample` 保持原样未被改写
