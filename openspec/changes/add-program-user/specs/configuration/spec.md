## MODIFIED Requirements

### Requirement: app 配置

每个应用的配置本体 SHALL 存放在其部署目录，默认文件名 `appkeeper.toml`，注册时可指定任意路径。app 配置 SHALL 由可选的 `[app]` 表（应用级默认值与 `description` 元数据，注册微调 flag 的落点）与一或多个 `[program.<name>]` 映射表组成：映射键即程序名，MUST NOT 再写 `name` 字段。程序字段 SHALL 采用简化写法：`work_dir`（工作目录）、`env`（环境变量表）、`user`（Unix 运行身份：用户名或数字 uid；缺省不写表示继承守护进程的有效用户，MUST NOT 把用户名写入配置文件）、`group`（Unix 主组：组名或数字 gid；缺省取目标用户的 passwd 主组，可单独使用而不声明 `user`）、`umask`（Unix 文件创建掩码：八进制字符串，如 `"0027"`；缺省继承守护进程 umask）、`log_max_size`/`log_rotate_keep`（日志轮转，无需独立 section；缺省 `50MB` 保留 `2` 份，即每流盘上最多 3 个文件；`log_max_size = "0"` 显式禁用轮转）、`health_check`（单字符串，按协议前缀分发：`http(s)://` 为 HTTP 探测、`tcp://` 为 TCP 连通、其余视为探测命令行）；`health_interval`/`health_timeout`/`health_retries`/`health_start_period` 为可选节奏字段。`command` SHALL 支持单行带参写法（如 `"python -m http.server 8000"`，按 shell 词法规则拆分为 argv、不经过 shell），与显式 `args` 数组互斥。`user` SHALL 在配置解析期解析为 uid/gid/附加组/HOME 等身份信息：值既非系统账号也非可解析的数字 uid 时 SHALL 被校验拒绝。`group` SHALL 同样在解析期解析为 gid：组名不存在或数字 gid 不可解析 SHALL 被校验拒绝。`umask` SHALL 在解析期按八进制校验：非八进制字符、超过 4 位或大于 `0o777` 的值 SHALL 被校验拒绝。三个字段在 Windows 平台出现 SHALL 被校验拒绝（明确说明仅 Unix 支持）。

#### Scenario: 最小 app 配置可用

- **WHEN** 部署目录 `appkeeper.toml` 只写 `[program.api]` 与其 `command = "python -m http.server 8000"`
- **THEN** `ak compose .` 即可注册，程序名为 `api`，参数经拆分后按默认值运行

#### Scenario: 健康检查按协议分发

- **WHEN** 程序声明 `health_check = "tcp://127.0.0.1:5432"`
- **THEN** 该程序的健康检查为 TCP 连通探测，无需再声明类型字段

#### Scenario: 缺省阈值触发轮转

- **WHEN** 程序未声明任何轮转字段，某流日志超过 50MB 后继续输出
- **THEN** 当前文件被轮转为 `.1`，该流盘上最多保留 3 个文件（当前 + 2 份轮转）

#### Scenario: 显式禁用轮转

- **WHEN** 程序声明 `log_max_size = "0"`
- **THEN** 该程序的日志不轮转，文件持续追加不设上限

#### Scenario: 未声明 user 继承守护进程身份

- **WHEN** 程序未声明 `user`，守护进程以 uid 1000 运行
- **THEN** 该程序以 uid 1000 拉起，解析依据是守护进程的有效 uid，而不是 CLI 进程的 `$USER` 环境变量

#### Scenario: user 指向不存在的账号被拒绝

- **WHEN** 程序声明 `user = "no-such-account"` 且系统中不存在该账号
- **THEN** `ak validate` 与 `ak compose up` 预检均报校验错误，且不启动任何进程、不写盘

#### Scenario: Windows 声明 user 被拒绝

- **WHEN** 在 Windows 上加载声明了 `user`、`group` 或 `umask` 的程序配置
- **THEN** 校验失败并明确提示这些字段仅 Unix 支持

#### Scenario: 数字 uid 被接受

- **WHEN** 程序声明 `user = "1000"` 且系统中存在 uid 1000 的账号
- **THEN** 该程序按 uid 1000 的身份解析并通过校验

#### Scenario: group 缺省取目标用户主组

- **WHEN** 程序声明 `user = "deploy"` 且未声明 `group`
- **THEN** 该程序的主组为 deploy 的 passwd 主组

#### Scenario: group 显式覆盖主组

- **WHEN** 程序声明 `user = "deploy"` 与 `group = "www-data"`
- **THEN** 该程序以 deploy 的 uid、www-data 的 gid 运行，deploy 的附加组仍保留

#### Scenario: group 独立于 user 使用

- **WHEN** 程序只声明 `group = "www-data"` 而未声明 `user`
- **THEN** 运行用户仍为守护进程的有效用户，主组切换为 www-data

#### Scenario: 不存在的 group 被拒绝

- **WHEN** 程序声明 `group = "no-such-group"`
- **THEN** `ak validate` 与 compose 预检均报校验错误，不启动进程、不写盘

#### Scenario: umask 按八进制解析

- **WHEN** 程序声明 `umask = "0027"`
- **THEN** 解析得到 `0o027` 并通过校验

#### Scenario: 非法 umask 被拒绝

- **WHEN** 程序声明 `umask = "08"` 或 `umask = "1000"`
- **THEN** 校验失败并指出该字段必须是取值不超过 `0o777` 的八进制字符串
