## Context

`run_daemon_console` 目前通过默认特性的 `ctrlc::set_handler` 请求停机；已解析的 ctrlc 3.5.2 默认仅处理 SIGINT，启用其 `termination` 特性会同时处理 SIGTERM 与 SIGHUP，不提供 SIGQUIT。现有 `Supervisor::run` 已在停机标记置位后统一调用 `shutdown_all`，逐程序同步执行 `ManagedProgram::stop`，Unix 上先 TERM、等待 `stop_timeout`，再 KILL 并 `wait` 回收。后台启动最终 re-exec 为同一前台入口，因此信号修复应落在该共同入口。

Windows SCM 已直接调用 `request_shutdown()` 并在服务壳里跟踪停机状态；它不经过 Unix 信号入口，不应被本变更改动。

## Goals / Non-Goals

**Goals:**

- 在共同 daemon 入口准确注册 SIGINT、SIGTERM、SIGQUIT，不引入额外信号行为。
- 信号只触发既有 supervisor 停机编排，保留单写者、停止顺序、暂存程序覆盖和进程回收保证。
- 用真实信号和具有可观察停止延迟的 app 验证 daemon 不会先于 app 退出。

**Non-Goals:**

- 不改变 SIGHUP 的默认行为，也不增加 SIGKILL 等不可捕获信号的处理。
- 不重新设计停止算法、控制面 shutdown API 或 Windows 服务生命周期。
- 不新增用户配置项；信号集合是固定的 Unix daemon 契约。

## Decisions

### D1: Unix 单独注册三个停机信号，Windows 保持现有入口

Unix 使用已有 `libc` 的 `sigaction` 将 SIGINT、SIGTERM、SIGQUIT 统一接到一个轻量通知机制；在专用普通线程中接收通知并调用 `Supervisor::request_shutdown()`。Windows 非服务模式继续使用 `ctrlc` 的 Ctrl+C 处理，SCM 继续走现有服务壳，避免为 Unix 扩展而影响 Windows 控制台行为。

**被否备选：** 仅启用 `ctrlc/termination`。它会顺带接管 SIGHUP，且仍不支持 SIGQUIT，无法满足已确认的精确信号范围。

**被否备选：** Unix 使用 `ctrlc` 后再用 `libc::signal` 补 SIGQUIT。两套全局注册器会分散停机入口和错误处理，且较简陋的 signal 接口不适合新增异步信号路径。

**被否备选：** 直接依赖 Tokio `signal`。daemon 与 supervisor 当前是同步线程模型，引入 runtime 只为三个信号会扩大生命周期与平台改动面。

### D2: 原始信号处理器只写 pipe，普通线程执行 Rust 停机请求

进程级静态 self-pipe 的读端设为非阻塞，写端保留阻塞语义；原始处理器只向写端发送一个固定字节并保存/恢复 `errno`，不获取 Rust mutex、不分配内存、不记录日志。专用线程循环读取 pipe，有通知时才记录信号日志并调用 `request_shutdown()`。目标信号的 `sigaction` 使用 `SA_RESTART`，从第一个处理器注册开始保持安装至进程退出，避免卸载/重建窗口落回默认动作；安装失败时在尚未进入守护循环前报错。未读到字节时唤醒按可重试处理，非暂时性读取故障记录后退出通知线程，不伪造停机。

采用一个静态 pipe 与一次初始化，而不把 raw fd 放进 `Supervisor` 公共状态：fd 只服务进程级信号 trampoline，supervisor 状态仍由原有 mutex/atomic 管理。每个注册的 daemon 进程只初始化一次，测试不得在 Rust 测试进程内安装这些全局处理器。

**被否备选：** 在处理器内直接调用 `request_shutdown()`。它会执行 mutex、atomic、condvar 和日志操作，上下文复杂且难以论证安全。

**被否备选：** 增加 `signal-hook` 依赖。现有 libc 足以实现很小的 self-pipe；是否后续改用它仅是等价实现优化，本变更不扩大依赖面。

### D3: 停机保持现有串行编排，信号只设置意图

通知线程收到任一目标信号后均进入同一个 `request_shutdown()`；重复信号可重复写入通知，但 supervisor 主循环只执行一次 `shutdown_all`。主循环退出条件已阻止后续 tick 与 `start_eligible`，因此不会为 backoff 中的程序再拉起进程。停止期间到达的排队控制命令不再执行，确保 start/restart/apply 不会与停机竞争；API 可对这类未处理请求断开，属于既有停机窗口语义。

`shutdown_all` 继续按 priority/app/name 的逆序逐程序 stop；每个 stop 等待自身的 `stop_timeout`，必要时强杀并回收。这里的「daemon 最后退出」以直接子进程已 `wait`、Unix 进程组已执行清理为准，不新增对脱离进程组的后代的无限等待。

### D4: 真实信号验收放在现有 Unix e2e pass，失败路径保证回收测试进程

在 `xtask/src/e2e.rs` 新增独立 Unix 场景，复用隔离 workspace 和真实 `ak daemon start --foreground` 启动器。每个 TERM/QUIT case 使用至少两个程序及不同的停止延迟，通过测试 app 的 TERM 处理器记录退出时间/顺序；程序之一可忽略 TERM 至较短 `stop_timeout`，证明超时强杀仍发生在 daemon 退出之前。

向 daemon 的 pid 直接发信号，而非其进程组，防止测试 app 错收 daemon 的 QUIT。等待期间先断言 app 仍存活且 daemon 未退出，再断言 app 消失、daemon 最后正常退出；停机中补发另一种信号覆盖幂等。后台形态用同一 re-exec 入口已由现有实现保证，至少在 QUIT case 经 `ak daemon start` 后台启动验证该链路。测试 cleanup guard 必须终止遗留 daemon 和 app，不依赖成功路径收尾。

单元测试仅覆盖可抽离的管道通知转换或参数/平台编译逻辑；全局信号注册与真实时序不放进共享测试进程。

## Risks / Trade-offs

- **[信号处理器中的锁或日志导致死锁/未定义行为]** → 原始处理器只执行固定字节 pipe write 并恢复 errno；日志和 Rust 同步操作全部移到专用线程。
- **[pipe 写端阻塞令高频信号处理器卡住]** → 采用阻塞写端，pipe 容量内写入通常可立即完成；不用非阻塞写端，避免缓冲区满时信号被静默丢弃。专用线程持续排空，停机一旦置位后的额外字节不影响结果。
- **[过早安装或跨模式注册改变其他命令的信号行为]** → 只在 `run_daemon_console` 成功初始化核心后安装，Unix/Windows 条件编译隔离，失败时明确报错不进入未受保护的守护循环。
- **[后台测试错误地向整个进程组发 QUIT]** → 直接向 daemon pid 投递；app 的独立进程组保持现有隔离。
- **[停机时长超过外层服务管理器预算]** → 保持现有逐程序串行停止和 systemd 超时计算，不在本次顺带改并发或预算算法；真实 e2e 采用小超时避免拖慢套件。

## Migration Plan

直接替换二进制的 daemon 信号注册即可，无配置或持久化数据迁移。部署后无需修改 systemd unit：它仍发送 SIGTERM，只是现在开始可靠走优雅停机。回滚为恢复旧二进制；不会有磁盘格式兼容问题，但回滚会重新失去 TERM/QUIT 的优雅停机保证。
