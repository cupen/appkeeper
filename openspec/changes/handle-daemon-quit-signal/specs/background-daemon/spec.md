## MODIFIED Requirements

### Requirement: 后台运行守护进程（daemon start）

`ak daemon start` SHALL 以后台方式启动守护进程（原 `ak daemon --detach` 的语义成为缺省，`--detach` 旗标移除）：进程 SHALL 在分离的进程组中运行（调用方终端的 Ctrl+C / 退出不影响它），stdout/stderr SHALL 追加重定向到 daemon 配置 `log_dir` 下的守护进程后台日志文件。后台实现 SHALL 以分离的子进程重新执行自身（re-exec argv 为 `ak daemon start --foreground -c <config>`），父进程等待守护进程就绪。命令 SHALL 等待守护进程就绪（控制端口健康探测通过）后返回：就绪 SHALL 打印监听地址与后台日志路径并以 0 退出；就绪超时或启动失败 SHALL 以非零码退出并指明日志位置。端口冲突行为 SHALL 沿用「启动冲突处理」规范（明确报错，不静默成功）。

`ak daemon start --foreground` MUST 保持前台语义，供 systemd ExecStart、调试与 re-exec 子进程使用。Unix 上前台形态及后台 re-exec 得到的前台守护进程收到 SIGTERM 或 SIGQUIT 时 SHALL 进入与 Ctrl+C、`ak daemon shutdown` 相同的优雅停机流程：停止对程序的自动拉起与重启，按既有启动逆序停止全部受管程序（含已启动但未落盘的 ad-hoc 程序），并等待每个程序优雅退出或到达其 `stop_timeout` 后完成强制终止与回收；全部程序停止完成后 daemon MUST 才退出。TERM 与 QUIT SHALL 触发同一停止顺序和超时语义，MUST NOT 直接终止 daemon 而遗留仍在运行的 app。停机期间重复收到 TERM/QUIT SHALL 保持幂等，不得重新执行停机或绕过正在进行的优雅等待。隐藏旗标 `--service <NAME>` SHALL 落于 start 之上（与 `--foreground` 互斥）：由系统服务安装器写入服务启动命令（Windows SCM 的 binPath，见 `service-registration`），以服务托管模式运行而非控制台模式；人手输入 SHALL 得到用法说明。裸 `ak daemon`（无子命令）SHALL 以退出码 2 报 usage 错误并列出 `start` / `install` / `uninstall` / `shutdown`。`run --detach` 不再是合法入口：守护形态的 `run` 被拦截的语义见 `control-plane` 能力规范。

#### Scenario: 后台启动并就绪

- **WHEN** 执行 `ak daemon start` 且端口可用
- **THEN** 命令打印监听地址与后台日志路径后以 0 退出，守护进程在后台存活，调用方终端关闭后仍运行

#### Scenario: 端口已占用时报错

- **WHEN** 已有守护进程实例占用端口时执行 `ak daemon start`
- **THEN** 以非零码退出，错误信息指出端口占用与已运行实例的可能，不产生第二个实例

#### Scenario: 就绪超时

- **WHEN** 派生的守护进程因配置错误始终无法就绪
- **THEN** 命令以非零码退出，错误信息指明后台日志位置与补救方式

#### Scenario: 前台语义不变

- **WHEN** 执行 `ak daemon start --foreground`
- **THEN** 守护进程前台运行，Ctrl+C 优雅停机

#### Scenario: SIGTERM 先停 app 再退出 daemon

- **WHEN** Unix 上运行中的 daemon 收到 SIGTERM，且多个 app 程序正在运行
- **THEN** daemon 按既有启动逆序停止并回收全部程序，在所有程序退出前保持存活，全部程序退出后才退出，不遗留存活的 app 进程

#### Scenario: SIGQUIT 与 SIGTERM 同样优雅停机

- **WHEN** Unix 上后台启动的 daemon 收到 SIGQUIT，且 app 程序需要一段时间响应优雅终止
- **THEN** daemon 走与 SIGTERM 相同的优雅停机流程，等待程序退出或按 `stop_timeout` 完成强杀与回收后才退出，不按 SIGQUIT 默认动作直接终止

#### Scenario: 停机中的重复 TERM 或 QUIT 保持幂等

- **WHEN** daemon 正在等待 app 优雅退出时又收到一个或多个 TERM/QUIT
- **THEN** 当前停机流程继续，不重复停止程序、不缩短既定 `stop_timeout`，最终仍在全部程序停止后退出

#### Scenario: re-exec 指向前台形态

- **WHEN** 执行 `ak daemon start` 触发后台拉起
- **THEN** 派生子进程的 argv 为 `ak daemon start --foreground -c <config>`，父进程就绪探测通过后退出

#### Scenario: 裸 daemon 报 usage 错误

- **WHEN** 执行 `ak daemon`（无子命令）
- **THEN** 以退出码 2 报 usage 错误并列出四个子命令，不启动守护进程
