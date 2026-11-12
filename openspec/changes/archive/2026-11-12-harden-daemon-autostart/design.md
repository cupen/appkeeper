# Design: harden-daemon-autostart

## Context

- `src/service.rs` 已把 Linux unit 的渲染、写入决策（`decide_write`）、停止预算（`compute_timeout_stop_sec`）做成平台无关纯函数并配单测；Windows 侧走 SCM。现有 unit 见 `render_unit`（`src/service.rs:269`），装出的键为 `Description` / `After=network.target` / `ExecStart` / `Restart` / `RestartSec=3` / `KillSignal=SIGTERM` / `TimeoutStopSec` / `RuntimeDirectory=appkeeper` / `RuntimeDirectoryMode=0755` / 可选 `User=` / `WantedBy`。
- `install` 只在 `compute_timeout_stop_sec` 里 `load_or_default` 并吞掉失败（回退 90s），因此配置缺失/损坏时安装仍成功。
- daemon 启动链：`run_daemon_core`（`src/main.rs:2487`）→ `Supervisor::new` → `bootstrap`（`src/supervisor.rs:828`，解析失败的 app 仅记日志跳过）→ `start_eligible`（`src/supervisor.rs:906`，按 `restart` 策略与 `depends_on` 拉起）。
- `POST /v1/shutdown`（`src/server.rs:657`）入队 `Command::Shutdown`；控制台形态由 `install_stop_signals` 接 SIGINT/SIGTERM/SIGQUIT（`src/main.rs:2462`），Windows 服务形态由 SCM 控制处理器接 STOP（`src/service.rs:1139`）。
- 状态投影 `StatusDoc`/`DaemonInfo`（`src/server.rs:82`）由 `/v1`、`/api`、WS 共享；新增字段必须三处同源。
- Windows 配置默认路径由 `default_config_path`（`src/main.rs:550`）按 `%APPDATA%` 推导；控制管道默认名由 `Endpoint::default_name`（`src/transport.rs:683`）按当前进程用户名推导。
- 「用户停止」记录 `state.json` 落在 `log_dir`（`src/supervisor.rs:566`），`unless-stopped` 的跨重启语义依赖它。

## Goals / Non-Goals

**Goals:**

- 生成的 unit 在 systemd 缺省语义下不破坏 daemon 的停止契约（逆序、按 `stop_timeout`），且不误删共享 `/run` 目录。
- 服务形态的环境（PATH、网络就绪、启动失败上限）确定且可解释。
- 持久状态按 State 类落位，「用户停止」记录不再受日志目录与临时目录影响。
- `install` 在装出一个「开不了机/开了也不托管」的服务之前给出明确拒绝或告警。
- 服务托管时 `shutdown` 不再与服务管理器的重启策略互相打架，且 CLI 能自助找到正确命令。
- Windows 默认配置与默认管道在服务账号下讲得通，既有安装可平滑过渡。

**Non-Goals:**

- 不改变 `restart` 策略语义与拉起顺序；「用户停止」记录的文件名与格式不变（只改落位）。
- 不做 systemd 之外的 init 系统（OpenRC/launchd）、不做 Windows delayed auto-start、服务依赖（Tcpip）或资源限制。
- 不要求在本仓库内引入 systemd 容器做 CI 常跑验收（见 D8 的门控冒烟）。

## Decisions

### D1: `KillMode=mixed`，而不是默认的 `control-group`

systemd 缺省 `KillMode=control-group` 在 `systemctl stop` 时把 SIGTERM 发给 cgroup 内所有进程，受管子进程会先于 daemon 的逆序停止收到信号，`stop_timeout` 与排序语义全部失效；`KillMode=process` 又只保证主进程被杀，超时后遗留子进程 `KillMode=mixed` 才同时满足「先让 daemon 自己停」与「超时统一 SIGKILL 兜底」。

- 备选 `KillMode=process`：超时后系统仍可能残留子进程，与 Job Object/进程组回收的既有承诺冲突。
- 备选不改、依赖 daemon 抢在 systemd 前停完：竞态不可控，属偶然正确。

### D2: `RuntimeDirectory=appkeeper/<uid>` + `RuntimeDirectoryMode=0700`（安装期解析的字面 uid）

`<uid>` 是安装期把 `--user` 解析成的数字 uid（passwd 数据库；未指定 `--user` 为 root 的 `0`；解析失败回退 `0` 并告警），以**字面量**写入 unit。不能用 `%U` 占位：systemd.unit(5) 明确 system manager 下 `%U` 恒展开为 `0`（"In case of the system manager this resolves to '0'. Note that this setting is not influenced by the User="）——照抄 `%U` 会让所有 `User=` 的 unit 共用同一 root 属主的 `appkeeper/0` 叶子，非 root 守护进程按真实 uid 推导的 socket 目录与它对不上（bind 失败），多账号也互相争抢。systemd 只创建并清理该路径本身，因此停服只删 `/run/appkeeper/<uid>`，同机其他用户的目录与 socket 不受影响；目录权限同时满足 control-plane 规范要求的「uid 子目录 0700」。

- 备选保留 `RuntimeDirectory=appkeeper`：停服递归删除共享根，直接破坏同机多实例。
- 备选完全不声明 `RuntimeDirectory`：daemon 自建虽可运行（既有兼容场景），但拿不到 systemd 的启动前创建与停止清理。
- 备选渲染 `%U`：system manager 下恒为 0，与 `User=` 脱钩（见上）。
- 兼容：`RuntimeDirectory` 嵌套路径需 systemd ≥ 219；既有 unit 的兼容路径（daemon 自建目录）保持不变。

### D3: unit 显式 `Environment=PATH=...`，不写 `HOME`

服务进程的环境来自 systemd 而非登录 shell，PATH 随发行版/systemd 版本浮动。写死标准系统路径让「同一份 app 配置在手工启动与服务形态下解析到同一批可执行文件」这一预期成立；`HOME` 不代填（代填 `/root` 或 `/` 都是猜），改为文档要求依赖 HOME/自定义变量的程序在 app 配置的 `env` 里声明。

- 备选不写 PATH：发行版差异不可控，且排障时无法从 unit 解释环境。
- 备选 `EnvironmentFile=`：引入第二份文件与权限问题，超出本变更范围。

### D4: `network-online` 与 `StartLimit*` 的落点

`Wants=`/`After=network-online.target` 与既有 `After=network.target` 并列，属于「尽力等待」，不承诺 DNS/DHCP 全部就绪；`StartLimitIntervalSec`/`StartLimitBurst` 放 `[Unit]`（systemd ≥ 230 的语义），显式给出 60s/5 次，避免不同发行版缺省值差异导致的无限重启或过早放弃。

### D5: install 预检 —— 缺配置硬失败，其余告警

- **配置缺失或校验失败 → 拒绝安装**：允许安装一个「开机空跑」的服务是纯误导；`--force` 也不放行（`--force` 只覆盖同名 unit/服务，不承担「假装配置可用」的职责）。
- **损坏 app → 告警不阻断**：bootstrap 本就跳过坏 app，且坏 app 往往是临时编辑态；阻断会让人无法先装服务再修配置。
- **`log_dir` 在 `/tmp`/`%TEMP%` → 告警**：日志随之丢失。不在安装期硬失败，是因为 root 之外的开发/容器场景常用临时目录，硬失败只换来一个无意义的绕过开关。
- **生效 `state_dir` 在 `/tmp`/`%TEMP%` → 告警**：停止记录随之丢失（见 D9）。正常情况下服务形态经由 `StateDirectory` 得到持久目录，这条例外只覆盖「用户显式把 state_dir 指到临时目录」。
- **Windows 管道不匹配 → 告警**：缺省管道名含当前进程用户名，服务以 `--user` 指定的内置账号运行时用户名不同；不硬失败是因为用户可能显式配置了 `socket`（此时无问题），无法仅凭名字判定意图。

预检顺序固定在 `check_install_flags` 之后、`decide_write`/SCM 查询之前，保证「零副作用」。

### D9: 持久状态按 systemd 的 State 分类落位，`log_dir` 只放日志

`state.json` 是「用户停止」意图，属于跨重启保留的状态，不是日志。systemd 对这个类别有专门机制：unit 声明 `StateDirectory=appkeeper/<uid>`（安装期解析的字面 uid，同 D2）后，systemd 在服务启动前按 `User=` 创建并 chown `/var/lib/appkeeper/<uid>`（tmpfiles 机制，daemon 不需要有权限自己建），把路径注入 `$STATE_DIRECTORY`，且停服/卸载都不删除。用户服务时同一指令自动映射到 `$XDG_STATE_HOME`。

因此新增可选 `[daemon] state_dir`，解析顺序为：显式配置 → `$STATE_DIRECTORY` → 平台 State 缺省（unix root `/var/lib/appkeeper/<uid>`；非 root `$XDG_STATE_HOME/appkeeper`，缺省 `~/.local/state/appkeeper`；Windows 服务模式 `%ProgramData%\appkeeper\state`、控制台 `%LOCALAPPDATA%\appkeeper\state`）。全部不可写时告警回退 `log_dir`，绝不因状态目录失败而拒绝启动。

- 备选「留在 `log_dir` + 安装期硬性要求持久日志目录」：会把矛盾推给 `config --init` 模板——模板写 `/var/log/appkeeper` 时普通用户启动即失败，写 `/tmp` 又等于没要求。
- 备选「放在 daemon 配置同级（`/etc/appkeeper`）」：`/etc` 是配置不是状态，`User=` 非 root 时通常不可写。
- 备选「放在 `RuntimeDirectory`」：停服即清，正好丢掉它要跨重启传递的东西。
- 备选「新增 `state_dir` 但默认等于 `log_dir`」：默认行为不变，等于没改。
- 迁移：升级后首次启动若目标文件不存在而旧 `log_dir/state.json` 存在，读旧写新、记日志后删旧，只做一次。`process-management` 已声明记录位置为实现细节，本次把「必须是 State 类目录、不得在日志/运行时目录」写成契约。

### D10: 为什么 unit 用 `StateDirectory=appkeeper/<uid>` 而不是 `appkeeper`

`StateDirectory=appkeeper` 固定解析到 `/var/lib/appkeeper`，同机两个不同 `User=` 的 unit 会争抢同一目录的属主。写入安装期解析的字面 uid（D2 的同一套解析与回退规则），两个实例各得 `/var/lib/appkeeper/<uid>`，与 `RuntimeDirectory` 保持同一套隔离规则——同样不能用 `%U`（system manager 下恒为 0，见 D2）；`StateDirectoryMode=0750` 因为状态里只有程序名，不需要向无关用户开放。

### D6: 托管识别用启动期判据，`shutdown` 在服务端 409

- 判据：Linux `INVOCATION_ID` 非空（systemd 每次启动注入，手工启动不会出现）；Windows 直接看 `--service` 运行模式。判定在 `run_daemon_core` 注入 supervisor 状态，纯函数化（入参为 `env` 查找闭包）便于测试。
- 拒绝点放在 HTTP 路由（`POST /v1/shutdown` 入队前）而非 supervisor：这样 `/api/shutdown`（webui）与 CLI 自动同源；返回 409 + 说明，不停止任何程序。
- unit 名从 `/proc/self/cgroup` 的 cgroup 路径末段去 `.service` 得到（纯函数，可测），失败回退「当前 unit」。
- 备选「允许 shutdown 并自行 `systemctl stop`」：daemon 需要特权且会先被自己停掉，语义绕。
- 备选把 unit 的 `Restart=` 默认改为 `on-failure`：改变了既有崩溃拉起承诺，且解决不了显式 `--restart always`。

`managed_by` 进入 `DaemonInfo`（可空），`/v1`、`/api`、WS 快照同源；CLI 无需强制展示，但 `ak daemon shutdown` 的 409 文本会直接引用它对应的命令。

### D7: Windows 默认配置路径改为 `%ProgramData%`，保留 `%APPDATA%` 回退

`%APPDATA%` 是「安装者」的漫游目录，LocalSystem 读写它既语义错误也不稳定。机器级 `%ProgramData%\appkeeper\daemon.toml` 与服务账号的读取权限天然匹配，并与 Linux `/etc/appkeeper` 对齐。回退规则：仅当新路径不存在且旧路径存在时读旧路径，保证已安装机器升级后配置不丢；`install` 在命中回退时提示迁移到新路径。

- 备选保持 `%APPDATA%`：服务永远读安装者 profile，非管理员安装者切换用户后即失效。
- 备选直接切换、不做回退：升级即丢配置（服务启动失败），对已装机用户是硬破坏。

### D8: 验收 —— 纯函数断言 + 模拟开机 e2e + 门控真机冒烟

- unit 新增键与预检文案用纯函数单测覆盖（渲染文本断言、`decide_preflight` 的输入输出）。
- 「模拟开机」e2e：用与服务等价的前台入口（`daemon start --foreground`）对同一配置重跑，断言按策略拉起、`unless-stopped` 记录跨重启保持、`managed_by` 为 null；不依赖 systemd。
- 真机冒烟以 `AK_E2E_SYSTEMD=1` + root 门控，缺省打印 skip：写临时 unit（`--unit-file`）→ enable/start → 断言 running → stop 后断言无残留子进程 → uninstall。容器/CI 无 systemd 时自动跳过。

## Risks / Trade-offs

- **`KillMode=mixed` 下 daemon 卡死** → `TimeoutStopSec` 到期后整体 SIGKILL，子进程可能被强杀；与现状（子进程先被杀）相比不更差，且 `TimeoutStopSec` 的精算逻辑不变。
- **`network-online.target` 在未启用 wait-online 的机器上是空目标** → 退化为原行为，不阻塞开机；不是回归。
- **显式 PATH 覆盖发行版缺省（如 `/snap/bin`）** → 依赖非标准路径的程序需在 app `env.PATH` 里显式声明；README 与 unit 注释说明。
- **拒绝 `shutdown` 是行为变更** → 升级速览与错误消息同时给出 `systemctl stop` / `sc stop`，手工启动路径不受影响。
- **Windows 配置路径迁移** → 回退保证平滑；`%ProgramData%` 新路径对非管理员 CLI 用户可读，写入仍需管理员/`ak config` 的既有权限模型（不新增提权）。
- **install 预检新增失败模式** → 「无配置先装服务」从成功变为失败，属有意收紧；`--force` 不豁免，文档写明先 `ak config --init`。
- **状态目录新增一个配置键** → `config set/get/delete` 白名单、`--init` 模板、绝对路径校验各加一处；已有 `<log_dir>/state.json` 由一次性迁移兜底，迁移失败只告警（记录按空集处理，最坏是多拉起一次）。
- **`state_dir` 不可写时静默回退** → 启动告警写日志，且该场景主要发生于显式配置了不可写路径的误配；服务形态由 `StateDirectory` 保证可写。
- **Windows 真机仍未验收**（本仓库既有缺口）→ 本变更的 Windows 改动全部落在纯函数与条件编译分支，单测在 Linux 覆盖逻辑；真机验证仍列为遗留项，不在本变更声称已验证。

## Migration Plan

1. 已装机 Linux：`sudo ak daemon install --force --now` 重新生成 unit（新增 `KillMode`/`RuntimeDirectory=<uid>`/`StateDirectory=<uid>`（安装期解析的字面 uid）/`Environment`/`network-online`/`StartLimit`）；旧 unit 不经重装仍可运行但没有新语义，且状态目录继续按旧推导（非服务形态的 State 缺省）。
2. 状态迁移：升级后首次启动把 `<log_dir>/state.json` 搬到生效 `state_dir`；服务形态下即 `$STATE_DIRECTORY`（`/var/lib/appkeeper/<uid>`）。
3. 已装机 Windows：默认配置路径在下次命令时自动回退到 `%APPDATA%`；建议迁移到 `%ProgramData%\appkeeper\daemon.toml` 后执行 `ak daemon install --force --now`（binpath 随之更新）。
4. 服务形态停机改用 `systemctl stop appkeeper` / `sc stop ak`；`ak daemon shutdown` 会明确拒绝并给出该命令。
5. 回滚：重新安装旧版本二进制并用旧版 `install --force` 重写 unit（旧版本不认识新键但会整文件覆盖），新位置的状态文件对旧版本不可见——回滚前把 `state.json` 复制回 `log_dir`。

## Open Questions

- `TimeoutStopSec` 仍是安装期快照：新增更慢的 app 后需要重跑 `install --force`。是否在 daemon 启动时探测并告警，留待后续变更（不影响本变更的规范与任务拆分）。
