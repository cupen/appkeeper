# appkeeper — Agent Instructions

轻量级进程守护工具（Rust），通过 TOML 配置守护任意多个子进程：退出后指数退避重启、
stdout/stderr 落盘、Ctrl+C / SIGTERM 优雅停机、Windows Job Object 清理进程树。
内嵌 Web 控制台由 daemon 配置的 `[webui]` 段驱动（缺省关闭，`ak reload` 热生效）。

## Architecture

- `src/main.rs` — CLI 入口：`daemon`（唯一守护入口，可 `--detach` 后台拉起；
  daemon 配置含 `[webui]` 段时伺服内嵌控制台）| `run`（纯客户端动词，**daemon-first**：
  `<名>`/`--app` 启动、`--`/`--cmd` ad-hoc 注册全部经控制面 API 由守护进程执行，
  统一返回 `{程序名, 配置路径, PID}` 并支持 `--json`；ad-hoc 注册「先启动成功、
  后落盘」，守护离线一律退出码 3 且不隐式拉起；守护形态的 `run` 在参数解析前
  拦截并给迁移提示）| `validate` | `config`（daemon 配置本地管理：init/set/get/
  delete/edit 五动作，白名单键表与读改写/原子落盘引擎在 `src/config.rs`）；控制子命令
  `status/stop/restart/log/pid/reload/shutdown`
  走本地 HTTP API；`add/remove/list` 管理 app 注册表（add 按路径形态分派：
  目录/`.toml` → 既有注册；其他普通文件 → 脚手架生成 `app_dir/<name>.toml`，
  `apply all` 中 `all` 为全量保留字，不可作应用名；`add --apply` 保留离线隐式拉起）。
- `src/supervisor.rs` — 守护循环（7 态状态机 × 程序），命令队列唯一写者；
  新增「暂存应用」`staged`（已启动未落盘的 ad-hoc app，检测合并使巡检不清理）与
  `AdhocUp/AdhocCommit/AdhocAbort` 命令；启动判定等待（`wait_startup`）跑在 HTTP
  连接线程而非命令处理器；`src/registry.rs` — app 注册表（app_dir 链接 + 脚手架生成文件
  + 占位 `*.toml.pending` 与原子 rename 落盘引擎）；`src/config.rs` — 分层配置
  （daemon + app 部署文件）；`src/pump.rs` — 输出接管/轮转/环形缓冲；
  `src/health.rs` — 健康探测；`src/client.rs` — CLI HTTP 客户端（含 ad-hoc NDJSON 流式读取）。
- `src/server.rs` — 控制平面：std 手写 HTTP（`/v1/*`，可选 Bearer 鉴权），
  状态投影（`StatusDoc`/`ProgramInfo`，含 `config_path`/`health_confirmed`）由此导出；
  `POST /v1/run`（chunked NDJSON 进度流）、`/v1/run/commit|abort` 承载 ad-hoc 注册。
- `appkeeper-bench/` — 独立基准测量二进制（workspace 成员）：日志生成器 =
  bench 二进制自我重入（隐藏 `__generate`），指标全部从 `/v1` 投影与磁盘日志
  外部采集（daemon 侧零改动）；轮转对账/清理契约见 README「性能基准」。
- `src/web.rs` + `src/api.rs` — Web 控制台（配置驱动：`[webui]` 段开启，
  supervisor 管理线程按意图 spawn/停 `web::serve`，reload 热生效）：axum 伺服内嵌
  SPA + `/api/*`（复用控制面投影）+ `/ws` 推送（快照/增量/日志/心跳，
  MessagePack 二进制帧）；与控制面共享 `Supervisor` 与环形缓冲。
- `src/assets.rs` — `rust-embed` 内嵌 webui `dist/`。
- `webui/` — Web 控制台前端（pnpm + Vite + TypeScript + Lit），release 构建
  后 `dist/` 经 rust-embed 嵌入二进制；dist 不提交。build.rs 在 release（或
  `AK_WEBUI_BUILD=force`）时自动构建，debug 默认跳过前端工具链（rust-embed
  运行时读盘，无 Node 时回退占位页）。

## Build & Run

```bash
cargo build --release                  # 产物 target/release/appkeeper(.exe)

cd webui
pnpm install
pnpm exec tsc --noEmit && pnpm test && pnpm build   # 类型检查 + 测试 + 产出 dist/，之后 cargo build 重新嵌入
pnpm dev                               # HMR dev server（:5273），代理 /api /health /ws → 后端（:9877）

cargo run -p xtask -- e2e             # 验收 e2e：CLI 检出/apply/范围/--restart/add 场景
                                       #   + playwright 驱动真实浏览器的 webui 交互（--no-browser 跳过浏览器段）
                                       #   + appkeeper-bench firehose 冒烟（JSON 报告 + 清理断言）
cargo run -- validate                  # 校验 daemon 配置 + 全部注册应用
cargo run -- daemon                    # 守护（daemon 配置含 [webui] 段时伺服内嵌控制台；
                                       #   config --set webui.listen=... + reload 可热开启）
cargo run -- status                    # 控制面 CLI（默认端口 7310）

cargo run -p appkeeper-bench -- --case firehose --duration 10
                                       # 性能基准：firehose/rotation/fanout/drip 四 case；
                                       #   缺省自带隔离环境（临时 workspace + 随机端口），
                                       #   --connect <addr> 连同机已运行 daemon（--token Bearer），
                                       #   --json <path> 导出机器可读报告；无阈值门禁

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
