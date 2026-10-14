## Why

控制面目前默认**完全没有鉴权**：`[daemon] auth_token` 不配置时，唯一防线是「绑在回环地址」，而任何本机用户都能连回环——`curl -X POST http://127.0.0.1:7310/v1/programs/api/stop` 即可停掉任一被守护程序。同一个洞还有第二个门：开启 `[webui]` 后 `src/web.rs` 的 `/api/*` 零鉴权且带 stop / apply 全权限，且它在进程内直连 `Arc<Supervisor>`，根本不经控制面。结果是「谁能控制守护进程」这个问题在系统里**没有任何一个由操作系统强制回答的地方**，全靠用户自觉去配一个默认关闭的 token。本变更把这个问题的答案收敛到唯一一个位置：本地 IPC 端点本身的属主。

## What Changes

- **BREAKING** 控制面传输层由 TCP 回环改为**本地 Unix domain socket**。守护进程 SHALL NOT 再监听任何控制面 TCP 端口，`/v1/*` 端点集合、请求/响应语义与错误码一字不变。
- **BREAKING** 控制面鉴权模型重写：身份由**操作系统**给出（unix：socket `0600` + uid 目录 `0700`；Windows：命名管道 + DACL 安全描述符），`[daemon] auth_token` 键 SHALL 被删除。
- **BREAKING** `[daemon] host` / `port` 两个键 SHALL 被删除；daemon 配置中出现这三个死键时 `validate` SHALL 报错而非忽略。
- 新增可选 `[daemon] socket` 键覆盖默认 socket 路径；缺省为 `/run/appkeeper/<uid>/ak.sock`（unix）/ `\\.\pipe\ak-<用户名>.sock`（Windows）。
- Windows 上控制面 SHALL 走命名管道并以 DACL 显式收紧到「运行账户 + SYSTEM + Administrators」，使权限语义与 unix 的 `0600` 等价。
- daemon 启动时若 bind 撞 `EADDRINUSE`，SHALL 先尝试连接：连得上报「已有 ak 在运行」并退出，连不上认定陈旧端点并清理后重建。
- **BREAKING** 客户端改为手写轻量传输层（`trait Conn: Read + Write`），HTTP/1.1 语义保留；`ureq` 继续保留在依赖中，仅用于健康探测等面向用户程序的 HTTP，不再承载控制面流量。
- `ak shell`、`appkeeper-bench` SHALL 跟随切换到 socket；bench 的 `--connect <addr>` 改为 `--socket <path>`。
- daemon 后台拉起协议的就绪探测（`daemonize.rs`）SHALL 改走 socket。
- systemd unit SHALL 增加 `RuntimeDirectory=appkeeper` 与 uid 分层目录，使非 root 部署与多用户并存场景可用。
- Web 控制台 SHALL 保留 TCP（浏览器无法访问 unix socket），但新增**独立**的 `[webui] auth_token`：默认关闭以不破坏既有部署，配置后以 cookie 会话保护 `/api/*`。

## Capabilities

### New Capabilities

（无——全部落在既有能力域内，沿用 `run-daemon-first` 的先例）

### Modified Capabilities

- `control-plane`: `本地 HTTP API` 的传输层由 TCP 端口改为本地 socket（端点集合与协议语义不变）；`API 鉴权` 整条重写为操作系统强制身份并删除 `auth_token`；`启动冲突处理` 由端口占用改为 socket 端点占用并新增陈旧端点清理；新增 `控制 socket 端点` 与 `控制面连接失败的错误区分` 两条需求。
- `configuration`: `daemon 根配置` 删除 `host` / `port` / `auth_token`、新增 `socket`；`校验规则` 增加死键报错与新键校验；`config 子命令` / `config --set 写入` / `config --get 读取` / `config --delete 删除` 的键表同步增删；`webui 配置段` 新增 `auth_token` 键（与既有「本段不新增鉴权键」的表述直接冲突，需改写）。
- `webui-api`: 新增 `/api/*` 的可选 token 鉴权与一次性 cookie 换票。
- `background-daemon`: 后台拉起协议的就绪探测从 TCP 端口改为 socket。
- `service-registration`: systemd unit 增加 `RuntimeDirectory` 与 uid 分层，使 socket 路径可写。
- `benchmark`: bench 客户端与隔离环境拓扑从随机端口改为随机 socket 路径。

## Impact

- 代码：新增传输层模块（connect / bind / chmod 0600 / ACL / 陈旧端点清理，全部 UDS 与管道细节封闭于此）；`src/server.rs`（`bind`/`serve`/`handle_connection` 的监听器类型与 `token` 参数删除）、`src/client.rs`（改用自建传输层，保留 HTTP 语义与退出码 0/1/2/3）、`src/main.rs`、`src/shell.rs`、`src/daemonize.rs`（就绪探测）、`src/config.rs`（删三个键、加 `socket` 与 `[webui] auth_token`、键表与模板渲染）、`src/service.rs`（`render_unit` 增加 `RuntimeDirectory`）、`appkeeper-bench/src/api.rs` 与 `topology.rs`。
- 测试：`xtask` e2e（多用户隔离、socket 权限位、陈旧端点、bench 隔离环境）、`server.rs` 与 `daemonize.rs` 现有测试的传输层替换（`daemonize.rs:178` 的「空闲端口假健康应答器」改为绑定临时 socket）。
- 文档：README（安装与配置、webui 鉴权、排障用 `curl --unix-socket`）、AGENTS.md 架构行、模板 `conf/daemon.toml` 与 `examples/daemon.toml`、升级说明（已有装机实例的 `host`/`port`/`auth_token` 会导致校验失败）。
- 依赖：**无新增第三方依赖**。`ureq` 保留（`health.rs` 仍需要它探测用户程序的 URL）；Windows 侧复用既有 `windows-sys 0.61`（已启用 `Win32_Security` / `Win32_Foundation`）。
- 运维影响：升级后守护进程不再监听任何控制面端口，防火墙/容器端口映射中相关条目失效；已有配置文件含 `host`/`port`/`auth_token` 的实例升级后将拒绝启动，必须先删键；远程控制能力被移除（`run-daemon-first` 已将 `run` 定为纯客户端动词，该能力本就是半吊子且默认不安全）。
