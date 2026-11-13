## MODIFIED Requirements

### Requirement: app 配置

每个应用的配置本体 SHALL 存放在其部署目录，默认文件名 `appkeeper.toml`，注册时可指定任意路径。app 配置 SHALL 由可选的 `[app]` 表（应用级默认值与 `description` 元数据，注册微调 flag 的落点）与一或多个 `[program.<name>]` 映射表组成：映射键即程序名，MUST NOT 再写 `name` 字段。程序字段 SHALL 采用简化写法：`work_dir`（工作目录）、`env`（环境变量表）、`log`（日志轮转表，下详）、`health_check`（单字符串，按协议前缀分发：`http(s)://` 为 HTTP 探测、`tcp://` 为 TCP 连通、其余视为探测命令行）；`health_interval`/`health_timeout`/`health_retries`/`health_start_period` 为可选节奏字段。`command` SHALL 支持单行带参写法（如 `"python -m http.server 8000"`，按 shell 词法规则拆分为 argv、不经过 shell），与显式 `args` 数组互斥。

`log` SHALL 是仅含可选字段 `max_size`（大小字符串，如 `"10MB"`；`"0"` 显式禁用轮转）与 `keep`（非负整数，保留归档份数）的表，缺省分别为 `50MB` 与 `2`。内联表写法（`log = { max_size = "10MB", keep = 2 }`）与 section 写法（`[program.<name>.log]`）SHALL 等价；两种写法都不得在同一程序上重复声明。`keep` 与 `max_size` 在 `[program.*]`、`[app]`、daemon `[app-default]` 三层均可用作默认值，按字段分别合并。旧扁平键 `log_max_size` / `log_rotate_keep` SHALL 被校验拒绝（破坏性变更），错误信息 MUST 给出改写后的 `log` 表片段。

#### Scenario: 最小 app 配置可用

- **WHEN** 部署目录 `appkeeper.toml` 只写 `[program.api]` 与其 `command = "python -m http.server 8000"`
- **THEN** `ak compose .` 即可注册，程序名为 `api`，参数经拆分后按默认值运行

#### Scenario: 健康检查按协议分发

- **WHEN** 程序声明 `health_check = "tcp://127.0.0.1:5432"`
- **THEN** 该程序的健康检查为 TCP 连通探测，无需再声明类型字段

#### Scenario: 缺省阈值触发轮转

- **WHEN** 程序未声明 `log` 表，某流日志超过 50MB 后继续输出
- **THEN** 当前文件被轮转并压缩归档，该流盘上最多保留 2 份归档

#### Scenario: 显式禁用轮转

- **WHEN** 程序声明 `log = { max_size = "0" }`
- **THEN** 该程序的日志不轮转，文件持续追加不设上限

#### Scenario: log 内联表与 section 等价

- **WHEN** 一个程序写 `log = { max_size = "10MB", keep = 3 }`，另一个程序写 `[program.x.log]` 下的 `max_size = "10MB"` 与 `keep = 3`
- **THEN** 两个程序解析出的日志策略完全相同

#### Scenario: 旧日志键被拒绝并给出迁移片段

- **WHEN** 程序仍声明 `log_max_size = "10MB"` 或 `log_rotate_keep = 2`
- **THEN** 校验失败，错误信息指出 `log_max_size` / `log_rotate_keep` 已移除，并给出 `log = { max_size = "10MB", keep = 2 }` 形式的改写片段

### Requirement: 字段优先级

同一字段 SHALL 按以下优先级取值（从高到低）：`[program.*]` 显式字段 > app 配置 `[app]` 表（注册微调 flag 落点，部署人可手改）> daemon `[app-default]` > 内置默认。`priority` 为应用级字段，仅存在于 `[app]` 与 `[app-default]` 层；`restart`（取代既有 `autorestart` 与 `autostart` 两个旧字段）为程序级字段，可在 `[app]` 与 `[app-default]` 层作应用级默认值。`log` 表 SHALL 按表内字段逐个参与上述优先级合并，而不是整表替换：程序只声明 `log.max_size` 时，`log.keep` 仍从更高层未设置时回落到 `[app]`、`[app-default]` 与内置默认。

#### Scenario: app 层默认生效

- **WHEN** `[app]` 设置 `restart = "on-failure"`，程序未显式声明
- **THEN** 该应用内程序按 on-failure 运行

#### Scenario: daemon 默认兜底

- **WHEN** daemon `[app-default]` 设 `restart_backoff = 2.0`，app 配置未在任何层级声明
- **THEN** 程序按 2 秒重启间隔运行

#### Scenario: 程序显式字段最高

- **WHEN** 某程序显式声明 `restart = "no"`，而 `[app]` 与 `[app-default]` 均为 always
- **THEN** 该程序按 no 运行，同应用其他程序不受影响

#### Scenario: log 表逐字段合并

- **WHEN** `[app]` 设置 `log = { keep = 5 }`，某程序只声明 `log = { max_size = "10MB" }`
- **THEN** 该程序的有效策略为 `max_size = "10MB"`、`keep = 5`，未因整表替换丢失 app 层默认
