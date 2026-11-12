## ADDED Requirements

### Requirement: 控制台引导端点

控制面 SHALL 提供只读端点 `GET /v1/webui`，返回 Web 控制台当前可供浏览器访问的状态。响应 SHALL 为 JSON 对象，至少包含：

- `enabled`（布尔）：daemon 配置的 `[webui]` 段是否存在（控制台意图）；
- `serving`（布尔）：控制台是否已成功绑定并伺服（端口占用等失败时为 false）；
- `listen`（字符串）：配置的监听地址（未配置时为内置默认 `127.0.0.1:9877`）；
- `url`（字符串）：浏览器可用的控制台基址（如 `http://127.0.0.1:9877`）。监听地址为通配地址（`0.0.0.0`、`::`）时 SHALL 以回环地址呈现，MUST NOT 输出不可访问的通配地址；
- `auth`（字符串）：`auto`（token 于启动时随机生成）或 `token`（固定 token）；
- `token`（字符串，仅 `auth == "auto"` 时存在）：本次守护进程运行的随机 token。

该端点 SHALL 受与其余 `/v1/*` 相同的准入控制（IPC 形态按端点属主，TCP 形态要求 Bearer 凭据）；未获准入 SHALL 返回 401。随机 token MUST NOT 出现在 `/v1/status`、`/v1/programs*`、任何 `/api/*` 响应或 WebSocket 帧中——它只经此端点向控制面客户端提供。

同一守护进程运行期间，`auth == "auto"` 的 `token` SHALL 保持不变（重复请求返回同一值），除非配置重载改变了控制台 token 模式（见 `webui-api`）。

#### Scenario: auto 模式返回随机 token

- **WHEN** `[webui]` 段存在且未写 `auth_token`，对运行中的守护进程请求 `GET /v1/webui`
- **THEN** 返回 200，`auth` 为 `auto`、`enabled` 与 `serving` 为 true，`token` 为非空字符串

#### Scenario: 固定 token 模式不回显 token

- **WHEN** `[webui] auth_token` 配置为固定字符串，请求 `GET /v1/webui`
- **THEN** 返回 200，`auth` 为 `token`，响应中不含 `token` 字段

#### Scenario: 控制台未启用

- **WHEN** daemon 配置不含 `[webui]` 段，请求 `GET /v1/webui`
- **THEN** 返回 200，`enabled` 与 `serving` 为 false，`url` 为内置默认地址，且不含 `token` 字段

#### Scenario: 随机 token 不出现在状态投影

- **WHEN** `[webui]` 以 auto 模式伺服，分别请求 `GET /v1/status` 与 `GET /v1/programs`
- **THEN** 两者响应均不含随机 token 的任何形式

### Requirement: 控制台打开命令

CLI SHALL 提供顶层子命令 `ak webui`，用于打开 Web 控制台。命令 SHALL 为 daemon-first：经控制面 `GET /v1/webui` 读取控制台状态，MUST NOT 读取或推断控制台的进程内 token。守护进程不可达时 SHALL 输出不可达提示并以退出码 3 退出，MUST NOT 隐式拉起守护进程。

控制台未启用（`enabled == false`）或未成功伺服（`serving == false`）时，命令 SHALL 以退出码 1 失败，并在错误信息中给出修复命令（启用：`ak config --set webui.listen=<addr>` 配合 `ak config --reload`；伺服失败：查看 daemon 日志与监听地址占用）。

打开前命令 SHALL 探测控制台可达性（`GET /api/health`），等待其上线的总时长 SHALL 有上界（约 5 秒）；超时或不可达 SHALL 按未伺服处理并以退出码 1 失败，不启动浏览器。可达后命令 SHALL 调用系统默认浏览器打开控制台 URL：

- `auth == "auto"` 时 URL SHALL 携带引导查询参数 `?token=<token>`（token 取自 `GET /v1/webui`）；
- `auth == "token"`（固定 token）时 URL MUST NOT 携带 token；
- 成功打开时输出一行不含 token 的地址信息；命令 MUST NOT 在常规成功输出中打印 token。

命令 SHALL 支持两种不打开浏览器的输出模式（二者互斥）：

- `--print-url`：把完整 URL（auto 模式下含 `?token=`）打印到 stdout 并以 0 退出；
- `--json`：输出机器可读对象，至少含 `url`、`listen`、`auth` 与 `opened` 字段，以 0 退出。

无图形会话的环境（Linux 且 `DISPLAY` 与 `WAYLAND_DISPLAY` 均未设置；macOS 的 `open`
不依赖这两个变量）SHALL NOT 尝试启动浏览器，改为打印完整 URL 与提示并以 0 退出。浏览器启动失败时 SHALL 打印完整 URL 与失败原因，并以退出码 1 失败。

`ak webui` MUST NOT 修改 daemon 配置、MUST NOT 停止或启动任何受管程序。

#### Scenario: auto 模式一条命令进入控制台

- **WHEN** `[webui]` 以 auto 模式伺服，执行 `ak webui`
- **THEN** 系统默认浏览器被调起且地址含 `?token=<token>`，命令输出不含 token，退出码 0

#### Scenario: 固定 token 模式不带 token

- **WHEN** `[webui] auth_token` 为固定值，执行 `ak webui`
- **THEN** 浏览器打开的地址不含 `?token=`，凭据门由用户在页面输入固定 token

#### Scenario: 只打印 URL

- **WHEN** auto 模式下执行 `ak webui --print-url`
- **THEN** stdout 输出含 `?token=<token>` 的完整 URL，不启动浏览器，退出码 0

#### Scenario: JSON 输出

- **WHEN** 执行 `ak webui --json`
- **THEN** stdout 输出含 `url`、`listen`、`auth`、`opened` 的 JSON 对象，退出码 0

#### Scenario: 控制台未启用

- **WHEN** daemon 配置不含 `[webui]` 段，执行 `ak webui`
- **THEN** 退出码 1，错误信息含启用控制台的配置命令，不启动浏览器

#### Scenario: 守护进程离线

- **WHEN** 守护进程未运行，执行 `ak webui`
- **THEN** 输出守护进程不可达提示，退出码 3，不尝试启动浏览器，不拉起守护进程

#### Scenario: 无图形会话

- **WHEN** Linux 环境下 `DISPLAY` 与 `WAYLAND_DISPLAY` 均未设置，执行 `ak webui`
- **THEN** 不尝试启动浏览器，打印完整 URL 与提示，退出码 0
