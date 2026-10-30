# appkeeper

跨平台、应用层的轻量级进程守护工具。Rust 单二进制 + TOML 分层配置：
崩溃自动拉起、启动顺序与依赖、健康检查、日志轮转与跟随、本地控制面
（HTTP API + CLI）、可选内嵌 Web 控制台。

## 快速上手

```bash
ak daemon start                                       # 1. 启动守护进程（后台，就绪即返回）
ak run --cmd "python -m http.server 8000" --app web    # 2. 注册并立即启动一条命令行
ak ps                                                 # 3. 查看状态
ak stop web                                           # 4. 停止
ak start web                                          # 5. 再启动
```

守护进程不依赖任何配置文件即可运行（内置默认值空启动）。生产部署建议
`ak config --init` 生成带注释的模板（内含 `restart = "always"`，开箱自动守护）。

## 命令速查

**守护进程（`ak daemon …`）**

| 命令 | 意图 |
| --- | --- |
| `ak daemon start` | 后台启动守护进程，就绪即返回 |
| `ak daemon start --foreground` | 前台运行（systemd / 调试用） |
| `ak daemon install [--start]` | 注册为系统服务（systemd unit / Windows 服务，需 root/管理员） |
| `ak daemon uninstall` | 注销系统服务 |
| `ak daemon shutdown` | 停止全部程序并退出守护进程 |

**启停（`ak start` / `ak stop` / `ak restart`，双位置参数 `<app> [<program>]`）**

| 命令 | 意图 |
| --- | --- |
| `ak start <app>` | 按依赖顺序启动整个 app 的全部程序（阻塞至启动判定完成） |
| `ak start <app> <program>` | 只启动该 app 内的单个程序 |
| `ak stop <app>` / `ak stop <app> <program>` | 停止整个 app（逆启动序）/ 单个程序 |
| `ak restart <app> [<program>]` | 重启整个 app / 单个程序 |
| `ak start <app> --json` | 机器可读输出：`name / config_path / pid / status`（扇出时为数组） |

**跑临时命令（`ak run`，仅 ad-hoc；守护离线一律退出码 3，不隐式拉起）**

| 命令 | 意图 |
| --- | --- |
| `ak run --cmd "python -m http.server 8000" --app web` | 注册一条命令行并立即启动（成功才落盘） |
| `ak run -- python -m http.server 8000 --app web` | 等价的 `--` 写法（脚手架旗标放在命令之后） |
| `ak run … --json` | 机器可读输出：`name / config_path / pid / status` |

`ak run` 只承载 ad-hoc 注册；启动/停止已注册的 app 或程序请用
`ak start|stop|restart <app> [<program>]`。ad-hoc 形态可附加
`--restart <no\|on-failure\|always\|unless-stopped>`、`--env K=V`、`--workdir`、
`--description`。

**日常控制**

| 命令 | 意图 |
| --- | --- |
| `ak ps` | 程序状态表（NAME/APP/STATE/PID/…/PENDING，守护离线退出 3） |
| `ak ps --all` | 含已注册未运行的 app；离线时降级为本地注册表清单（退出 0） |
| `ak ps --out json` | JSON 投影（等价 `GET /v1/status`，脚本消费） |
| `ak log <名…> [-f] [--tail N] [--stream out\|err\|both]` | 查看/跟随日志（多名时行首带 `[名]`） |
| `ak pid <名>` | 打印程序 pid |
| `ak signal <程序> USR1` | 投递白名单信号（unix） |
| `ak action <程序> <动作>` | 执行配置声明的自定义动作（CLI 透传动作退出码） |

`ps` 表格的 PENDING 列标记磁盘配置与运行定义存在待应用差异的程序（`changed` /
`new`），表格尾部追加 pending 摘要行（新增/移除的 app、daemon 级提示、检出错误）；
`ak apply` 后标记消失。`pid`/`log`/`action`/`signal` 以程序名寻址——单程序 app
的程序名与 app 名相同。

**注册与配置流转**

| 命令 | 意图 |
| --- | --- |
| `ak compose <目录\|.toml\|可执行>` | 注册 app：目录/配置→`app_dir` 链接；可执行→脚手架生成配置 |
| `ak compose … --apply` | 注册后立即生效（守护离线时自动后台拉起） |
| `ak remove <app>` | 注销：守护进程一次完成，立即清除注册与内存投影，**无需 apply**；要求该 app 无 `running/starting/stopping/backoff` 程序，否则 409 列出阻塞项并提示先 `ak stop <app>`（守护离线退出 3，不做本地直删） |
| `ak config --reload` | 立即重扫配置，检出待应用变更（pending），**不触碰任何进程** |
| `ak apply [<app> [<程序>]] [--restart]` | 应用待应用变更（见下「两阶段」） |
| `ak validate [路径]` | 校验 daemon 配置 + 全部注册应用（或单个文件） |
| `ak config --init \| --set k=v \| --get k \| --delete k \| --edit \| --reload` | daemon 配置管理（本地动作 + 可选在线重载） |

**其他**：`ak shell [-e "ps"]` 交互式控制台（supervisorctl 风格，内置
`ps/start/stop/restart/log/pid/pending/apply/action/signal/shutdown/open` 词；
`start|stop|restart` 同样是 `<app> [<program>]` 双位置参数）。

CLI 退出码：`0` 成功、`1` 一般错误、`2` 配置/用法错误、`3` 守护进程不可达。

## 三个核心概念

- **两层模型**：daemon 全局唯一，持有注册目录 `app_dir`；**app** 是部署单元
  （部署目录里的 `appkeeper.toml`）；**program** 是被守护的子进程，名字跨全部
  应用全局唯一——任何面板直接用程序名寻址。
- **配置变更两阶段**：磁盘配置被修改（手改 / compose）→ 守护进程
  周期重扫，差异只记为 **pending**（绝不碰进程）→ `ak apply` 显式生效。
  `ak config --reload` 只是「立即重扫一次」。
- **移除是命令式的一步**：`ak remove <app>` 由守护进程原子执行——校验活态、
  删除注册记录、清理内存状态，当场收敛，不进入 pending、不需要 apply、
  也不停止任何程序（存在活态程序时直接拒绝）。手工删除注册文件或 app 内的
  程序定义则走两阶段，且 apply 会整批拒绝仍在运行/启动/停止中/退避的删除目标：
  先 `ak stop`，再 `ak apply`。
- **删除边界**：移除只动注册记录——脚手架生成在 `app_dir` 内的配置随记录删除；
  外部部署目录里的 `appkeeper.toml` 本体保留；日志文件一律不动。
- **restart 策略**（docker 字面量）：`no` / `on-failure` / `always` /
  `unless-stopped`。优先级 `[program.*]` > `[app]` > daemon `[app-default]` >
  内置默认 `no`。

## 安装

发布产物在 [GitHub Releases](https://github.com/cupen/appkeeper/releases)，
按平台三选一（包内含 `ak`、`LICENSE`、`README.md`；musl 静态链接适合 alpine）：

| 平台 | 归档 |
| --- | --- |
| Linux musl（静态） | `ak-<version>-x86_64-unknown-linux-musl.tar.gz` |
| Linux glibc | `ak-<version>-x86_64-unknown-linux-gnu.tar.gz` |
| Windows | `ak-<version>-x86_64-pc-windows-msvc.zip` |

```bash
version=0.1.0; target=x86_64-unknown-linux-musl
curl -LO "https://github.com/cupen/appkeeper/releases/download/v${version}/ak-${version}-${target}.tar.gz"
curl -LO "https://github.com/cupen/appkeeper/releases/download/v${version}/SHA256SUMS"
sha256sum -c SHA256SUMS --ignore-missing && tar -xzf "ak-${version}-${target}.tar.gz"
```

源码构建：`cargo build --release`（产物 `target/release/ak`）。

## 配置

完整模板见 `conf/daemon.toml`、`conf/app.toml`，可运行示例见 `examples/`。

daemon 配置（缺省 Linux `/etc/appkeeper/daemon.toml`，Windows
`%APPDATA%\appkeeper\daemon.toml`，`-c` 可覆盖）：

```toml
[daemon]
log_dir = "/var/log/appkeeper"      # 缺省 /tmp/appkeeper/logs；必须绝对路径
app_dir = "app.d"                   # 注册目录（相对 daemon 配置所在目录）
# socket = "tcp://127.0.0.1:7310"   # 缺省本地 IPC：/run/appkeeper/<uid>/ak.sock
# auth_token = "<随机串>"           # 仅 tcp:// 形态必配，路径形态拒绝

[app-default]
restart = "always"                  # 全体应用的默认策略
```

app 配置（部署目录 `appkeeper.toml`，`ak compose <目录>` 注册）：

```toml
[app]
description = "演示应用"

[program.api]                       # 键即程序名；绝大多数字段可省略
command = "python -m http.server 8000"       # 词法拆分直 spawn，不经 shell
restart = "on-failure"
depends_on = ["db"]                 # 依赖编排；可跨应用（程序名全局唯一）
health_check = "http://127.0.0.1:8000/health"  # http(s):// | tcp://host:port | exec 命令
```

### 自定义动作

在程序表下声明，经平台 shell 执行（与程序 `command` 的不经 shell 有意分叉），
支持 `${program.<名>.pid}` 等变量替换、`timeout`、同名互斥：

```toml
[program.api.action.flush]
command = "curl -fsS http://127.0.0.1:8000/flush?pid=${program.api.pid}"
```

### Web 控制台（可选）

daemon 配置存在 `[webui]` 段即开启（缺省关闭），浏览器访问
`http://127.0.0.1:9877`：

```bash
ak config --set webui.listen=127.0.0.1:9877          # 写入即配置
ak config --reload                                    # 守护在线时热开启
ak config --set webui.auth_token="$(openssl rand -hex 24)" && ak config --reload   # 可选鉴权
ak config --delete webui                              # 删段 = 关闭
```

未配 token 时 `/api/*` 完全开放（socket 属主隔离不覆盖控制台），`ak validate`
会对该组合告警。

控制台的 App 概况页提供「移除应用」操作（需确认）：与 `ak remove` 走同一个
`POST /api/apps/<app>/remove`，活态冲突时页面保留并展示阻塞原因，成功后提示
无需 apply，侧栏与当前视图随 `/ws` 推送自动收敛。

## 控制面与远程管理

控制面是控制端点上的标准 HTTP/1.1 JSON API（`/v1/*`），端点两种形态同一时刻
只取其一：缺省**本地 IPC**（unix socket `0600` + uid 目录 `0700`，Windows
命名管道 DACL——身份由 OS 属主给出，无应用层凭据）；或显式
`socket = "tcp://host:port"`（除 `/v1/health` 外全部要求
`Authorization: Bearer <auth_token>`，仅建议用于受信网络 / SSH 隧道）。

远程机器操作只需一份指向 `tcp://` 的配置：`ak -c <配置> ps`。排障可直接
`curl --unix-socket /run/appkeeper/$(id -u)/ak.sock http://localhost/v1/status`。

移除应用由 `POST /v1/apps/<app>/remove`（控制台用同构的
`POST /api/apps/<app>/remove`）承载：`200` 返回
`{app, config_path, deleted_file, kept_config, programs, applied, result}`；
未知 app 为 `404`，存在活态程序为 `409`（消息列出 `program (state)` 与
`ak stop <app>` 提示），注册记录删除失败为 `500`。手工删除注册文件后
`POST /v1/apply` 在范围内存在活态删除目标时同样返回 `409`，且整批不执行。

## 升级速览（历史 BREAKING 摘要）

- 旧动词已删（无别名）：`service install/uninstall` → `ak daemon install/uninstall`；
  `status`/`list` → `ak ps` / `ak ps --all`（`status --json` → `ps --out json`）；
  `shutdown` → `ak daemon shutdown`；`add` → `ak compose`。
- 启动面收敛到 `start|stop|restart <app> [<program>]`：`ak run <name>` →
  `ak start <app>`；`ak run --app <app>` → `ak start <app>`；旧
  `ak stop|restart --app <app>` → `ak stop|restart <app>`（`--app` 旗标已删）。
  `ak run` 仅保留 ad-hoc 形态，应用命名 `--name` → `--app`（如
  `ak run -- python -m http.server 8000 --app web`）。
- 顶层 `reload` 已删：`ak reload` → `ak config --reload`（可与 `--set` /
  `--delete` / `--edit` 组合，`--get`/`--init` 组合为用法错误）。
- 已装机 systemd unit 必须重新生成：`sudo ak daemon install --force --now`。
- `[daemon]` 死键 `host`/`port` 被校验拒绝（端点由 `socket` 表达）；
  `auth_token` 仅 `tcp://` 形态合法。
- `app_dir` 默认名 `apps` → `app.d`；`startsecs` 默认 1.0s → 3.0s（可显式声明覆盖）。
- `run` ad-hoc 全形态 daemon-first：离线退出 3；先启动成功再落盘（失败不留残留）。
- v0.1 单文件配置：`ak compose <旧文件>` 自动识别转换。
- 标识符（app/程序/动作名）仅允许 `[A-Za-z0-9_-]`。
- `ak remove <app>` 不再是本地直删 + 事后 sync：改为守护进程执行的原子动作
  （活态程序拒绝、成功即收敛、不产生 pending）；守护离线退出 3，不做本地删除。
  旧流程「`ak remove` 后 `ak apply`」不再需要。

## 开发

```bash
cargo test                                    # 单测（状态机用真实子进程）
cargo run -p xtask -- e2e --no-browser        # 跨进程验收（真实 daemon + CLI + HTTP）
scripts/docker-test.sh                        # Docker 一次性全量验收（含 webui 三连）
appkeeper-bench --case firehose --duration 10 # 性能基准（firehose/rotation/fanout/drip）
```

Web 控制台（`webui/`，pnpm + Vite + TypeScript + Lit）：`pnpm dev` 起热更新
（:5273，代理到后端 :9877）；前端改动后必须
`pnpm exec tsc --noEmit && pnpm test && pnpm build` 再 `cargo build`。
Ansible 部署见 [.ansible/README.md](.ansible/README.md)。

平台差异要点：Windows 优雅停止等价 TerminateProcess（Job Object 兜底清树）；
`signal` 仅 unix；Windows 无符号链接权限时注册降级为硬链接（`sed -i` 式编辑会
断链，重跑 `ak compose .` 刷新）；`/etc/appkeeper` 需 root 写权限（非 root 用
`-c` 指向用户级配置）。

权威规范按能力存放在 [openspec/specs/](openspec/specs/)（术语表 glossary、
配置 configuration、控制面 control-plane、配置应用 apply-workflow、注册表
app-registry、进程管理 process-management、动作 actions、Web 控制台
webui-api / webui-ui 等），进行中的变更提案见 [openspec/changes/](openspec/changes/)。
