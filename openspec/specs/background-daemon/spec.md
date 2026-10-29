# background-daemon Specification

## Purpose
定义 appkeeper 守护进程的后台化能力：`ak daemon start` 的后台运行语义（前台形态为 `ak daemon start --foreground`），以及「立即生效」入口（`compose --apply`；run 的 ad-hoc 形态 `--`/`--cmd` 已移出该名单）在守护进程离线时的隐式拉起协议（就绪判定、竞态容忍、失败语义），使「一条命令从冷启动跑起来」成立，同时守护进程保持全局单例语义。

## Requirements

### Requirement: 后台运行守护进程（daemon start）

`ak daemon start` SHALL 以后台方式启动守护进程（原 `ak daemon --detach` 的语义成为缺省，`--detach` 旗标移除）：进程 SHALL 在分离的进程组中运行（调用方终端的 Ctrl+C / 退出不影响它），stdout/stderr SHALL 追加重定向到 daemon 配置 `log_dir` 下的守护进程后台日志文件。后台实现 SHALL 以分离的子进程重新执行自身（re-exec argv 为 `ak daemon start --foreground -c <config>`），父进程等待守护进程就绪。命令 SHALL 等待守护进程就绪（控制端口健康探测通过）后返回：就绪 SHALL 打印监听地址与后台日志路径并以 0 退出；就绪超时或启动失败 SHALL 以非零码退出并指明日志位置。端口冲突行为 SHALL 沿用「启动冲突处理」规范（明确报错，不静默成功）。

`ak daemon start --foreground` MUST 保持前台语义（Ctrl+C / SIGTERM 优雅停机），供 systemd ExecStart、调试与 re-exec 子进程使用。隐藏旗标 `--service <NAME>` SHALL 落于 start 之上（与 `--foreground` 互斥）：由系统服务安装器写入服务启动命令（Windows SCM 的 binPath，见 `service-registration`），以服务托管模式运行而非控制台模式；人手输入 SHALL 得到用法说明。裸 `ak daemon`（无子命令）SHALL 以退出码 2 报 usage 错误并列出 `start` / `install` / `uninstall` / `shutdown`。`run --detach` 不再是合法入口：守护形态的 `run` 被拦截的语义见 `control-plane` 能力规范。

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

#### Scenario: re-exec 指向前台形态

- **WHEN** 执行 `ak daemon start` 触发后台拉起
- **THEN** 派生子进程的 argv 为 `ak daemon start --foreground -c <config>`，父进程就绪探测通过后退出

#### Scenario: 裸 daemon 报 usage 错误

- **WHEN** 执行 `ak daemon`（无子命令）
- **THEN** 以退出码 2 报 usage 错误并列出四个子命令，不启动守护进程
### Requirement: 立即生效入口的隐式拉起协议

`compose --apply` 在守护进程离线时 SHALL 自动后台拉起守护进程后继续应用：

- 拉起方式 SHALL 与 `ak daemon start` 一致（分离进程组、同一 `-c/--config` 配置解析、stdio 重定向到 `log_dir` 后台日志文件）。
- 就绪判定 SHALL 采用有界轮询健康探测，探测请求 SHALL 经控制端点发出——端点形态由配置的 `socket` 值决定（本地 IPC 路径或 `tcp://host:port`），`tcp://` 形态的探测 SHALL 自动携带配置的 `auth_token` 凭据：就绪后继续执行 apply；超时 SHALL 以非零码退出，错误信息指明日志位置与补救方式。
- 竞态容忍：多个调用同时触发拉起时，因控制端点占用而退出的实例 SHALL 自行终止；任一调用只要轮询到健康的守护进程即视为就绪并继续，MUST NOT 因自身派生的实例退出而误报失败。
- 守护进程在线时 MUST 直接执行 apply，不派生新实例。
- 拉起后的守护进程为全局单例，MUST NOT 与调用方生命周期绑定（调用退出后守护进程继续运行）。

`run` 的 ad-hoc 形态（`--`/`--cmd`）SHALL NOT 适用本隐式拉起协议：该形态为 daemon-first（见 `control-plane` 与 `app-registry`），守护进程离线时 SHALL 直接以退出码 3 报错，MUST NOT 后台拉起守护进程。

#### Scenario: 冷启动一条命令生效

> 本变更将 run 的 ad-hoc 形态移出隐式拉起名单（标题为既有场景锚点，现行行为以下文为准）：冷启动一条命令仍可由 `compose --apply` 达成；run 的 ad-hoc 形态离线时改为退出码 3。

- **WHEN** 守护进程未运行时执行 `ak compose ./server --name web --apply`
- **THEN** 守护进程被自动后台拉起，`web` 注册并立即生效进入运行状态，命令以 0 退出

#### Scenario: add --apply 离线同样自动拉起

> 本变更将 `add` 改名 `compose`（标题为既有场景锚点，现行行为以下文为准）。

- **WHEN** 守护进程未运行时执行 `ak compose ./server --name abc --apply`
- **THEN** 行为与在线一致：守护进程被拉起后 `abc` 生效，不报「守护进程离线」错误

#### Scenario: 在线短路

- **WHEN** 守护进程已运行时执行 `compose --apply`
- **THEN** 直接执行 apply，不派生新的守护进程实例

#### Scenario: 并发拉起只有一个守护进程存活

- **WHEN** 两个 `compose --apply` 调用几乎同时触发隐式拉起
- **THEN** 仅一个守护进程实例存活并服务两者，两个调用均成功完成

#### Scenario: run 离线不拉起

- **WHEN** 守护进程未运行时执行 `ak run -- python -m http.server 8000 --name web`
- **THEN** 命令以退出码 3 报守护进程不可达，MUST NOT 后台拉起守护进程

#### Scenario: 就绪探测经 socket 且无需凭据

> 本变更后端点形态由 `socket` 值决定（标题为既有场景锚点，现行行为以下文为准）：IPC 路径形态无需凭据；`tcp://` 形态探测自动携带配置的 `auth_token`。

- **WHEN** 守护进程离线时执行 `ak compose ./server --name abc --apply` 触发隐式拉起（IPC 路径形态）
- **THEN** 就绪轮询通过控制端点完成，不携带任何 token；系统上不存在控制面 TCP 端口可供探测

#### Scenario: 就绪探测经配置端点且自动带凭据

- **WHEN** 配置为 `socket = "tcp://127.0.0.1:7310"` 与 `auth_token`，守护进程离线时执行 `ak compose ./server --name abc --apply` 触发隐式拉起
- **THEN** 就绪轮询经该 TCP 端点完成并自动携带凭据（`/v1/health` 免凭据亦可判定存活），系统上不存在任何 socket 文件
