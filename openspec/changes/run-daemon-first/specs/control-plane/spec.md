## MODIFIED Requirements

### Requirement: CLI 控制命令

单二进制 SHALL 同时承担守护进程与客户端两种角色。守护进程 SHALL 由显式子命令 `ak daemon` 运行；`ak run` 为客户端动词（docker run 式心智：向守护进程发指令），SHALL NOT 承载任何守护入口语义：

- `daemon`（可带 `--detach`）为唯一守护形态：无 `--detach` 时前台运行（Ctrl+C / SIGTERM 优雅停机），后台形态与拉起协议见 `background-daemon` 能力规范；
- `run <name>` / `run --app <app>` 为启动形态：启动已注册的程序或按既定顺序扇出启动整个 app 的程序，语义与控制面动作一致（经本地 API 操作守护进程；对已 `running` 程序启动返回 409，未知程序返回 404），即原 `start` 动词的语义——`start` 动词 SHALL 移除，调用 SHALL 得到未知子命令错误（含改用 `run` 的提示）；
- run 的 ad-hoc 形态（`-- <命令行>` 或 `--cmd "<字符串>"`，二选一）：命令行由守护进程注册并立即生效，注册落盘顺序为「先启动成功、后落盘」（语义见 `app-registry` 能力规范）；
- 守护形态的 `run`（裸 `run`、`run --detach`、仅带 `--config` 的 `run`）SHALL 被移除并在参数解析前拦截：以退出码 2 失败，错误提示按形态分流——`--detach` 形态提示改用 `ak daemon --detach`；仅带 `--config` 的形态（systemd 旧 ExecStart 特征）SHALL 额外提示用 `ak service install --force --now` 重新生成 unit（见 `service-registration`）。

`run` 的**全部四种形态** SHALL 统一为 daemon-first：CLI SHALL NOT 自行读写注册目录或自行拉起/操作进程，而是把意图交给守护进程，由守护进程完成启动与（ad-hoc 形态的）落盘，并把结果回传 CLI。`run` SHALL 阻塞等待守护进程返回最终结果为止，MUST NOT 提前退出。守护进程离线时 `run` 的**全部形态** SHALL 输出不可达提示并以退出码 3 退出，MUST NOT 隐式后台拉起守护进程。

`run` 的每种形态 SHALL 返回并打印所涉程序的 `{程序名, 配置路径, PID}`：单程序形态输出一行；`run --app <app>` 扇出形态输出逐程序一行（含程序名、配置路径、PID 与结果状态），全部程序成功才以 0 退出，任一程序失败则以非零码退出并逐项列出失败者与原因。`run` SHALL 支持 `--json` 输出等价的机器可读结构（单程序对象 / 扇出数组），供脚本消费。

`run` 失败时（命令无法解析、启动失败、健康探测未通过、落盘失败等）SHALL 在 stderr 明确展示守护进程回传的错误信息，退出码遵循既有约定（配置/解析类 2、一般错误 1、守护不可达 3），且 MUST NOT 因失败在 `app_dir` 留下注册残留。

裸 `ak`（无子命令）SHALL 输出与 `ak --help` 等价的完整 help 到 stdout 并以退出码 0 退出，MUST NOT 启动守护进程。新语义下裸 `ak run`（无程序名、无 `--app`、无 `--`/`--cmd`、无 `--detach`）SHALL 报用法错误并列出三种客户端形态（`run <name>` / `run --app <app>` / `run -- <cmd...>`），以退出码 2 退出。

`ak status|stop|restart|reload|apply|log|pid|shutdown` 为其余控制子命令（通过本地 API 操作守护进程），`ak add|remove|list` 为应用注册子命令（离线直接读写 daemon 配置，在线时经控制 API 同步）。`ak apply` SHALL 支持范围参数（缺省全部 / `<app>` / `<app> <program>`）与 `--restart` 开关，输出逐程序的变更与动作结果。

`ak log` SHALL 支持一次查看多个程序：`log <p1> [p2...] [-f] [--tail N] [--stream out|err|both]`。多程序时 SHALL 按 `[name] ` 前缀标识每行归属；单程序时输出 MUST NOT 添加前缀。`--stream both` SHALL 合并双流输出。`-f/--follow` 跟随输出与 `--tail N` 语义保持不变。

`ak status` SHALL 默认输出人读表格：每程序一行，列与 `appkeeper shell` 的 status 表格一致（NAME、APP、STATE、PID、RESTARTS、UNHEALTHY），unhealthy 程序 SHALL 带标记；`--json` SHALL 输出与 `GET /v1/status` 等价的 pretty JSON（即本变更之前的默认输出，作为脚本迁移路径）。守护进程离线时两种形态均输出不可达提示并以退出码 3 退出。

`appkeeper action <program> <action>` SHALL 执行自定义动作并输出退出码与输出尾部，CLI 退出码 SHALL 透传动作自身的退出码（调用失败为 1，守护不可达为 3）；`appkeeper signal <program> <SIGNAL>` SHALL 投递白名单信号；`stop|restart --app <name>` SHALL 执行 app 级扇出（服务端按序展开），裸 `<name>` 参数维持既有程序名语义不变（启动扇出由 `run --app` 承接）。

控制命令 SHALL 使用如下退出码约定：0 成功（含 apply 无变更的幂等情形、裸 `ak` 的 help 输出）；1 一般错误（如非法状态转换、未知程序、注册校验失败）；2 配置错误（含被拦截的守护形态 `run` 与无操作数的裸 `run` 用法错误、命令无法解析）；3 守护进程不可达。

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

## ADDED Requirements

### Requirement: 注册并启动端点

守护进程 SHALL 提供「注册并启动」端点（ad-hoc `run` 的服务端实现），接受一个尚未注册的临时应用定义（命令、参数、环境变量、工作目录、名称、restart/backoff 等微调 flag），并 SHALL 按以下顺序处理：

1. 校验定义（名称合法且非保留字 `all`、命令可解析为绝对路径、程序名不与已注册程序冲突）；
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

- **WHEN** 提交的定义其应用名或程序名与已注册者冲突
- **THEN** 守护进程以校验错误回传，不启动任何进程、不写盘

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
