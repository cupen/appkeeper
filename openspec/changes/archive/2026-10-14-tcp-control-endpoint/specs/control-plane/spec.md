## MODIFIED Requirements

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

- **WHEN** 配置 `socket = "tcp://127.0.0.1:7310"` 但未配置 `auth_token`，执行 `ak validate` 或 `ak daemon`
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

- **WHEN** 另一台机器的配置文件写 `socket = "tcp://<host>:7310"` 与对应 `auth_token`，执行 `ak -c <该配置> status`
- **THEN** CLI 经 TCP 端点操作远端守护进程；无需任何额外机制

### Requirement: 控制面连接失败的错误区分

客户端连接控制端点失败时 SHALL 区分失败原因并给出对应提示，退出码均为 3：

- **端点不存在**（守护进程未运行）报「守护进程不可达」，与既有约定一致；
- **本地 IPC 端点存在但当前用户无权连接**（权限错误）SHALL 指明端点属主与当前用户不一致并提示需要更高权限，而不是笼统的「不可达」；
- **TCP 形态凭据被拒**（HTTP 401）SHALL 作为业务答案透传（既有 401 处理路径），提示中说明 `tcp://` 形态需要 `[daemon] auth_token`。

#### Scenario: 守护进程未运行时不可达

- **WHEN** 守护进程未启动时执行 `ak status`
- **THEN** 输出守护进程不可达的提示，退出码为 3

#### Scenario: 权限不足的专属提示

- **WHEN** 守护进程以 uid 0 运行，而当前用户为 uid 1000，执行 `ak status`
- **THEN** 退出码为 3，错误信息指出控制端点属主为 uid 0、当前用户为 uid 1000，并提示需要更高权限；MUST NOT 表述为守护进程未运行

#### Scenario: TCP 凭据错误的专属提示

- **WHEN** 配置指向 `tcp://` 端点且 `auth_token` 与守护进程不一致，执行 `ak status`
- **THEN** 退出码为 3，错误信息包含 401 与「tcp:// 形态需要 auth_token」的提示；MUST NOT 表述为守护进程未运行
