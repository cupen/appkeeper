## ADDED Requirements

### Requirement: 控制台访问控制

Web 控制台 SHALL 保留 TCP 监听并继续经 `webui.listen` 的绑定地址提供服务，MUST NOT 因控制面改用本地 socket 而要求浏览器经 IPC 端点访问——浏览器不具备该能力。控制台 SHALL NOT 复用控制面的 socket 权限模型（其端点由任意进程而非浏览器访问）。

未配置 `[webui] auth_token` 时，`/api/*` SHALL 不做应用层鉴权，仅依赖 `webui.listen` 的绑定地址；此为既有行为的延续，规范 SHALL 明示其含义：**开启 Web 控制台即视为将该绑定地址上的访问权授予任何可达该地址的客户端**。

配置 `[webui] auth_token` 后，除 `/api/health` 外的 `/api/*` 请求 SHALL 要求有效凭据，缺失或不匹配 SHALL 返回 401；WebSocket 连接同样 SHALL 受该约束。凭据 SHALL 通过一次性换票建立会话：客户端先提交 token 换取会话 cookie，后续请求凭 cookie 访问；SHALL NOT 接受长期把 token 置于 URL 查询参数（会落入浏览器历史与访问日志）。会话 cookie SHALL 设置 `HttpOnly` 与 `SameSite`。

控制台鉴权 SHALL 与控制面鉴权相互独立：控制面不使用任何应用层 token（准入由 socket 属主判定），控制台使用 `[webui] auth_token`，两者 MUST NOT 互相读取或复用对方的凭据。

#### Scenario: 未配置 token 时维持无鉴权

- **WHEN** `[webui]` 段未配置 `auth_token`，浏览器访问 `http://<listen>/api/overview`
- **THEN** 正常返回控制台状态投影，行为与本变更之前一致

#### Scenario: 配置 token 后拒绝匿名请求

- **WHEN** 配置了 `webui.auth_token`，浏览器未携带凭据请求 `/api/programs`
- **THEN** 返回 401，程序状态不被泄露

#### Scenario: 一次性换票建立会话

- **WHEN** 浏览器以正确 token 提交换票请求
- **THEN** 服务端返回会话 cookie（`HttpOnly`、`SameSite`），后续携带该 cookie 的 `/api/*` 请求正常返回 200

#### Scenario: token 不经 URL 查询传递

- **WHEN** 浏览器以 `?token=<token>` 形式访问任一 `/api/*` 端点
- **THEN** 该 token 不被接受为有效凭据；凭据只能经换票流程建立会话

#### Scenario: WebSocket 同样受约束

- **WHEN** 配置了 `webui.auth_token` 且客户端在未建立会话的情况下连接 `/ws`
- **THEN** 连接被拒绝，不推送任何状态帧或日志帧

#### Scenario: 控制面与控制台凭据互不影响

- **WHEN** 配置了 `[webui] auth_token` 而未配置（且已移除）`[daemon] auth_token`
- **THEN** 控制台要求其自身 token；控制面不读取该 token，且控制面连接仅依据 socket 属主判定准入

#### Scenario: 开启控制台的隔离含义被明示

- **WHEN** 用户配置 `[webui]` 且未配置 `auth_token`，`ak` 以 root 运行
- **THEN** 任何能连到 `webui.listen` 地址的本地用户均可通过控制台执行 stop / apply 等动作；该事实由文档与 `ak validate` 的提示明确告知
