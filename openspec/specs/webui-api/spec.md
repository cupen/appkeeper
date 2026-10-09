# webui-api Specification

## Purpose
定义 xkeeper Web 控制台的伺服与推送契约：控制台服务器与守护进程同进程、
复用控制面的状态投影与命令队列；WebSocket 通道向浏览器推送状态增量与日志
分块；规定各通道的数据格式与效率约束。

## Requirements

### Requirement: 控制台端点复用控制面投影
控制台服务器 SHALL 在自己的回环端口（`xkeeper webui --listen`，默认
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
- **WHEN** 配置被编辑使某程序进入 pending，随后 `xkeeper apply` 清空 pending
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
退出（Ctrl+C 或 `POST /v1/shutdown`）。`xkeeper run` SHALL 是唯一的守护入口
子命令；控制台是否伺服 SHALL 由 daemon 配置决定（见「配置驱动的控制台生命周期」），
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

控制台 SHALL 由 daemon 配置的 `[webui]` 段驱动启停：段存在即伺服，`webui.listen` 缺省 `127.0.0.1:9877`；段不存在 SHALL NOT 伺服。`xkeeper run` 启动时 SHALL 按该配置决定是否伺服。运行中的守护进程执行 `xkeeper reload` 时 SHALL 重读 `[webui]` 段并立即热生效（MUST NOT 等待 apply、MUST NOT 进入 pending 预览）：由关到开 SHALL 起伺服；由开到关 SHALL 停止伺服并将既有 WebSocket 连接 graceful 关闭（先发关闭帧再断开，不丢已在途推送帧）；`listen` 变更 SHALL 停旧起新完成重绑。webui 监听地址被占用时 SHALL 降级为记录错误日志并继续守护运行（无控制台），MUST NOT 致使守护进程退出。reload 时 `[webui]` 段非法 SHALL 视为本轮 reload 失败（保持既有伺服状态不变并上报错误）。

#### Scenario: 启动时按配置伺服

- **WHEN** daemon 配置含 `[webui]`（未写 `listen`）且执行 `xkeeper run`
- **THEN** 控制台在 `127.0.0.1:9877` 可访问；不含 `[webui]` 段时不监听任何 webui 端口

#### Scenario: reload 动态开启

- **WHEN** 守护进程以无 `[webui]` 段的配置运行，执行 `xkeeper config --set webui.listen=127.0.0.1:9877` 后执行 `xkeeper reload`
- **THEN** 控制台立即在该地址可访问，无需重启守护进程

#### Scenario: reload 动态关闭

- **WHEN** 控制台伺服中存在 WebSocket 订阅，执行 `xkeeper config --delete webui` 后执行 `xkeeper reload`
- **THEN** 控制台停止伺服，WebSocket 连接以关闭帧 graceful 断开，守护进程与其余受管程序不受影响

#### Scenario: listen 变更重绑

- **WHEN** 控制台伺服于 `127.0.0.1:9877`，将配置改为 `webui.listen=127.0.0.1:9898` 后执行 `xkeeper reload`
- **THEN** 旧地址停止伺服、新地址开始伺服，守护进程不重启

#### Scenario: webui 端口占用降级

- **WHEN** `[webui]` 启用且目标端口已被其他进程占用，执行 `xkeeper run` 或 `xkeeper reload`
- **THEN** 记录控制台不可用的错误日志，守护进程与受管程序照常运行，后续修正配置并 reload 可恢复伺服
