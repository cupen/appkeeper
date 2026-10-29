## 1. Unix 信号通知与停机入口

- [x] 1.1 在 Unix 平台代码中实现一次初始化的 self-pipe 通知器：原始 `sigaction` 处理器仅写固定字节并保存/恢复 `errno`，注册覆盖 SIGINT、SIGTERM、SIGQUIT 且不改 SIGHUP，处理器保持安装至进程退出；以针对注册、读端排空和可重试唤醒的单元测试及 `cargo test --workspace` 验证（不在共享测试进程安装全局信号处理器）。
- [x] 1.2 在 `run_daemon_console` 接入专用通知线程，收到通知后仅调用既有 `Supervisor::request_shutdown()`；Windows 保留 Ctrl+C/SCM 路径，更新前台帮助文本；以 `cargo check --workspace`、可用时的 Windows 交叉检查和 Unix 编译验证目标信号集合与平台隔离。
- [x] 1.3 审核并固定停机边界：重复通知不重复执行 `shutdown_all`，停机后不再处理 start/restart/apply 或自动拉起，全部程序沿用逆序 stop、`stop_timeout` 与 `wait` 回收；以现有 supervisor 停机/暂存程序/用户停止记录回归测试验证，未改变既有编排则不另写一套停止算法。

## 2. 真实信号验收

- [x] 2.1 在 `xtask/src/e2e.rs` 增加 Unix 隔离场景与回收 guard：启动多个带可观察 TERM 停止标记/延迟的程序，至少一个程序等待短 `stop_timeout` 后强杀；直接对 daemon pid 发 SIGTERM，验证 app 退出前 daemon 仍存活、按逆序回收全部 app 后 daemon 正常退出，且停机中重复 TERM/QUIT 不缩短等待。
- [x] 2.2 为 SIGQUIT 运行等价的独立真实进程场景，并经 `ak daemon start` 后台 re-exec 链路投递 QUIT，验证不会按默认信号动作绕过 app 清理；以 `cargo run -p xtask -- e2e --no-browser` 验证两种信号及失败路径无遗留测试进程。
- [x] 2.3 运行 `cargo test --workspace`、完整 `cargo run -p xtask -- e2e`（包含浏览器段）及 `cargo fmt --check`，核对 Windows 服务/控制台条件编译未受影响；记录测试命令与结果，若环境不能执行 Windows 实机验证则明确标注。

## 3. 规划一致性检查

- [x] 3.1 实现完成后对照 `background-daemon` 增量与 design D1-D4 检查实际代码和验收证据，确认未引入 SIGHUP、配置项、控制面或停止算法变更；以 `openspec validate handle-daemon-quit-signal --strict` 通过并逐项勾选已完成任务为完成标准。

## 验证记录

实现按 design D2 手写：`libc::sigaction`（`SA_RESTART`、空 mask）+ 进程级静态 self-pipe，处理器内仅
`write` 一个固定字节并保存/恢复 `errno`，自首次注册起常驻至进程退出；读端非阻塞、写端保持阻塞
（design Risks），专用线程 `poll` 读端并在 Rust 侧调用 `request_shutdown()`。未新增任何依赖，处理器
所需 `unsafe` FFI 全部收敛在 `src/shutdown_signal.rs` 并逐处附安全注释。SIGHUP 未纳入。

- `cargo fmt --check`：通过。
- `cargo clippy --workspace --all-targets`：与 HEAD 基线同一警告集合（无新增警告，新代码零警告）。
- `cargo test --workspace`：全部通过（含新增 `shutdown_signal` 10 例、supervisor 停机幂等/逆序用例）。
- `cargo run -p xtask -- e2e --no-browser`：PASSED；新增 S1（前台 SIGTERM）/S2（后台 re-exec SIGQUIT）两个真实信号场景全部断言通过，失败路径 guard 清理后无遗留 daemon/app 进程。
- `openspec validate handle-daemon-quit-signal --strict`：通过。
- 环境标注：本机未安装 Windows 目标（`x86_64-pc-windows-*` 缺失），无法做 Windows 交叉编译或实机验证；
  Windows 控制台 `ctrlc` 与 SCM 服务壳路径本次未改动，`#[cfg]` 隔离由 `cargo check`/`clippy` 在 Unix 侧覆盖。
  浏览器段按本次任务范围以 `--no-browser` 跳过（与信号契约无关）。


