# tasks: daemon-subcommand

## 1. CLI 入口重构（src/main.rs）

- [x] 1.1 新增 `Cmd::Daemon { detach }` 子命令（doc 注释：唯一守护入口），`detach` 分派到现 `run_detach`、缺省走 `run_daemon`；`Cmd::Run` 删除 `detach` 字段及其守护分支（`else if *detach` / 裸分支），`run` 只剩 name/app/ad-hoc 三种客户端形态。验证：`cargo build` 通过，`ak daemon` 前台守护、`ak daemon --detach` 后台拉起行为与原裸 `run` 一致
- [x] 1.2 `dispatch` 的 `None` 分支改为 `Cli::command().print_help()` 输出完整 help 到 stdout 后返回 EXIT_OK（design D1）。验证：执行裸 `ak`，stdout 输出与 `ak --help` 等价，退出码 0，不启动守护进程
- [x] 1.3 新语义下裸 `ak run`（无 name/app/`--`/`--cmd`）在 dispatch 层报「nothing to run」用法错误并列出三种客户端形态，退出码 2（design D4）。验证：`ak run` 以退出码 2 失败且错误含 `run <name>` / `run --app` / `run --` 提示
- [x] 1.4 `logger init` 条件（`src/main.rs:379` 的 `None | Run` 匹配）改为含 `Daemon`：守护入口与客户端子命令的日志初始化行为保持原状。验证：`ak daemon` 下配置 `log_level` 生效、客户端子命令 warning 不丢

## 2. 守护形态 run 拦截（src/main.rs）

- [x] 2.1 新增 clap 前拦截函数（模式沿用 `reject_removed_start`）：首子命令 `run` 且其余 token 仅 `-c/--config <path>`（含 `--config=<path>`）与 `--detach` 时，exit 2，提示按形态分流——含 `--detach` 提示 `ak daemon --detach`；仅 config 形态追加 `ak service install --force --now` 重新生成 unit 提示（design D2/D3 分流）。验证：`ak run --detach`、`ak run -c /tmp/x.toml`、`ak -c /tmp/x.toml run` 均以 2 退出且提示正确；`ak run -- node x`、`ak run web` 不被误伤
- [x] 2.2 `main()` 中在 `reject_removed_start` 之后调用该拦截。验证：`ak run --detach` 在 clap 解析前即被拦截（错误信息为迁移提示而非 clap 用法错误）

## 3. 内部调用与文案同步

- [x] 3.1 `src/daemonize.rs`：re-exec 参数 `cmd.arg("run")` 改为 `cmd.arg("daemon")`，模块注释同步。验证：daemon 离线时 `ak run -- sleep 30` 自动拉起守护进程成功（隐式拉起协议不变）
- [x] 3.2 `src/service.rs`：ExecStart 渲染改 `daemon --config <path>`，单元测试期望值（`ExecStart=... run --config ...` 两处）同步。验证：`cargo test -p appkeeper service` 通过，渲染输出含 `daemon --config`
- [x] 3.3 `src/client.rs:262` 不可达提示 `(is \`ak run\` active?)` 改为 `(is \`ak daemon\` active?)`。验证：守护进程离线时 `ak status` 输出新提示

## 4. 单测与 e2e 同步

- [x] 4.1 `src/main.rs` 测试模块更新：删除/改写「Bare `run` (daemon) and `run --detach` parse」用例为 `daemon` 子命令解析用例；新增裸 `ak`（help, exit 0）与守护形态 `run` 拦截的用例。验证：`cargo test` 全绿
- [x] 4.2 `xtask` e2e/stress 与 `appkeeper-bench` topology 中以 `run` 起 daemon 的调用点（`xtask/src/e2e.rs:160,578`、`xtask/src/stress.rs:130`、`appkeeper-bench/src/topology.rs:264`）改为 `daemon`。验证：`cargo run -p xtask -- e2e` 全部场景通过（含 webui 段；另含 detach_pass R9、tests/revamp_integration.rs 与 appkeeper-bench/tests/integration.rs 中同类调用点，实跑见 6.1）
- [x] 4.3 e2e 补守护形态拦截断言：`ak run --detach` 与仅 `--config` 形态返回退出码 2 且输出对应迁移提示。验证：新增 e2e 场景通过（offline_pass 新增 G0c4：`run --detach`、`run --config <path>`、`run --config=<path>`、`--config <path> run` 均断言 exit 2 与分流提示；另断言裸 `run` 的三形态用法错误 exit 2 与 `run <name>` 不被误伤（离线 exit 3）；R2 补 404 提示含 `run -- <cmd...>` 断言；`cargo run -p xtask -- e2e --no-browser` 全绿）
- [x] 4.4 `appkeeper-bench` firehose 冒烟（自带隔离环境）。验证：`cargo run -p appkeeper-bench -- --case firehose --duration 5` 正常出报告且清理断言通过（exit 0，integrity PASS 2576824 rows，无残留进程/临时目录，隔离 daemon 端口关闭；e2e bench 段 L 亦通过）

## 5. 文档同步

- [x] 5.1 README 中以裸 `run` / `run --detach` 指代守护入口的示例与说明改为 `daemon` / `daemon --detach`；AGENTS.md 的 `run`（唯一守护入口）描述同步。验证：`grep -n "ak run\|`run`" README.md AGENTS.md` 无守护语义残留
- [x] 5.2 `.ansible/README.md` 增加跨版本升级说明：ExecStart 变更后须设 `appkeeper_force_overwrite: true` 重跑（或手动 `ak service install --force --now`），并注明回滚须同时恢复旧 unit。验证：文档含完整升级与回滚步骤

## 6. 整体验收

- [x] 6.1 全量回归：`cargo build --release && cargo test`，`cargo run -p xtask -- e2e`，`cargo run -- validate`；对照 specs/control-plane 增量逐场景人工核对（裸 ak help、daemon 前台/后台、拦截分流提示、客户端形态不变）
  > 注（验收收口，2026-10-13）：全部实跑通过——`cargo build --release` / `cargo test --workspace`（214+53+2 passed）全绿；`cargo run -p xtask -- e2e`（含真实浏览器 browser pass，BROWSER PASS OK）与 `--no-browser` 双跑 PASSED；`cargo run --release -- validate` exit 0（/etc 无 daemon.toml 时按配置规范报内置默认值，非失败）；control-plane 增量逐场景核对完毕（裸 ak help=stdout+exit 0 且与 `ak --help` 等价；daemon 前台由 e2e 全程使用、`daemon --detach` 见 R9；拦截分流见 G0c4；客户端形态见 R1/R2/R3）。ignored 集成测试实跑：revamp_integration 12/12、appkeeper-bench 14/14（串行；`--test-threads=2` 下 3–4 个 timing 敏感用例失败，HEAD 同样复现，属 pre-existing 抖动，非本变更引入）。review 报出的 xtask `build_daemon()` 悬空路径（`target/debug/appkeeper` → bin 实名 `ak`，rename 遗留）已修（xtask/src/main.rs:140）。
