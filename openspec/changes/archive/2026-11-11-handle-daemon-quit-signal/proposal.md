## Why

daemon 的信号入口当前未可靠覆盖 SIGTERM 与 SIGQUIT；两种信号都会走 Unix 默认终止行为，绕过受管程序的停止与回收流程。需要让它们都遵守「先停止全部 app，最后退出 daemon」的同一契约，避免服务管理或运维信号造成子进程残留。

## What Changes

- Unix 上为 daemon 补齐 SIGTERM 处理并增加 SIGQUIT 处理，两者复用同一优雅停机路径。
- 明确 daemon 收到 SIGTERM 或 SIGQUIT 后，停止重启与拉起活动，按既有逆序停止全部受管程序（含暂存的 ad-hoc 程序），等待每个程序优雅退出或完成超时强杀与回收后才退出。
- 重复收到 TERM/QUIT 时保持停机流程幂等，不重复执行停止流程，也不强杀正在优雅退出的 app。
- 增加真实进程信号验收，分别覆盖 TERM 与 QUIT，验证 app 的停止顺序及 daemon 最后退出。
- 不改变 Ctrl+C、`ak daemon shutdown`、Windows SCM 停止、现有停止顺序或 `stop_timeout` 语义；不将 SIGHUP 等其他信号纳入停机范围。

## Capabilities

### New Capabilities

无。

### Modified Capabilities

- `background-daemon`: 扩展前台/后台 daemon 的信号契约，要求 Unix SIGTERM 与 SIGQUIT 共用优雅停机，并明确 app 全部停止后 daemon 才退出。

## Impact

- `src/main.rs`: daemon 控制台入口的 Unix 信号注册与帮助文本。
- `src/supervisor.rs`、`src/program.rs`: 复用现有停机和程序回收路径，不改变其对外状态语义。
- `xtask/src/e2e.rs`: 真实 daemon/app 进程的 TERM、QUIT 停机验收与进程清理检查。
- Unix 信号处理可能需调整 `ctrlc` 特性或采用已有 `libc` 依赖；不引入新的控制面 API，不影响 Windows 服务控制路径。
