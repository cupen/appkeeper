# control-plane Delta: refine-cli-verbs

## MODIFIED Requirements

### Requirement: CLI 控制命令

单二进制 SHALL 同时承担守护进程与客户端两种角色。守护进程 SHALL 由显式子命令族 `ak daemon`（`start` / `install` / `uninstall` / `shutdown`）运行；客户端动词分工：`ak start` 启动已注册的程序/app（与 `stop` 对偶），`ak run` 仅承载 ad-hoc 注册（注册并跑一个新的命令行），SHALL NOT 承载任何守护入口语义：

- `daemon start` 为唯一守护运行入口：缺省后台拉起、就绪即返回；`daemon start --foreground` 前台运行（Ctrl+C / SIGTERM 优雅停机）；裸 `ak daemon` 以退出码 2 报 usage 错误并列出子命令。后台形态与拉起协议见 `background-daemon` 能力规范；
- `start <app> [<program>]` 为启动形态（原 `run` 启动形态的承接者，与 `stop`/`restart`/`apply` 同构的双位置范围参数）：裸 `<app>` 按既定顺序扇出启动整个 app 的全部程序，第二位置参数收窄到该 app 内的单个程序；语义与控制面动作一致（经本地 API 操作守护进程；对已 `running` 程序启动返回 409，未知 app/程序返回 404）；
- run 的 ad-hoc 形态（`-- <命令行>` 或 `--cmd "<字符串>"`，二选一）：命令行由守护进程注册并立即生效，注册落盘顺序为「先启动成功、后落盘」（语义见 `app-registry` 能力规范）；ad-hoc 的应用命名参数为 `--app`（原 `--name` 改名；`ak run -- python -m http.server --app web`）；
- 守护形态的 `run`（裸 `run`、`run --detach`、仅带 `--config` 的 `run`）SHALL 被移除并在参数解析前拦截：以退出码 2 失败，错误提示按形态分流——`--detach` 形态提示改用 `ak daemon start`；仅带 `--config` 的形态（systemd 旧 ExecStart 特征）SHALL 额外提示用 `ak daemon install --force --now` 重新生成 unit（见 `service-registration`）。

`run` 的 ad-hoc 形态与 `start` 的全部形态 SHALL 统一为 daemon-first：CLI SHALL NOT 自行读写注册目录或自行拉起/操作进程，而是把意图交给守护进程，由守护进程完成启动与（ad-hoc 形态的）落盘，并把结果回传 CLI。`start` 与 ad-hoc `run` SHALL 阻塞等待守护进程返回最终结果为止，MUST NOT 提前退出。守护进程离线时两者的全部形态 SHALL 输出不可达提示并以退出码 3 退出，MUST NOT 隐式后台拉起守护进程。

`start` 与 ad-hoc `run` 的每种形态 SHALL 返回并打印所涉程序的 `{程序名, 配置路径, PID}`：单程序形态输出一行；`--app` 扇出形态输出逐程序一行（含程序名、配置路径、PID 与结果状态），全部程序成功才以 0 退出，任一程序失败则以非零码退出并逐项列出失败者与原因。`start` 与 `run` SHALL 支持 `--json` 输出等价的机器可读结构（单程序对象 / 扇出数组），供脚本消费。

`run` 失败时（命令无法解析、启动失败、健康探测未通过、落盘失败等）SHALL 在 stderr 明确展示守护进程回传的错误信息，退出码遵循既有约定（配置/解析类 2、一般错误 1、守护不可达 3），且 MUST NOT 因失败在 `app_dir` 留下注册残留。

裸 `ak`（无子命令）SHALL 输出与 `ak --help` 等价的完整 help 到 stdout 并以退出码 0 退出，MUST NOT 启动守护进程。帮助信息 SHALL 简洁明了（动词一句话说明，不堆细节）；顶层 help 末尾 SHALL 附三行基本用法示例，演示「启动 daemon → 启动 app → 停止 app」的最小工作流。裸 `ak run`（无 `--`/`--cmd`）SHALL 报用法错误并列出 ad-hoc 的两种形态（`run -- <cmd...>` / `run --cmd "<字符串>"`），以退出码 2 退出。

`ak ps|start|stop|restart|apply|log|pid` 为其余控制子命令（通过本地 API 操作守护进程）；`ak daemon shutdown` SHALL 停止全部程序并退出守护进程；`ak compose|remove` 为应用注册子命令（离线直接读写 daemon 配置，在线时经控制 API 同步，见 `app-registry`）。`ak apply` SHALL 支持范围参数（缺省全部 / `<app>` / `<app> <program>`）与 `--restart` 开关，输出逐程序的变更与动作结果。顶层 `reload` 动词 SHALL 移除，重载入口迁至 `ak config --reload`（daemon 配置面动作：经既有 `/v1/reload` 端点触发守护进程重读配置并立即重扫，组合规则与退出码见 `configuration`「config 子命令」；控制面 API 零变化）；`ak reload` 调用 SHALL 得到未知子命令错误。

`ak log` SHALL 支持一次查看多个程序：`log <p1> [p2...] [-f] [--tail N] [--stream out|err|both]`。多程序时 SHALL 按 `[name] ` 前缀标识每行归属；单程序时输出 MUST NOT 添加前缀。`--stream both` SHALL 合并双流输出。`-f/--follow` 跟随输出与 `--tail N` 语义保持不变。

`ak ps` SHALL 默认输出人读表格：每程序一行，列与 shell 的 `ps` 表格一致（共享渲染器：NAME、APP、STATE、PID、RESTARTS、UNHEALTHY）并增列配置本体路径与描述，以及 **PENDING** 列——磁盘配置与运行定义存在待应用差异的程序 SHALL 带变更标记（如 changed/new），无差异为空；app 级注册增删（apps_added/apps_removed）、daemon 级 hints 与检出错误 SHALL 以表格尾部的摘要行呈现；unhealthy 程序 SHALL 带标记；`--out json` SHALL 输出与 `GET /v1/status` 等价的 pretty JSON（`--out table` 为缺省；pending 信息投影已含于其中，`--out json` 形态零新增字段）。`--all` SHALL 展示全量视图：除守护进程已加载的程序外，还包含已注册但未运行/未 apply 的 app（由 CLI 本地读注册表与 `/v1` 投影在客户端合并），其程序行同样带 PENDING 标记。守护进程离线时：不带 `--all` 的 `ps` SHALL 输出不可达提示并以退出码 3 退出；`ps --all` SHALL 降级为本地注册表清单（名称、配置本体路径、描述）并注明 daemon offline，以退出码 0 退出。

`appkeeper action <program> <action>` SHALL 执行自定义动作并输出退出码与输出尾部，CLI 退出码 SHALL 透传动作自身的退出码（调用失败为 1，守护不可达为 3）；`appkeeper signal <program> <SIGNAL>` SHALL 投递白名单信号；`pid`/`log`/`action`/`signal` 为进程面动词，SHALL 以全局唯一的程序名无前缀寻址（单程序 app 的程序名与 app 名相同，帮助文案 SHALL 注明）。`start|stop|restart` SHALL 采用 `<app> [<program>]` 双位置参数（裸 `<app>` 整 app 扇出，服务端按序展开；第二参数收窄单程序），原 `--app` 旗标 SHALL 移除，`ak start --app x` 类调用得到 clap 未知参数错误。

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

> 本变更将启动形态移至 `start`（标题为既有场景锚点，现行行为以下文为准）。

- **WHEN** 执行 `ak start web` 且 `web` 为已注册的已停止程序
- **THEN** 请求经控制面 API 下发，程序进入运行状态，输出含程序名、配置路径与 PID；退出码 0

#### Scenario: CLI 扇出启动

- **WHEN** 执行 `ak start gateway`
- **THEN** 服务端按排序规则逐程序启动，输出逐程序结果；未知 app 报错退出码 1

#### Scenario: run 扇出启动整个 app

> 本变更将扇出形态移至 `start <app>`（标题为既有场景锚点，现行行为以下文为准）。

- **WHEN** 执行 `ak start demo` 且 demo 的全部程序成功拉起
- **THEN** 输出逐程序一行（程序名、配置路径、PID、状态），全部成功时退出码 0；任一失败时非零退出并列出失败项

#### Scenario: run --json 机器可读

> `--json` 由 `start` 与 ad-hoc `run` 共同支持（标题为既有场景锚点，现行行为以下文为准）。

- **WHEN** 执行 `ak start demo --json`
- **THEN** stdout 输出等价结构的 JSON（含每程序 name/config/pid/status），可被脚本解析

#### Scenario: run 阻塞至守护进程返回

- **WHEN** 执行 `ak start web`（或 `ak run -- ./server --app web`）且程序需要数秒才判定启动成功
- **THEN** 命令持续等待，直到收到守护进程的最终结果才打印并退出，MUST NOT 在结果返回前退出

#### Scenario: run 离线退出 3 且不拉起

- **WHEN** 守护进程未运行时执行 `ak start web` 或 `ak run -- ./server --app web`
- **THEN** 输出守护进程不可达提示，退出码 3，MUST NOT 后台拉起守护进程，`app_dir` 不变

#### Scenario: run 失败不污染注册目录

- **WHEN** 执行 `ak run -- /nonexistent/binary --app bad` 且守护进程在线
- **THEN** stderr 明确展示启动失败原因，以非零码退出，`app_dir` 中不出现 `bad.toml` 或任何占位残留

#### Scenario: start 动词已移除

> 历史锚点：`start` 曾被移除并改用 `run` 启动；本变更起 `start` 为正式启动动词（与 `stop` 对偶），拦截器已删除。

- **WHEN** 执行 `ak start web` 且 `web` 为已注册的已停止程序
- **THEN** 程序经控制面启动成功，退出码 0（不再报「已移除」错误）

#### Scenario: 裸 ak 输出 help

- **WHEN** 执行 `ak`（不带任何子命令与参数）
- **THEN** 完整 help 输出到 stdout（与 `ak --help` 等价，含子命令列表），每个动词一句话说明；末尾附三行基本用法示例（`ak daemon start` 启动守护进程、`ak start <app>` 启动应用、`ak stop <app>` 停止应用），退出码 0，不启动守护进程

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

> 本变更后 `run` 仅剩 ad-hoc 两形态（标题为既有场景锚点，现行行为以下文为准）。

- **WHEN** 执行 `ak run`（无 `--`、无 `--cmd`）
- **THEN** 以退出码 2 失败，错误信息列出 ad-hoc 的两种形态（`run -- <cmd...>` / `run --cmd "<字符串>"`）

#### Scenario: run 的 ad-hoc 命名参数为 --app

- **WHEN** 执行 `ak run -- python -m http.server 8000 --app web`
- **THEN** 注册的应用名为 `web`；`--name` 不再是合法参数（clap 报未知参数，退出码 2）

#### Scenario: status 默认输出人读表格

- **WHEN** 执行 `ak ps` 且守护进程在线、存在运行中与已停止的程序
- **THEN** 输出每程序一行的表格，列与 shell 的 `ps` 表格一致（NAME/APP/STATE/PID/RESTARTS/UNHEALTHY）并含配置本体路径与描述，不健康程序带标记；无 JSON 大括号输出

#### Scenario: status --json 保留机器可读输出

- **WHEN** 执行 `ak ps --out json`
- **THEN** 输出与本变更之前默认输出等价的 pretty JSON（守护进程信息 + 程序状态投影），供脚本消费

#### Scenario: ps 直显待应用变更

- **WHEN** 修改某运行中程序的磁盘配置并等待一个重扫周期后执行 `ak ps`
- **THEN** 该程序行的 PENDING 列带变更标记（changed），其余程序为空；app 级注册增删以表格尾部摘要行呈现；执行 `ak apply` 后标记消失

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

- **WHEN** 执行 `ak start nosuch`
- **THEN** 返回 404 的控制面错误信息并以退出码 1 退出

#### Scenario: 旧动词已移除

- **WHEN** 执行 `ak status`、`ak list`、`ak shutdown`、`ak add`、`ak service ...` 或 `ak reload`
- **THEN** 报未知子命令错误（无迁移提示、无别名），退出非零

#### Scenario: 裸 daemon 报 usage 错误

- **WHEN** 执行 `ak daemon`（无子命令）
- **THEN** 以退出码 2 报 usage 错误并列出四个子命令，不启动守护进程

#### Scenario: daemon shutdown 停机

- **WHEN** 守护进程在线时执行 `ak daemon shutdown`
- **THEN** 全部程序被优雅停止后守护进程退出，命令以 0 退出；守护进程离线时以退出码 3 报不可达

#### Scenario: ps --all 全量视图

- **WHEN** 守护进程在线时执行 `ak ps --all`，存在已注册但未 apply（pending）的 app
- **THEN** 表格同时包含已加载程序与已注册未运行的 app（标注其未运行/未 apply 状态），退出码 0

#### Scenario: ps --all 离线降级

- **WHEN** 守护进程未运行时执行 `ak ps --all`
- **THEN** 输出注明 daemon offline 的本地注册表清单（名称、配置本体路径、描述），退出码 0

### Requirement: shell 子命令

单二进制 SHALL 另提供客户端入口 `appkeeper shell`：交互式 REPL（详见
`shell-client` 能力规范），经控制面 API 与根进程通信，`-e "<命令>"` 进入单命令
模式。SHALL 遵循既有退出码约定：0 成功；1 一般错误；2 配置错误；3 守护进程
不可达。（原 `appkeeper system webui` 随 webui 子命令一并移除，控制台改为
daemon 配置驱动，见 webui-api 能力规范。）

#### Scenario: shell 纳入退出码约定

- **WHEN** 守护进程未启动时执行 `appkeeper shell -e "ps"`
- **THEN** 输出守护进程不可达提示，退出码为 3
