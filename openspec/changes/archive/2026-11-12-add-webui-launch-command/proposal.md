## Why

Web 控制台目前只有两个入口：手动拼出 `webui.listen` 地址打开浏览器，或在交互 shell 里
输入 `open`。若配置了 `webui.auth_token`，用户还得先把 token 抄到页面的凭据门；若没配
token，控制台默认完全免鉴权——任何能连到监听地址的本地用户都能 stop / apply，`ak
validate` 只能发警告。缺一条「一条命令安全地进入控制台」的路径。

## What Changes

- 新增顶层 `ak webui` 子命令：经控制面读取控制台运行态，探测 `/api/health` 后用系统
  浏览器打开控制台；控制台使用随机 token 时，URL 自动携带 `?token=<token>` 完成引导。
  支持 `--print-url`（只打印完整 URL，不打开）与 `--json`。守护离线退出 3，不隐式拉起；
  控制台未启用/未成功伺服退出 1 并给出修复命令。
- **BREAKING** `[webui]` 段未写 `auth_token` 时语义改为 `auto`：token 在守护进程启动时
  随机生成；控制台 `/api/*` 与 `/ws` 一律要求凭据。原「未配置 token 即免鉴权」模式移除，
  `ak validate` 的未鉴权暴露警告随之删除。
- `auth_token = "auto"` 成为 `[webui]` 的默认生效值与保留字（字面量 token `"auto"` 不可用）；
  仍可配置固定 token。`ak config --get webui.auth_token` 未配置时打印 `auto`。
- 新增控制面只读端点 `GET /v1/webui`：返回控制台 `enabled`/`serving`/`listen`/`url`/
  `auth` 以及 `auto` 模式下的 `token`；受既有控制面准入门保护（IPC 属主 / TCP Bearer），
  token MUST NOT 出现在 `/v1/status` 投影中。
- 前端引导：SPA 读取启动 URL 的 `?token=`，立即 `POST /api/auth` 换会话 cookie 并写入
  浏览器存储（`localStorage`），随后 `history.replaceState` 剥离查询串；后续 401 先静默
  用存储的 token 换票，失败才弹凭据门并清除失效 token（守护重启导致 token 轮换时提示
  重新执行 `ak webui`）。服务端仍拒绝把 query token 当作凭据。
- 交互 shell 的 `open` 与 `ak webui` 共用同一 URL 构造与浏览器打开逻辑，同样携带引导 token。
- e2e 调整：删除「`ak webui` 是已移除子命令」断言（`system webui` 仍移除）；浏览器段经
  `GET /v1/webui` 取 token 后以 `?token=` 进入控制台。

## Capabilities

### New Capabilities

<!-- 无新增能力：CLI 命令、控制面端点、配置语义与前端行为分别归属既有能力。 -->

### Modified Capabilities

- `control-plane`: CLI 控制命令集新增 `ak webui`；新增只读端点 `GET /v1/webui`。
- `webui-api`: 控制台访问控制改为 auto 随机 token 默认；新增 URL 引导 token 与前端存储
  契约；配置驱动生命周期补充 token 生成/保持/轮换规则。
- `configuration`: `[webui] auth_token` 的默认值、保留值与 `--get` 生效值语义；移除未鉴权
  控制台的暴露警告。
- `shell-client`: `open` 命令使用与控制台 token 模式匹配的引导 URL。

## Impact

- Rust：`src/main.rs`（新子命令与派发）、`src/config.rs`（auto 解析/校验/键值显示/警告
  删除）、`src/web.rs`（`ConsoleAuth` 暴露 token 与模式）、`src/supervisor.rs`（
  `WebuiIntent` 承载解析后的 token 与伺服状态）、`src/server.rs`（`GET /v1/webui`）、
  `src/shell.rs`（提取共用打开逻辑）、`src/client.rs`（调用 `/v1/webui`）。
- 前端：`webui/src/lib/credentials.ts`、`webui/src/lib/api.ts`、新增引导模块与
  `components/credential-gate.ts` 的失效提示；`app-shell.ts` 启动时执行引导。
- 依赖：新增 CSPRNG 依赖 `getrandom`（跨平台；替代现有基于时间的弱随机）。
- 测试：`xtask/src/e2e.rs`（命令存在性、`/v1/webui`、浏览器带 token 进入）、各模块单测。
- 兼容性：升级后旧书签首次访问会落到凭据门，需执行一次 `ak webui`；`ak webui` 命令名
  与旧版（已移除的守护入口）同名但语义完全不同——新命令不启动守护进程。
