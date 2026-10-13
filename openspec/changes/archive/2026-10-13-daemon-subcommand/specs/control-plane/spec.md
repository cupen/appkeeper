# control-plane 变更增量：daemon-subcommand

## MODIFIED Requirements

### Requirement: CLI 控制命令

单二进制 SHALL 同时承担守护进程与客户端两种角色。守护进程 SHALL 由显式子命令 `ak daemon` 运行；`ak run` 为客户端动词（docker run 式心智：向守护进程发指令），SHALL NOT 承载任何守护入口语义：

- `daemon`（可带 `--detach`）为唯一守护形态：无 `--detach` 时前台运行（Ctrl+C / SIGTERM 优雅停机），后台形态与拉起协议见 `background-daemon` 能力规范；
- `run <name>` / `run --app <app>` 为启动形态：启动已注册的程序或按既定顺序扇出启动整个 app 的程序，语义与控制面动作一致（经本地 API 操作守护进程；对已 `running` 程序启动返回 409，未知程序返回 404），即原 `start` 动词的语义——`start` 动词 SHALL 移除，调用 SHALL 得到未知子命令错误（含改用 `run` 的提示）；
- run 的 ad-hoc 形态（`-- <命令行>` 或 `--cmd "<字符串>"`，二选一）：命令行注册并立即生效（注册语义见 `app-registry` 能力规范，离线拉起见 `background-daemon`）；
- 守护形态的 `run`（裸 `run`、`run --detach`、仅带 `--config` 的 `run`）SHALL 被移除并在参数解析前拦截：以退出码 2 失败，错误提示按形态分流——`--detach` 形态提示改用 `ak daemon --detach`；仅带 `--config` 的形态（systemd 旧 ExecStart 特征）SHALL 额外提示用 `ak service install --force --now` 重新生成 unit（见 `service-registration`）。

裸 `ak`（无子命令）SHALL 输出与 `ak --help` 等价的完整 help 到 stdout 并以退出码 0 退出，MUST NOT 启动守护进程。新语义下裸 `ak run`（无程序名、无 `--app`、无 `--`/`--cmd`、无 `--detach`）SHALL 报用法错误并列出三种客户端形态（`run <name>` / `run --app <app>` / `run -- <cmd...>`），以退出码 2 退出。

`ak status|stop|restart|reload|apply|log|pid|shutdown` 为其余控制子命令（通过本地 API 操作守护进程），`ak add|remove|list` 为应用注册子命令（离线直接读写 daemon 配置，在线时经控制 API 同步）。`ak apply` SHALL 支持范围参数（缺省全部 / `<app>` / `<app> <program>`）与 `--restart` 开关，输出逐程序的变更与动作结果。

`ak log` SHALL 支持一次查看多个程序：`log <p1> [p2...] [-f] [--tail N] [--stream out|err|both]`。多程序时 SHALL 按 `[name] ` 前缀标识每行归属；单程序时输出 MUST NOT 添加前缀。`--stream both` SHALL 合并双流输出。`-f/--follow` 跟随输出与 `--tail N` 语义保持不变。

`ak status` SHALL 默认输出人读表格：每程序一行，列与 `appkeeper shell` 的 status 表格一致（NAME、APP、STATE、PID、RESTARTS、UNHEALTHY），unhealthy 程序 SHALL 带标记；`--json` SHALL 输出与 `GET /v1/status` 等价的 pretty JSON（作为脚本迁移路径）。守护进程离线时两种形态均输出不可达提示并以退出码 3 退出。

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
