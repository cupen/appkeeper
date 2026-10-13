# configuration 变更增量：daemon-subcommand

## MODIFIED Requirements

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
