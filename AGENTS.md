# appkeeper — Agent Instructions

轻量级进程守护工具（Rust），通过 TOML 配置守护任意多个子进程：退出后指数退避重启、
stdout/stderr 落盘、Ctrl+C / SIGTERM 优雅停机、Windows Job Object 清理进程树。
控制面端点由 `[daemon] socket` 单键双形态决定（tcp-control-endpoint）：缺省
**本地 IPC**（unix `/run/appkeeper/<uid>/ak.sock`、Windows 命名管道，身份由 OS
属主强制），或显式 `tcp://host:port`（Bearer 凭据准入，`auth_token` 必配）；
同一时刻只监听一种形态。内嵌 Web 控制台由 daemon 配置的 `[webui]` 段驱动
（缺省关闭，`ak config --reload` 热生效；`webui.auth_token` 可选配置后以 cookie 会话
保护 `/api/*`）。

## Architecture

- `src/main.rs` — CLI 入口：`daemon` 子命令族（唯一守护入口：`start`
  后台缺省拉起 / `start --foreground` 前台 / `install` / `uninstall` 系统服务 /
  `shutdown` 停止全部程序并退出；裸 `ak daemon` 为退出码 2 的 usage 错误；daemon
  配置含 `[webui]` 段时伺服内嵌控制台）| `start|stop|restart <app> [<program>]`
  （应用管理动词，双位置范围参数：裸 `<app>` 整 app 按序扇出、第二参数收窄单程序；
  `start` 返回 `{程序名, 配置路径, PID}` 并支持 `--json`；`--app` 旗标已删）|
  `run`（**仅** ad-hoc 注册：`--`/`--cmd` 二选一，应用命名 `--app`，**daemon-first**：
  经控制面 API 由守护进程执行，返回同样的 `{程序名, 配置路径, PID}` 并支持 `--json`；
  「先启动成功、后落盘」，守护离线一律退出码 3 且不隐式拉起；裸 `run` 为退出码 2
  的用法错误并列出 ad-hoc 两形态；守护形态的 `run` 在参数解析前拦截并给迁移提示）|
  `validate`（含 webui 无 token 的暴露警告）| `config`
  （daemon 配置动作：init/set/get/delete/edit/reload，白名单键表与读改写/
  原子落盘引擎在 `src/config.rs`；`--reload` 经 `/v1/reload` 触发在线重扫，
  可与 `--set`/`--delete`/`--edit` 组合、与 `--get`/`--init` 互斥；
  `[daemon]` 死键 host/port 报错，
  `auth_token` 为条件键：`tcp://` 形态必配、路径形态拒绝，
  组合校验 `auth_pair_error` 被 load/validate/set/get 共用）；
  控制子命令 `ps/start/stop/restart/log/pid/apply` 走配置端点上的 HTTP API
  （`ps` 缺省只读在线投影、`--all` 客户端合并本地注册表、`--out json|table`，
  表格带 PENDING 列与 pending 摘要行；离线时 `--all` 降级为注册表清单并退出 0）；
  `compose/remove` 管理 app 注册表
  （compose 按路径形态分派：目录/`.toml` → 既有注册；其他普通文件 → 脚手架生成
  `app_dir/<name>.toml`，`apply all` 中 `all` 为全量保留字，不可作应用名；
  `compose --apply` 保留离线隐式拉起；`remove` 是纯在线调用
  `POST /v1/apps/<app>/remove`，**不做本地直删**，守护离线退出 3，成功即由
  守护进程清理注册与内存状态、无需 apply）。顶层 `reload` 已删（`ak reload` 为未知子命令）。
- `src/supervisor.rs` — 守护循环（7 态状态机 × 程序），命令队列唯一写者；
  新增「暂存应用」`staged`（已启动未落盘的 ad-hoc app，检测合并使巡检不清理）与
  `AdhocUp/AdhocCommit/AdhocAbort` 命令；启动判定等待（`wait_startup`）跑在 HTTP
  连接线程而非命令处理器；
  `Command::Remove` 是移除的唯一入口：先按 `ProgramState::removable()`
  （`running/starting/stopping/backoff` 拒绝，`stopped/exited/fatal` 接受）查活态，
  再删注册记录、清内存/pending/用户停止记录、同步健康任务并重扫 pending，
  返回 `RemoveOutcome`（`deleted_file`/`kept_config`/`programs`/`applied`）与
  `RemoveError`（404/409/500 映射）；`apply_detection` 在改动任何状态前用同一
  判定做整批预检（`check_removable_deletions`，`--restart` 不豁免、范围外不阻塞）；
  `src/registry.rs` — app 注册表（app_dir 链接 + 脚手架生成文件
  + 占位 `*.toml.pending` 与原子 rename 落盘引擎；`remove` 只删记录，生成文件随
  记录删除、外部部署本体与日志保留）；`src/config.rs` — 分层配置
  （daemon + app 部署文件）；`src/pump.rs` — 输出接管/轮转/环形缓冲；
  `src/health.rs` — 健康探测（`ureq` 仅供它探测用户程序 URL，不得接回控制面）；
  `src/client.rs` — CLI HTTP/1.1 客户端（自建传输层，含 ad-hoc NDJSON 流式读取）。
- `src/transport.rs` — **唯一**知晓传输形态细节的模块：`Conn`/`Listener`/
  `Endpoint`（`tcp://` 解析与 TCP 监听/拨号在此；IPC 侧 chmod 0600/uid 0700
  目录/陈旧端点清理/Windows 命名管道 DACL 同样封死在此；`is_tcp()` 是唯一
  形态外泄面），模块内不出现 HTTP 语义，上层代码不出现端点原始串。
- `src/server.rs` — 控制平面：std 手写 HTTP/1.1 over 传输层（`/v1/*`；
  准入按端点形态：IPC 形态即 socket 属主、忽略 Authorization 头，TCP 形态
  Bearer 门——除 `/v1/health` 免凭据外 401），状态投影（`StatusDoc`/`ProgramInfo`，
  含 `config_path`/`health_confirmed`）由此导出；
  `POST /v1/run`（chunked NDJSON 进度流）、`/v1/run/commit|abort` 承载 ad-hoc 注册；
  `POST /v1/apps/<app>/remove` 复用 `Command::Remove`，`/v1/apply` 对活态删除目标
  回 409（与 `/api/apply` 同一消息）。
- `appkeeper-bench/` — 独立基准测量二进制（workspace 成员）：日志生成器 =
  bench 二进制自我重入（隐藏 `__generate`），指标全部从 `/v1` 投影与磁盘日志
  外部采集（daemon 侧零改动）；轮转对账/清理契约见 benchmark 规范
  （openspec/specs/benchmark）。
- `src/web.rs` + `src/api.rs` — Web 控制台（配置驱动：`[webui]` 段开启，
  supervisor 管理线程按意图 spawn/停 `web::serve`，reload 热生效）：axum 伺服内嵌
  SPA + `/api/*`（复用控制面投影；可选 `webui.auth_token` → `/api/auth` 换票
  cookie 会话）+ `/ws` 推送（快照/增量/日志/心跳，MessagePack 二进制帧）；
  `POST /api/apps/<app>/remove` 与 `/v1` 同构（同一 `Command::Remove` 与结果类型，
  `/ws` 增量帧新增 `removed`（程序名）与 `apps_gone`（app 名）让前端收敛）。
- `src/assets.rs` — `rust-embed` 内嵌 webui `dist/`。
- `webui/` — Web 控制台前端（pnpm + Vite + TypeScript + Lit），release 构建
  后 `dist/` 经 rust-embed 嵌入二进制；dist 不提交。build.rs 在 release（或
  `AK_WEBUI_BUILD=force`）时自动构建，debug 默认跳过前端工具链（rust-embed
  运行时读盘，无 Node 时回退占位页）。`lib/api.ts`（统一 fetch 封装 +
  401 换票重放）与 `lib/credentials.ts`（凭据状态机）+ `components/credential-gate.ts`
  构成可选 token 的前端配合；App 概况页的「移除应用」走 `lib/api.ts` 的
  `removeApp`，确认框说明删除边界且不自动 stop，成功后 `store` 记录
  `removedApps`/`flash` 并由 app-shell 收敛导航。

## Build & Run

```bash
cargo build --release                  # 产物 target/release/appkeeper(.exe)

cd webui
pnpm install
pnpm exec tsc --noEmit && pnpm test && pnpm build   # 类型检查 + 测试 + 产出 dist/，之后 cargo build 重新嵌入
pnpm dev                               # HMR dev server（:5273），代理 /api /health /ws → 后端（:9877）

cargo run -p xtask -- e2e             # 验收 e2e：CLI 检出/apply/范围/--restart/compose 场景
                                       #   + remove 守卫（活态拒绝、backoff 拒绝、stop→remove 一步、
                                       #     离线退出 3、手工删除的 apply 整批拒绝、/api 与 /v1 同构）
                                       #   + playwright 驱动真实浏览器的 webui 交互（--no-browser 跳过浏览器段）
                                       #   + appkeeper-bench firehose 冒烟（JSON 报告 + 清理断言）
cargo run -- validate                  # 校验 daemon 配置 + 全部注册应用
cargo run -- daemon start --foreground # 守护（缺省后台用 `daemon start`；daemon 配置含 [webui] 段时
                                       #   伺服内嵌控制台；config --set webui.listen=... + config --reload 可热开启）
cargo run -- ps                        # 控制面 CLI（经控制 socket；无任何 TCP 端口；
                                       #   --all 含未运行 app，--out json 等价 /v1/status）

cargo run -p appkeeper-bench -- --case firehose --duration 10
                                       # 性能基准：firehose/rotation/fanout/drip 四 case；
                                       #   缺省自带隔离环境（临时 workspace + 随机 socket 路径），
                                       #   --socket <path> 连同机已运行 daemon，
                                       #   --json <path> 导出机器可读报告；无阈值门禁

scripts/docker-test.sh                 # Docker 一次性全量验收（debian:bookworm-slim 基座）：
                                       #   cargo test --workspace + webui 三连 + xtask e2e（含浏览器段）
                                       #   + bench firehose 报告；产物落 docker-out/<时间戳>/
                                       #   --region cn 切大陆镜像；--no-build 复用镜像；--stages 选段

## OpenSpec 规范驱动开发（`.agents`）

本仓库使用 [OpenSpec](https://github.com/Fission-AI/OpenSpec) 管理变更流程，
技能安装于 `.agents/skills/openspec-*`（6 个），规范与变更存放在 `openspec/`
（`openspec/specs/` 存能力规范，`openspec/changes/` 存进行中的变更，config 为
`openspec/config.yaml`，schema: spec-driven）。需要 `openspec` CLI（≥ 1.12）。

技能一览（通过 `Skill` 工具加载，或直接参考 `openspec <cmd>`）：

| Skill | 用途 |
|---|---|
| `openspec-propose` | 一步生成完整变更提案（proposal + specs 增量 + design + tasks） |
| `openspec-apply-change` | 按已批准提案实施，逐任务打钩 |
| `openspec-archive-change` | 变更完成后归档，把 specs 增量合入正式规范 |
| `openspec-sync-specs` | 修补/同步归档后未合并的 spec delta |
| `openspec-update-change` | 实施中途补充/更新变更工件 |
| `openspec-explore` | 动手前的方案探索与头脑风暴 |

### 工作流约定

- **先规划后编码**：非平凡变更先走 `openspec-propose` 产出提案工件；提案只做规划，
  不得在同一次响应里顺手改业务代码。实施须等用户明确批准后调用 `openspec-apply-change`。
- 新建变更：`openspec new change "<name>" --json`；查看状态：`openspec status --change "<name>" --json`；
  校验：`openspec validate --change "<name>"`；完成后归档：`openspec archive <name> --yes`。
- 归档前用 `openspec validate --specs` 确认正式规范无损合入。
- 遵循 `.agents/skills/openspec-*/SKILL.md` 中各自声明的边界（如 propose 的 planning boundary）。

## Constraints

- 前端改动后必须 `pnpm exec tsc --noEmit && pnpm test && pnpm build`，再 `cargo build`，
  否则二进制里嵌的是旧前端。
- 后端进程管理逻辑（重启退避、Job Object、信号处理）涉及平台差异
  （unix `libc` / windows-sys），改动需两条平台路径都过一遍。
- webui 与控制面共享 `server.rs` 的状态投影与 `Supervisor` 命令队列：改状态
  字段/命令语义时 `/v1`、`/api`、WS 帧三处必须同源修改。
