# configuration Specification

## Purpose

定义 appkeeper 的分层配置契约：唯一的 daemon 根配置（只做全局：守护进程设置、`app_dir`、`[app-default]` 应用默认值）、随应用部署目录分布的 app 配置（map 接口、扁平字段）、四层字段优先级、校验规则、v0.1 迁移路径与按应用粒度的热更新语义。

## Requirements

### Requirement: daemon 根配置

daemon 配置全局唯一，SHALL 只包含全局内容：`[daemon]`（监听地址/端口、鉴权 token、日志、巡检间隔、`app_dir` 应用注册目录）、可选的 `[app-default]`（全部应用共享的默认值表，字段名与程序字段一致）与可选的 `[webui]`（内嵌控制台段：`listen` 监听地址；段存在即启用控制台，`listen` 缺省 `127.0.0.1:9877`）。daemon 配置 MUST NOT 承载任何应用专属内容：应用条目（如 `[[app]]`）、应用名、配置路径等 SHALL 被校验直接拒绝。`app_dir` 缺省为 daemon 配置文件同级的 `apps` 目录。`daemon.log_dir` 缺省为 `/tmp/appkeeper/logs`（绝对路径，不随 daemon 配置文件位置漂移）；配置的 `log_dir` MUST 为绝对路径，相对路径 SHALL 被校验拒绝（守护进程拒绝启动）。daemon 配置默认路径 SHALL 为 Linux `/etc/appkeeper/daemon.toml`、Windows `%APPDATA%\appkeeper\daemon.toml`，并 SHALL 可用 `-c/--config` 覆盖。daemon 配置文件不存在时守护进程 SHALL 以内置默认值空启动，首次写入时创建。

#### Scenario: 全新机器空启动

- **WHEN** 在没有任何 daemon 配置文件的机器上执行 `ak daemon`
- **THEN** 守护进程以默认设置启动，受管程序为空，控制平面正常可用，控制台不伺服

#### Scenario: daemon 配置中的应用条目被拒绝

- **WHEN** daemon 配置中出现 `[[app]]` 等应用专属条目
- **THEN** 校验失败并报未知字段，守护进程拒绝以该配置启动

#### Scenario: 缺省日志目录固定

- **WHEN** daemon 配置未声明 `log_dir`，守护进程拉起任一程序并产生输出
- **THEN** 程序日志写入 `/tmp/appkeeper/logs`，与 daemon 配置文件所在目录无关

### Requirement: app 配置

每个应用的配置本体 SHALL 存放在其部署目录，默认文件名 `appkeeper.toml`，注册时可指定任意路径。app 配置 SHALL 由可选的 `[app]` 表（应用级默认值与 `description` 元数据，注册微调 flag 的落点）与一或多个 `[program.<name>]` 映射表组成：映射键即程序名，MUST NOT 再写 `name` 字段。程序字段 SHALL 采用简化写法：`work_dir`（工作目录）、`env`（环境变量表）、`log_max_size`/`log_rotate_keep`（日志轮转，无需独立 section；缺省 `50MB` 保留 `2` 份，即每流盘上最多 3 个文件；`log_max_size = "0"` 显式禁用轮转）、`health_check`（单字符串，按协议前缀分发：`http(s)://` 为 HTTP 探测、`tcp://` 为 TCP 连通、其余视为探测命令行）；`health_interval`/`health_timeout`/`health_retries`/`health_start_period` 为可选节奏字段。`command` SHALL 支持单行带参写法（如 `"python -m http.server 8000"`，按 shell 词法规则拆分为 argv、不经过 shell），与显式 `args` 数组互斥。

#### Scenario: 最小 app 配置可用

- **WHEN** 部署目录 `appkeeper.toml` 只写 `[program.api]` 与其 `command = "python -m http.server 8000"`
- **THEN** `ak add .` 即可注册，程序名为 `api`，参数经拆分后按默认值运行

#### Scenario: 健康检查按协议分发

- **WHEN** 程序声明 `health_check = "tcp://127.0.0.1:5432"`
- **THEN** 该程序的健康检查为 TCP 连通探测，无需再声明类型字段

#### Scenario: 缺省阈值触发轮转

- **WHEN** 程序未声明任何轮转字段，某流日志超过 50MB 后继续输出
- **THEN** 当前文件被轮转为 `.1`，该流盘上最多保留 3 个文件（当前 + 2 份轮转）

#### Scenario: 显式禁用轮转

- **WHEN** 程序声明 `log_max_size = "0"`
- **THEN** 该程序的日志不轮转，文件持续追加不设上限

### Requirement: 字段优先级

同一字段 SHALL 按以下优先级取值（从高到低）：`[program.*]` 显式字段 > app 配置 `[app]` 表（注册微调 flag 落点，部署人可手改）> daemon `[app-default]` > 内置默认。`priority` 为应用级字段，仅存在于 `[app]` 与 `[app-default]` 层；`restart`（取代既有 `autorestart` 与 `autostart` 两个旧字段）为程序级字段，可在 `[app]` 与 `[app-default]` 层作应用级默认值。

#### Scenario: app 层默认生效

- **WHEN** `[app]` 设置 `restart = "on-failure"`，程序未显式声明
- **THEN** 该应用内程序按 on-failure 运行

#### Scenario: daemon 默认兜底

- **WHEN** daemon `[app-default]` 设 `restart_backoff = 2.0`，app 配置未在任何层级声明
- **THEN** 程序按 2 秒重启间隔运行

#### Scenario: 程序显式字段最高

- **WHEN** 某程序显式声明 `restart = "no"`，而 `[app]` 与 `[app-default]` 均为 always
- **THEN** 该程序按 no 运行，同应用其他程序不受影响

### Requirement: app_dir 注册目录

daemon 配置指定的 `app_dir` SHALL 集中存放全部已注册应用的链接，且链接 SHALL 作为注册记录本体：守护进程与 `ak list` 通过扫描 `app_dir` 发现应用。注册成功时自动以应用名创建 `<name>.toml` 链接指向 app 配置本体（优先符号链接，平台不允许时回退硬链接）。链接 MUST 因任何原因（权限不足、平台限制等）无法建立时，注册 SHALL 视为失败：错误信息 MUST 指明 `app_dir` 路径与修复方式（如 sudo、目录归属）并以非零码退出，MUST NOT 输出注册成功。app 配置本体 MUST 始终以部署目录中的原件为准，链接仅为集中查看的投影；add/remove SHALL 同步维护链接，发现链接缺失或悬空时 SHALL 告警。

#### Scenario: 注册后自动链接

- **WHEN** 在部署目录执行 `ak add . --name web` 成功且平台支持符号链接
- **THEN** `app_dir/web.toml` 出现并指向部署目录中的配置本体，本体内容变化即时可见

#### Scenario: 链接创建被拒绝

- **WHEN** 链接无法建立（如 `app_dir` 归属其他用户且当前用户无写权限，或 Windows 未开启符号链接权限且非同卷）
- **THEN** 注册失败：错误信息指明 `app_dir` 不可写与修复方式，退出码非 0，`app_dir` 中不出现链接，守护进程不感知该应用

### Requirement: 校验规则

配置校验 SHALL 拒绝未知字段（拼写错误报错而非忽略）、空的应用（无任何 `[program.*]`）、超出合法范围的数值字段。应用名、程序名与自定义动作名 SHALL 仅由英文字母、数字、下划线与连字符组成（`[A-Za-z0-9_-]+`），含其它字符的名字（含点号、空格、非 ASCII 字符）SHALL 被校验拒绝（破坏性收紧：此前合法的含点号等名字在升级后校验失败，需改名）。`command` 非空且单行拆分与显式 `args` 不得同时出现；`health_check` 字符串 SHALL 在加载期按协议解析并校验。`daemon.log_dir` SHALL 为绝对路径：相对路径或空值 SHALL 被校验拒绝，守护进程 MUST 拒绝启动并在错误信息中指明字段与修复方式。`webui.listen` SHALL 为非空且可解析的 `host:port`（端口 1–65535），非法值 SHALL 被校验拒绝。程序名 SHALL 在全部已注册应用之间全局唯一：注册与 reload 时检测到跨应用重名 SHALL 拒绝并指明冲突双方。`depends_on` 引用 SHALL 允许跨应用，成环在注册/校验期被拒绝。自定义动作定义 SHALL 随配置解析一并校验：`command` 非空、`timeout` 大于 0、引用的变量名全部已知、动作名不与内置动作保留字（start、stop、restart、signal、reload、shutdown）冲突。

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

### Requirement: v0.1 旧配置迁移

v0.1 的单文件配置（`[daemon]` + `[[program]]` 数组）SHALL 可通过 `ak add <旧文件>` 直接导入：导入器 SHALL 以独立的旧版 schema 解析，将 `[[program]]` 转写为 `[program.*]` 映射并注册为一个应用，`[daemon]` 段被检测并提示需要并入 daemon 配置；导入 MUST NOT 修改原文件。

#### Scenario: 一键迁移旧配置

- **WHEN** 对 v0.1 的 `config.toml` 执行 `ak add config.toml`
- **THEN** 其中全部程序注册为一个应用并可被守护进程拉起，输出提示 `[daemon]` 段需并入 daemon 配置，原文件内容未变

### Requirement: 按应用粒度的热更新

reload SHALL 重扫 `app_dir` 注册并重读全部 app 配置本体与 daemon 配置，但
仅将差异记为**待应用（pending）变更**并输出预览，MUST NOT 停止、重启或拉起
任何程序（应用由 apply 触发，见 apply-workflow 能力）。**唯一例外**：
`[webui]` 段的启用/关闭/`listen` 变更 SHALL 由 reload 立即热应用（见 webui-api
「配置驱动的控制台生命周期」），MUST NOT 进入 pending。守护进程 SHALL 周期性
自动执行该重扫，`ak reload` 触发一次立即重扫。单个 app 文件非法时
SHALL 隔离失败——该 app 不进入 pending、保持旧定义继续运行并上报错误，其余
app 照常检出。跨应用校验失败时本轮 SHALL 整体不形成 pending。daemon 配置中
守护进程自身字段变更（`[webui]` 段之外）SHALL 尽量热应用（如日志级别），无法
热更的字段（如监听端口）SHALL 在 pending 预览中明确提示需要重启守护进程。

#### Scenario: 单个 app 文件损坏不影响其他

- **WHEN** 重扫时 app B 的配置存在语法错误，app A 的配置正常且已修改
- **THEN** A 的变更进入 pending，B 保持旧定义继续运行，错误信息被上报

#### Scenario: reload 只检出不应用

- **WHEN** app A 的程序 `web` 配置已修改且正在运行，执行 `ak reload`
- **THEN** 输出包含 `web` 的 pending 预览，进程未被停止或重启

#### Scenario: 不可热更字段提示

- **WHEN** daemon 配置的 `port` 被修改，执行 `ak reload`
- **THEN** pending 预览提示该字段需重启守护进程方可生效，控制面仍监听旧端口

#### Scenario: 移除注册立即生效

- **WHEN** 守护进程运行中执行 `ak remove <app>`
- **THEN** `app_dir` 链接即时清理，该 app 的移除进入 pending，其程序在
  apply 前继续运行

### Requirement: validate 子命令

`ak validate` SHALL 校验 daemon 配置及 `app_dir` 中全部已注册 app 配置本体；`ak validate <path>` SHALL 单独校验一个 app 配置文件。通过时输出应用与程序清单并以 0 退出；失败时输出全部错误并以非零码退出；两种情况均 MUST NOT 启动任何进程。

#### Scenario: 校验通过

- **WHEN** 对合法的 daemon 配置与全部 app 配置执行 validate
- **THEN** 输出应用/程序清单，退出码 0，无进程被启动

#### Scenario: 校验失败

- **WHEN** 任一文件存在错误时执行 validate
- **THEN** 逐条列出错误（含文件与应用归属），退出码非 0

### Requirement: config 子命令

`ak config` SHALL 作为 daemon 配置文件的统一本地管理入口，动作由 flag 指定：`--set`（写入，可重复）、`--get`（读取）、`--delete`（删除，可重复）、`--edit`（编辑器编辑）、`--init`（初始化）；一次调用 SHALL 只执行一类动作（`--set`/`--get`/`--delete` 可各自多键，`--edit` 与 `--init` 不与其他动作同用），无任何动作 flag 时 SHALL 打印用法帮助并以 0 退出。目标文件为解析后的 daemon 配置路径（全局 `-c/--config` 可覆盖，缺省平台位置）。config SHALL 是纯本地文件操作：MUST NOT 依赖运行中的 daemon，MUST NOT 直接改变运行态（写入生效仍经由既有 reload/apply 流程）。daemon 在线且本次动作改写了文件时，SHALL 在输出中提示需要 reload 方可生效；在线检测失败不阻塞动作本身。

#### Scenario: 无动作打印帮助

- **WHEN** 执行 `ak config`
- **THEN** 打印五个动作 flag 的用法说明，以 0 退出

#### Scenario: 离线可用

- **WHEN** daemon 未运行，执行 `ak config --set port=7311`
- **THEN** 文件被写入并校验通过，输出中说明 daemon 当前离线、下次启动生效

#### Scenario: 动作互斥

- **WHEN** 同时给定 `--init` 与 `--get port`
- **THEN** 以非零码退出并说明一次只能执行一类动作

### Requirement: config --set 写入

`ak config --set key=value` SHALL 仅接受 `[daemon]` 表的已知键白名单：`log_level`、`log_dir`、`monitor_interval`、`host`、`port`、`auth_token`、`log_buffer_lines`、`app_dir`，值按目标字段类型解析（`port` 为 1–65535 整数、`monitor_interval` 为正浮点、`log_buffer_lines` 为非负整数、`log_level` 限定既有日志级别集合、其余为字符串/路径）；未知键或类型不符 MUST 拒绝并以非零码退出，不落盘。写入 SHALL 采用读-改-写：文件中未被本次触碰的键、顺序与注释 SHALL 保持原样；写后 SHALL 对整个配置做整体校验，校验失败 MUST 回滚为原文件内容（磁盘上不留半成品）并以非零码退出。daemon 配置文件不存在时 set MUST 拒绝并提示先执行 `--init`。`--set` SHALL 可重复以一次写入多个键。成功 SHALL 打印写入的键与新值；目标 daemon 在线时 SHALL 提示需 reload 生效。

#### Scenario: 强类型拒绝

- **WHEN** 执行 `ak config --set port=abc` 或 `ak config --set foo=1`
- **THEN** 非零码退出并说明类型不符/未知键，配置文件内容不变

#### Scenario: 校验失败回滚

- **WHEN** 一次 `--set` 写入使整体配置非法（如 `--set log_level=verbose`）
- **THEN** 命令非零退出，文件内容保持写入前的原样（含注释）

#### Scenario: 未触碰内容保持

- **WHEN** 对一份带手工注释与自定义键序的合法配置执行 `--set port=8080`
- **THEN** 仅 port 值改变，其余键的顺序、值与全部注释保持原样

#### Scenario: 缺失文件拒绝

- **WHEN** daemon 配置文件不存在，执行 `ak config --set port=7311`
- **THEN** 非零码退出，提示先执行 `ak config --init`

### Requirement: config --get 读取

`ak config --get <key>` SHALL 接受与 set 相同的白名单裸键名，打印该键的生效值：配置文件中显式设置了该键 SHALL 打印文件值，未设置 SHALL 打印内置默认值（如 `port` 未配置打印 7310，`log_dir` 未配置打印平台缺省路径）。输出 SHALL 仅为值本身（便于脚本消费）。未知键 MUST 以非零码拒绝。配置文件不存在时 SHALL 等价于全默认配置（所有键返回内置默认值）。`--get` SHALL 可重复以一次读取多个键（每键一行 `key=value` 形式；单键时仅打印值）。

#### Scenario: 回退内置默认

- **WHEN** 配置文件存在但未写 `port`，执行 `ak config --get port`
- **THEN** 打印 `7310`，以 0 退出

#### Scenario: 文件值优先

- **WHEN** 配置文件写了 `port = 8080`，执行 `ak config --get port`
- **THEN** 打印 `8080`

#### Scenario: 文件缺失全默认

- **WHEN** daemon 配置文件不存在，执行 `ak config --get host`
- **THEN** 打印 `127.0.0.1`

### Requirement: config --delete 删除

`ak config --delete <key>` SHALL 按 TOML 点路径解析删除目标：省略表名的裸键 SHALL 视为 `[daemon]` 表内叶键的简写（`port` 等价 `daemon.port`）；`表.键` 形式 SHALL 删除指定表内的叶键；点路径解析命中已知表名时 SHALL 删除整张表。可删除目标 SHALL 受与 set/get 相同的已知键白名单约束（本能力当前仅 `[daemon]` 8 键；后续能力扩展键表后自动纳入同套机制）。删除 SHALL 使目标回到「未配置」态：有内置默认值的键随删除回退默认值；未被触碰的键、顺序与注释 SHALL 保持原样；写后 SHALL 整体校验并原子落盘（校验失败回滚不落盘），daemon 在线时 SHALL 提示需 reload 生效。目标在文件中本不存在时 SHALL 幂等成功（文件不变，输出说明无变化）；未知键或未知表 MUST 以非零码拒绝且文件不变。daemon 配置文件不存在时 delete MUST 拒绝并提示先执行 `--init`。`--delete` SHALL 可重复以一次删除多个目标。

#### Scenario: 删键回退内置默认

- **WHEN** 配置文件写了 `port = 8080`，执行 `ak config --delete port`
- **THEN** 文件中 `port` 行被移除、其余内容（含注释与键序）原样，`config --get port` 此后打印默认值 `7310`

#### Scenario: 目标不存在幂等成功

- **WHEN** 配置文件未设置 `host`，执行 `ak config --delete host`
- **THEN** 以 0 退出，文件内容不变，输出说明该键本未设置

#### Scenario: 未知键拒绝

- **WHEN** 执行 `ak config --delete foo` 或 `ak config --delete webui`（当前键表无此表）
- **THEN** 非零码退出并说明未知键，配置文件内容不变

#### Scenario: 删除与回滚

- **WHEN** 一次 `--delete` 使整体配置非法（如删除必填后无法通过校验的组合场景）
- **THEN** 命令非零退出，文件内容保持写入前的原样（含注释）

### Requirement: config --edit 编辑

`ak config --edit` SHALL 沿用原 `edit` 子命令语义：用 `$VISUAL`（优先）或 `$EDITOR`（均未设置时取平台默认：Unix `vi`、Windows `notepad`）打开解析后的 daemon 配置路径；配置文件不存在时 SHALL 先连同父目录创建再打开；编辑器退出后 SHALL 校验该配置，合法以 0 退出、非法逐条输出错误并以 2 退出且文件保持编辑器保存的内容不回滚；编辑器启动失败或异常退出以非零码退出。

#### Scenario: 编辑后校验通过

- **WHEN** 编辑器中写入合法配置并正常退出
- **THEN** 校验通过，以 0 退出

#### Scenario: 保存非法配置

- **WHEN** 编辑器中写入未知字段并退出
- **THEN** 错误逐条列出、退出码 2，文件保持编辑器保存的内容

### Requirement: config --init 初始化

`ak config --init` SHALL 在解析后的 daemon 配置路径生成全新配置文件（父目录不存在时先创建）：渲染 `[daemon]` 表全部已知键（log_level、log_dir、monitor_interval、host、port、auth_token、log_buffer_lines、app_dir）并赋内置默认值，逐键附说明注释；`auth_token` SHALL 渲染为空值并附设置指引注释；另 SHALL 渲染一段全部注释掉的 `[app-default]` 模板段作为提示；`app_dir` SHALL 渲染相对值 `apps`（注释说明其相对配置文件所在目录解析）。目标位置已存在任何配置文件时 MUST 拒绝（非零码退出），MUST NOT 覆盖或修改既有文件。init SHALL 连带初始化 app 配置目录：按渲染的 `app_dir` 创建目录，并在其中生成 `example.toml.sample`——内容为全部注释掉的 app 配置模板（`[app]` 表与 `[program.*]` 样例），已存在时 SHALL 跳过且不报错。成功 SHALL 打印生成的配置文件路径、app 配置目录与后续步骤提示（如 `ak config --edit`、`ak add`）。

#### Scenario: 全新机器初始化

- **WHEN** 在既无配置文件也无 app 目录的机器执行 `ak config --init`
- **THEN** 生成含全部默认值与注释的 daemon 配置文件、app 配置目录及 `example.toml.sample`，打印路径与下一步提示，以 0 退出

#### Scenario: 拒绝覆盖既有配置

- **WHEN** daemon 配置文件已存在，执行 `ak config --init`
- **THEN** 非零码退出，说明文件已存在拒绝初始化，既有文件内容不变

#### Scenario: example 样例不参与加载

- **WHEN** init 完成后执行 `ak list`（或触发注册表扫描）
- **THEN** `example.toml.sample` 不作为应用出现（`.sample` 后缀不被注册表扫描）

#### Scenario: 重复 init 幂等安全

- **WHEN** 连续两次执行 `ak config --init`（第二次前已手工删除 daemon 配置文件但保留 app 目录与 example）
- **THEN** 第二次成功重建配置文件，`example.toml.sample` 保持原样未被改写

### Requirement: restart 字段域与旧字段兼容

`restart` SHALL 是唯一的启动/重启策略字段（docker 字面量语义），值域为字符串 `no` | `on-failure` | `always` | `unless-stopped`：

- `no`：退出后不重启；守护进程启动（及 apply 引入新程序）时不自动拉起。
- `on-failure`：退出码不在 `exit_codes` 期望列表（默认仅 0）时重启；守护进程启动时拉起。
- `always`：无论退出码如何都重启；守护进程启动时无条件拉起（不受用户停止记录影响）。
- `unless-stopped`：无论退出码如何都重启；守护进程启动时若无「用户停止」持久化记录则拉起，有记录则保持停止（记录的写入与清除见 process-management 能力的重启策略 requirement）。

解析 SHALL 接受宽容写法：布尔 `false` 等价 `"no"`；四层级联到底均未设置（留空）时 SHALL 取内置默认 `"no"`。守护语义的开箱体验由模板引导：`ak config --init` 生成的 `[app-default]` SHALL 显式写入 `restart = "always"`，仓库随附的 daemon 配置模板同步。

旧字段兼容（弃用别名，仅读取）：`autorestart`（`never` 映射 `no`，`on-failure`/`always` 原值）与 `autostart`（布尔，应用级）SHALL 继续可读，读取时按以下规则归并为 `restart`：`autostart = false` → `no`；`autostart = true`（或未设置）且 `autorestart = X` → `X` 的映射值；`autostart = true` 且 `autorestart = "never"` → `no`，并 SHALL 输出迁移警告（该组合的「守护启动拉起」行为在新模型中不存在）。同一层级同时显式出现新旧字段 SHALL 被校验拒绝（歧义，指明二者不得混用）。所有写入路径（add 微调 flag、run ad-hoc、脚手架生成、模板）SHALL 只写新字段 `restart`。

#### Scenario: false 与留空等价 no

- **WHEN** 程序声明 `restart = false`，另一程序未声明 `restart` 且各级层级均未设置
- **THEN** 两个程序均按 `no` 运行：退出不重启，守护进程启动不拉起

#### Scenario: 新旧字段同现被拒绝

- **WHEN** 同一程序同时声明 `restart = "always"` 与 `autorestart = "on-failure"`
- **THEN** 校验失败，错误指明新旧字段不得混用，配置被拒绝

#### Scenario: 旧字段组合迁移告警

- **WHEN** 既有 app 配置声明 `autostart = true` 与 `autorestart = "never"`
- **THEN** 按 `restart = "no"` 读取并输出迁移警告，说明「守护启动拉起 + 不重启」组合在新模型中不存在

#### Scenario: init 模板保持开箱守护

- **WHEN** 执行 `ak config --init` 后以模板默认值注册并启动程序
- **THEN** 模板 `[app-default]` 的 `restart = "always"` 生效，程序崩溃自动拉起、守护重启自动恢复

### Requirement: 动作定义表

app 配置 SHALL 支持在 `[program.<name>]` 下声明自定义动作表 `[program.<name>.action.<action-name>]`：map 键即动作名。字段 SHALL 仅有 `command`（字符串，必填；支持 TOML 多行字符串书写长命令，或显式调用外部脚本并自选解释器）与 `timeout`（秒数，可省略，缺省 30，必须大于 0）。`command` 的执行行为契约见 actions 规范。内置变量 SHALL 以 `${域.名.字段}` 形式引用，可用变量 SHALL 为：`program.<name>.pid`、`program.<name>.state`、`program.<name>.app`、`program.<name>.work_dir`、`program.<name>.log_dir`、`app.<name>.path`、`daemon.pid`、`daemon.host`、`daemon.port`、`daemon.log_dir`、`daemon.app_dir`；跨程序引用 SHALL 允许（程序名全局唯一）。程序未运行时其 `pid` 变量 SHALL 替换为空串。

#### Scenario: 最小动作定义可用

- **WHEN** 程序 api 声明 `[program.api.action.flush]` 且 `command = "curl -fsS http://127.0.0.1:8080/flush"`
- **THEN** validate 通过，动作可经 `appkeeper action api flush` 执行

#### Scenario: 多行长命令与显式脚本调用

- **WHEN** 动作 command 使用 TOML 多行字符串书写多行命令，或书写 `bash scripts/upgrade.sh` 调用外部脚本
- **THEN** 校验通过，执行时命令文本整体交给平台 shell，脚本相对路径按程序 work_dir 解析

#### Scenario: timeout 缺省与非法值

- **WHEN** 动作未写 timeout，或写 `timeout = 0` / 负数
- **THEN** 未写时以 30 秒缺省生效；0 与负数校验失败并指明字段

### Requirement: webui 配置段

daemon 配置的 `[webui]` 段 SHALL 由 config 子命令按统一键表机制管理：`config --set webui.listen=<addr>` 写入监听地址（写入即开启控制台，reload 后生效或热生效）、`config --get webui.listen` 打印生效值（段未配置时 SHALL 回退内置默认 `127.0.0.1:9877`）、`config --delete webui` 删除整段（即关闭控制台）。键表机制、类型校验、读改写与原子落盘 SHALL 与 `[daemon]` 键一致（见 config 子命令相关 requirement）。`webui.listen` 值 SHALL 为非空的 `host:port` 形式：无法解析或为空 SHALL 被校验拒绝（写入期与加载期一致拒绝）；`[webui]` 段内未知键 SHALL 被校验拒绝。`[webui]` 段不新增鉴权键：控制台访问控制沿用既有机制，不在本段范围。

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
