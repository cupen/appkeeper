## Context

控制台现状（见 `webui-api` 与 `src/web.rs`）：

- `[webui]` 段存在即伺服；`auth_token` 未配置 = 完全免鉴权，`ak validate` 只发警告。
- 配置 token 时走一次性换票：`POST /api/auth` 提交 `X-Auth-Token`（或 JSON body），换取
  `HttpOnly; SameSite=Strict; Max-Age=12h` 的 `ak_console` cookie；服务端明确拒绝 URL
  查询参数中的 token（防浏览器历史/访问日志泄露）。
- 前端 `webui/src/lib/credentials.ts` 采用「服务端 401 才升起凭据门」的延迟模型；
  `api.ts` 统一包 `withSession`。交互 shell 已有 `open`（`webui_health` + `browser_command`
  + `webui_url_of`），但只按配置拼地址、不带凭据。
- 守护进程内存中 `Supervisor.console_auth` 持有当前 `ConsoleAuth`（token 私有），
  `WebuiControl` 记录控制台意图与最近一次收敛结果。

约束：

- 浏览器无法访问本地 IPC socket，控制台必须保留 TCP 监听。
- 控制面已有两种准入形态（IPC 属主 / TCP Bearer），新增读取 token 的端点必须沿用，
  且 token 不能进入常规状态投影。
- `webui.listen` 默认回环，但仍可能被改为 `0.0.0.0`。

## Goals / Non-Goals

**Goals:**

- 一条命令 `ak webui` 从任意终端安全进入控制台，auto 模式下无需手工抄 token。
- 默认配置不再存在「开启控制台即授权」的暴露面，且不依赖用户记得设 token。
- token 只在控制面与浏览器之间流转；不落盘、不进日志、不进常规状态投影。
- 浏览器刷新/新标签页能凭存储的 token 静默续票，URL 中的 token 立即消失。

**Non-Goals:**

- 不做多用户账户体系、权限分级或 token 吊销列表（沿用内存会话集合与容量上限）。
- 不改变控制面鉴权模型（`[daemon] auth_token` 仅 TCP 形态、IPC 按属主）。
- 不引入 OAuth/OIDC/反向代理身份透传；不做 HTTPS（控制台仍是本地 HTTP）。
- 不持久化 auto token（重启即轮换，见 D4）。

## Decisions

### D1: 未配置 `auth_token` 即 `auto`，删除免鉴权模式（BREAKING）

`[webui]` 只保留一种默认：启动时随机 token。`auth_token` 的合法取值收敛为「保留字
`auto`」或「固定非空字符串」，空值仍拒绝。`ak validate` 的未鉴权暴露警告删除。

- 备选：保留 `auth_token = "none"` 逃生舱 —— 少一个魔法值、少一个长期不安全模式；
  但会保留「忘配即裸奔」的历史心态，且需再定义 `none` 与字面 token 的歧义。
- 备选：仅显式 `auto` 才生成，未配置维持免鉴权 —— 不满足「默认值」诉求，暴露面不变。

### D2: 新增控制面 `GET /v1/webui` 分发 token

CLI 通过控制面读取 `{enabled, serving, listen, url, auth, token?}`。token 仅在
`auth == "auto"` 时出现；端点沿用既有准入（IPC 属主 / TCP Bearer），`/v1/status` 等
投影不新增任何 token 字段。

- 备选：token 文件（`state_dir/webui.token`，0600）—— 引入落盘秘密、权限/残留/多实例
  语义，且 reload 轮换时还要维护文件一致性。
- 备选：塞进 `/v1/status` 的 `daemon.webui` —— token 会随常规状态投影被脚本、日志、
  截图带走，泄露面远大于收益。

### D3: CSPRNG 依赖 `getrandom`，输出 64 位小写十六进制

32 字节随机 → 64 个 `[0-9a-f]`，满足 ≥256 bit 熵且 URL 无需转义。生成在守护进程启动/
模式切换时完成，失败则记录错误并降级为不伺服控制台（与端口占用降级同一路径）。

- 备选：复用现有 `random_session_id`（`SystemTime`+pid+栈地址）—— 可预测，不可作为
  网络可达端点的凭据。
- 备选：手写 `/dev/urandom` + `BCryptGenRandom` —— 两条平台分支，Windows 侧需加
  `windows-sys` 加密特性，维护成本高于一个已审计的 9KB crate。

### D4: auto token 只在启动/模式切换时生成，reload 保持

解析规则放在守护进程侧（配置只表达意图）：

| 变化 | 行为 |
|---|---|
| daemon 启动（首次 enabled） | 生成新 auto token |
| reload：auto → auto | 保持当前 token（浏览器会话/存储不失效） |
| reload：关 → 开 | 生成新 token |
| reload：fixed → auto | 生成新 token |
| reload：auto → fixed | 采用配置值（旧会话失效） |

- 备选：每次 reload 都轮换 —— 无收益地踢掉所有浏览器，且自动重扫周期远短于人工 reload。
- 备选：落盘到 `state_dir` 跨重启保持 —— 违背「启动时随机生成」，并让 token 变成长期
  静态秘密。

### D5: 引导 token 走 `?token=`，但服务端仍不接受 query 凭据

`ak webui` 打开 `http://<host>:<port>/?token=<token>`；SPA 启动时读取该参数，立即
`POST /api/auth`（`X-Auth-Token`）换 cookie，成功后 `history.replaceState` 剥离查询串
并写入 `localStorage`。`/api/*` 端点依旧只认 cookie，query token 直接访问仍是 401。

- 备选：服务端直接接受 query token —— 省一次换票，但 token 长期留在 URL、Referer 与
  访问日志，正是既有规范禁止的形态。
- 备选：一次性 bootstrap ticket（`?code=`，短 TTL 单次使用）—— 泄露面更小，但需要新端点、
  新状态机与过期策略，超出「URL 里是 token」的明确要求；当前默认回环监听 + 重启轮换
  已把风险压到可接受范围。

### D6: 浏览器存储用 `localStorage`，且只在 401 时静默续票

键名固定（如 `ak.webui.token`，按 origin 天然隔离）。app 启动时若有 `?token=` 则立刻换票；
否则保持既有「401 才动作」模型：先尝试存储 token 换票，失败则清除存储、升起凭据门并
提示重新执行 `ak webui`。

- 备选：`sessionStorage` —— 新标签页/重启即失，续票能力消失。
- 备选：每次启动无条件换票 —— 多一次请求；无 token 时纯属噪音，还会破坏「无凭据门
  不打扰」的既有体验。

### D7: CLI 为 daemon-first，离线不拉起

`ak webui` 先 `GET /v1/webui`；离线 → 退出 3（与其他控制命令一致，`ak daemon start`
是唯一守护入口）；未启用/未伺服 → 退出 1 并给出 `ak config --set webui.listen=...` +
`ak config --reload` 的修复命令。打开前探测 `/api/health`（总预算约 5s，覆盖刚启动的
绑定窗口）。`--print-url` / `--json` 为不打开浏览器的输出模式；无 `DISPLAY`/
`WAYLAND_DISPLAY`（Linux；macOS 的 `open` 不依赖）时不尝试拉起浏览器，打印完整 URL
退出 0。

- 备选：离线时自动 `ak daemon start` —— 与其他客户端动词的约定冲突，还会在脚本里
  意外拉起常驻进程。
- 备选：CLI 直接读配置文件拼地址 —— 拿不到 auto token，且无法知道绑定是否成功。

### D8: shell `open` 与 `ak webui` 共用实现

把 `webui_health`、`browser_command`、URL 构造提取为共享模块（`src/webui_open.rs`），
两处都消费 `GET /v1/webui` 的结果。shell 的「不可达不启动浏览器」语义保留。

- 备选：各自维护 URL 拼装 —— 引导 token 规则一改就漂移，正是本次要消除的重复。

### D9: 配置键与键表

- `parse_webui_auth_token` 放行小写 `auto`；其余值仍要求非空（拒绝空串）。
- `webui_key_display("auth_token")` 未配置时打印 `auto`（与 `listen` 回退默认同构）。
- `DaemonConfig::webui_auth_token()` 的返回类型不足以表达「未配置 vs 固定值」，改为
  由 `WebuiSettings` 派生 `WebuiTokenMode { Auto, Fixed(String) }`，配置层只表达意图，
  token 生成仍在守护进程。
- 校验保持单一入口：写入期与加载期都接受 `auto`/非空固定值、拒绝空串。

- 备选：新键 `auth = "auto"` 与 `auth_token` 并存 —— 两个键表达同一件事，迁移与文档成本翻倍。
- 备选：空串表示 auto —— 与既有「空值即错误」规则冲突，且读起来像 bug。

### D10: 状态与投影接线

- `WebuiIntent` 增加解析后的 token 与模式；`WebuiControl` 暴露 `snapshot()`
  （intent + 最近一次 applied 的 `serving`/`error`）。
- `GET /v1/webui` 从 `Supervisor.console_auth` 读取实际 token（enabled 时），从
  `WebuiControl` 读取 `serving`/`error`；`enabled` 取 intent。这样「配置说开了但绑定
  失败」可如实报告，`ak webui` 不会对着坏端口开浏览器。
- `ConsoleAuth` 增加只读 `token()`（`pub(crate)`）；仍不参与任何序列化结构。

## Risks / Trade-offs

- **[token 短暂进入浏览器历史 / 进程 argv]**（`xdg-open` 的参数、跳转前的历史记录）→
  默认监听回环、token 随守护进程重启轮换、成功输出与日志均不打印；`--print-url`
  的存在明确说明这是有意输出；文档标注「勿把自动 token URL 贴到公开渠道」。
- **[localStorage 中的 token 受 XSS 威胁]** → 控制台只伺服内嵌 SPA、无第三方脚本；
  cookie 仍为 `HttpOnly`，存储 token 只用于续票；固定 token 模式不写入 URL/存储。
- **[升级后旧书签落 401 门]** → 凭据门提示「执行 `ak webui` 获取新 token」；发布说明
  标注 BREAKING；`ak validate` 不再警告，避免噪音，但 `ak webui` 是明确的迁移动作。
- **[e2e/浏览器测试原本免鉴权]** → 测试改为先从 `GET /v1/webui` 取 token 再以
  `?token=` 进入；G0c2 的「webui 子命令已移除」断言反转为「命令存在且为打开语义」。
- **[多守护进程同机不同端口]** → localStorage 按 origin（含端口）隔离，互不串扰；
  CLI 永远按 `-c/--config` 指向的端点取 token。
- **[CSPRNG 不可用]** → 记录错误、控制台不伺服，`ak webui` 报 `serving=false` 并给出
  排障方向；绝不以空 token 伺服。

## Migration Plan

1. 配置无需改写：`[webui]` 字段不变，未写 `auth_token` 的部署自动进入 auto 模式。
2. 升级后首次进入控制台执行一次 `ak webui`（或 `ak webui --print-url` 复制到目标浏览器）；
   旧会话 cookie 随守护进程重启失效属预期。
3. e2e 与浏览器测试同步调整；shell `open` 自动获得新行为，无需用户动作。
4. 回滚：换回旧二进制即可；配置未引入必填新字段，auto 值对旧版本不可行（旧版本会把它
   当固定 token），回滚前若显式写过 `auth_token = "auto"` 需删除该行。
