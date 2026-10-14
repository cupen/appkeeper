## Context

见 proposal.md 的 Why。此处只记录塑造实现的现状事实：

- 控制面服务端是**手写 HTTP/1.1、同步 std、每连接一线程**：`serve`（`src/server.rs:227`）对每个 accept 起的连接 `std::thread::spawn`，`handle_connection` 靠 `stream.try_clone()` 拆读写两半，响应固定带 `Connection: close`。这意味着把 `TcpStream` 换成 `UnixStream`（同样有 `try_clone`）或管道 handle 时，HTTP 解析层**一行都不用改**。
- 客户端是 `ureq` 2.10，**只支持 TCP**，且无自定义传输的口子。它的 chunked 解码本来就是手搓的：`follow_lines`（`src/client.rs:224`）与 `adhoc_run`（`src/client.rs:289`）。
- `ureq` 的 5 个使用点里**只有 `client.rs` 是控制面**：`health.rs` 探测的是用户自己的 URL/TPC/命令（`HealthKind::Http{url}` / `Tcp{addr}` / `Exec`），从不打 `/v1`。因此 `ureq` 必须保留在依赖中，不得因本变更被移除。
- `daemonize.rs:55` 用 `Client::probe` 轮询 `/v1/health` 做后台拉起就绪判定——这是控制面流量，必须跟随切换。其测试 `daemonize.rs:178` 用「空闲端口上的假健康应答器」，需改为绑定临时 socket。
- `src/web.rs` 的 `/api/*` **零鉴权**（`auth`/`token`/`Bearer` 全无匹配），且经 `Arc<Supervisor>` 进程内直连，不经 `/v1`。浏览器无法访问 unix socket，因此 webui 不可能跟着换 socket。
- systemd unit 由 `render_unit`（`src/service.rs:125`）渲染，**当前没有 `RuntimeDirectory=`**，但**已支持 `User={u}`**——非 root 部署是既有的一等场景。
- Windows 侧 `windows-sys 0.61` 已启用 `Win32_Foundation` / `Win32_Security` / `Win32_Storage_FileSystem`；但 **Rust std 在 Windows 上没有 UDS**。

## Goals / Non-Goals

**Goals:**

- 「谁能控制守护进程」在系统中**只有一个答案来源**，且该答案由操作系统强制给出，而非用户是否记得配了一个默认关闭的 token。
- 传输层细节（connect / bind / 权限位 / ACL / 陈旧端点清理）收敛到单一模块，其余代码不得出现 socket 路径或平台 IPC 类型。
- 保留 HTTP/1.1 语义，使 `/v1/*` 端点契约与既有测试 harness 基本不变，并让运维能用 `curl` 排障。
- unix 与 Windows 的权限语义等价。

**Non-Goals:**

- 不保留任何 TCP 控制面端点，也不提供远程控制。远程能力本就是「默认无鉴权的回环 TCP」，`run-daemon-first` 已把 `run` 定为纯客户端动词。
- 不改 `/v1/*` 端点集合、请求/响应结构、错误码。
- 不改健康探测（`health.rs`）的语义与实现。
- 不改 webui 前端除鉴权外的任何行为，也不改它的 axum/TcpListener 本身。

## Decisions

### D1 身份由操作系统强制给出，token 删除

socket 文件权限 `0600`、其所在 uid 目录 `0700`，内核在 `connect()` 阶段拒绝非属主。`[daemon] auth_token` 随之删除。

- 否决「socket + token 叠加」：token 唯一的工作是决定谁能控制 daemon，而这正是文件系统做得更好且**配不错**的事；保留它等于保留一个能被忘记的安全开关。
- 否决「只把 TCP 换成 UDS、token 照旧」：同上。

### D2 单一 socket，控制面 TCP 监听彻底消失

`[daemon] host` / `port` 删除。守护进程 SHALL NOT 再监听任何控制面端口。

- 否决「双监听（socket + TCP 保留给远程）」：两套传输、两套鉴权、两套错误路径，换来的远程访问还要用户自己配 token 才安全。
- 否决「写操作走 socket、只读走 TCP」：读写分叉使规范面翻倍，且只读面仍会泄露完整状态与日志。

### D3 Windows 走命名管道 + DACL

`\\.\pipe\ak-<用户名>.sock`，以 `ConvertStringSecurityDescriptorToSecurityDescriptor` 构造 DACL，仅允许运行账户 SID、SYSTEM、Administrators，**显式移除 Everyone / Users / Authenticated Users**（命名管道默认 ACL 对 Everyone 可读写，不显式收紧等于本变更在 Windows 上完全失效）。

- **否决 AF_UNIX**：Windows 的 AF_UNIX 只是把 socket 映射为文件路径，没有 unix 权限位，`chmod 0600` 的语义不存在——D1 的核心机制整体失效，且依赖 `uds_windows` 一类生态薄弱的 crate。
- 否决「回落 TCP + 保留 token」：等于把 D1 要消灭的洞在 Windows 上原样开回来。
- 否决「Windows 列为平台不支持」：`signal` / `service` 有此先例，但它意味着 Windows 上的 `ak run` / `stop` / `restart` 全部失效，是对已发布 Windows 产物的能力回退。
- 取舍：DACL 构造是本变更在 Windows 侧的主要工作量，也是主要风险点（见 Risks）。

### D4 手写传输层，保留 HTTP/1.1 语义

```
trait Conn: Read + Write {}
trait Transport { fn connect(&self) -> io::Result<Box<dyn Conn>>; }
    unix     -> UnixStream
    windows  -> named-pipe HANDLE
```

- 否决「换 hyper / hyperlocal / reqwest」：服务端是同步 std，客户端若上 tokio 会把 `main.rs` 的整条同步调用链传染成 async——为一次 `connect()` 抽象付出整个 async 的代价不划算。
- 否决「复用 ureq」：2.x 的 `Request` 只能打 TCP，无自定义传输接口。

保留 HTTP 的三个实际收益：端点契约与 spec 不变；`curl --unix-socket <path> http://localhost/v1/status` 可直接排障；现有裸 TCP 字符串测试 harness 只需换连接方式。

### D5 socket 路径按 uid 分层

默认 `/run/appkeeper/<uid>/ak.sock`（unix），父目录 `0755`、uid 子目录 `0700`；另加可选 `[daemon] socket` 键覆盖。Windows 默认 `\\.\pipe\ak-<用户名>.sock`，用户名进入管道名以避免多用户时后启动者撞 `EADDRINUSE` 或接到别人的管道。

- 否决 `$XDG_RUNTIME_DIR`（`/run/user/<uid>/`）：无登录 session 的场景（纯 systemd 服务、容器、CI）该变量为空，回落到 uid 分层方案——于是 uid 分层成了降级分支而非替代品，概念数量反而增加。
- 否决 `/tmp/appkeeper-<uid>/`：把特权 IPC 端点放进全局可写目录是最不该选的一条；且每次启动都要额外防御「目录被换成别人的」这一经典攻击面（校验属主、不匹配即报错退出），比 uid 分层多一层对抗却少一分语义清晰。
- **uid 分层是唯一让「同一台机器上 root 与普通用户各跑一个 daemon」天然正确**的方案：谁也进不了谁的目录，无需额外检查。

### D6 陈旧端点：先试连再决定

bind 撞 `EADDRINUSE` 时先 `connect()` 一次：连得上 → 报「已有 ak 在运行」并以既有的启动冲突退出码退出（见 `control-plane` 的「启动冲突处理」）；连不上（`ECONNREFUSED`）→ 认定为上次崩溃留下的陈旧端点，删除后重建。删除前 SHALL 再次确认路径确实是个 socket 而非任意文件。

### D7 废弃键删除而非忽略

`host` / `port` / `auth_token` 从 `[daemon]` 删除，daemon 配置中出现时 `validate` SHALL 报错。

- 否决「保留但忽略」：配置里躺着永不生效的死键，比启动时明确报错更迷惑。

### D8 webui 保留 TCP，新增独立 `[webui] auth_token`

默认关闭以不破坏既有部署；配置后 `/api/*` 要求凭据，浏览器侧做一次性换票（请求得 401 → 前端收集 token → 提交换取会话 cookie）。用 cookie 而非 URL query token，避免 token 落进浏览器历史与访问日志。

这与 D1「删 token」不矛盾：控制面的 token 因为有 socket 替代品而该删；webui 的 token 因为浏览器是唯一入口而必须留。两者是不同面、不同理由。现状「开 webui 即等于放弃隔离」这一事实同时写入规范，不再沉默。

- 否决「不动 webui」：目标在开了 webui 的机器上直接落空，且该事实今日无人记录。
- 否决「只绑回环 + CSRF token」：挡得住恶意网页，挡不住本机 `curl`——而本机 `curl` 正是要防的东西。

### D9 封装边界：UDS 细节只允许出现在最底下两层

```
  main.rs / shell.rs / appkeeper-bench      只看见 Client
    status() / run() / log_follow() / apply()       同步方法，退出码 0/1/2/3
        |
  src/client.rs                             API 层：HTTP 语义 + 退出码契约
  +-------------------------------------------------+
  |  传输层：UDS / 管道细节全部封死在这一层          |
  |  connect / bind / chmod 0600 / ACL / 陈旧清理    |
  +-------------------------------------------------+
        |
  UnixStream          |        named-pipe HANDLE
```

反向同理：`supervisor.rs` 只下发命令、看不见监听器；`server.rs` 只做 HTTP 解析与路由、看不见 socket 路径。健康探测那条用 ureq 打用户 URL 的路径与控制面路径互不污染。

这条约束决定了改动的爆炸半径：8 个 CLI 调用点（`main.rs` 里 `Client::from_config` 的全部调用）无需各自了解 socket。

## Risks / Trade-offs

- **Windows DACL 构造出错 → 该紧的没紧或该开的没开** → 在 `windows-latest` 上写针对性测试：未授权账户（与运行账户不同的 SID）连接管道 SHALL 失败，运行账户连接 SHALL 成功；DACL 主体清单在 design 与 specs 中各写一份，验收时逐条对照。
- **Windows 缺 `curl --unix-socket` 等价物，排障手段弱一档** → 在 README 给出 PowerShell `NamedPipeClientStream` 的最小调用示例；明确这是调试工具缺口而非协议缺口。
- **升级即坏：已有配置文件含 `host`/`port`/`auth_token` 的实例升级后拒绝启动**（D7 的刻意选择）→ README 升级说明列出需删除的三个键，并给出 `ak config --delete` 的一行命令；考虑在 `validate` 的错误文案里直接给出该命令。
- **`/run/appkeeper` 父目录需要预先创建** → daemon 启动时自建（`0755`）与 uid 子目录（`0700`），父目录不存在时创建；systemd 侧由 `RuntimeDirectory=appkeeper` 保证（见 `service-registration` delta）。
- **socket 路径写死在 bench 与 e2e 的临时目录里可能过长**（unix `sun_path` 约 108 字节上限）→ bench 隔离环境用短临时目录前缀而非完整 `std::env::temp_dir()` 路径，并在 e2e 中加一条「路径超长时报清晰错误」的断言。
- **手写 HTTP 客户端的边界**（缺失 `Content-Length`、异常分块、连接被提前关闭）→ chunked 解码有两处现成先例可循（`client.rs:224` / `289`），且 3c 的集成测试会覆盖流式路径。
- **`ureq` 保留带来的依赖体积不变但语义易混** → 在 `client.rs` 与 `health.rs` 的模块注释里写明「ureq 仅用于用户程序探测」，避免后来者误把控制面流量接回去。

## Migration Plan

1. 传输层模块先行落地（无行为变化，不接线），单测覆盖：路径推导、uid 分层、陈旧端点判定与删除前校验。
2. 服务端与客户端同时切到传输层，删除 `token` 参数与 `host`/`port` 读取；既有 `server.rs` / `daemonize.rs` 测试改绑临时 socket。
3. 配置层删除三个键、新增 `socket` 与 `[webui] auth_token`，同步键表、模板渲染与 `validate` 死键报错。
4. `shell.rs`、`appkeeper-bench`、`daemonize.rs` 就绪探测跟随切换。
5. systemd unit 增加 `RuntimeDirectory`，README 升级说明与排障文档补齐。

回滚策略：本变更是单次提交可回滚的；若升级后装机实例因死键无法启动，回滚到上一版本即可（配置里的死键在旧版本仍是合法键）。
