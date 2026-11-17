## MODIFIED Requirements

### Requirement: 控制台打开命令

CLI SHALL 提供顶层子命令 `ak webui`，用于打开 Web 控制台。命令 SHALL 为 daemon-first：经控制面 `GET /v1/webui` 读取控制台状态，MUST NOT 读取或推断控制台的进程内 token。守护进程不可达时 SHALL 输出不可达提示并以退出码 3 退出，MUST NOT 隐式拉起守护进程，也不打印 URL（地址与 auto token 均不可知）。

控制台未启用（`enabled == false`）或未成功伺服（`serving == false`）时，命令 SHALL 以退出码 1 失败，并在错误信息中给出修复命令（启用：`ak config --set webui.listen=<addr>` 配合 `ak config --reload`；伺服失败：查看 daemon 日志与监听地址占用），MUST NOT 打印 URL，MUST NOT 尝试启动浏览器：这是配置问题，不是便利功能的降级。

daemon 报告已启用且正在伺服后，命令 SHALL 在 stdout 打印完整 URL，且该打印 SHALL 发生在可达性探测与浏览器启动**之前**——URL 是命令的交付物，输出模式、默认模式、无图形会话、探测失败与启动失败都必须给出它；探测（至多约 5 秒）与浏览器启动（launcher 可能阻塞到浏览器退出）MUST NOT 延迟或抑制该打印：

- `auth == "auto"` 时 URL SHALL 携带引导查询参数 `?token=<token>`（token 取自 `GET /v1/webui`）；
- `auth == "token"`（固定 token）时 URL MUST NOT 携带 token；
- 打印的就是浏览器将收到的完整地址，不额外包裹文案（MUST NOT 使用 `opening ` 一类前缀），使终端能直接渲染为可点击链接。

命令 SHALL 支持两种不打开浏览器的输出模式（二者互斥），二者打印 URL 后即以 0 退出，且 MUST NOT 向 stderr 输出任何提示：

- `--print-url`：只输出完整 URL；
- `--json`：输出机器可读对象，至少含 `url`、`listen`、`auth` 与 `opened` 字段。`opened` 描述「本次调用是否把 URL 交给系统浏览器且启动成功」；输出模式从不启动浏览器，因此该字段恒为 `false`，调用方 MUST NOT 据此推断默认模式的行为。

默认模式（无 `--print-url` / `--json`）在打印 URL 后决定是否启动浏览器：

- 无图形会话（Linux 且 `DISPLAY` 与 `WAYLAND_DISPLAY` 均未设置；macOS 的 `open` 不依赖这两个变量）时 MUST NOT 探测可达性、MUST NOT 启动浏览器：没有浏览器可开，探测没有意义；
- 有图形会话时 SHALL 先探测控制台可达性（`GET /api/health`，等待其上线的总时长有上界，约 5 秒）：探测成功才调用系统默认浏览器打开该 URL；探测超时或不可达 MUST NOT 启动浏览器（打开一个不应答的地址没有意义）；
- 浏览器启动失败（含系统 launcher 不存在或启动后返回非 0）时 MUST NOT 回退尝试其他 launcher。

无图形会话、探测失败与启动失败三种降级 SHALL 在 stderr 各给出一行提示（分别说明无图形会话、探测失败原因、启动失败原因，并在需要时提示手动打开 stdout 上的 URL），前缀统一为 `ak: warning:`（命令并未失败，MUST NOT 使用 `error:` 前缀），三种路径均以退出码 0 结束。自动打开与可达性探测属于便利功能，MUST NOT 使命令失败，MUST NOT 以退出码 1 表达；退出码 1 只留给「daemon 自述控制台未启用/未伺服」的配置问题。

`ak webui` MUST NOT 修改 daemon 配置、MUST NOT 停止或启动任何受管程序。

#### Scenario: auto 模式一条命令进入控制台

- **WHEN** `[webui]` 以 auto 模式伺服，执行 `ak webui`
- **THEN** stdout 恰为裸 URL（含 `?token=<token>`，不带 `opening ` 前缀，可直接点击），stderr 为空，系统默认浏览器随后被调起，退出码 0

#### Scenario: 固定 token 模式不带 token

- **WHEN** `[webui] auth_token` 为固定值，执行 `ak webui`
- **THEN** stdout 与浏览器打开的地址均不含 `?token=`，凭据门由用户在页面输入固定 token，退出码 0

#### Scenario: URL 先于探测与启动交付

- **WHEN** 存在图形会话，执行 `ak webui`
- **THEN** URL 在探测与浏览器启动开始前即写入 stdout：探测等待与 launcher 阻塞都不会推迟它

#### Scenario: 只打印 URL

- **WHEN** auto 模式下执行 `ak webui --print-url`
- **THEN** stdout 只输出含 `?token=<token>` 的完整 URL，stderr 为空，不探测、不启动浏览器，退出码 0

#### Scenario: JSON 输出

- **WHEN** 执行 `ak webui --json`
- **THEN** stdout 输出含 `url`、`listen`、`auth`、`opened` 的 JSON 对象，stderr 为空，不探测、不启动浏览器，退出码 0

#### Scenario: JSON 的 opened 恒为 false

- **WHEN** 以 `--json` 执行（唯一的 JSON 输出模式）
- **THEN** `opened` 为 `false`：输出模式从不启动浏览器，不存在 `opened` 为 `true` 的 JSON 调用

#### Scenario: 控制台未启用

- **WHEN** daemon 配置不含 `[webui]` 段，执行 `ak webui`
- **THEN** 退出码 1，错误信息含启用控制台的配置命令，stdout 为空（不打印 URL），不启动浏览器

#### Scenario: 守护进程离线

- **WHEN** 守护进程未运行，执行 `ak webui`
- **THEN** 输出守护进程不可达提示，退出码 3，不打印 URL，不尝试启动浏览器，不拉起守护进程

#### Scenario: 无图形会话

- **WHEN** Linux 环境下 `DISPLAY` 与 `WAYLAND_DISPLAY` 均未设置，执行 `ak webui`
- **THEN** stdout 恰为完整 URL（一行），stderr 给出一行 `ak: warning:` 无图形会话提示，不探测可达性、不启动浏览器，退出码 0

#### Scenario: 探测失败仍交付 URL

- **WHEN** daemon 报告 `enabled && serving`，但 `/api/health` 在约 5 秒内无应答（例如监听地址被代理抢占或刚 reload 尚未就绪）
- **THEN** stdout 打印完整 URL，stderr 给出一行 `ak: warning:` 失败原因与排查提示，不启动浏览器，退出码 0

#### Scenario: 浏览器启动失败不影响命令成功

- **WHEN** 存在图形会话且探测成功，但系统 launcher 不存在或启动后返回非 0（例如主机没有安装浏览器），执行 `ak webui`
- **THEN** stdout 打印完整 URL，stderr 给出一行 `ak: warning:` 提示（含失败原因与「手动打开 stdout 上的 URL」），退出码 0
