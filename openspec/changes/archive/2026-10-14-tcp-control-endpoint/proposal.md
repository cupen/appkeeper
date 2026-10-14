## Why

`unix-socket-control-plane`（已归档）把控制面收敛为「仅本地 IPC + 属主准入」，代价是放弃了 TCP 形态：容器编排的健康检查与 sidecar 通常只会说 HTTP over TCP、另一容器摸不到 UDS（除非共享 volume），Windows 侧部分工具链也没有 `curl --unix-socket` 等价物。控制面在 unix UDS 与 TCP 上承载的 HTTP/1.1 字节完全相同——两者的区别仅在端点 URL 与「谁回答你是谁」：UDS 由内核以文件属主在 `connect()` 阶段强制判定，TCP 必须由应用层凭据回答。本变更把两种形态统一为**同一个配置键的两种 URL 写法**，默认仍为 UDS，TCP 作为显式选择且强制配对 token，使默认配置的安全性与现状一字不差。

## What Changes

- **BREAKING（配置面扩张，默认行为不变）** `[daemon] socket` 键接受两种 URL 形态，二选一：本地 IPC 路径（缺省形态，unix UDS / Windows 命名管道）或 `tcp://host:port`。同一守护进程同一时刻只监听其中一种形态。
- **BREAKING** `[daemon] auth_token` 由「无条件死键」改为**条件键**：仅当 `socket` 为 `tcp://` 形态时合法且必配；`socket` 为路径形态时出现 `auth_token` SHALL 被校验拒绝。`host` / `port` 保持死键报错不变。
- 校验规则新增组合约束（与既有 `command`/`args` 互斥同类）：`socket` 为 `tcp://` 而 `auth_token` 为空 → 拒绝启动；`tcp://` 无法解析（缺 host、端口越界）→ 拒绝。
- `tcp://` 形态下鉴权回到 Bearer：除 `/v1/health` 外的所有 `/v1/*` 请求 SHALL 要求 `Authorization: Bearer <token>`，缺失或不匹配 SHALL 返回 401；`/v1/health` 免凭据（就绪探测与编排健康检查依赖它）。路径形态下与现状一致：无应用层凭据、忽略 `Authorization` 头。
- 客户端（CLI 全部子命令、`shell`、`daemonize` 就绪探测、bench）SHALL 按配置解析端点形态拨号，TCP 形态自动携带 token；退出码契约 0/1/2/3 不变。
- 远程管理 SHALL 是配置的产物而非新代码路径：另一台机器的配置写 `socket = "tcp://<host>:<port>"` + `auth_token` 后即可用 `ak -c <配置> <子命令>` 操作。
- benchmark：`--socket` 接受两种 URL 形态；`--token` 恢复且仅对 `tcp://` 形态有意义。

## Capabilities

### New Capabilities

（无——全部落在既有能力域内）

### Modified Capabilities

- `control-plane`: `本地 HTTP API` 由「MUST NOT 监听任何控制面 TCP 端口」改为「按配置二选一：本地 IPC 端点或 `tcp://` 端点」；`API 准入控制` 重写为「准入由端点形态判定」——路径形态属主准入、无凭据，`tcp://` 形态 Bearer 凭据、属主模型不适用；`控制面连接失败的错误区分` 增加 TCP 形态的不可达/凭据错误语义。
- `configuration`: `daemon 根配置` 的 `socket` 双形态与 `auth_token` 条件键、死键清单收窄为 `host`/`port`；`校验规则` 增加形态组合校验；`config 子命令` / `config --set 写入` / `config --get 读取` / `config --delete 删除` / `config --init 初始化` 键表与死键表述同步。
- `benchmark`: `连接既有 daemon 拓扑` 的 `--socket` 接受双形态、`--token` 恢复（仅 tcp:// 有意义）。
- `background-daemon`: 就绪探测语句与端点形态对齐（配置驱动，探测自动带凭据，无行为分叉）。

## Impact

- 代码：`src/transport.rs`（`Endpoint::parse` 识别 `tcp://`、TCP 的 `Conn`/`Listener` 实现、`BindError` 分类）、`src/server.rs`（按配置选监听器；TCP 形态恢复 per-connection token 校验，路径形态保持忽略 `Authorization`）、`src/client.rs`（拨号跟随配置，TCP 形态带 `Authorization` 头）、`src/config.rs`（`auth_token` 出死键清单、组合校验、`socket` 解析）、`appkeeper-bench/src/cli.rs` 与 `api.rs`（`--token` 恢复）、`xtask/src/e2e.rs`（TCP 形态用例）。
- 测试：transport 双形态解析与 bind 单测；server 的 token 门（tcp 形态 401/放行、路径形态忽略头）；config 组合校验逐分支；e2e 增加「tcp:// + token 全链路」与「tcp:// 缺 token 拒绝启动」。
- 文档：README 配置示例（tcp:// 形态与远程管理）、`conf/daemon.toml` / `examples/daemon.toml` / `config --init` 模板注释（`auth_token` 以注释形式回模板，注明仅 tcp:// 形态生效）。
- 依赖：无新增。
- 运维影响：默认形态与现状完全一致；升级零动作。选择 TCP 形态的部署显式承担「凭据即唯一防线」的责任，错误配置（缺 token）在启动期被拒绝而不是静默敞开。
