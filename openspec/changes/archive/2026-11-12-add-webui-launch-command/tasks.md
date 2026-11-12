## 1. 配置层（`src/config.rs`）

- [x] 1.1 引入 `WebuiTokenMode`（`Auto` / `Fixed(String)`）并从 `WebuiSettings` 派生；未知/空值仍拒绝，小写 `auto` 解析为 `Auto`。验证：新增单测覆盖未配置→Auto、`auto`→Auto、固定值→Fixed、空串→错误。
- [x] 1.2 `parse_webui_auth_token` 放行 `auto` 并更新错误文案为「`auto` 或非空字符串」。验证：`config --set webui.auth_token=auto` 单测通过，空值写入仍失败。
- [x] 1.3 `webui_key_display("auth_token")` 未配置时打印 `auto`。验证：`config --get webui.auth_token` 单测断言 `auto` 且不回写文件。
- [x] 1.4 删除 `webui_open_console_warning` 及其调用与两个断言测试；`DaemonConfig::validate` 接受 auto/固定 token。验证：`cargo test` 中 validate 相关测试通过，`validate` 无警告输出。

## 2. Token 生成与生命周期（`src/web.rs`、`src/supervisor.rs`）

- [x] 2.1 增加 `getrandom` 依赖并实现 `generate_console_token()`（32 字节 → 64 位小写 hex）；生成失败返回错误。验证：单测断言长度 64、字符集 `[0-9a-f]`、两次生成不同。
- [x] 2.2 `ConsoleAuth` 增加 `pub(crate) fn token(&self) -> &str`（仅 enabled 时非空），不参与任何 serde 结构。验证：单测确认 disabled 空值、enabled 返回注入值。
- [x] 2.3 `WebuiIntent` 承载解析后的 token 与模式；`WebuiIntent::from_config` 改为接受上一次 intent（用于保持 auto token），实现 design D4 的保持/轮换表。验证：单测覆盖 auto→auto 保持、关→开生成、fixed→auto 生成、auto→fixed 采用配置值。
- [x] 2.4 `WebuiControl` 暴露 `snapshot()`，返回 intent、`serving`、最近一次 `error`；生成失败走与端口占用相同的降级路径（记录错误、不伺服）。验证：单测断言绑定失败后 `snapshot().serving == false` 且 note 非空。
- [x] 2.5 `reload` 路径把「上一轮 intent」传入解析（避免 auto token 被无谓轮换）。验证：单测或集成测试对同一 config 连续两次 reload，token 保持一致。

## 3. 控制面端点（`src/server.rs`、`src/client.rs`）

- [x] 3.1 新增 `GET /v1/webui`，返回 `enabled`/`serving`/`listen`/`url`/`auth`（auto 时附 `token`）；通配监听地址的 `url` 回落 `127.0.0.1`。验证：`server.rs` 单测断言 auto 模式字段齐全、fixed 模式无 `token` 键、disabled 时 `enabled=false`。
- [x] 3.2 确认 token 不出现在 `/v1/status`、`/v1/programs*` 投影。验证：单测扫描响应体不含当时 token 字符串。
- [x] 3.3 `client.rs` 增加读取 `/v1/webui` 的封装（返回结构化结果，离线映射为退出码 3 的既有错误类型）。验证：客户端单测用本地测试端点断言解析与错误映射。

## 4. `ak webui` 命令与共享打开逻辑（`src/main.rs`、新模块）

- [x] 4.1 把 `shell.rs` 的 `webui_health`、`browser_command`、URL 构造提取为共享模块（如 `src/webui_open.rs`），shell 改为调用它。验证：`cargo test` 与 `cargo build` 通过，shell `open` 既有测试不变绿。
- [x] 4.2 新增顶层 `Cmd::Webui`（`--print-url` 与 `--json` 互斥），派发实现：离线退出 3、未启用/未伺服退出 1 并给修复命令、`/api/health` 探测预算约 5s。验证：单测覆盖各退出码与错误文案。
- [x] 4.3 实现 URL 构造与输出：auto 模式附 `?token=`，fixed 模式不带；默认打开后打印浏览器将打开的完整 URL（auto 含 token，可点击）；`--print-url` 只打印完整 URL 且不打开；`--json` 输出 `url`/`listen`/`auth`/`opened`。验证：单测断言三种模式的 URL 与输出。
- [x] 4.4 无图形会话（unix 且 `DISPLAY`/`WAYLAND_DISPLAY` 均空）不拉起浏览器、打印完整 URL、退出 0；浏览器启动失败打印 URL 并退出 1。验证：单测用环境变量注入与伪造启动失败路径。

## 5. shell `open` 对齐（`src/shell.rs`）

- [x] 5.1 `open` 改为消费 `GET /v1/webui` 的结果（auto 带 token、fixed 不带），不可达时保留既有提示且不启动浏览器。验证：shell 单测覆盖 auto/fixed/不可达三态。

## 6. 前端引导与存储（`webui/src`）

- [x] 6.1 新增引导模块：读取 `location.search` 的 `token`，启动时 `POST /api/auth`（`X-Auth-Token`）换票，成功后 `history.replaceState` 剥离查询串并写入 `localStorage`。验证：新增 `*.test.ts` 覆盖「有 token → 换票 → 地址栏清理 → 存储写入」与「无 token 不动作」。
- [x] 6.2 `credentials.ts` 增加存储 token 的读取/清除，并在 401 时先静默换票、失败后清存储再升门；保持「凭据门只在 401 后出现」。验证：`credentials.test.ts` 覆盖静默续票、失效清理、重复 401 不循环。
- [x] 6.3 `credential-gate.ts` 增加失效提示（「token 已失效，请重新执行 `ak webui`」），仅在存储 token 被拒后显示。验证：组件测试断言提示文案与显示条件。
- [x] 6.4 首轮请求前先跑引导模块（实际落在 `lib/store.ts` 的 `ConsoleController.hostConnected`，`app-shell.ts` 未改）。验证：`app-shell.test.ts` 断言引导先于首轮请求，且地址栏已无 token。

## 7. 验收与回归（`xtask/src/e2e.rs`）

- [x] 7.1 反转 G0c2：`ak webui` 不再是未知子命令，`system webui` 仍为未知；断言 `ak webui` 在离线时退出 3。验证：`cargo run -p xtask -- e2e` 对应步骤通过。
- [x] 7.2 浏览器段改为先 `GET /v1/webui` 取 token，再以 `/?token=<token>` 进入；断言加载后地址栏无 token 且 `localStorage` 有值。验证：e2e 浏览器步骤通过（`--no-browser` 时跳过）。
- [x] 7.3 新增 CLI 步骤：`ak webui --print-url` 输出含 `?token=`、`--json` 字段齐全、`[webui]` 未配置时退出 1、auto token 在 reload 后保持。验证：e2e CLI 段通过。

## 8. 全量验证

- [x] 8.1 `cargo test --workspace` 与 `cargo build --release` 通过（505 passed / 0 failed；release 56.86s）。
- [x] 8.2 `cd webui && pnpm exec tsc --noEmit && pnpm test && pnpm build` 通过（162 tests），再 `cargo build` 重新嵌入。
- [x] 8.3 `cargo run -p xtask -- e2e` 全绿（含浏览器段，185.5s）；`openspec validate add-webui-launch-command` 通过。
