# control-plane Specification

## Purpose

定义 appkeeper 的本地控制平面：守护进程暴露的 HTTP JSON API（端点、鉴权、错误码）与映射到该 API 的 CLI 控制命令（含退出码约定），使用户可以查询和操控运行中的守护进程。

## Requirements

### Requirement: 本地 HTTP API

守护进程 SHALL 在**控制端点**上提供 JSON API，控制端点由 `[daemon] socket` 决定且恰为两种形态之一：

- **本地 IPC 形态**（缺省）：本地 Unix domain socket（Windows 上为命名管道）；
- **TCP 形态**：`socket = "tcp://host:port"` 声明的 TCP 端点。

同一守护进程同一时刻 SHALL 只监听其中一种形态。端点上承载的协议仍为 HTTP/1.1：端点集合、请求与响应结构、状态码语义均不因形态而改变。API SHALL 提供以下端点：

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

#### Scenario: 守护进程不监听任何控制面 TCP 端口

- **WHEN** 守护进程以缺省（本地 IPC 形态）配置启动完成
- **THEN** 进程不监听任何控制面 TCP 端口；运维可直接用
  `curl --unix-socket <socket 路径> http://localhost/v1/status` 查询状态

#### Scenario: TCP 形态提供同一套端点

- **WHEN** 配置 `socket = "tcp://127.0.0.1:7310"` 且携带正确 token 请求 `GET /v1/status`
- **THEN** 返回与本地 IPC 形态字节等价的 JSON；端点集合、状态码与错误语义不因形态改变

### Requirement: CLI 控制命令

单二进制 SHALL 同时承担守护进程与客户端两种角色。守护进程 SHALL 由显式子命令族 `ak daemon`（`start` / `install` / `uninstall` / `shutdown`）运行；`ak run` 为客户端动词（docker run 式心智：向守护进程发指令），SHALL NOT 承载任何守护入口语义：

- `daemon start` 为唯一守护运行入口：缺省后台拉起、就绪即返回；`daemon start --foreground` 前台运行（Ctrl+C / SIGTERM 优雅停机）；裸 `ak daemon` 以退出码 2 报 usage 错误并列出子命令。后台形态与拉起协议见 `background-daemon` 能力规范；
- `run <name>` / `run --app <app>` 为启动形态：启动已注册的程序或按既定顺序扇出启动整个 app 的程序，语义与控制面动作一致（经本地 API 操作守护进程；对已 `running` 程序启动返回 409，未知程序返回 404），即原 `start` 动词的语义——`start` 动词 SHALL 移除，调用 SHALL 得到未知子命令错误（含改用 `run` 的提示）；
- run 的 ad-hoc 形态（`-- <命令行>` 或 `--cmd "<字符串>"`，二选一）：命令行由守护进程注册并立即生效，注册落盘顺序为「先启动成功、后落盘」（语义见 `app-registry` 能力规范）；
- 守护形态的 `run`（裸 `run`、`run --detach`、仅带 `--config` 的 `run`）SHALL 被移除并在参数解析前拦截：以退出码 2 失败，错误提示按形态分流——`--detach` 形态提示改用 `ak daemon start`；仅带 `--config` 的形态（systemd 旧 ExecStart 特征）SHALL 额外提示用 `ak daemon install --force --now` 重新生成 unit（见 `service-registration`）。

`run` 的**全部四种形态** SHALL 统一为 daemon-first：CLI SHALL NOT 自行读写注册目录或自行拉起/操作进程，而是把意图交给守护进程，由守护进程完成启动与（ad-hoc 形态的）落盘，并把结果回传 CLI。`run` SHALL 阻塞等待守护进程返回最终结果为止，MUST NOT 提前退出。守护进程离线时 `run` 的**全部形态** SHALL 输出不可达提示并以退出码 3 退出，MUST NOT 隐式后台拉起守护进程。

`run` 的每种形态 SHALL 返回并打印所涉程序的 `{程序名, 配置路径, PID}`：单程序形态输出一行；`run --app <app>` 扇出形态输出逐程序一行（含程序名、配置路径、PID 与结果状态），全部程序成功才以 0 退出，任一程序失败则以非零码退出并逐项列出失败者与原因。`run` SHALL 支持 `--json` 输出等价的机器可读结构（单程序对象 / 扇出数组），供脚本消费。

`run` 失败时（命令无法解析、启动失败、健康探测未通过、落盘失败等）SHALL 在 stderr 明确展示守护进程回传的错误信息，退出码遵循既有约定（配置/解析类 2、一般错误 1、守护不可达 3），且 MUST NOT 因失败在 `app_dir` 留下注册残留。

裸 `ak`（无子命令）SHALL 输出与 `ak --help` 等价的完整 help 到 stdout 并以退出码 0 退出，MUST NOT 启动守护进程。新语义下裸 `ak run`（无程序名、无 `--app`、无 `--`/`--cmd`、无 `--detach`）SHALL 报用法错误并列出三种客户端形态（`run <name>` / `run --app <app>` / `run -- <cmd...>`），以退出码 2 退出。

`ak ps|stop|restart|reload|apply|log|pid` 为其余控制子命令（通过本地 API 操作守护进程）；`ak daemon shutdown` SHALL 停止全部程序并退出守护进程；`ak compose|remove` 为应用注册子命令（离线直接读写 daemon 配置，在线时经控制 API 同步，见 `app-registry`）。`ak apply` SHALL 支持范围参数（缺省全部 / `<app>` / `<app> <program>`）与 `--restart` 开关，输出逐程序的变更与动作结果。

`ak log` SHALL 支持一次查看多个程序：`log <p1> [p2...] [-f] [--tail N] [--stream out|err|both]`。多程序时 SHALL 按 `[name] ` 前缀标识每行归属；单程序时输出 MUST NOT 添加前缀。`--stream both` SHALL 合并双流输出。`-f/--follow` 跟随输出与 `--tail N` 语义保持不变。

`ak ps` SHALL 默认输出人读表格：每程序一行，列与 `appkeeper shell` 的 status 表格一致（NAME、APP、STATE、PID、RESTARTS、UNHEALTHY）并增列配置本体路径与描述；unhealthy 程序 SHALL 带标记；`--out json` SHALL 输出与 `GET /v1/status` 等价的 pretty JSON（`--out table` 为缺省）。`--all` SHALL 展示全量视图：除守护进程已加载的程序外，还包含已注册但未运行/未 apply 的 app（由 CLI 本地读注册表与 `/v1` 投影在客户端合并）。守护进程离线时：不带 `--all` 的 `ps` SHALL 输出不可达提示并以退出码 3 退出；`ps --all` SHALL 降级为本地注册表清单（名称、配置本体路径、描述）并注明 daemon offline，以退出码 0 退出。

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

#### Scenario: 裸 ak 输出 help

- **WHEN** 执行 `ak`（不带任何子命令与参数）
- **THEN** 完整 help 输出到 stdout（与 `ak --help` 等价，含子命令列表），退出码 0，不启动守护进程

#### Scenario: daemon 前台守护

- **WHEN** 执行 `ak daemon start --foreground`
- **THEN** 守护进程前台运行（与本变更之前裸 `ak run` 的行为一致），Ctrl+C 优雅停机

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

- **WHEN** 执行 `ak ps` 且守护进程在线、存在运行中与已停止的程序
- **THEN** 输出每程序一行的表格，列与 shell 的 status 表格一致（NAME/APP/STATE/PID/RESTARTS/UNHEALTHY）并含配置本体路径与描述，不健康程序带标记；无 JSON 大括号输出

#### Scenario: status --json 保留机器可读输出

- **WHEN** 执行 `ak ps --out json`
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

### Requirement: shell 子命令

单二进制 SHALL 另提供客户端入口 `appkeeper shell`：交互式 REPL（详见
`shell-client` 能力规范），经控制面 API 与根进程通信，`-e "<命令>"` 进入单命令
模式。SHALL 遵循既有退出码约定：0 成功；1 一般错误；2 配置错误；3 守护进程
不可达。（原 `appkeeper system webui` 随 webui 子命令一并移除，控制台改为
daemon 配置驱动，见 webui-api 能力规范。）

#### Scenario: shell 纳入退出码约定

- **WHEN** 守护进程未启动时执行 `appkeeper shell -e "status"`
- **THEN** 输出守护进程不可达提示，退出码为 3

### Requirement: 注册并启动端点

守护进程 SHALL 提供「注册并启动」端点（ad-hoc `run` 的服务端实现），接受一个尚未注册的临时应用定义（命令、参数、环境变量、工作目录、名称、restart/backoff 等微调 flag），并 SHALL 按以下顺序处理：

1. 校验定义（名称合法且非保留字 `all`、命令可解析为绝对路径、程序名不与**其他应用**的已注册程序冲突）；同名应用自身的重复提交不属冲突——它是全量再生/upsert，语义见 `app-registry` 能力规范；
2. 启动该程序并等待判定「启动成功」——进程存活超过 `startsecs`，且该程序若配置了健康探测，则其首轮探测通过；
3. 判定成功后，将定义写入 `app_dir` 完成注册（落盘契约见 `app-registry` 能力规范）。

任一步骤失败时，守护进程 SHALL 回传结构化错误（失败阶段 + 可读原因），且 MUST NOT 在 `app_dir` 留下注册文件或占位残留；已启动但判定失败的进程 SHALL 被停止并清理。

端点 SHALL 回传成功结果，含 `{程序名, 配置路径, PID}`；对扇出（app 级）请求 SHALL 回传逐程序结果数组。

#### Scenario: 成功注册并启动

- **WHEN** 客户端提交合法定义且程序在 `startsecs` 内稳定运行（无健康探测）
- **THEN** 守护进程启动程序、判定成功、写入 `app_dir/<name>.toml`，回传程序名、配置路径与 PID

#### Scenario: 启动失败不留残留

- **WHEN** 客户端提交的命令在 `startsecs` 内退出（或 `startretries` 耗尽进入 `fatal`）
- **THEN** 守护进程回传启动失败的结构化错误，`app_dir` 中不出现该注册文件与占位残留

#### Scenario: 名称冲突被拒绝

- **WHEN** 提交的定义其程序名与**其他应用**的已注册程序冲突
- **THEN** 守护进程以校验错误回传，不启动任何进程、不写盘

#### Scenario: 同名重复提交走再生

- **WHEN** 对已由 ad-hoc 注册的同名应用再次提交定义
- **THEN** 按 `app-registry` 的全量再生/upsert 语义处理（无变化幂等返回现有 PID、有变化重启返回新 PID），MUST NOT 以「已注册」为由拒绝

#### Scenario: 保留字 all 被拒绝

- **WHEN** 提交的定义应用名为 `all`
- **THEN** 守护进程以校验错误回传，不启动进程、不写盘

### Requirement: 启动进度流式反馈

「注册并启动」端点 SHALL 在程序配置了健康探测时向客户端流式回传进度：至少包含开始阶段（含预估等待上限）与最终结果帧；无健康探测时 SHALL 直接回传最终结果，无需中间帧。进度信息 SHALL 至少说明当前所处阶段（spawn 后等待 `startsecs` / 等待健康探测）与预估等待上限，使客户端可据此向用户展示等待进度。

客户端 SHALL 逐行打印进度，使用户在等待期间看到进展；最终结果 SHALL 以成功或失败的形式呈现，失败时完整展示错误原因。

#### Scenario: 有健康探测时流式反馈

- **WHEN** 提交的定义其程序配置了健康探测，且启动判定需要等待
- **THEN** 守护进程流式回传含预估等待上限与阶段信息的进度帧，最终回传成功或失败结果；客户端逐行打印这些进度

#### Scenario: 无健康探测直接返回结果

- **WHEN** 提交的定义其程序未配置健康探测
- **THEN** 端点回传最终结果（成功含 PID 或失败原因），不产生中间进度帧

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

### Requirement: 控制 socket 端点

守护进程 SHALL 通过 `[daemon] socket` 配置的单一控制端点对外服务。该值 SHALL 为 URL 形态自识别的字符串：`tcp://host:port` 前缀声明 TCP 形态（host 与端口 SHALL 可解析，端口 1–65535），其它非空值 SHALL 视为本地 IPC 路径。缺省值为 `/run/appkeeper/<uid>/ak.sock`（unix）与 `\\.\pipe\<用户名>.sock`（Windows），两者均把当前 uid / 用户名纳入路径以支持同机多用户并存。unix 上 `/run/appkeeper` 父目录 SHALL 为 `0755`、其下 uid 子目录 SHALL 为 `0700`，守护进程 SHALL 在缺失时创建这些目录。空值 SHALL 被校验拒绝；`tcp://` 值无法解析 SHALL 被校验拒绝。

客户端 SHALL 从同一配置解析端点形态并拨号，TCP 形态 SHALL 自动附带 `auth_token` 凭据。`ak shell`、`ak run` 及全部控制子命令、后台拉起协议的就绪探测、基准测量工具 SHALL 全部经同一端点通信，MUST NOT 存在绕过该端点的第二条控制面路径。

#### Scenario: 默认路径含 uid 分层

- **WHEN** 未配置 `[daemon] socket`，守护进程以 uid 1000 启动
- **THEN** 控制端点为 `/run/appkeeper/1000/ak.sock`，其所在目录权限为 `0700`

#### Scenario: 同机多用户并存

- **WHEN** root 用户与 uid 1000 用户各自运行一个守护进程实例
- **THEN** 两者使用各自的端点，互不冲突；任一方的客户端无法连接另一方的端点

#### Scenario: 自定义 socket 路径

- **WHEN** 配置 `[daemon] socket = "/tmp/ak-test.sock"` 后启动守护进程
- **THEN** 控制端点创建于该路径，客户端从同一配置解析并成功通信

#### Scenario: TCP 形态启动

- **WHEN** 配置 `[daemon] socket = "tcp://127.0.0.1:7310"` 与 `auth_token` 后启动守护进程
- **THEN** 守护进程监听该 TCP 端点且不创建任何 socket 文件；客户端从同一配置拨号通信

#### Scenario: 非法 socket 值被拒绝

- **WHEN** 配置 `[daemon] socket = ""`（空值）或 `socket = "tcp://127.0.0.1"`（缺端口）或 `socket = "tcp://127.0.0.1:99999"`（端口越界）
- **THEN** 校验失败并拒绝以该配置启动，错误信息指明形态与解析失败原因

#### Scenario: 远程管理即配置

- **WHEN** 另一台机器的配置文件写 `socket = "tcp://<host>:7310"` 与对应 `auth_token`，执行 `ak -c <该配置> ps`
- **THEN** CLI 经 TCP 端点操作远端守护进程；无需任何额外机制

### Requirement: 启动冲突与陈旧端点处理

守护进程启动时若控制端点路径已被占用，SHALL 先尝试连接该端点以判定占用性质：连接成功说明已有实例在运行，SHALL 以明确的错误信息退出（非零码）并提示可能已有实例在运行；连接被拒（无监听者）说明是上次异常退出留下的陈旧端点，SHALL 删除该端点后重建并正常启动。删除前 SHALL 校验该路径确为 socket/管道端点而非任意文件，MUST NOT 删除非端点路径。同机重复启动 SHALL NOT 静默成功。

#### Scenario: 已有实例占用时拒绝启动

- **WHEN** 第二个守护进程实例以相同配置启动，且能连接到既有端点
- **THEN** 启动失败，错误信息指出已有实例正在运行，且既有实例的端点未被删除

#### Scenario: 陈旧端点被清理后正常启动

- **WHEN** 控制 socket 路径存在但连接被拒（上次进程崩溃残留）
- **THEN** 守护进程删除该陈旧端点并重新绑定，正常启动；既有程序不受影响

#### Scenario: 非端点路径不被删除

- **WHEN** 目标路径存在但不是 socket/管道端点
- **THEN** 启动失败并明确报告该路径被非端点文件占用，MUST NOT 删除该文件

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
