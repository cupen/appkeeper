## MODIFIED Requirements

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
