# control-plane Delta: refactor-cli-verbs

## MODIFIED Requirements

### Requirement: CLI 控制命令

单二进制 SHALL 同时承担守护进程与客户端两种角色。守护进程 SHALL 由 `ak daemon`
子命令族运行（`start` / `install` / `uninstall` / `shutdown`，见 `background-daemon`
与 `service-registration` 能力规范）；`ak run` 为客户端动词（docker run 式心智：向守护
进程发指令），SHALL NOT 承载任何守护入口语义：

- `daemon start` 为唯一守护运行入口：缺省后台拉起、就绪即返回（即原 `--detach` 语义，
  该旗标已删除）；`daemon start --foreground` 前台运行（Ctrl+C / SIGTERM 优雅停机，
  供 systemd ExecStart、调试使用）；拉起协议见 `background-daemon` 能力规范。裸
  `ak daemon`（无子命令）SHALL 以退出码 2 报 usage 错误并列出全部子命令。
- `run <name>` / `run --app <app>` 为启动形态：启动已注册的程序或按既定顺序扇出启动整个 app 的程序，语义与控制面动作一致（经本地 API 操作守护进程；对已 `running` 程序启动返回 409，未知程序返回 404），即原 `start` 动词的语义——`start` 动词 SHALL 移除，调用 SHALL 得到未知子命令错误（含改用 `run` 的提示）；
- run 的 ad-hoc 形态（`-- <命令行>` 或 `--cmd "<字符串>"`，二选一）：命令行由守护进程注册并立即生效，注册落盘顺序为「先启动成功、后落盘」（语义见 `app-registry` 能力规范）；
- 守护形态的 `run`（裸 `run`、`run --detach`、仅带 `--config` 的 `run`）SHALL 被移除并在参数解析前拦截：以退出码 2 失败，错误提示按形态分流——`--detach` 形态提示改用 `ak daemon start`；仅带 `--config` 的形态（systemd 旧 ExecStart 特征）SHALL 额外提示用 `ak daemon install --force --now` 重新生成 unit（见 `service-registration`）。

`run` 的**全部四种形态** SHALL 统一为 daemon-first：CLI SHALL NOT 自行读写注册目录或自行拉起/操作进程，而是把意图交给守护进程，由守护进程完成启动与（ad-hoc 形态的）落盘，并把结果回传 CLI。`run` SHALL 阻塞等待守护进程返回最终结果为止，MUST NOT 提前退出。守护进程离线时 `run` 的**全部形态** SHALL 输出不可达提示并以退出码 3 退出，MUST NOT 隐式后台拉起守护进程。

`run` 的每种形态 SHALL 返回并打印所涉程序的 `{程序名, 配置路径, PID}`：单程序形态输出一行；`run --app <app>` 扇出形态输出逐程序一行（含程序名、配置路径、PID 与结果状态），全部程序成功才以 0 退出，任一程序失败则以非零码退出并逐项列出失败者与原因。`run` SHALL 支持 `--json` 输出等价的机器可读结构（单程序对象 / 扇出数组），供脚本消费（`run` 的 `--json` 旗标维持不变，`--out` 仅存在于 `ps`）。

`run` 失败时（命令无法解析、启动失败、健康探测未通过、落盘失败等）SHALL 在 stderr 明确展示守护进程回传的错误信息，退出码遵循既有约定（配置/解析类 2、一般错误 1、守护不可达 3），且 MUST NOT 因失败在 `app_dir` 留下注册残留。

裸 `ak`（无子命令）SHALL 输出与 `ak --help` 等价的完整 help 到 stdout 并以退出码 0 退出，MUST NOT 启动守护进程。新语义下裸 `ak run`（无程序名、无 `--app`、无 `--`/`--cmd`、无 `--detach`）SHALL 报用法错误并列出三种客户端形态（`run <name>` / `run --app <app>` / `run -- <cmd...>`），以退出码 2 退出。

`ak ps|stop|restart|reload|apply|log|pid` 为其余控制子命令（通过本地 API 操作守护进程）；`ak daemon shutdown` SHALL 停止全部程序并退出守护进程（原顶层 `shutdown` 动词，客户端语义不变）；`ak compose|remove` 为应用注册子命令（见 `app-registry`）。`ak apply` SHALL 支持范围参数（缺省全部 / `<app>` / `<app> <program>`）与 `--restart` 开关，输出逐程序的变更与动作结果。

`ak log` SHALL 支持一次查看多个程序：`log <p1> [p2...] [-f] [--tail N] [--stream out|err|both]`。多程序时 SHALL 按 `[name] ` 前缀标识每行归属；单程序时输出 MUST NOT 添加前缀。`--stream both` SHALL 合并双流输出。`-f/--follow` 跟随输出与 `--tail N` 语义保持不变。

`ak ps` SHALL 默认输出人读表格：每程序一行，列与 `appkeeper shell` 的 status 表格一致（NAME、APP、STATE、PID、RESTARTS、UNHEALTHY），并 SHALL 增列注册表信息（配置本体路径；存在描述时一并展示）；unhealthy 程序 SHALL 带标记。`ak ps --out json` SHALL 输出与 `GET /v1/status` 等价的 pretty JSON（`--out table` 为缺省，取值仅 `json|table`）。`ak ps --all` SHALL 展示全量视图：除守护进程已加载的程序外，还包含已注册但未运行/未 apply 的 app（由 CLI 本地读注册表与 `/v1` 投影在客户端合并，控制面 API 零变化）。守护进程离线时：不带 `--all` 的 `ps` SHALL 输出不可达提示并以退出码 3 退出；`ps --all` SHALL 降级为本地注册表清单（名称、配置本体路径、描述），输出注明 daemon offline，以退出码 0 退出；`--all --out json` 组合在降级时 SHALL 输出仅含注册表的 JSON 数组。原顶层 `status` 与 `list` 动词 SHALL 移除：调用 SHALL 得到未知子命令错误（无别名、无迁移拦截）。

`appkeeper action <program> <action>` SHALL 执行自定义动作并输出退出码与输出尾部，CLI 退出码 SHALL 透传动作自身的退出码（调用失败为 1，守护不可达为 3）；`appkeeper signal <program> <SIGNAL>` SHALL 投递白名单信号；`stop|restart --app <name>` SHALL 执行 app 级扇出（服务端按序展开），裸 `<name>` 参数维持既有程序名语义不变（启动扇出由 `run --app` 承接）。

控制命令 SHALL 使用如下退出码约定：0 成功（含 apply 无变更的幂等情形、裸 `ak` 的 help 输出、`ps --all` 的离线降级清单）；1 一般错误（如非法状态转换、未知程序、注册校验失败）；2 配置错误（含被拦截的守护形态 `run`、无操作数的裸 `run` 用法错误、裸 `ak daemon` 的 usage 错误、命令无法解析）；3 守护进程不可达。

#### Scenario: 守护进程未运行

- **WHEN** 守护进程未启动时执行 `ak ps`
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

#### Scenario: run 启动已注册程序

- **WHEN** 执行 `ak run web` 且 `web` 为已注册的已停止程序
- **THEN** 请求经控制面 API 下发，程序进入运行状态，输出含程序名、配置路径与 PID；退出码 0

#### Scenario: CLI 扇出启动

- **WHEN** 执行 `ak run --app gateway`
- **THEN** 服务端按排序规则逐程序启动，输出逐程序结果；未知 app 报错退出码 1

#### Scenario: run 扇出启动整个 app

- **WHEN** 执行 `ak run --app demo` 且 demo 的全部程序成功拉起
- **THEN** 输出逐程序一行（程序名、配置路径、PID、状态），全部成功时退出码 0；任一失败时非零退出并列出失败项

#### Scenario: run --json 机器可读

- **WHEN** 执行 `ak run --app demo --json`
- **THEN** stdout 输出等价结构的 JSON（含每程序 name/config/pid/status），可被脚本解析

#### Scenario: run 阻塞至守护进程返回

- **WHEN** 执行 `ak run web` 且程序需要数秒才判定启动成功
- **THEN** 命令持续等待，直到收到守护进程的最终结果才打印并退出，MUST NOT 在结果返回前退出

#### Scenario: run 离线退出 3 且不拉起

- **WHEN** 守护进程未运行时执行 `ak run web` 或 `ak run -- ./server --name web`
- **THEN** 输出守护进程不可达提示，退出码 3，MUST NOT 后台拉起守护进程，`app_dir` 不变

#### Scenario: run 失败不污染注册目录

- **WHEN** 执行 `ak run -- /nonexistent/binary --name bad` 且守护进程在线
- **THEN** stderr 明确展示启动失败原因，以非零码退出，`app_dir` 中不出现 `bad.toml` 或任何占位残留

#### Scenario: start 动词已移除

- **WHEN** 执行 `ak start web`
- **THEN** 报未知子命令错误并提示改用 `ak run web`，退出非零

#### Scenario: 旧动词已移除

- **WHEN** 执行 `ak status`、`ak list`、`ak shutdown`、`ak add` 或 `ak service ...`
- **THEN** 报未知子命令错误（无迁移提示、无别名），退出非零

#### Scenario: 裸 ak 输出 help

- **WHEN** 执行 `ak`（不带任何子命令与参数）
- **THEN** 完整 help 输出到 stdout（与 `ak --help` 等价，含子命令列表），退出码 0，不启动守护进程

#### Scenario: 裸 daemon 报 usage 错误

- **WHEN** 执行 `ak daemon`（无子命令）
- **THEN** 以退出码 2 报 usage 错误并列出 `start` / `install` / `uninstall` / `shutdown` 四个子命令，不启动守护进程

#### Scenario: daemon 前台守护

> 本变更将前台形态移至 `daemon start --foreground`（标题为既有场景锚点，现行行为以下文为准）。

- **WHEN** 执行 `ak daemon start --foreground`
- **THEN** 守护进程前台运行，Ctrl+C 优雅停机

#### Scenario: daemon shutdown 停机

- **WHEN** 守护进程在线时执行 `ak daemon shutdown`
- **THEN** 全部程序被优雅停止后守护进程退出，命令以 0 退出；守护进程离线时以退出码 3 报不可达

#### Scenario: 守护形态 run 被拦截并提示

- **WHEN** 执行 `ak run --detach`
- **THEN** 以退出码 2 失败，错误信息提示改用 `ak daemon start`，不启动任何守护进程

#### Scenario: 仅带 config 的守护形态 run 提示 unit 重新生成

- **WHEN** 执行 `ak run --config /etc/appkeeper/daemon.toml`（无其他操作数，systemd 旧 ExecStart 形态）
- **THEN** 以退出码 2 失败，错误信息提示改用 `ak daemon start --foreground`，并提示既有 systemd unit 须以 `ak daemon install --force --now` 重新生成

#### Scenario: 无操作数的裸 run 报用法错误

- **WHEN** 执行 `ak run`（新语义下无程序名、无 `--app`、无 `--`/`--cmd`）
- **THEN** 以退出码 2 失败，错误信息列出三种客户端形态（`run <name>` / `run --app <app>` / `run -- <cmd...>`）

#### Scenario: status 默认输出人读表格

> 本变更将 `status` 并入 `ps`（标题为既有场景锚点，现行行为以下文为准）。

- **WHEN** 执行 `ak ps` 且守护进程在线、存在运行中与已停止的程序
- **THEN** 输出每程序一行的表格，列与 shell 的 status 表格一致（NAME/APP/STATE/PID/RESTARTS/UNHEALTHY）并含配置本体路径（存在描述时一并展示），不健康程序带标记；无 JSON 大括号输出

#### Scenario: status --json 保留机器可读输出

> 本变更将 `--json` 旗标改为 `ps --out json`（标题为既有场景锚点，现行行为以下文为准）。

- **WHEN** 执行 `ak ps --out json`
- **THEN** 输出与 `GET /v1/status` 等价的 pretty JSON（守护进程信息 + 程序状态投影），供脚本消费

#### Scenario: ps --all 全量视图

- **WHEN** 守护进程在线时执行 `ak ps --all`，存在已注册但未 apply（pending）的 app
- **THEN** 表格同时包含已加载程序与已注册未运行的 app（标注其未运行/未 apply 状态），退出码 0

#### Scenario: ps --all 离线降级

- **WHEN** 守护进程未运行时执行 `ak ps --all`
- **THEN** 输出注明 daemon offline 的本地注册表清单（名称、配置本体路径、描述），退出码 0

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

### Requirement: API 准入控制

控制端点的准入 SHALL 由端点形态唯一决定，两种形态的准入机制 SHALL NOT 混合：

**本地 IPC 形态（缺省）**：请求的准入 SHALL 完全由操作系统依据端点属主判定：unix 上控制 socket 的权限位 SHALL 为 `0600`、其所在 uid 目录 SHALL 为 `0700`，Windows 上控制管道的 DACL SHALL 仅允许守护进程运行账户、SYSTEM 与 Administrators，并显式移除 Everyone / Users / Authenticated Users。此形态下控制面 SHALL NOT 使用应用层凭据：`[daemon] auth_token` MUST NOT 被设置（配置组合校验拒绝），服务端 SHALL 忽略请求携带的任何 `Authorization` 头。非属主用户 SHALL 在 `connect()` 阶段即被内核拒绝，SHALL NOT 收到任何 API 响应，程序状态与日志 MUST NOT 被泄露。

**TCP 形态**：属主模型不适用（TCP 无内核层身份），准入 SHALL 由 `[daemon] auth_token` 承担：除 `/v1/health` 外的所有请求 SHALL 要求 `Authorization: Bearer <token>`，缺失或不匹配 SHALL 返回 401 且不泄露程序状态；`/v1/health` SHALL 免凭据（其响应 MUST NOT 包含任何程序信息，仅存活信号）。`auth_token` 为空的 `tcp://` 配置 SHALL 在启动期被组合校验拒绝（见 `configuration`），MUST NOT 存在「TCP 无 token」的可运行状态。

#### Scenario: 非属主用户无法连接

- **WHEN** 以非守护进程运行账户的用户尝试连接本地 IPC 控制端点
- **THEN** 连接在内核层被拒绝（权限错误），无任何 API 响应返回，程序状态不被泄露

#### Scenario: 属主用户正常连接

- **WHEN** 以守护进程运行账户的用户请求 `/v1/status`
- **THEN** 正常返回 200 响应；响应中不包含任何鉴权相关字段

#### Scenario: 携带旧 token 头不受影响

- **WHEN** 请求携带任意 `Authorization` 头的 `/v1/status`，且调用者为端点属主
- **THEN** 返回 200；服务端不读取、不校验、不记录该头

#### Scenario: TCP 形态拒绝匿名请求

- **WHEN** 配置 `socket = "tcp://..."` 且 `auth_token` 已设置，请求未携带正确凭据的 `/v1/status`
- **THEN** 返回 401，程序状态不被泄露

#### Scenario: TCP 形态放行正确凭据

- **WHEN** 请求携带正确的 Bearer token 的 `/v1/status`
- **THEN** 正常返回 200 响应

#### Scenario: TCP 形态健康探测免凭据

- **WHEN** 未携带凭据请求 TCP 形态的 `/v1/health`
- **THEN** 返回 200 与存活信号；响应不包含任何程序状态

#### Scenario: TCP 无 token 不可启动

- **WHEN** 配置 `socket = "tcp://127.0.0.1:7310"` 但未配置 `auth_token`，执行 `ak validate` 或 `ak daemon start`
- **THEN** 校验失败并说明 tcp:// 形态必须配置 auth_token，守护进程不启动

### Requirement: 控制面连接失败的错误区分

客户端连接控制端点失败时 SHALL 区分失败原因并给出对应提示，退出码均为 3：

- **端点不存在**（守护进程未运行）报「守护进程不可达」，与既有约定一致；
- **本地 IPC 端点存在但当前用户无权连接**（权限错误）SHALL 指明端点属主与当前用户不一致并提示需要更高权限，而不是笼统的「不可达」；
- **TCP 形态凭据被拒**（HTTP 401）SHALL 作为业务答案透传（既有 401 处理路径），提示中说明 `tcp://` 形态需要 `[daemon] auth_token`。

#### Scenario: 守护进程未运行时不可达

- **WHEN** 守护进程未启动时执行 `ak ps`
- **THEN** 输出守护进程不可达的提示，退出码为 3

#### Scenario: 权限不足的专属提示

- **WHEN** 守护进程以 uid 0 运行，而当前用户为 uid 1000，执行 `ak ps`
- **THEN** 退出码为 3，错误信息指出控制端点属主为 uid 0、当前用户为 uid 1000，并提示需要更高权限；MUST NOT 表述为守护进程未运行

#### Scenario: TCP 凭据错误的专属提示

- **WHEN** 配置指向 `tcp://` 端点且 `auth_token` 与守护进程不一致，执行 `ak ps`
- **THEN** 退出码为 3，错误信息包含 401 与「tcp:// 形态需要 auth_token」的提示；MUST NOT 表述为守护进程未运行
