# appkeeper

跨平台、应用层的轻量级进程守护工具，Rust 单二进制，TOML 分层配置。
Linux 与 Windows 上行为一致：崩溃自动拉起、启动顺序与依赖、健康检查、
日志轮转与跟随、本地控制平面（HTTP API + CLI），内置 Web UI 另立变更。

```text
         ┌────────────────────────── ak run ──────────────────────────┐
         │  supervisor loop（7 态状态机 × N 程序）                          │
         │    spawn/stop/restart · startsecs/startretries · 退避重启        │
         │    priority/depends_on 启动编排 · reload · shutdown              │
         │  ├─ pump×2/程序   输出接管 → 轮转落盘 + 环形缓冲 + follow 广播     │
         │  ├─ health checker tcp/http/exec 探测 → unhealthy/自动重启        │
         │  └─ control API  127.0.0.1:7310 /v1/*（可选 Bearer 鉴权）         │
         └───────────────△─────────────────────────────────────────────────┘
                         │ HTTP JSON
   ak run（daemon | <名> | -- <命令行>）· status/stop/restart/log/pid/reload/apply/shutdown
   ak add/remove/list        ← 离线可用，在线时自动同步
```

## 模型：daemon 全局 + app 分布 + app_dir 注册

- **daemon 根配置**（全局唯一）：Linux `/etc/appkeeper/daemon.toml`，Windows
  `%APPDATA%\appkeeper\daemon.toml`，`-c/--config` 可覆盖；**文件可以不存在**——
  appkeeper 以内置默认值空启动。只放全局内容：`[daemon]`（含 `app_dir` 注册
  目录）、可选 `[app-default]`（所有应用共享的默认值）与可选 `[webui]`
  （内嵌 Web 控制台，缺省关闭）。任何应用专属条目都会被未知字段校验拒绝。
- **app 配置**（每个应用一份）：放在应用自己的部署目录，默认名
  `appkeeper.toml`，由部署人维护。`[program.<name>]` map 接口，键即程序名，
  绝大多数字段可省略。
- **注册即链接**：在部署目录执行 `ak add .`（名字默认取目录名），
  `app_dir` 里出现 `<name>.toml` 链接指向配置本体——这就是注册记录；
  `ak list` 扫描它，`ak remove` 删除它（部署文件永不删除）。
  Windows 无符号链接权限时降级为硬链接（见下方"注意"）。
- **裸程序脚手架**：`ak add ./path/to/proc --name abc` 对可执行程序直接
  生成 `app_dir/abc.toml`（真实文件，注册记录即文件本身）：command 写绝对
  路径，`work_dir` 缺省为执行 add 时的目录，另有 `--args "<字符串>"`/
  `--env K=V`（可重复）/`--workdir <路径>`/`--restart=<字面量>`。重复执行同名
  add 会以本次参数 **全量再生**该文件（文件上的手改会被覆盖），内容有差异时
  提示需先 `ak apply`；`--apply` 在注册后立即对该 app 应用。`ak remove`
  对这类生成记录连带删除文件并打印路径。
- **命令行即注册（run ad-hoc）**：`ak run -- <命令行>` / `run --cmd "<字符串>"`
  复用同一脚手架引擎——首 token 解析为绝对路径写入 `command`，其余按 shell
  词法作 `args`，缺省名取命令 stem，注册后立即以该 app 范围 apply。等价于
  「add 脚手架 + 自动 apply」，是冷启动的最短路径（见「快速上手」路径一）。

字段优先级（高 → 低）：`[program.*]` 显式字段 > app 配置 `[app]` 表 >
daemon `[app-default]` > 内置默认。`priority` 为应用级字段；`restart` 为程序级
字段，可在 `[app]`/`[app-default]` 层作应用级默认值。

## 术语表

权威定义以 `openspec/specs/glossary/spec.md` 为准，速查：

| 术语 | 定义 |
| --- | --- |
| daemon（守护进程） | 全局唯一的 appkeeper 监督进程本体，持有 daemon 配置与 `app_dir`，唯一执行监督循环的实体 |
| app（应用） | 注册的部署单元；配置本体为部署目录中的 `appkeeper.toml`，注册记录为 `app_dir` 中的链接。app 不是进程，是程序的容器 |
| program（程序） | app 配置 `[program.<name>]` 定义的被守护子进程，有独立状态机与日志流；名称跨全部已注册应用**全局唯一**，任一面板用程序名即可无歧义寻址 |
| action（动作） | 面向 daemon/app/program 发出的执行指令；分**内置动作**（start/stop/restart/signal/reload/shutdown，实现于 appkeeper 自身）与**自定义动作**（配置声明、映射为 shell 命令执行） |

标识符规则：app 名、程序名、动作名统一只允许英文字母、数字、下划线与连字符
（`[A-Za-z0-9_-]+`）。点号、空格、非 ASCII 一律校验拒绝——`gateway.api`
这类复合名只会被当作非法标识符，不会被解释为 `app.program` 寻址语法。
观察类命令（status/pid/log/list/pending）与 `apply` 不属于动作词表，
自定义动作可以叫 `status` 而不冲突。

## 快速上手

```bash
cargo build --release          # 产物: target/release/appkeeper(.exe)

ak config --init          # 可选：初始化 daemon 配置（8 个已知键的默认值 + 注释模板），
                               #   已存在则拒绝；连带创建 app 注册目录与 example.toml.sample
ak config --edit          # 可选：用 $VISUAL/$EDITOR（缺省 vi/notepad）打开 daemon 配置，
                               #   不存在则创建；保存退出后自动校验，非法以退出码 2 报告

# 路径一：冷启动一条命令跑起来（命令行注册 + 自动后台拉起守护进程 + 立即生效）
ak run -- python -m http.server 8000 --name web
ak run --cmd "node server.js" --name api   # 等价写法（shell 词法切分，适合不便传 -- 的场景）
ak run web                                 # 之后启动/重启已注册程序
ak stop web                                # 停止（`restart = "unless-stopped"` 会记住这次用户停止）

# 路径二：注册部署（离线可用，适合生产部署目录）
# 1. 在你的应用部署目录写一个 appkeeper.toml（见 examples/demo-app）
# 2. 注册（空配置也能先跑起来）；或从可执行程序直接脚手架生成：
cd /opt/myapp && ak add . --name myapp --restart always
ak add /opt/srv/web-server --name abc --args "--port 8080" --env LOG=debug --apply
ak list

# 3. 启动守护进程（默认空无一物，只拉起已注册应用）
ak run                    # 前台；Ctrl+C 优雅停机
ak run --detach           # 后台：就绪后打印监听地址与日志路径即返回

# 4. 控制
ak status                 # 人读表格（NAME/APP/STATE/PID/RESTARTS/UNHEALTHY）
ak status --json          # 脚本消费：等价 GET /v1/status 的 pretty JSON
ak stop myapp-程序名
ak log web api --tail 50 -f   # 多程序聚合（>1 名时行首加 [名] 前缀）
ak log web --stream both      # 合并 stdout/stderr
appkeeper signal <程序名> USR1     # 向子进程投递白名单信号（unix）
ak restart --app myapp     # 对整个 app 扇出（按启动排序，停止逆序）
appkeeper action api flush        # 执行自定义动作（退出码透传）
ak reload                 # 重扫配置并检出待应用变更（pending），不触碰任何进程
ak apply                  # 应用待应用变更（apply all 等价；apply demo / apply demo web 限定范围）
ak apply --restart        # 无变更的程序也重启（手动停止的保持停止）
ak remove web             # 注销：删除注册记录与生成文件（部署目录文件不动）
ak shutdown
```

> **`run` 是统一入口动词**：裸 `run`（可加 `--detach`）是守护进程；
> `run <名>` / `run --app <app>` 启动已注册程序或整个 app；`run -- <命令行>` /
> `run --cmd "<字符串>"` 注册一条新命令行并立即生效（守护进程离线时自动后台拉起）。
> 旧 `start` 动词已移除，调用会报未知子命令并提示改用 `run`。
>
> 冷启动时程序是否被拉起取决于其 `restart` 策略：`no` 只注册不拉起，
> 其余策略拉起。`ak config --init` 生成的 `[app-default] restart = "always"`
> 让上面路径一开箱即跑；若你手写 daemon 配置且未设 `[app-default]`，级联到底的
> 默认是 `no`——此时需 `ak run web` 手动启动，或在 `[app-default]` 显式
> 写 `restart = "always"`。
>
> **`--restart=no`**：命令行注册形态加 `--restart=no` 时只注册、不拉起
> （覆盖「先登记、暂不运行」），之后 `ak run <名>` 可随时启动。

> `all` 是 apply 的全量保留字：`ak apply all` 与裸 `ak apply`
> 严格等价，因此应用名不能叫 `all`（add 与 run ad-hoc 会拒绝并说明保留字）。

## 配置示例

daemon（`examples/daemon.toml`，带注释模板在 `conf/daemon.toml`；app 模板在 `conf/app.toml`）：

```toml
[daemon]
log_level = "info"
log_dir = "/var/log/appkeeper" # 可选；缺省 /tmp/appkeeper/logs，必须为绝对路径
monitor_interval = 1.0
host = "127.0.0.1"          # 控制平面仅回环
port = 7310
auth_token = ""             # 非空则要求 Bearer 鉴权
log_buffer_lines = 1000
app_dir = "apps"            # 注册目录，缺省 daemon 配置同级 apps/

[app-default]               # 可选：全体应用默认值
restart = "always"          # 启动/重启策略：no | on-failure | always | unless-stopped
restart_backoff = 1.0
```

> **`restart` 取代了旧的 `autostart` + `autorestart`**（docker 字面量语义）：
> - `no`：退出不重启，守护启动/apply 不拉起；
> - `on-failure`：退出码不在 `exit_codes`（默认仅 0）时重启，守护启动拉起；
> - `always`：任何退出都重启，守护启动无条件拉起；
> - `unless-stopped`：任何退出都重启；守护启动时若无「用户停止」记录则拉起
>   （显式 `stop` 写入记录并持久化到 `{log_dir}/state.json`，显式启动清除，
>   正常关机不写入）。
>
> 宽容写法：布尔 `false` ≡ `"no"`；四层级联到底均未设置 ≡ 内置默认 `no`。
> `ak config --init` 生成的 `[app-default]` 显式写 `restart = "always"`
> 以保持开箱守护体验。
>
> **迁移**：旧字段 `autorestart`（`never`→`no`，其余原值）与 `autostart`
> （布尔）作为弃用别名继续可读（`autostart = false` → `no`；`autostart = true`
> 或未设 + `autorestart = X` → X；`autostart = true` + `autorestart = "never"`
> → `no` 并输出迁移告警）；同一层级同时出现新旧字段会被校验拒绝。所有写入
> 路径（add 微调、run ad-hoc、脚手架、模板）只写新字段 `restart`。
>
> **级联默认变化高亮**：旧模型在无 `[app-default]` 时默认「守护启动拉起」
> （`autostart` 缺省 true）；新模型级联到底默认 `no`——既无旧字段、又无
> `[app-default] restart` 的既有部署升级后，守护重启将不再自动拉起程序。
> 依赖旧行为的部署请在 `[app-default]` 显式 `restart = "always"`（`config --init`
> 模板已内置）。

### Web 控制台开关（`[webui]` 段）

内嵌 Web 控制台是**配置驱动**的可选段：daemon 配置里存在 `[webui]` 段即开启，
缺省关闭。无需任何额外子命令——`ak run` 启动时按配置伺服，运行中改动
由 `ak reload` 热生效（开启/关闭/换址重绑；端口被占用时降级运行并在
reload 输出中说明，守护进程不受影响）：

```toml
[webui]
listen = "127.0.0.1:9877"   # 缺省 127.0.0.1:9877
```

```bash
ak config --set webui.listen=127.0.0.1:9877   # 一次性写入（等价手改 [webui] 段）
ak reload                                     # 守护在线时热开启；输出附 webui: 结果行
ak config --delete webui                      # 删整段 = 关闭控制台；reload 热关闭
```

app（部署目录 `appkeeper.toml`，`examples/demo-app/appkeeper.toml`）：

```toml
[app]                       # 可选；add 的微调 flag 写在这里，部署人可手改
description = "演示应用"
restart = "on-failure"      # 应用级默认（程序未显式声明时生效）

[program.api]               # 键即程序名
command = "python -m http.server 8000"   # 单行写法（词法拆分，不经 shell）；或 command + args
work_dir = "."
env = { FOO = "bar" }
startsecs = 1.0             # 存活超过此时长才算启动成功（预算 startretries 次失败）
stop_timeout = 10
exit_codes = [0]            # on-failure 的"期望退出码"
depends_on = ["db"]         # 可跨应用引用（写程序名，全局唯一）；成环在注册期拒绝
log_max_size = "10MB"       # 可选；缺省 50MB，"0" 显式禁用轮转
log_rotate_keep = 2          # 可选；缺省保留 2 份轮转文件
health_check = "http://127.0.0.1:8000/health"   # http(s):// | tcp://host:port | exec 命令行
health_interval = 10        # 健康连续失败 health_retries 次 -> unhealthy
restart_on_unhealthy = true # unhealthy 触发与崩溃一致的重启
```

## 日志

未配置 `daemon.log_dir` 时，日志固定写入 `/tmp/appkeeper/logs`，且 `log_dir`
必须是绝对路径。每个程序的 stdout 与 stderr 分别按大小轮转：缺省单文件
`50MB`，保留 2 个轮转文件（当前文件加 `.1`、`.2`，每流最多 3 个文件）。设置
`log_max_size = "0"` 可禁用该程序的轮转。

`/tmp` 可能是 tmpfs，或被系统的临时文件清理机制删除；需要跨重启保留日志时，
请显式配置绝对目录，例如 `/var/log/appkeeper`。多实例部署也应使用不同的绝对目录。

升级前依赖默认相对 `logs` 目录的部署，新日志会改写到 `/tmp/appkeeper/logs`，旧日志
仍留在 `<daemon 配置目录>/logs`，不会自动迁移。已有 `log_dir = "logs"` 等相对路径
配置会被拒绝启动，需改为绝对路径。

## daemon 配置管理（`ak config`）

daemon 配置的脚本化管理入口，纯本地文件操作（不依赖运行中的守护进程）。
动作由 flag 指定，一次调用只执行一类动作；无任何动作 flag 时打印用法并以 0 退出：

```bash
ak config --init                        # 全新初始化（拒绝覆盖既有文件）
ak config --get port                    # 查询生效值：文件显式值优先，未设置回退内置默认（单键只打印值）
ak config --get port --get host         # 多键查询：每行 key=value
ak config --set port=8080 --set log_level=debug   # 白名单强类型写入（可重复）
ak config --delete port                 # 删除（回到「未配置」态；幂等；点路径 daemon.port 等价）
ak config --edit                        # $VISUAL/$EDITOR 全文编辑 + 保存后校验
```

- **白名单强类型**：`--set`/`--get`/`--delete` 接受 `[daemon]` 表的 8 个已知键
  （`log_level`、`log_dir`、`monitor_interval`、`host`、`port`、`auth_token`、
  `log_buffer_lines`、`app_dir`）与 `[webui]` 段的 `webui.listen`；`port` 限
  1–65535 整数、`monitor_interval` 为正浮点、`log_buffer_lines` 为非负整数、
  `log_level` ∈ trace|debug|info|warn|error、`webui.listen` 须为 host:port。
  未知键或类型不符以退出码 2 拒绝，不落盘。
- **读-改-写保留格式**：写入只改动目标键的值节点，未触碰键的顺序、值与注释原样保持。
- **写后整体校验 + 原子落盘**：先写临时文件、整体校验通过后 rename 覆盖；校验失败
  回滚为原内容（磁盘不留半成品）。
- **删除即回退默认**：删除有默认值的键后回到「未配置」态；目标本不存在时幂等成功
  （文件不变）；未知键/未知表拒绝。删除整段 `--delete webui` 即关闭 Web 控制台。
- **缺失文件语义**：`--get` 等价全默认回答；`--set`/`--delete` 拒绝并提示先执行
  `ak config --init`；`--edit` 先创建再打开。
- **在线提示**：写动作成功后对既有控制面地址发一次短超时探活——在线则提示执行
  `ak reload` 生效，离线提示下次启动生效。探测失败不阻塞动作本身。

app 配置不进入 `config` 的键空间（继续走 add/apply 体系）；`[app-default]` 仅在
`--init` 模板中以注释形式提示。

## 配置变更：检出与应用（两阶段）

编辑 app 配置或 daemon 配置后，守护进程会在一个巡检周期内自动检出差异
（pending），**不会**触碰任何进程。`ak apply` 显式应用：

- `apply` 应用全部 pending；`apply all` 与缺省严格等价（`all` 为全量保留字，
  不可用作应用名）；`apply <app>` / `apply <app> <program>` 限定范围，
  不影响范围外的应用；
- 配置有变化的程序：停止 → 以新定义重建 → 原先在跑则重新拉起（手动停止的
  保持停止，只更新定义）；
- `apply --restart`：无配置变更的程序也重启——但手动停止的（stopped/exited/
  fatal）保持停止，崩溃退避中的（backoff）立即拉起；
- 无待应用变更时 `apply` 什么都不做（幂等，退出码 0）。

`ak reload` 现在只「重扫 + 检出 + 输出预览」。**迁移说明**：依赖
「reload 即生效」的脚本请改为 `ak reload && ak apply`（或直接
`ak apply`）。`ak add/remove` 在线同步同样进入 pending，需
`apply` 后才拉起/停止对应程序。

## 状态机与重启语义

```
[stopped] ──spawn──▶ [starting] ──存活≥startsecs──▶ [running]
                        │ startsecs 内退出（消耗 startretries 预算）
                        ▼
                     [backoff] ──退避到期──▶ [starting]
     startretries 耗尽 ──▶ [fatal]（仅显式 start / reload 清除）
[running] ──退出──按 restart 策略──▶ [backoff] 或 [exited]
任意 ──stop──▶ [stopping]（SIGTERM→超时强杀 / TerminateProcess）──▶ [stopped]
```

- 重启策略（`restart` 字段）：`always`（含干净退出）、`on-failure`（退出码 ∉
  `exit_codes` 时重启）、`unless-stopped`（会话内同 always，守护启动看「用户停止」
  记录）、`no`（不重启）
- 守护启动 / apply 引入新程序时的拉起语义：`no` 不拉起；`on-failure`/`always`
  拉起；`unless-stopped` 无「用户停止」记录才拉起（`always` 不受记录影响）
- 稳态崩溃退避：`restart_backoff` 指数翻倍至 `max_restart_backoff`；
  连续运行 `backoff_reset_after` 秒后计数清零
- 依赖编排：priority 小者先启动；依赖未 running 则等待；依赖 fatal/exited
  则依赖方进入 fatal（原因注明）；关闭按逆序停止

## 动作（action）

动作是改变运行状态的执行指令，按目标分层（权威定义见
`openspec/specs/actions/spec.md`）：

| 层级 | 动作 | 入口 |
| --- | --- | --- |
| daemon | shutdown、reload | 既有命令，语义不变 |
| program | start、stop、restart | 既有命令；signal 见下 |
| program | 自定义动作 | `appkeeper action <program> <动作名>`，配置声明见下 |
| app | start、stop、restart 扇出 | `ak run\|stop\|restart --app <名>` |

**app 扇出**：作用于该 app 全部程序，排序复用启动规则——启动按依赖拓扑序
（同名次按字典序），停止按逆序；逐程序复用既有 start/stop/restart 逻辑，
所以扇出 start 会拉起手动停止的程序（并清除 user_stopped 标记），输出逐程序
结果。裸 `ak run <程序名>` 与旧 `ak start <程序名>` 语义一致。

**signal（内置动作，unix）**：`appkeeper signal <程序名> <SIGNAL>` 向该程序的
子进程 pid 投递信号（不投进程组——清理进程组是 stop 的语义）。白名单：
`TERM INT HUP QUIT USR1 USR2`（大小写不敏感，可带 `SIG` 前缀）；白名单外
（含 KILL/STOP/CONT）报错——需要停止请用 `ak stop`。投递成功不改变
程序状态机；Windows 上明确报平台不支持。

### 自定义动作

在 app 配置的程序表下声明（字段仅 `command` 必填 + `timeout` 可选，缺省
30 秒且必须 > 0）：

```toml
[program.api]
command = "python -m http.server 8000"

[program.api.action.flush]
command = "curl -fsS http://127.0.0.1:8000/flush?pid=${program.api.pid}"
timeout = 10

[program.api.action.upgrade]
# TOML 多行字符串整段交给平台 shell：管道/重定向/&& 都可用
command = """
set -e
cd /opt/myapp
git pull && ./build.sh
ak restart api          # 动作中链式控制命令：CLI 自动读取 daemon 配置连接控制面
"""
```

执行契约（actions 规范）：

- **经平台 shell 执行**：unix `/bin/sh -c`、Windows `cmd /C`。这是与程序
  `command` 字段（词法拆分直 spawn、不经 shell）的有意分叉：动作是运维者
  编写的维护逻辑，shell 语义是核心价值。多行命令整段作为脚本文本；也可
  `bash scripts/upgrade.sh` 调用外部脚本（相对路径按程序 `work_dir` 解析）。
- **变量替换**：`${...}` 中以已知域开头的引用在 spawn 期替换——
  `program.<名>.pid|state|app|work_dir|log_dir`、`app.<名>.path`、
  `daemon.pid|host|port|log_dir|app_dir`（程序名全局唯一，可跨程序引用）。
  程序未运行时其 `pid` 替换为**空串**。`validate`/`reload` 会拒绝未知字段
  （拼错即报）；**非已知域的 `${...}` 原样保留给 shell**——`${HOME}` 等
  shell 变量不受影响。
- **互斥**：同一程序的同名动作执行中再次调用返回冲突（API 409 / CLI 退出
  码 1），不排队；同一程序的不同动作允许并行。
- **超时**：超过 `timeout` 终止整个动作进程树（unix 杀进程组 / Windows
  Job Object），响应标记 `timed_out`。
- **输出去向**：完整输出按行写入 daemon 日志（前缀
  `[program.<程序>.action.<动作>]`）；调用响应只带截断的输出尾部。动作不进
  状态投影，webui 不展示。
- 动作执行不阻塞监督循环（独立工作线程）；动作命令等于配置作者在本机的
  任意代码执行能力，与 health exec 同级信任模型，调用面受 Bearer 鉴权覆盖。

## 控制平面

守护进程在 `host:port`（默认回环 7310）提供 JSON API：
`GET /v1/health|status|programs|programs/{name}|programs/{name}/logs`，
`POST /v1/programs/{name}/start|stop|restart`、`/v1/programs/{name}/signal`（白名单信号）、
`/v1/programs/{name}/actions/{动作}`（同步执行自定义动作，返回退出码/耗时/超时标记/输出尾部）、
`/v1/apps/{name}/start|stop|restart`（app 扇出，返回逐程序结果）、
`/v1/reload`（检出 pending 并返回预览）、
`GET /v1/pending`、`POST /v1/apply`、`/v1/shutdown`。
配置 `auth_token` 后除 `/v1/health` 外都要求 `Authorization: Bearer <token>`。
`GET /v1/programs/{name}/logs?stream=out|err&tail=N&follow=1` 支持流式跟随。

CLI 退出码：`0` 成功、`1` 一般错误、`2` 配置错误、`3` 守护进程不可达。
例外：`appkeeper action` 成功时**透传动作自身的退出码**（超时/调用失败为 1，
守护不可达为 3）。

### 统一入口动词 `run`

`ak run` 承载三种形态，分派规则如下：

| 形态 | 调用 | 行为 |
| --- | --- | --- |
| daemon | `ak run` / `ak run --detach` | 前台守护 / 后台守护（分离进程组，就绪后打印监听地址与 `{log_dir}/daemon.out.log` 路径并返回 0） |
| 启动 | `ak run <name>` / `ak run --app <app>` | 经控制面 API 启动已注册程序 / 扇出启动整个 app（对已 running 程序返回 409，未知程序 404） |
| ad-hoc | `ak run -- <cmd...>` / `ak run --cmd "<字符串>"` | 命令行注册并立即生效（首 token 解析为绝对路径；守护离线时自动后台拉起） |

- ad-hoc 形态与 `name`/`--app`/`--detach` 互斥；`--detach` 仅 daemon 形态合法。
- ad-hoc 的脚手架 flag：`--name`（缺省取命令 stem）、`--description`、
  `--restart=<no|always|unless-stopped|on-failure>`、`--restart-backoff`、
  `--env K=V`（可重复）、`--workdir <路径>`；`--args` 不存在（命令行本身即参数）。
- 旧 `start` 动词已移除：`ak start web` 报未知子命令并提示 `ak run web`。
- 旧 flag `--autostart`/`--no-autostart`/`--autorestart` 已移除，统一
  `--restart=<字面量>`（add 与 run ad-hoc 共用）。

**后台拉起协议**（`run --detach` 与「立即生效」入口共用）：派生
`ak run -c <配置>`（unix `process_group(0)` / Windows
`CREATE_NEW_PROCESS_GROUP | DETACHED_PROCESS`），stdio 追加写
`{log_dir}/daemon.out.log`；以 ~200ms 间隔轮询 `/v1/health`，总超时 ~10s。
就绪即成功（并发拉起只留一个实例，端口竞争败者自灭）；超时以非零码退出并
指明日志位置。守护进程为全局单例，不与调用方生命周期绑定。

Web 控制台与控制面同属一个守护进程：存在 `[webui]` 配置段时（见上文
「Web 控制台开关」）另开一个回环端口，伺服内嵌 UI、`/api/*` 查询与 `/ws`
WebSocket 推送——状态投影与本控制面完全一致（见下文 Web UI）。

### 交互式 shell（`appkeeper shell`）

supervisorctl 风格的终端入口，经控制面 API 与守护进程通信（纯客户端，不改变守护行为）：

```bash
appkeeper shell                        # 进入 REPL（行编辑 + 历史，Ctrl+C 中断当前行，exit/quit 离开）
appkeeper shell -e "status"            # 单命令模式：执行一条后退出（脚本友好，退出码同 CLI 约定）
```

内置命令：`status`（对齐表格：NAME/APP/STATE/PID/RESTARTS/UNHEALTHY）、
`run|stop|restart <name>`（或 `--app <app>` 扇出，语义同 CLI）、
`action <program> <action>`（输出尾部；`-e` 模式退出码透传动作退出码）、
`signal <program> <SIGNAL>`、`pid <name>`、`log <name> [-f] [--tail N] [--stream out|err]`、
`pending`、`apply [<app> [<program>]] [--restart]`、`shutdown`、`open`（用系统浏览器打开 webui 控制台）、`help`/`?`、`exit`/`quit`。

### 一键打开控制台（`appkeeper shell` 的 `open` 动词）

控制台默认关闭：先在 daemon 配置里加 `[webui]` 段（或
`ak config --set webui.listen=127.0.0.1:9877`），守护在线时
`ak reload` 热开启（离线则下次 `ak run` 自动伺服），然后：

```bash
appkeeper shell -e "open"     # 探活 GET /api/health 可达才调起系统浏览器；不可达给出开启提示
```

## Web UI

### 技术栈

`webui/` 是一个 Vite + TypeScript + Lit 的 SPA：

- **Lit 3**（Web Components / Shadow DOM）渲染全部界面，视图即自定义元素 `<appkeeper-*>`；
- **Web Awesome** 提供 Web Components 基础控件，主题经 `webui/src/styles/wa-overrides.css`
  映射到仓库自己的 design tokens（`tokens.css`，indigo 品牌、暗色优先）；
- **pnpm** 管理依赖，**vitest**（happy-dom/jsdom 环境）跑组件测试，TypeScript `strict` 模式；
- 图标是零依赖的内联 SVG 集合（`src/components/icons.ts`），无字体/CDN 外链。

前端产物经 **rust-embed** 编译进二进制：`cargo build --release` 时 `build.rs` 检查
`webui/` 输入是否比 `dist/` 新，需要时自动执行 `pnpm install` + `pnpm build`，
然后把 `webui/dist` 嵌入；`dist/` 本身**不提交**，全新 checkout 只要有 Rust +
Node 工具链即可一次 `cargo build --release` 得到带完整 UI 的二进制。

**debug 构建默认跳过前端工具链**（不调 pnpm，Rust 调试迭代不被拖慢）：debug 下
rust-embed 在运行时直接读取磁盘上的 `webui/dist`——手动 `pnpm build` 一次后，
改前端再 `pnpm build` 即可，无需重新编译 Rust；`dist` 缺失时伺服占位页。
需要 debug 下自动构建可设 `AK_WEBUI_BUILD=force`。

### 开发调试（pnpm dev）

双进程工作流：后端伺服 JSON API（`127.0.0.1:9877`），Vite 伺服 SPA 并热更新，
`/api`、`/health` 经代理转发到后端——开发与部署访问的是同一组路径。

```bash
# 终端 A：后端（API + 内嵌 UI）——daemon 配置含 [webui] 段即伺服控制台
cargo run -- run                    # 配置未开控制台时，先: ak config --set webui.listen=127.0.0.1:9877

# 终端 B：前端热更新开发服务器
cd webui
pnpm install
pnpm dev                            # http://localhost:5273（代理 /api /health /ws → 9877）
pnpm test                           # vitest 组件测试
```

`/ws` WebSocket 推送同样经 dev server 代理，dev 模式下实时状态与日志可用；
代理目标可用 `AK_WEBUI_DEV_BACKEND` 覆盖（默认 `http://127.0.0.1:9877`）。

前端改完后 `pnpm build` 产出 `dist/`，再 `cargo build --release` 即把新 UI 嵌入
二进制（debug 构建则运行时直接读盘，无需重编）。

### 发布

```bash
cargo build --release
# 产物: target/release/appkeeper(.exe) —— 单文件，自带全部前端资源
```

- 发布产物是**单个二进制**：运行时不需要 Node、不需要静态文件目录。
- 没有 Node 工具链时 `cargo build` 不会失败：build.rs 写入一个占位页并给出 warning。
- 设置 `AK_WEBUI_BUILD=skip` 可跳过前端构建（CI 无 Node 环境时），
  直接使用磁盘上已有的 `webui/dist/`；旧变量 `AK_WEBUI_BUILD` 仍被兼容识别。

### Ansible 部署

Ansible role 部署见 [.ansible/README.md](.ansible/README.md)：默认免 inventory，
本机一条命令完成"分发二进制 → 装 daemon 配置 → 注册 systemd 服务"，也可直接引用
`roles/appkeeper` 扩展到多主机。默认部署对既有 appkeeper 安装零影响——检测到
既有安装立即停止并说明原因，需显式设置 `appkeeper_force_overwrite: true` 才允许覆盖。

### API 概览

守护进程按 daemon 配置的 `[webui]` 段伺服控制台（守护循环 + 控制台一体），
HTTP API 与 WebSocket 同端口伺服。`/api/*` 与控制面 `/v1/*` 使用**同一份状态投影**
（`server.rs`），数据同源：程序状态来自守护循环，日志来自内存环形缓冲（不受轮转影响）：

| 端点 | 说明 |
|---|---|
| `GET /api/health` | 存活探针 |
| `GET /api/overview` | 守护进程概况 + 全部程序状态（JSON 快照） |
| `GET /api/programs` | 程序状态列表 |
| `GET /api/programs/{name}` | 单个程序详情 |
| `GET /api/programs/{name}/logs?stream=out\|err&tail=n` | 日志末尾 n 行（取自内存环形缓冲） |
| `POST /api/programs/{name}/start\|stop\|restart` | 控制命令（经命令队列由守护循环执行；非法迁移返回 409，响应 `{"result": …}`） |

WebSocket `/ws`（二进制帧 = 1 字节类型 + 载荷，最高位=zlib 压缩标记）：

| 类型 | 载荷 | 说明 |
|---|---|---|
| 1 snapshot | MessagePack | 连接建立后立即下发的全量快照 |
| 2 status | MessagePack（变更程序数组） | 状态增量事件 |
| 3 log | 程序名 + 流向 + 原始 UTF-8 文本 | 日志分块（订阅后先补发 tail 再跟随） |
| 4 heartbeat | — | 周期心跳 |
| 5 error | JSON | 订阅失败等原因 |

客户端向 `/ws` 发送 `{"action":"subscribe","program":"…","stream":"out\|err"}`
（JSON 或 MessagePack 均可）订阅日志流。结构化消息与 REST JSON 由同一组
serde 结构体派生，字段语义完全一致；体积敏感的 WS 通道用 MessagePack（实测
约为 JSON 的 82–87%），日志文本以无转义的原始 UTF-8 承载。

### 当前状态

`webui/` 目前是**最小占位骨架**（品牌侧栏 + 单路由占位页，展示 `/api/health`
探活结果）。从模板项目带入的会话工作台代码已全部删除；控制台应提供的界面元素
——左侧 app→进程两层导航、状态徽章、启停控制、日志查看等——定义在
`openspec/specs/webui-ui`（经变更 `webui-docs-and-ui-scope` 固化），两层导航的
第一层即 app-registry 的注册应用。服务端已按 `openspec/specs/webui-api`
（变更 `webui-api`）实现：控制台 REST + WebSocket 实时推送，与控制面共享状态
投影与命令队列；前端界面将在后续变更中按规格落地。

## 从 v0.1 迁移

v0.1 单文件配置（`[daemon]` + `[[program]]`）在 `ak add <旧文件>` 时被
自动识别并转换：程序转写为 `[program.*]` 注册为一个应用，`[daemon]` 段提示
并入 daemon 配置，原文件不修改。

## 升级注意（标识符字符集收紧）

应用名、程序名与自定义动作名现在只允许 `[A-Za-z0-9_-]`（英文字母、数字、
下划线、连字符）。含点号、空格或非 ASCII 字符的既有名字升级后会被
validate/reload 拒绝。迁移步骤：升级二进制 → `ak validate`（暴露全部
违规名）→ 改名（`appkeeper.toml` 中的表键与 `depends_on` 引用）→
`ak reload && ak apply`。

回滚提示：`[program.<名>.action.*]` 动作表是新增配置结构，旧版本二进制
会因未知字段拒绝整个 app 配置——回滚前先移除这些表再降级。

## 升级注意（cli-revamp：run 统一动词与 restart 字段）

本版把「跑」收敛到 `run` 一个动词、启动/重启策略收敛到单一 `restart` 字段，
含三处 BREAKING：

- **`start` 动词移除**：`ak start <名>` → `ak run <名>`；
  `ak start --app <app>` → `ak run --app <app>`（报错信息自带提示）；
  `shell` 内置动词同步由 `start` 改为 `run`。
- **`status` 默认输出改为人读表格**：脚本消费请加 `--json`，其输出与本变更前
  的默认 pretty JSON 逐字节等价。
- **配置字段统一为 `restart`**：旧 `autostart`/`autorestart` 继续可读（弃用别名，
  映射规则见「配置示例」），同层新旧同现被拒绝；级联到底的默认从旧的
  「守护启动拉起」翻转为 `no`——依赖旧默认的部署请在 `[app-default]` 显式
  `restart = "always"`（`config --init` 模板已内置）。

另：`add --apply` 与 run 的 ad-hoc 形态在守护进程离线时不再报错，而是**自动
后台拉起守护进程**后继续生效（曾依赖「离线报错」的自动化需知悉）；无 systemd
等场景可用 `ak run --detach` 直接后台常驻，`ak shutdown` 统一管理。

## 作为系统服务运行（守护 appkeeper 本身）

Linux (systemd) 一条命令安装/卸载（需要 root）：

```bash
sudo ak service install            # 生成 /etc/systemd/system/ak.service + daemon-reload + enable
sudo ak service install --now      # 安装后立即 start
sudo ak service uninstall          # stop + disable + 删除 unit + daemon-reload
```

可选参数：`-c/--config <daemon 配置>`（全局参数；写入 unit 的 ExecStart，缺省 `/etc/appkeeper/daemon.toml`）、
`--unit-file <path>`（unit 文件完整路径，必须以 `.service` 结尾，默认
`/etc/systemd/system/ak.service`）、`--user <name>`（服务运行用户）、
`--force`（目标 unit 已存在且内容不同时覆盖）。重复安装内容一致时幂等跳过。
`TimeoutStopSec` 按已注册程序的最大 `stop_timeout` 自动估算（2×最大值 + 10s，
配置不可加载时 90s）；需要定制可直接修改生成后的 unit 文件再
`systemctl daemon-reload`。

Windows 暂不支持 `ak service`（nssm 仍可用）：

```bat
nssm install appkeeper D:\tools\appkeeper\ak.exe
nssm set appkeeper AppParameters run
nssm start appkeeper
```

appkeeper 被强杀时，Windows 上子进程树由 Job Object（kill-on-close）由内核
兜底清理；Unix 上子进程位于独立进程组，正常路径按进程组终止。

## 平台差异与注意

- Windows 优雅停止无 SIGTERM，stop 等价 TerminateProcess（子进程树由 Job
  Object 兜底）；Unix 先 SIGTERM 等待 `stop_timeout` 再 kill 整个进程组。
- Windows 符号链接需要管理员/开发者模式：无权限时 `app_dir` 降级为硬链接。
  硬链接会被 `sed -i` 等"替换文件式"编辑断开——编辑配置后重跑一次
  `ak add .` 刷新链接即可（幂等）；symlink 模式不受影响。
- Linux 上 `/etc/appkeeper/daemon.toml` 需 root 写权限：非 root 用户执行 add/remove
  请用 sudo，或 `-c/--config` 指向用户级 daemon 配置。
- 单行 `command` 只做词法拆分（空白 + 引号），不经过 shell；需要 shell 语义
  显式写 `bash -c "..."`。自定义动作的 `command` 相反，整体交给平台 shell
  （unix `/bin/sh -c`、Windows `cmd /C`），见「动作」一节。
- `signal` 动作仅 unix：Windows 上返回明确的平台不支持错误。

## 性能基准（`appkeeper-bench`）

独立二进制 `appkeeper-bench`（workspace 成员 crate，随 workspace 一同构建）对真实
appkeeper daemon 做负载基准测量：按 case 拉起负载、从既有 `/v1` 控制面与磁盘日志
**纯外部**采集指标，输出人类可读报告（stdout）与 `--json` 机器可读结果（rows/s、
bytes/s、峰值/均值 RSS、轮转次数、wall time、逐行完整性校验）。退出码只反映运行
成败，不做阈值门禁——版本间回归请自行对比两次 JSON。

```bash
# 自带隔离环境（缺省）：临时 workspace 拉起自己的 daemon 与负载程序，结束自动清理
appkeeper-bench --case firehose --duration 10

# 连接已运行的 daemon（同机；daemon 开启 Bearer 鉴权时加 --token）
appkeeper-bench --case firehose --connect 127.0.0.1:7310 --token <tok>

# 导出机器可读报告
appkeeper-bench --case rotation --log-rows 200000 --json report.json
```

四个 case（`--case`，必选）：

| case | 语义 | 看什么 |
|---|---|---|
| `firehose` | 单程序全速（或 `--rate` 定速）产出 | 泵/落盘链路吞吐上限（rows/s、bytes/s） |
| `rotation` | bench 自动设定小轮转阈值 + 足够 keep | 高频轮转下的逐行完整性与轮转次数 |
| `fanout` | `--programs` 个程序并行，stdout/stderr 双流 | 聚合吞吐与每程序吞吐分解 |
| `drip` | 低速率稳态长跑（缺省 100 行/秒） | daemon RSS 峰值/均值（内存有界性） |

负载量界参数（任一先到即终止）：`--log-rows`（每程序总行数，0/缺省不限）、
`--log-row-size`（单行载荷字节数，缺省 128）、`--log-total-size`（全部程序合计
字节上限）、`--rate`（行/秒，0 = 全速；drip 缺省用内置 100）、`--duration`
（墙钟上限，缺省 30 秒）、`--programs`（fanout 程序数，缺省 4，仅 fanout 消费）。
负数/非数字等非法组合以非零码拒绝。

- **负载程序** = bench 二进制自我重入（隐藏 `__generate` 模式），作为 daemon 普通
  子进程精确控速产出真实日志；行内嵌 `[流] 序号 时间戳` 前缀，测量结束后按轮转序
  逐行对账——丢行/缺口/乱序/重复 → 报告 integrity 失败 → 非零退出。
- **清理契约**：无论成功、失败或 Ctrl-C，自动卸载临时 app 并删除其日志文件
  （spawn 模式连临时 workspace 一并删除）；`--keep` 保留现场供检查（保留
  workspace/日志，临时 app 仍会卸载）。
- **`--connect` 注意**：负载经由目标 daemon 写真实磁盘日志，量界参数请量力设置；
  bench 与 daemon 必须同机（需读取其 app_dir 与 log_dir）；目标上已存在
  `appkeeper-bench-` 前缀 app 时拒绝开跑，绝不触碰非 bench 创建的 app 或文件。
- **与 `cargo run -p xtask -- stress` 的关系**：stress 是零反压/WS 背压的
  **验证**工具（CI 断言口径），bench 是面向用户的**测量**工具（对比 JSON 口径），
  两者实现独立、互不依赖。
- **Windows**：RSS 外部采样不可用，报告该项为 null，其余指标不受影响。

## 测试

```bash
cargo test
```

覆盖：分层配置解析与四层优先级、单行 command 拆分、健康检查协议分发、
跨应用重名/依赖成环校验、标识符字符集校验、动作定义表校验（保留字/变量名/
timeout）与变量替换、动作执行（捕获/超时杀树/互斥）、signal 白名单、
app 扇出排序、legacy 导入、日志轮转与环形缓冲、7 态状态机
（真实子进程：重启/启动预算/策略/停止/落盘）、tcp/http/exec 探测。

跨进程验收（真实 daemon + CLI + HTTP，含动作全链路/signal/--app 扇出/
字符集拒绝场景，以及 appkeeper-bench 的 firehose 冒烟 + JSON 报告 + 清理断言；
`--no-browser` 跳过浏览器段）：

```bash
cargo run -p xtask -- e2e --no-browser
```

appkeeper-bench 自身的完整周期集成测试（真实 daemon，每条数秒）默认忽略：

```bash
cargo test -p appkeeper-bench --test integration -- --ignored --test-threads=1
```
