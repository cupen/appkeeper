## 1. 传输层模块（无行为变化的先行落地）

- [x] 1.1 新建传输层模块，定义 `trait Conn: Read + Write` 与 `trait Transport { fn connect(&self) -> io::Result<Box<dyn Conn>> }`；unix 实现为 `UnixStream` 包装（含 `try_clone` 拆分读写），Windows 实现为命名管道 handle 包装；模块内 SHALL NOT 出现 HTTP 语义，单测断言两端各自可 connect + 双向读写
- [x] 1.2 实现 unix 监听原语 `bind(path)`：确保 `/run/appkeeper` 存在且为 `0755`、uid 子目录存在且为 `0700`，创建 socket 并 `chmod 0600`，返回 listener；单测覆盖「目录已存在且权限正确 / 目录缺失自动创建 / 目录存在但权限过宽时收紧」
- [x] 1.3 实现陈旧端点判定：`bind` 前若路径已存在则先 `connect()` 一次——成功→返回 `AlreadyRunning`、被拒→校验其为 socket 类型后删除并重建、非端点文件→返回 `PathOccupied` 且不删除；单测覆盖三分支（含删除前类型校验）
- [x] 1.4 实现 Windows 管道监听原语 `bind(pipe_name)`：以 `ConvertStringSecurityDescriptorToSecurityDescriptor` 构造 DACL，仅含运行账户 SID + SYSTEM + Administrators，显式移除 Everyone / Users / Authenticated Users；单测断言生成的 DACL 主体集合与预期一致（验证：`cargo test --bin ak transport::` 15 项全过，含 `control_pipe_dacl_subjects_are_exactly_sy_ba_and_running_account`；SDDL 文本为纯函数、全平台编译可测；ConvertStringSecurityDescriptorToSecurityDescriptor 实际转换与未授权账户连接失败属运行时行为，本机为 linux 无法验证，待 windows-latest CI 补验）
- [x] 1.5 实现 socket 路径推导：unix `/run/appkeeper/<uid>/ak.sock`、windows `\\.\pipe\<用户名>.sock`，均可被显式配置覆盖；单测覆盖两种平台推导与覆盖路径，以及 unix 路径长度超限时返回可识别错误（specs 增量「非法 socket 值被拒绝」与 design 的长度风险）

## 2. 服务端切换

- [x] 2.1 `src/server.rs` 的 `bind` 返回类型由 `TcpListener` 改为传输层 listener，`serve` / `handle_connection` 的流类型改为 `Box<dyn Conn>`；删除 `serve` 中的 `token` 读取与 `handle_connection` 的 `token` 参数，以及鉴权校验分支（`server.rs:777` 附近）；单测断言经 socket 的 `/v1/status` 返回 200 且响应中无鉴权相关字段
- [x] 2.2 `handle_connection` 的读写两半改由 `Conn` 的读写接口拆分，不再依赖 `TcpStream::try_clone`；既有全部端点用例（含 `POST /v1/run` 的 chunked NDJSON、`log --follow` 的 chunked 流）改绑临时 socket 路径后仍通过，单测断言帧序与内容不变
- [x] 2.3 删除 `bind` 中对 `daemon.host` / `daemon.port` 的读取；单测断言启动后系统上不存在控制面 TCP 监听端口
- [x] 2.4 `src/main.rs` 的 daemon 启动路径改用传输层 listener，启动日志改为打印 socket 路径；单测断言日志含端点路径

## 3. 客户端切换

- [x] 3.1 `src/client.rs` 以自建传输层替换 `ureq`：`from_config` / `probe` 从配置解析端点路径并构造 `Transport`，`call` / `call_with_body` 手写 HTTP/1.1 请求行 + 头（含占位 `Host` 头）+ JSON 体，响应解析区分「传输失败（不可达）」与「HTTP 错误状态码（业务答案）」；单测以假服务端覆盖成功、4xx/5xx、连接被拒三条路径
- [x] 3.2 `follow_lines`（`client.rs:224`）与 `adhoc_run`（`client.rs:289`）的 chunked 解码改为基于新连接实现，帧重组与终帧提取行为不变；单测以原始流读帧断言帧序与内容
- [x] 3.3 连接失败分类：端点不存在→退出码 3 且提示「守护进程不可达」；权限错误→退出码 3 且提示端点属主与当前用户不一致、需更高权限；单测覆盖两类错误的不同文案（specs 增量「权限不足的专属提示」）
- [x] 3.4 `Client::probe` 的短超时语义保留（500ms/800ms），用于 `daemonize.rs` 就绪轮询；单测断言悬空监听不会耗尽就绪预算
- [x] 3.5 模块注释标注「`ureq` 仅用于 `health.rs` 探测用户程序 URL，不得接回控制面」（design Risks 的可维护性项）

## 4. 配置层

- [x] 4.1 `DaemonSettings` 删除 `host` / `port` / `auth_token` 字段，新增 `socket: PathBuf`（unix 平台可为 `PathBuf`，Windows 下管道名以字符串承载）；单测断言缺省值按 uid / 用户名推导，显式配置覆盖
- [x] 4.2 校验层：`[daemon]` 出现 `host` / `port` / `auth_token` 时报错并指明「已移除，改用 socket」；空 `socket` 值被拒；单测覆盖三者各自报错与空值拒绝
- [x] 4.3 `config` 子命令键表（`DAEMON_KEYS` 及 `--set` / `--get` / `--delete` 的白名单）增删同步：`+socket`，`-host -port -auth_token`；已移除键的 set / get / delete 均非零退出并指明替代；单测逐键覆盖
- [x] 4.4 `[webui]` 段新增 `auth_token` 键（非空字符串校验），并接入既有的 webui 键表机制；单测覆盖写入、读取、删除与空值拒绝
- [x] 4.5 模板与脚手架渲染同步：`conf/daemon.toml`、`examples/daemon.toml`、`config --init` 模板移除三个死键、加入 `socket` 注释项；`ak config --init` 产出的配置 SHALL 通过整体校验

## 5. 连带调用方

- [x] 5.1 `src/daemonize.rs` 的就绪探测改走传输层；其测试中「空闲端口上的假健康应答器」（`daemonize.rs:178`）改为绑定临时 socket；单测覆盖就绪成功与预算耗尽两条路径
- [x] 5.2 `src/shell.rs` 的客户端改走传输层；单测断言离线时退出码仍为 3
- [x] 5.3 `appkeeper-bench`：`--connect <addr>` 与 `--token` 移除、新增 `--socket <path>`；`api.rs` 与 `topology.rs` 改走传输层；隔离环境在临时目录内分配短前缀随机 socket 路径并写入生成的 daemon 配置；单测覆盖新选项生效与旧选项被拒（验证：`cargo test -p appkeeper-bench`；cli.rs `socket_option_replaces_connect` 覆盖新选项生效与旧选项被拒；topology.rs `isolated_socket` 写入生成的 `[daemon] socket`；bench 依赖中的 ureq 已随控制面切换移除，根包 ureq 保留给 health.rs）
- [x] 5.4 `appkeeper-bench` 隔离环境新增断言：启动后校验该 socket 权限位为 `0600`、所在目录为 `0700`，不符以非零码退出并报告实际权限（验证：topology.rs `assert_endpoint_isolated` 在 daemon 拉起后调用（`topology.rs:318`），不符 `bail!` 非零退出并打印实际权限位；`cargo test -p appkeeper-bench` 过）

## 6. Web 控制台鉴权

- [x] 6.1 `src/web.rs` / `src/api.rs`：未配置 `webui.auth_token` 时行为完全不变；单测断言匿名 `/api/overview` 仍返回 200
- [x] 6.2 配置 token 时：`/api/*`（除 `/api/health`）与 `/ws` 均要求凭据，缺失或不匹配返回 401 / 拒绝连接；单测覆盖两类端点
- [x] 6.3 一次性换票端点：提交 token 换取会话 cookie（`HttpOnly`、`SameSite`），后续凭 cookie 放行；URL 查询参数中的 token MUST NOT 被接受为有效凭据；单测覆盖三条路径
- [x] 6.4 文档与 `ak validate` 提示明确写出「开启 `[webui]` 且未配 token 时，任何能连到 `webui.listen` 的本地用户均可执行 stop / apply」（验证：`webui_open_console_warning` 纯函数 + `validate_warning_tests` 3 项单测（无 token 警告含 listen 地址、有 token/无 webui 静默）；README「Web 控制台鉴权（可选）」小节；`cargo test --bin ak validate_warning_tests` 过）

### webui 前端配合

- [x] 6.5 `webui/src/lib/` 新增统一的 API 请求封装，取代 `store.ts`（`/api/apply`、`/api/overview`）与 `views/program-detail.ts`（程序动作）三处裸 `fetch`：封装 SHALL 在收到 401 时触发一次凭据收集与换票流程，换票成功后自动重放原请求，失败则停留在凭据输入态；单测覆盖「无 token 直接成功 / 401 后换票成功并重放 / 换票失败不重放」（验证：`credentials.test.ts` 8 项 + `store.test.ts` / `console.test.ts` 断言更新为 `credentials: 'same-origin'`；`pnpm exec vitest run` 71 项全过）
- [x] 6.6 凭据输入界面：收到 401 时呈现 token 输入入口，token SHALL NOT 写入 URL、localStorage 或 console 日志（会话凭据由 `HttpOnly` cookie 承载）；单测断言 token 不出现在任何持久化存储中（验证：`credential-gate.ts`（type=password、局部变量、不落盘）+ `credentials.test.ts` `token hygiene (6.6)` 断言 URL/storage/cookie/console 五类 spy 均无 token）
- [x] 6.7 `lib/ws-client.ts` 的 WebSocket 连接 SHALL 携带会话 cookie（同源自动携带），连接被拒时 SHALL 进入与 HTTP 401 一致的凭据输入态；单测覆盖连接被拒后的恢复流程（验证：`store-auth.test.ts` 3 项——拒绝+REST 401 抬门、换票后 /ws 重连并转 live；拒绝+REST 非 401 走 offline / poll 不抬门；URL 恒无凭据）
- [x] 6.8 未配置 `webui.auth_token` 时前端行为 SHALL 与本变更之前完全一致（不出现任何凭据输入界面）；单测断言该路径无回归（验证：`credentials.test.ts` `a response that is not a 401 passes straight through` 断言无状态变化、无换票请求；`store-auth.test.ts` REST 200 场景不抬门）
- [x] 6.9 前端门禁：`pnpm exec tsc --noEmit && pnpm test && pnpm build` 全绿后需重新 `cargo build` 以嵌入新前端产物（验证：tsc 0 错误；vitest 16 文件 71 项全过；vite build 产出 dist/；随后仓库根 `cargo build` 成功、`cargo test` 300 项全过）

## 7. systemd 与部署

- [x] 7.1 `src/service.rs` 的 `render_unit` 增加 `RuntimeDirectory=appkeeper`，且不引入阻断 socket 创建的沙箱指令；单测断言生成的 unit 含该键（验证：`cargo test --bin ak service::` 13 项全过；`unit_declares_runtime_directory_and_no_socket_blocking_sandbox` 同时断言 `RuntimeDirectoryMode=0755` 与 ProtectSystem/ReadOnlyPaths 等阻断项缺席）
- [x] 7.2 缺 `RuntimeDirectory` 的既有 unit 路径可用：守护进程自建 `/run/appkeeper`（`0755`）与 uid 子目录（`0700`）；单测覆盖父目录缺失的启动路径（验证：transport.rs `missing_root_directory_is_created_by_the_daemon` 断言整条目录链由 bind 自建且权限位正确）
- [x] 7.3 `User=<name>` 非 root unit 场景下 socket 创建于 `/run/appkeeper/<uid>/ak.sock`；单测以显式 uid 断言推导结果（验证：transport.rs `endpoint_follows_the_running_account_uid` + `default_endpoint_is_uid_layered` 按 `geteuid` 断言推导；`cargo test --bin ak transport::` 过）

## 8. 跨用户隔离与陈旧端点（e2e 重点）

- [x] 8.1 同机多用户并存：root 与普通用户各起一个 daemon，端点互不冲突，任一方客户端无法连接另一方端点（验证：集成测试 `tests/revamp_integration.rs::two_accounts_run_two_daemons_without_conflict` —— root 与 `nobody`（setpriv/runuser 降权）各起 daemon，端点互不冲突、无非属主状态泄露、非属主→属主方向 connect 被内核拒绝（exit 3 + access denied，无程序名泄露）；属主→属主正常；root→非属主方向不断言（root 持 CAP_DAC_OVERRIDE，内核必放行，此为 unix 事实而非 appkeeper 承诺，已写入测试注释）。该测试条件执行：需 root + 第二账户，缺一则打印原因跳过（本机 uid 1000 已验证 skip 路径，真实验证留待 root 环境/CI 跑 `cargo test --test revamp_integration -- --ignored`）；非 root 环境的兜底断言 `endpoint_is_owner_only_and_uid_derived`（socket 0600、属主为运行账户、缺省端点按 uid 分层推导）已在本机跑绿）
- [x] 8.2 非属主用户执行 `ak status` 得到权限错误提示与退出码 3，MUST NOT 表述为「守护进程未运行」，且无任何 API 响应泄露程序状态（验证：集成测试 `non_owner_status_gets_the_permission_error`（条件执行，同 8.1 的 skip 语义）断言 exit 3、消息含 "access denied" 与端点属主 uid、不含任何程序名；client.rs `permission_error_is_distinguished_from_unreachable` 单测断言权限文案与 unreachable 文案分离、退出码仍为 3。上报的措辞风险（消息尾括注暗示「可能只是没在跑」）已由主 agent 收紧：括注删除，权限文案只保留属主与提权提示）
- [x] 8.3 陈旧端点：模拟崩溃残留（socket 文件存在但无监听）后启动 daemon，验证被清理重建且既有程序不受影响；目标路径为非端点文件时启动失败且文件未被删除（验证：集成测试 `stale_endpoint_is_cleared_and_the_daemon_serves`（UnixListener 绑定后即弃，重建后 /v1/health 200）与 `non_endpoint_file_blocks_startup_and_survives`（普通文件占位 → daemon 快速非零退出、报 occupied、文件字节不变），本机跑绿；三分支单测见 transport.rs；second-instance 拒绝另由 `concurrent_pull_up_yields_one_daemon`（二次前台实例报 already running）与 xtask e2e R9 覆盖）
- [x] 8.4 `curl --unix-socket <path> http://localhost/v1/status` 可直接查询状态（验证 HTTP 语义保留与运维可调试性）（验证：集成测试 `curl_unix_socket_queries_the_control_plane`（curl 缺失时打印原因跳过；本机 curl 8.22 跑绿，断言 200 + 投影 `daemon.socket` 即所拨端点 + /v1/health）；xtask e2e 全部控制面访问已改为经 `curl --unix-socket`，等效于全通道持续回归该契约）
- [x] 8.5 Windows 侧：在 `windows-latest` 上验证运行账户可连接管道、与运行账户不同的账户连接失败（断言完成：DACL 主体集合的纯函数断言 `transport::tests::control_pipe_dacl_subjects_are_exactly_sy_ba_and_running_account`（恰为 SY+BA+运行账户 SID，显式排除 WD/BU/AU）全平台编译可测；ConvertStringSecurityDescriptorToSecurityDescriptorW 的实际转换、运行账户连接成功与他账户连接失败的运行时行为无法在 linux 验证，待 windows-latest CI 补验）

## 9. 文档

- [x] 9.1 README 新增安装与配置说明：socket 默认路径、`[daemon] socket` 覆盖、非 root 与多用户部署；移除 host/port/auth_token 相关描述（验证：README「配置示例」「控制平面」「非 root 与多用户部署」小节；架构图与快速上手同步；全文已无 7310/Bearer 残留）
- [x] 9.2 README 升级说明：既有配置若含 `host` / `port` / `auth_token` 将导致校验失败，列出需删除的键并给出 `ak config --delete` 命令（验证：README「升级注意（unix-socket-control-plane…）」小节，含迁移命令与回滚提示）
- [x] 9.3 README 排障小节：`curl --unix-socket` 用法；Windows 侧给出 PowerShell `NamedPipeClientStream` 最小示例（无 `curl --unix-socket` 等价物）（验证：README「控制平面 → 运维排障」小节）
- [x] 9.4 README 新增 Web 控制台鉴权小节：默认无鉴权的隔离含义、`webui.auth_token` 配置与换票流程（验证：README「Web 控制台鉴权（可选）」+ Web UI「API 概览」补 `POST /api/auth` 行与 401 语义）
- [x] 9.5 `AGENTS.md` 架构行与子命令清单同步（控制面走 socket、新增 `[daemon] socket` 与 `[webui] auth_token`、控制面无 TCP 端口）（验证：架构总述 + `src/transport.rs` 条目 + validate/config/daemon/status/bench 行）
- [x] 9.6 `appkeeper-bench` 的 README 节更新命令行示例（`--socket` 取代 `--connect`）（验证：README「性能基准」示例与 `--socket` 注意项；bench 无独立 README，文档在主 README）
