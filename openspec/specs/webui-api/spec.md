# webui-api Specification

## Purpose
定义 appkeeper Web 控制台的伺服与推送契约：控制台服务器与守护进程同进程、
复用控制面的状态投影与命令队列；WebSocket 通道向浏览器推送状态增量与日志
分块；规定各通道的数据格式与效率约束。

## Requirements

### Requirement: 控制台端点复用控制面投影
控制台服务器 SHALL 在自己的回环端口（`appkeeper webui --listen`，默认
`127.0.0.1:9877`）提供 `/api/*` 查询端点，其状态数据 SHALL 与控制面 `/v1/*`
使用同一投影（同构类型、同源数据）：
- `GET /api/health` —— 控制台存活探针；
- `GET /api/overview` —— 守护进程概况 + 全部程序状态（与 `GET /v1/status` 同构）；
- `GET /api/programs`、`GET /api/programs/{name}`；
- `GET /api/programs/{name}/logs?stream=out|err&tail=<n>` —— 取自内存环形缓冲；
- `POST /api/programs/{name}/start|stop|restart` —— 经命令队列由守护循环执行；
- `GET /api/pending` —— 待应用变更清单（与 `GET /v1/pending` 同源）；
- `POST /api/apply` —— 应用待应用变更（与 `POST /v1/apply` 同源：范围参数、
  `restart` 布尔、逐程序结构化结果），经命令队列由守护循环执行。

未知程序 MUST 返回 404 与错误对象；非法状态迁移 MUST 返回 409 与原因。

#### Scenario: 投影与控制面一致
- **WHEN** 控制面 `/v1/status` 与控制台 `/api/overview` 在同一时刻被请求
- **THEN** 两者的程序状态字段语义一致（同源投影）

#### Scenario: pending 与控制面一致
- **WHEN** 控制面 `GET /v1/pending` 与控制台 `GET /api/pending` 在同一时刻被请求
- **THEN** 返回的待应用变更清单一致（同源投影）

#### Scenario: apply 结果与控制面一致
- **WHEN** 分别经 `/api/apply` 与 `/v1/apply` 对同一 pending 状态发起 apply
- **THEN** 两者接受相同的请求参数并返回同构的逐程序结果

#### Scenario: 非法迁移被拒绝
- **WHEN** 对 running 的程序 `POST /api/programs/{name}/start`
- **THEN** 返回 409 与原因，程序状态不受影响

#### Scenario: 日志来自环形缓冲
- **WHEN** 程序日志文件已被轮转后请求 `tail=N`
- **THEN** 返回的最近 N 行来自内存缓冲，与磁盘轮转状态无关

### Requirement: WebSocket 推送通道
控制台 SHALL 在 `/ws` 提供 WebSocket 推送通道：
- 连接建立后服务端 MUST 立即下发一次全量快照（与 `/api/overview` 同构）；
- 其后状态变化 MUST 以增量事件推送（仅变化的程序条目），推送延迟不高于
  一个 monitor_interval 量级；
- 待应用（pending）变更 SHALL 随快照携带，且 pending 变化（检出更新、apply
  清空）MUST 以增量事件推送，推送延迟不高于一个重扫周期量级；
- 客户端 SHALL 可按程序与流方向订阅日志；订阅后先补发环形缓冲的 tail 再
  跟随新行；
- 服务端 MUST 周期发送心跳；连接断开后客户端重连时重新获得全量快照（无跨
  连接续传要求）。

#### Scenario: 连接即快照
- **WHEN** 客户端建立 WebSocket 连接
- **THEN** 首条消息为全量快照（含 pending 状态），其后才是增量事件

#### Scenario: 状态变化推送
- **WHEN** 某程序在守护循环中由 running 进入 backoff
- **THEN** 已连接客户端在一个 monitor_interval 量级内收到该程序的增量状态事件

#### Scenario: pending 变化推送
- **WHEN** 配置被编辑使某程序进入 pending，随后 `ak apply` 清空 pending
- **THEN** 已连接客户端分别在一个重扫周期量级内收到 pending 增加与清空的事件

#### Scenario: 日志订阅先补发后跟随
- **WHEN** 客户端订阅某程序的 stdout 且订阅时缓冲已有内容
- **THEN** 先收到缓冲 tail，再持续收到新输出行

### Requirement: 数据格式
- REST 端点 SHALL 返回 JSON（`application/json`）。
- WebSocket 的结构化消息（快照、状态增量）SHALL 使用二进制帧：1 字节消息
  类型 + MessagePack 载荷；日志分块 SHALL 以二进制帧承载原始 UTF-8 文本
  （不做 JSON/MessagePack 字符串转义）。
- WS 结构化载荷与 REST JSON MUST 来自同一组 serde 结构体（编码同构），
  避免两份字段定义。
- HTTP 响应与 WS 结构化帧 SHOULD 启用压缩（HTTP gzip；WS 帧头压缩标记位 +
  zlib 或等价机制）。

#### Scenario: 编码同构
- **WHEN** 同一快照分别以 JSON 与 MessagePack 编码
- **THEN** 解码后字段与语义完全一致（同一结构体源）

#### Scenario: MessagePack 体积收益
- **WHEN** 对 10 个程序的状态快照分别编码
- **THEN** MessagePack 体积 ≤ 同构 JSON 的 90%

#### Scenario: 日志帧无转义放大
- **WHEN** 一段 100 行的日志文本经日志帧推送
- **THEN** 帧体为原始 UTF-8 字节（加固定头部），不含引号/反斜杠转义放大

### Requirement: 与守护进程同进程共存

控制台服务器 MUST 与守护循环运行在同一进程并共享同一 `Supervisor`：状态读取
SHALL 来自共享状态（与控制面相同的锁），控制命令 SHALL 经命令队列在守护循环
中执行，日志 SHALL 来自 pump 环形缓冲。控制台服务器 SHALL 随守护进程关闭而
退出（Ctrl+C 或 `POST /v1/shutdown`）。`ak daemon` 子命令族 SHALL 是唯一的守护入口
（运行入口为 `ak daemon start`）；控制台是否伺服 SHALL 由 daemon 配置决定（见「配置驱动的控制台生命周期」），
MUST NOT 存在独立的启动模式子命令。

#### Scenario: webui 反映真实状态

- **WHEN** 控制台伺服中且某程序进入 fatal
- **THEN** `GET /api/programs/{name}` 与 WebSocket 推送均反映 fatal

#### Scenario: 随守护进程退出

- **WHEN** 发送 `POST /v1/shutdown` 或 Ctrl+C
- **THEN** 控制台服务器随之退出，进程正常终止

### Requirement: 投影携带指标

`/api/overview` 与 `/ws` 快照/状态增量中的程序条目 SHALL 携带指标能力
（`metrics`）定义的字段：`cpu_percent`（可空）、`mem_bytes`（可空）、
stdout 与 stderr 的日志速率。守护概况 SHALL 携带系统级 CPU 与内存概况。
指标字段 MUST 与控制面 `/v1` 投影同源同构（同一 serde 结构体），且指标
数值的更新节奏 MUST 不低于每程序采样周期（即变化后至多一个采样周期内可
被 REST/WS 消费者观察到）。

#### Scenario: overview 含指标字段

- **WHEN** 请求 `GET /api/overview` 且某程序运行中
- **THEN** JSON 中该程序条目含 cpu_percent、mem_bytes 与日志速率字段，
  与 `GET /v1/status` 同构

#### Scenario: WS 增量携带指标变化

- **WHEN** 某程序的 CPU 利用率在两次推送之间发生变化
- **THEN** WebSocket 状态增量事件包含该程序条目，其中含新的指标值

### Requirement: WS 日志批量推送与背压

`/ws` 日志订阅通道 SHALL 支持批量帧以承载高速输出：

- 单个日志帧 SHALL 可包含多行（原始 UTF-8 文本拼接），帧发送节奏与单帧
  字节数 MUST 有上界；
- 服务端到客户端的订阅队列 MUST 有界；客户端消费不及时时，服务端 SHALL
  丢弃积压批次并发送显式的「跳过 N 行」标记帧（含程序、流方向、丢弃行数），
  标记 MUST 以独立帧类型承载，不得与日志内容混载；
- 向订阅者的投递 MUST 非阻塞：订阅者的慢消费 MUST NOT 反压日志泵对
  子进程管道的排空或落盘路径（零反压约束见 log-management 能力）；
- 丢弃 MUST 只影响该订阅者的实时流：环形缓冲、落盘文件与其他订阅者不受
  影响；
- 「跳过 N 行」标记后，订阅 SHALL 以订阅时刻的实时位置继续（不回放已
  丢弃内容）。

#### Scenario: 多行单帧

- **WHEN** 某程序短时间输出大量行且客户端正常消费
- **THEN** 客户端收到的日志帧数量明显少于行数（一帧含多行），内容完整

#### Scenario: 慢客户端收到丢弃标记

- **WHEN** 客户端消费速度持续低于生产速度导致积压越过队列上界
- **THEN** 客户端收到「跳过 N 行」标记帧，且随后的日志从当前位置继续，
  不出现内容与标记混淆

#### Scenario: 慢客户端不拖慢子进程

- **WHEN** 一个客户端长期停止消费而子进程持续高速输出
- **THEN** 子进程的输出与落盘节奏不受该客户端影响（其投递被丢弃计数）

#### Scenario: 丢弃不影响落盘与其他订阅者

- **WHEN** 一个慢客户端触发丢弃
- **THEN** 该程序日志文件照常完整落盘，另一正常消费的订阅者不丢行

### Requirement: 配置驱动的控制台生命周期

控制台 SHALL 由 daemon 配置的 `[webui]` 段驱动启停：段存在即伺服，`webui.listen` 缺省 `127.0.0.1:9877`；段不存在 SHALL NOT 伺服。`ak daemon start` 启动时 SHALL 按该配置决定是否伺服。运行中的守护进程重载配置（CLI 入口 `ak config --reload`，可单独执行，也可与 `ak config --set/--delete` 组合一步触发，见 `configuration`「config 子命令」）时 SHALL 重读 `[webui]` 段并立即热生效（MUST NOT 等待 apply、MUST NOT 进入 pending 预览）：由关到开 SHALL 起伺服；由开到关 SHALL 停止伺服并将既有 WebSocket 连接 graceful 关闭（先发关闭帧再断开，不丢已在途推送帧）；`listen` 变更 SHALL 停旧起新完成重绑。webui 监听地址被占用时 SHALL 降级为记录错误日志并继续守护运行（无控制台），MUST NOT 致使守护进程退出。reload 时 `[webui]` 段非法 SHALL 视为本轮 reload 失败（保持既有伺服状态不变并上报错误）。

控制台 token SHALL 按下述规则解析。`auth_token` 未配置或为保留值 `auto` 时，守护进程启动时 SHALL 以密码学安全随机源生成 token（至少 256 位熵；字符集 SHALL 无需 URL 转义），token 仅保存在内存，MUST NOT 落盘、MUST NOT 写入日志或状态投影（对外提供途径仅限控制面 `GET /v1/webui`，见 `control-plane`）。配置为固定字符串时 SHALL 直接使用该值。

重载配置时 token SHALL 按以下规则保持或轮换：token 模式仍为 auto 且 `listen` 未变时 MUST 保持当前 token 不轮换（既有浏览器会话与浏览器存储的 token 继续有效）；控制台由关到开、或由固定 token 改为 auto 时 SHALL 生成新 token；由 auto 改为固定 token 时 SHALL 采用配置值并使既有会话失效。守护进程重启后 auto 模式 SHALL 生成新 token（此前签发的 URL 与浏览器存储的 token 随之失效）。随机源不可用时 SHALL 记录错误并降级为不伺服控制台（与端口占用降级一致），MUST NOT 以空 token 伺服。

#### Scenario: 启动时按配置伺服

- **WHEN** daemon 配置含 `[webui]`（未写 `listen`）且执行 `ak daemon start`
- **THEN** 控制台在 `127.0.0.1:9877` 可访问；不含 `[webui]` 段时不监听任何 webui 端口

#### Scenario: reload 动态开启

> 本变更后 CLI 重载入口为 `ak config --reload`（标题为既有场景锚点，现行行为以下文为准）。

- **WHEN** 守护进程以无 `[webui]` 段的配置运行，执行 `ak config --set webui.listen=127.0.0.1:9877` 后执行 `ak config --reload`
- **THEN** 控制台立即在该地址可访问，无需重启守护进程

#### Scenario: reload 动态关闭

- **WHEN** 控制台伺服中存在 WebSocket 订阅，执行 `ak config --delete webui` 后执行 `ak config --reload`
- **THEN** 控制台停止伺服，WebSocket 连接以关闭帧 graceful 断开，守护进程与其余受管程序不受影响

#### Scenario: listen 变更重绑

- **WHEN** 控制台伺服于 `127.0.0.1:9877`，将配置改为 `webui.listen=127.0.0.1:9898` 后执行 `ak config --reload`
- **THEN** 旧地址停止伺服、新地址开始伺服，守护进程不重启

#### Scenario: webui 端口占用降级

- **WHEN** `[webui]` 启用且目标端口已被其他进程占用，执行 `ak daemon start` 或 `ak config --reload`
- **THEN** 记录控制台不可用的错误日志，守护进程与受管程序照常运行，后续修正配置并重载（`ak config --reload`）可恢复伺服

#### Scenario: auto token 在启动时生成

- **WHEN** `[webui]` 段存在且未写 `auth_token`，执行 `ak daemon start` 后请求 `GET /v1/webui`
- **THEN** 返回非空随机 token，且 `auth` 为 `auto`

#### Scenario: reload 不轮换 auto token

- **WHEN** 控制台以 auto 模式伺服，执行一次无关配置的 `ak config --reload`
- **THEN** `GET /v1/webui` 返回的 token 与 reload 前一致，既有浏览器会话保持有效

#### Scenario: 模式切换使旧 token 失效

- **WHEN** auto 模式下将配置改为 `webui.auth_token=<固定值>` 并执行 `ak config --reload`
- **THEN** `GET /v1/webui` 的 `auth` 变为 `token`，此前浏览器存储的 auto token 不再被接受

### Requirement: 控制台访问控制

Web 控制台 SHALL 保留 TCP 监听并继续经 `webui.listen` 的绑定地址提供服务，MUST NOT 因控制面改用本地 socket 而要求浏览器经 IPC 端点访问——浏览器不具备该能力。控制台 SHALL NOT 复用控制面的 socket 权限模型（其端点由任意进程而非浏览器访问）。

控制台不存在免鉴权模式：`auth_token` 未配置时按 `auto` 处理（启动时随机生成，见「配置驱动的控制台生命周期」）。除 `/api/health` 外的 `/api/*` 请求 SHALL 要求有效凭据，缺失或不匹配 SHALL 返回 401；WebSocket 连接同样 SHALL 受该约束。

凭据 SHALL 通过一次性换票建立会话：客户端先提交 token 换取会话 cookie，后续请求凭 cookie 访问。服务端 MUST NOT 接受 URL 查询参数中的 token 作为凭据；token 只能经请求头或请求体提交给换票端点。会话 cookie SHALL 设置 `HttpOnly` 与 `SameSite`。

为支持「一条命令进入控制台」，引导 URL SHALL 允许携带 `?token=<token>`，但该参数只由浏览器端单页应用读取：应用 SHALL 在启动时立即将 token 提交换票端点换取会话 cookie，随后用 `history.replaceState` 从地址栏与当前历史记录中移除查询串，使地址栏不再显示 token。换票成功后应用 SHALL 把 token 存入浏览器本地存储（`localStorage`），供会话 cookie 过期或丢失时静默续票；续票失败（401）SHALL 清除该存储值、升起凭据门，并提示 token 可能因守护进程重启已轮换、需重新执行 `ak webui`。固定 token 模式下 `ak webui` MUST NOT 把 token 放入 URL。

控制台鉴权 SHALL 与控制面鉴权相互独立：控制面准入由端点属主或 Bearer 凭据判定，控制台使用其自身 token，两者 MUST NOT 互相读取或复用对方的凭据。

#### Scenario: 未配置 token 时维持无鉴权

> 标题为既有场景锚点；本变更后语义反转，现行行为以下文为准。

- **WHEN** `[webui]` 段未配置 `auth_token`（生效值为 auto），浏览器未携带凭据请求 `http://<listen>/api/overview`
- **THEN** 返回 401，程序状态不被泄露；免鉴权访问不再存在

#### Scenario: 配置 token 后拒绝匿名请求

- **WHEN** 配置了 `webui.auth_token`，浏览器未携带凭据请求 `/api/programs`
- **THEN** 返回 401，程序状态不被泄露

#### Scenario: 一次性换票建立会话

- **WHEN** 浏览器以正确 token 提交换票请求
- **THEN** 服务端返回会话 cookie（`HttpOnly`、`SameSite`），后续携带该 cookie 的 `/api/*` 请求正常返回 200

#### Scenario: token 不经 URL 查询传递

> 标题为既有场景锚点；本变更允许引导 URL 携带 token，但服务端仍不把它当凭据，下述行为不变。

- **WHEN** 浏览器以 `?token=<token>` 形式直接访问任一 `/api/*` 端点
- **THEN** 该 token 不被接受为有效凭据；凭据只能经换票流程建立会话

#### Scenario: 引导 URL 自动登录并清理地址栏

- **WHEN** 浏览器以 `http://<listen>/?token=<token>` 打开控制台
- **THEN** 应用自动完成换票并正常加载状态，随后地址栏不再包含 `token` 参数，token 被写入浏览器本地存储

#### Scenario: 存储 token 静默续票

- **WHEN** 浏览器本地存储中有有效 token 且会话 cookie 缺失
- **THEN** 应用在服务端返回 401 时用存储 token 自动换票并重放请求，不弹出凭据门

#### Scenario: 失效 token 清理并提示

- **WHEN** 浏览器本地存储中的 token 已被服务端拒绝（如守护进程重启导致轮换）
- **THEN** 应用清除该存储值、升起凭据门，并提示重新执行 `ak webui` 获取新 token

#### Scenario: WebSocket 同样受约束

- **WHEN** 配置了 `webui.auth_token` 且客户端在未建立会话的情况下连接 `/ws`
- **THEN** 连接被拒绝，不推送任何状态帧或日志帧

#### Scenario: 控制面与控制台凭据互不影响

- **WHEN** 配置了 `[webui] auth_token` 而未配置（且已移除）`[daemon] auth_token`
- **THEN** 控制台要求其自身 token；控制面不读取该 token，且控制面连接仅依据 socket 属主判定准入

#### Scenario: 开启控制台的隔离含义被明示

> 标题为既有场景锚点；本变更后免鉴权模式不复存在，现行行为以下文为准。

- **WHEN** 用户配置 `[webui]` 且未配置 `auth_token`，`ak` 以 root 运行
- **THEN** 控制台仍要求启动时随机生成的 token，任何未持有该 token 的本地用户均被 401 拒绝；`ak validate` 不再输出暴露警告

### Requirement: Web App 移除端点

控制台 SHALL 提供 `POST /api/apps/<app>/remove`，经与控制面 `POST /v1/apps/<app>/remove` 相同的 Supervisor 命令执行。两端点 SHALL 具有相同的前置状态检查、注册配置删除边界、成功结果字段及错误语义；Web 端点 MUST NOT 自行停止程序、直接删除注册或触发 apply。

未知 app SHALL 返回 404 错误对象；存活或待自动重启程序阻塞时 SHALL 返回 409 与阻塞程序及状态；注册删除失败 SHALL 返回服务端错误。配置了 Web 凭据时，该端点 SHALL 与现有 `/api` 写操作一样要求有效会话。成功后的状态投影与 WS 更新 SHALL 反映 app 及其程序已移除，不保留该 app 的注销 pending。

#### Scenario: Web 移除复用控制面语义

- **WHEN** 对同一已停止 app 分别使用 `/api/apps/<app>/remove` 与 `/v1/apps/<app>/remove` 的移除操作
- **THEN** 两者返回同构移除结果，产生相同的注册删除、配置保留及状态清理结果

#### Scenario: Web 移除冲突

- **WHEN** 对含 running、starting、stopping 或 backoff 程序的 app 请求 Web 移除端点
- **THEN** 返回 409 与阻塞原因，app 继续运行且注册不变，未触发 stop 或 apply

#### Scenario: Web 移除要求会话

- **WHEN** Web 配置了 auth_token，未持有效会话请求 app 移除端点
- **THEN** 返回 401，注册与程序状态均不变

#### Scenario: 移除后投影同步

- **WHEN** Web 移除成功且存在 WebSocket 订阅
- **THEN** 后续状态查询与 WS 更新均不再包含该 app/program，其他 app 的状态与 pending 不受影响

### Requirement: 守护概况携带构建元数据

`/v1/status`、`/api/overview` 与 `/ws` 快照中的守护进程概况 SHALL 新增只读 `build` 对象，
包含 `git_commit`、`built_at`、`target`、`rustc` 四个字符串字段：分别为构建时的 git 提交
（短哈希，工作区不干净时带 `-dirty` 后缀）、构建时间（RFC 3339 UTC）、目标三元组与 Rust
编译器版本。任一信息在构建期不可得时，该字段 SHALL 为 `unknown`，构建 MUST NOT 因此失败。
三处投影 MUST 取自同一结构体、同源同构；未知字段对旧消费者 MUST 可安全忽略。

#### Scenario: 控制面投影含构建信息

- **WHEN** 请求 `GET /v1/status`
- **THEN** `daemon.build` 含 `git_commit`、`built_at`、`target`、`rustc` 四个非空字符串

#### Scenario: 控制台投影与 WS 同构

- **WHEN** 同一时刻读取 `GET /api/overview` 与收到 `/ws` 快照帧
- **THEN** 两者的 `daemon.build` 与 `/v1/status` 完全一致

#### Scenario: 元数据缺失不阻断构建

- **WHEN** 构建环境没有 git 仓库或无法取得编译器版本
- **THEN** 二进制仍能构建成功，对应字段返回 `unknown`
