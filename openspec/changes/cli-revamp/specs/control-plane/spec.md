## MODIFIED Requirements

### Requirement: CLI 控制命令

单二进制 SHALL 同时承担守护进程与客户端两种角色。`xkeeper run` 为统一入口动词：

- 裸 `run`（可带 `--detach`）为守护进程形态（后台形态与拉起协议见 `background-daemon` 能力规范），无参数时前台运行的行为不变；
- `run <name>` / `run --app <app>` 为启动形态：启动已注册的程序或按既定顺序扇出启动整个 app 的程序，语义与控制面动作一致（经本地 API 操作守护进程；对已 `running` 程序启动返回 409，未知程序返回 404），即原 `start` 动词的语义——`start` 动词 SHALL 移除，调用 SHALL 得到未知子命令错误（含改用 `run` 的提示）；
- run 的 ad-hoc 形态（`-- <命令行>` 或 `--cmd "<字符串>"`，二选一）：命令行注册并立即生效（注册语义见 `app-registry` 能力规范，离线拉起见 `background-daemon`）。

`xkeeper status|stop|restart|reload|apply|log|pid|shutdown` 为其余控制子命令（通过本地 API 操作守护进程），`xkeeper add|remove|list` 为应用注册子命令（离线直接读写 daemon 配置，在线时经控制 API 同步）。`xkeeper apply` SHALL 支持范围参数（缺省全部 / `<app>` / `<app> <program>`）与 `--restart` 开关，输出逐程序的变更与动作结果。

`xkeeper log` SHALL 支持一次查看多个程序：`log <p1> [p2...] [-f] [--tail N] [--stream out|err|both]`。多程序时 SHALL 按 `[name] ` 前缀标识每行归属；单程序时输出 MUST NOT 添加前缀。`--stream both` SHALL 合并双流输出。`-f/--follow` 跟随输出与 `--tail N` 语义保持不变。

`xkeeper status` SHALL 默认输出人读表格：每程序一行，列与 `xkeeper shell` 的 status 表格一致（NAME、APP、STATE、PID、RESTARTS、UNHEALTHY），unhealthy 程序 SHALL 带标记；`--json` SHALL 输出与 `GET /v1/status` 等价的 pretty JSON（即本变更之前的默认输出，作为脚本迁移路径）。守护进程离线时两种形态均输出不可达提示并以退出码 3 退出。

控制命令 SHALL 使用如下退出码约定：0 成功（含 apply 无变更的幂等情形）；1 一般错误（如非法状态转换、未知程序、注册校验失败）；2 配置错误；3 守护进程不可达。

#### Scenario: 守护进程未运行

- **WHEN** 守护进程未启动时执行 `xkeeper status`
- **THEN** 输出守护进程不可达的提示，退出码为 3

#### Scenario: CLI 停止程序

- **WHEN** 执行 `xkeeper stop <name>` 且该程序正在运行
- **THEN** 命令返回成功，程序进入 `stopped`

#### Scenario: CLI 跟随日志

- **WHEN** 执行 `xkeeper log <name> -f`
- **THEN** 先输出最近的日志，随后持续输出新增行，直到用户中断

#### Scenario: apply 无变更幂等退出

- **WHEN** 配置无 pending 时执行 `xkeeper apply`
- **THEN** 输出「无变更」提示，退出码 0，无程序受影响

#### Scenario: run 启动已注册程序

- **WHEN** 执行 `xkeeper run web` 且 `web` 为已注册的已停止程序
- **THEN** 请求经控制面 API 下发，程序进入运行状态（与原 `xkeeper start web` 行为一致）

#### Scenario: run 扇出启动整个 app

- **WHEN** 执行 `xkeeper run --app demo`
- **THEN** demo 的全部程序按既定启动顺序被拉起（与原 `xkeeper start --app demo` 行为一致）

#### Scenario: start 动词已移除

- **WHEN** 执行 `xkeeper start web`
- **THEN** 报未知子命令错误并提示改用 `xkeeper run web`，退出非零

#### Scenario: status 默认输出人读表格

- **WHEN** 执行 `xkeeper status` 且守护进程在线、存在运行中与已停止的程序
- **THEN** 输出每程序一行的表格，列与 shell 的 status 表格一致（NAME/APP/STATE/PID/RESTARTS/UNHEALTHY），不健康程序带标记；无 JSON 大括号输出

#### Scenario: status --json 保留机器可读输出

- **WHEN** 执行 `xkeeper status --json`
- **THEN** 输出与本变更之前默认输出等价的 pretty JSON（守护进程信息 + 程序状态投影），供脚本消费

#### Scenario: log 多程序聚合跟随

- **WHEN** 执行 `xkeeper log web api -f` 且两个程序均有新增输出
- **THEN** 两个程序的行交错输出，每行以 `[web] `/`[api] ` 前缀标识归属，直到用户中断

#### Scenario: log 单程序不加前缀

- **WHEN** 执行 `xkeeper log web --tail 50`
- **THEN** 输出最近 50 行且不带任何前缀，与本变更之前的输出一致

#### Scenario: log 合并双流

- **WHEN** 执行 `xkeeper log web --stream both --tail 20`
- **THEN** out 与 err 两个流的最近行均被输出（跟随形态下实时交错）

#### Scenario: 未知程序报错

- **WHEN** 执行 `xkeeper run nosuch`（不带 `--`）
- **THEN** 返回 404 的控制面错误信息并以退出码 1 退出，提示新命令应使用 `run --` 形态
