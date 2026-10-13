# control-plane Specification

## Purpose

定义 appkeeper 的本地控制平面：守护进程暴露的 HTTP JSON API（端点、鉴权、错误码）与映射到该 API 的 CLI 控制命令（含退出码约定），使用户可以查询和操控运行中的守护进程。

## Requirements

### Requirement: 本地 HTTP API

守护进程 SHALL 在 `127.0.0.1` 的可配置端口（`[daemon] port`）上提供 JSON API，MUST NOT 默认绑定非回环地址。API SHALL 提供以下端点：

- `GET /v1/health`：守护进程存活探测
- `GET /v1/status`：守护进程信息与全部程序的状态汇总
- `GET /v1/programs`：程序列表
- `GET /v1/programs/{name}`：单个程序详情（所属应用、状态、pid、存活时长、重启/退出计数、健康状态、等待原因、CPU 利用率与驻留内存（可空，metrics 能力口径）、stdout/stderr 日志输出速率）
- `POST /v1/programs/{name}/start` / `stop` / `restart`
- `POST /v1/programs/{name}/signal`：向该程序的子进程投递白名单信号，请求体指定信号名
- `POST /v1/programs/{name}/actions/{action}`：执行该程序声明的自定义动作，同步等待至完成或超时，返回退出码、耗时与截断输出
- `POST /v1/apps/{name}/start` / `stop` / `restart`：app 级扇出动作，按启动排序（停止为逆序）作用于该 app 全部程序，返回逐程序结果
- `GET /v1/programs/{name}/logs`：按 `stream=out|err`、`tail=N` 查询日志
- `POST /v1/reload`：重扫配置并检出待应用变更，返回 pending 预览（不应用）
- `GET /v1/pending`：查询当前待应用变更（逐程序 changed 清单、app 级注册变化、不可热更字段提示、检出错误）
- `POST /v1/apply`：应用待应用变更，请求体可指定范围（app、program）与 `restart` 布尔参数，返回逐程序结构化结果
- `POST /v1/shutdown`：停止全部程序并退出守护进程

程序详情与状态汇总中的指标字段为增量扩展：仅新增可空字段，既有字段名与
语义不变，既有消费者（含 CLI 控制命令）行为不受影响。

#### Scenario: 状态汇总可查询

- **WHEN** 向运行中的守护进程请求 `GET /v1/status`
- **THEN** 返回 JSON，包含守护进程信息与每个程序的名称、状态、pid 与 unhealthy 标记

#### Scenario: reload 返回 pending 预览

- **WHEN** app 配置修改后向运行中的守护进程请求 `POST /v1/reload`
- **THEN** 返回 pending 预览 JSON（含变更程序清单），无任何程序被停止或重启

#### Scenario: pending 可独立查询

- **WHEN** 配置存在未应用的变更时请求 `GET /v1/pending`
- **THEN** 返回逐程序 changed 清单与 app 级注册变化；无变更时返回空清单

#### Scenario: apply 带范围与 restart 参数

- **WHEN** 请求 `POST /v1/apply`（body 含 `{"app": "A", "restart": false}`）
- **THEN** 仅应用 app A 的 pending，返回逐程序 changed/action/result 的 JSON

#### Scenario: 非法状态转换被拒绝

- **WHEN** 对已经 `running` 的程序发送 `POST /v1/programs/{name}/start`
- **THEN** 返回 409 与解释性错误，程序状态不受影响

#### Scenario: 未知程序返回 404

- **WHEN** 请求不存在的程序名
- **THEN** 返回 404 与错误信息

#### Scenario: shutdown 优雅退出

- **WHEN** 发送 `POST /v1/shutdown`
- **THEN** 守护进程按关闭流程停止全部程序后进程退出

#### Scenario: 详情含指标且向后兼容

- **WHEN** 请求 `GET /v1/programs/{name}` 且该程序运行中
- **THEN** 详情含可空的 cpu_percent、mem_bytes 与日志速率字段；不带新
  字段消费旧 JSON 的既有客户端仍能解析全部既有字段

#### Scenario: app 扇出动作

- **WHEN** 请求 `POST /v1/apps/gateway/start`
- **THEN** gateway 的全部程序按排序规则启动，返回逐程序结果；未知 app 返回 404

#### Scenario: 执行自定义动作

- **WHEN** 请求 `POST /v1/programs/api/actions/flush`
- **THEN** 动作同步执行，返回含退出码、耗时、是否超时与输出尾部的 JSON；未知动作名返回 404

#### Scenario: 动作执行中冲突

- **WHEN** 同一程序的同名动作执行中再次请求
- **THEN** 返回 409 与冲突说明，原执行不受影响

#### Scenario: signal 投递

- **WHEN** 请求 `POST /v1/programs/web/signal`（body `{"signal": "USR1"}`）
- **THEN** 信号投递给该程序子进程并返回成功；非白名单信号返回 400 与白名单说明

### Requirement: API 鉴权

未配置鉴权时，API 仅依赖回环绑定保护。配置了 `[daemon] auth_token` 后，除健康探测外，所有请求 SHALL 要求 `Authorization: Bearer <token>`；缺失或不匹配 SHALL 返回 401。

#### Scenario: 配置 token 后拒绝匿名请求

- **WHEN** 配置了 `auth_token` 且请求未携带正确的 Authorization 头
- **THEN** 返回 401，程序状态不被泄露

#### Scenario: 配置 token 后放行正确请求

- **WHEN** 请求携带正确的 Bearer token
- **THEN** 正常返回 200 响应

### Requirement: CLI 控制命令

单二进制 SHALL 同时承担守护进程与客户端两种角色。守护进程 SHALL 由显式子命令 `ak daemon` 运行；`ak run` 为客户端动词（docker run 式心智：向守护进程发指令），SHALL NOT 承载任何守护入口语义：

- `daemon`（可带 `--detach`）为唯一守护形态：无 `--detach` 时前台运行（Ctrl+C / SIGTERM 优雅停机），后台形态与拉起协议见 `background-daemon` 能力规范；
- `run <name>` / `run --app <app>` 为启动形态：启动已注册的程序或按既定顺序扇出启动整个 app 的程序，语义与控制面动作一致（经本地 API 操作守护进程；对已 `running` 程序启动返回 409，未知程序返回 404），即原 `start` 动词的语义——`start` 动词 SHALL 移除，调用 SHALL 得到未知子命令错误（含改用 `run` 的提示）；
- run 的 ad-hoc 形态（`-- <命令行>` 或 `--cmd "<字符串>"`，二选一）：命令行注册并立即生效（注册语义见 `app-registry` 能力规范，离线拉起见 `background-daemon`）；
- 守护形态的 `run`（裸 `run`、`run --detach`、仅带 `--config` 的 `run`）SHALL 被移除并在参数解析前拦截：以退出码 2 失败，错误提示按形态分流——`--detach` 形态提示改用 `ak daemon --detach`；仅带 `--config` 的形态（systemd 旧 ExecStart 特征）SHALL 额外提示用 `ak service install --force --now` 重新生成 unit（见 `service-registration`）。

裸 `ak`（无子命令）SHALL 输出与 `ak --help` 等价的完整 help 到 stdout 并以退出码 0 退出，MUST NOT 启动守护进程。新语义下裸 `ak run`（无程序名、无 `--app`、无 `--`/`--cmd`、无 `--detach`）SHALL 报用法错误并列出三种客户端形态（`run <name>` / `run --app <app>` / `run -- <cmd...>`），以退出码 2 退出。

`ak status|stop|restart|reload|apply|log|pid|shutdown` 为其余控制子命令（通过本地 API 操作守护进程），`ak add|remove|list` 为应用注册子命令（离线直接读写 daemon 配置，在线时经控制 API 同步）。`ak apply` SHALL 支持范围参数（缺省全部 / `<app>` / `<app> <program>`）与 `--restart` 开关，输出逐程序的变更与动作结果。

`ak log` SHALL 支持一次查看多个程序：`log <p1> [p2...] [-f] [--tail N] [--stream out|err|both]`。多程序时 SHALL 按 `[name] ` 前缀标识每行归属；单程序时输出 MUST NOT 添加前缀。`--stream both` SHALL 合并双流输出。`-f/--follow` 跟随输出与 `--tail N` 语义保持不变。

`ak status` SHALL 默认输出人读表格：每程序一行，列与 `appkeeper shell` 的 status 表格一致（NAME、APP、STATE、PID、RESTARTS、UNHEALTHY），unhealthy 程序 SHALL 带标记；`--json` SHALL 输出与 `GET /v1/status` 等价的 pretty JSON（即本变更之前的默认输出，作为脚本迁移路径）。守护进程离线时两种形态均输出不可达提示并以退出码 3 退出。

`appkeeper action <program> <action>` SHALL 执行自定义动作并输出退出码与输出尾部，CLI 退出码 SHALL 透传动作自身的退出码（调用失败为 1，守护不可达为 3）；`appkeeper signal <program> <SIGNAL>` SHALL 投递白名单信号；`stop|restart --app <name>` SHALL 执行 app 级扇出（服务端按序展开），裸 `<name>` 参数维持既有程序名语义不变（启动扇出由 `run --app` 承接）。

控制命令 SHALL 使用如下退出码约定：0 成功（含 apply 无变更的幂等情形、裸 `ak` 的 help 输出）；1 一般错误（如非法状态转换、未知程序、注册校验失败）；2 配置错误（含被拦截的守护形态 `run` 与无操作数的裸 `run` 用法错误）；3 守护进程不可达。

#### Scenario: 守护进程未运行

- **WHEN** 守护进程未启动时执行 `ak status`
- **THEN** 输出守护进程不可达的提示，退出码为 3

#### Scenario: CLI 停止程序

- **WHEN** 执行 `ak stop <name>` 且该程序正在运行
- **THEN** 命令返回成功，程序进入 `stopped`

#### Scenario: CLI 跟随日志

- **WHEN** 执行 `ak log <name> -f`
- **THEN** 先输出最近的日志，随后持续输出新增行，直到用户中断

#### Scenario: apply 无变更幂等退出

- **WHEN** 配置无 pending 时执行 `ak apply`
- **THEN** 输出「无变更」提示，退出码 0，无程序受影响

#### Scenario: CLI 执行动作并透传退出码

- **WHEN** 执行 `appkeeper action api upgrade` 且动作命令以退出码 3 结束
- **THEN** stdout 展示动作的输出尾部，CLI 以退出码 3 退出

#### Scenario: CLI 发送信号

- **WHEN** 执行 `appkeeper signal web USR1`，随后执行 `appkeeper signal web KILL`
- **THEN** 前者投递成功退出码 0；后者报错（不在白名单）退出码 1

#### Scenario: CLI 扇出启动

- **WHEN** 执行 `ak run --app gateway`
- **THEN** 服务端按排序规则逐程序启动，输出逐程序结果；未知 app 报错退出码 1

#### Scenario: run 启动已注册程序

- **WHEN** 执行 `ak run web` 且 `web` 为已注册的已停止程序
- **THEN** 请求经控制面 API 下发，程序进入运行状态（与原 `ak start web` 行为一致）

#### Scenario: run 扇出启动整个 app

- **WHEN** 执行 `ak run --app demo`
- **THEN** demo 的全部程序按既定启动顺序被拉起（与原 `ak start --app demo` 行为一致）

#### Scenario: start 动词已移除

- **WHEN** 执行 `ak start web`
- **THEN** 报未知子命令错误并提示改用 `ak run web`，退出非零

#### Scenario: 裸 ak 输出 help

- **WHEN** 执行 `ak`（不带任何子命令与参数）
- **THEN** 完整 help 输出到 stdout（与 `ak --help` 等价，含子命令列表），退出码 0，不启动守护进程

#### Scenario: daemon 前台守护

- **WHEN** 执行 `ak daemon`
- **THEN** 守护进程前台运行（与本变更之前裸 `ak run` 的行为一致），Ctrl+C 优雅停机

#### Scenario: 守护形态 run 被拦截并提示

- **WHEN** 执行 `ak run --detach`
- **THEN** 以退出码 2 失败，错误信息提示改用 `ak daemon --detach`，不启动任何守护进程

#### Scenario: 仅带 config 的守护形态 run 提示 unit 重新生成

- **WHEN** 执行 `ak run --config /etc/appkeeper/daemon.toml`（无其他操作数，systemd 旧 ExecStart 形态）
- **THEN** 以退出码 2 失败，错误信息提示改用 `ak daemon`，并提示既有 systemd unit 须以 `ak service install --force --now` 重新生成

#### Scenario: 无操作数的裸 run 报用法错误

- **WHEN** 执行 `ak run`（新语义下无程序名、无 `--app`、无 `--`/`--cmd`）
- **THEN** 以退出码 2 失败，错误信息列出三种客户端形态（`run <name>` / `run --app <app>` / `run -- <cmd...>`）

#### Scenario: status 默认输出人读表格

- **WHEN** 执行 `ak status` 且守护进程在线、存在运行中与已停止的程序
- **THEN** 输出每程序一行的表格，列与 shell 的 status 表格一致（NAME/APP/STATE/PID/RESTARTS/UNHEALTHY），不健康程序带标记；无 JSON 大括号输出

#### Scenario: status --json 保留机器可读输出

- **WHEN** 执行 `ak status --json`
- **THEN** 输出与本变更之前默认输出等价的 pretty JSON（守护进程信息 + 程序状态投影），供脚本消费

#### Scenario: log 多程序聚合跟随

- **WHEN** 执行 `ak log web api -f` 且两个程序均有新增输出
- **THEN** 两个程序的行交错输出，每行以 `[web] `/`[api] ` 前缀标识归属，直到用户中断

#### Scenario: log 单程序不加前缀

- **WHEN** 执行 `ak log web --tail 50`
- **THEN** 输出最近 50 行且不带任何前缀，与本变更之前的输出一致

#### Scenario: log 合并双流

- **WHEN** 执行 `ak log web --stream both --tail 20`
- **THEN** out 与 err 两个流的最近行均被输出（跟随形态下实时交错）

#### Scenario: 未知程序报错

- **WHEN** 执行 `ak run nosuch`（不带 `--`）
- **THEN** 返回 404 的控制面错误信息并以退出码 1 退出，提示新命令应使用 `run --` 形态

### Requirement: 启动冲突处理

守护进程启动时若控制端口已被占用，SHALL 以明确的错误信息退出（非零码），提示可能已有实例在运行；同机重复启动 SHALL NOT 静默成功。

#### Scenario: 端口被占用时拒绝启动

- **WHEN** 第二个守护进程实例以相同配置启动
- **THEN** 启动失败，错误信息指出端口占用与已运行实例的可能

### Requirement: shell 子命令

单二进制 SHALL 另提供客户端入口 `appkeeper shell`：交互式 REPL（详见
`shell-client` 能力规范），经控制面 API 与根进程通信，`-e "<命令>"` 进入单命令
模式。SHALL 遵循既有退出码约定：0 成功；1 一般错误；2 配置错误；3 守护进程
不可达。（原 `appkeeper system webui` 随 webui 子命令一并移除，控制台改为
daemon 配置驱动，见 webui-api 能力规范。）

#### Scenario: shell 纳入退出码约定

- **WHEN** 守护进程未启动时执行 `appkeeper shell -e "status"`
- **THEN** 输出守护进程不可达提示，退出码为 3
