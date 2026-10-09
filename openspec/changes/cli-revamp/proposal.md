# Proposal: cli-revamp

## Why

xkeeper 目前是「部署中心」工具：跑起一个受守护的进程需要 `add`（只接受文件路径形态）→ 前台 `run`（守护进程占终端，需自行 nohup/systemd）→ `apply` 三道门槛，且 `add --apply` 在守护进程离线时直接报错；启动已注册程序用 `start`、ad-hoc 跑一条新命令又没有入口，「跑起来」这件事分裂在多个动词上。控制面 `log` 一次只能看一个程序、`status` 输出裸 JSON 面向机器。启动/重启策略分裂在 `autorestart` + `autostart` 两个字段、值域自造（never/on-failure/always + 布尔），与 docker 等主流工具的心智不通。

## What Changes

核心是把「跑」统一到 `run` 一个动词上，并用 docker 字面量的单一 `restart` 字段统一启动/重启策略：

- **`run` 统一入口动词**（用户拍板：run 只吃名字+命令行，路径注册留在 add；`start` 直接移除）：
  - 裸 `run` = 守护进程前台（不变）；新增 `run --detach` = 守护进程后台（分离进程组、stdio 落 `log_dir`、就绪后返回）；
  - `run <name>` / `run --app <app>` = 启动已注册程序/app（吸收原 `start` 语义）；
  - run 的 ad-hoc 形态 = 命令行注册并立即生效：入口两种等价形态 `run -- <cmd...>` 与 `run --cmd "<命令行字符串>"`（二选一）；首 token 解析为绝对路径（canonicalize / `PATH` 解析，Windows 含 `PATHEXT`）；复用既有脚手架引擎与 flag（`--name/--env/--workdir/--restart-backoff/--description`），apply 隐含。
- **（BREAKING）`start` 动词移除**：调用报未知子命令并提示改用 `run`；shell 内置动词同步 `start` → `run`。
- **（BREAKING）配置字段统一为 `restart`**（取代 `autorestart` 与 `autostart`，docker 字面量按字面语义实现）：值域 `no | on-failure | always | unless-stopped`；`false`/留空（级联到底）等价 `no`；守护进程启动按策略拉起（`no` 不拉起，`always` 无条件拉起，`unless-stopped` 无「用户停止」持久化记录才拉起，`on-failure` 拉起）；`unless-stopped` 的停止记录由守护进程持久化维护（显式 stop 写入、显式启动清除、正常关机不写入）。旧字段 `autorestart`/`autostart` 作为弃用别名继续可读（映射归并，同层新旧字段同现拒绝，`autostart=true`+`autorestart=never` 组合迁移告警）；写入路径一律新字段；`config --init` 模板 `[app-default]` 显式 `restart = "always"` 保持开箱守护体验。CLI flag 面收敛：`--autostart/--no-autostart/--autorestart` 移除，统一 `--restart=<字面量>`（run ad-hoc 与 add 共用）。
- **`add --apply` 离线自动后台拉起守护进程**：替代现行「报错 + 补救提示」，与 run ad-hoc 共用同一拉起模块。`add` 其余两路径形态（目录/`.toml`、可执行文件）保持纯注册、离线可用。
- **`log` 扩展多程序与双流**：`log web api -f` 多程序聚合（多名字时 `[name] ` 前缀，单程序输出不变）、`--stream both` 合并双流。聚合在 CLI 侧并发跟随连接实现。
- **`status` 人读表格**（**BREAKING**：默认输出从 pretty JSON 改为表格，列与 shell 的 status 表格一致）：新增 `--json` 保留既有 JSON 输出，脚本迁移路径明确。
- **使用场景叙事重写**：README 快速上手第一路径改为 `xkeeper run -- <cmd>`（冷启动一条命令），注册/部署流程退为第二路径。

## Capabilities

### New Capabilities

- `background-daemon`: 守护进程后台化——`run --detach` 显式入口与「立即生效」入口（`add --apply` 与 run 的 ad-hoc 形态）的隐式拉起协议（就绪判定、竞态容忍、超时语义、日志重定向）。

### Modified Capabilities

- `control-plane`: 「CLI 控制命令」需求变化——`run` 升级为统一入口动词（裸=daemon、`<name>`/`--app`=启动、ad-hoc 形态=命令行注册生效），`start` 移除；`log` 扩展多程序聚合与 `--stream both`；`status` 默认输出改为人读表格并以 `--json` 保留原 JSON 输出。
- `app-registry`: 「注册应用（add）」与「离线可用」需求变化——命令行注册形态改经 run ad-hoc 形态进入（`--`/`--cmd` 二选一；PATH/PATHEXT 解析、缺省名取命令 stem、`--restart` 字面量直书）；脚手架与微调 flag 收敛为 `--restart`（`--autostart/--autorestart` 移除）；`--apply` 离线语义从「报错+补救」改为「自动后台拉起后生效」。
- `configuration`: 「字段优先级」需求变化（`restart` 取代 `autorestart`/`autostart`、程序级、可作应用级默认）+ 新增「restart 字段域与旧字段兼容」需求（值域、`false`/留空≡`no`、弃用别名映射、同现拒绝、模板引导）。
- `process-management`: 「程序生命周期状态机」与「重启策略」需求变化——四种 `restart` 策略的字面语义（含守护启动/apply 拉起语义）与 `unless-stopped` 的「用户停止」记录持久化契约。
- `apply-workflow`: 「apply 范围与幂等」需求变化——新注册程序按 `restart` 启动语义处理（`no` 只注册不启动）。

## Impact

- **代码**：`src/config.rs`（`restart` 字段域、宽容解析、弃用别名归并、级联）、`src/supervisor.rs`（守护启动拉起语义、unless-stopped 停止记录持久化与查询）、`src/program.rs`（四策略）、`src/registry.rs` + `src/main.rs`（脚手架/微调 flag 收敛、run 统一动词、start 移除、log/status 动词面、后台拉起模块——unix `process_group(0)` / Windows `CREATE_NEW_PROCESS_GROUP | DETACHED_PROCESS`，std 即可无新依赖）、`src/client.rs`（follow 重构为行回调）、`src/shell.rs`（start→run 改名 + 表格渲染器提为共享）。
- **模板与文档**：`conf/daemon.toml`、`conf/app.toml`、`config --init` 模板、`examples/`、README/架构图、shell 帮助文本。
- **守护进程侧有改动**（本变更已从纯 CLI 扩展为含配置模型与守护语义：restart 字段与 unless-stopped 记录）；`/v1` API 端点与投影字段不变。
- **验收**：`xtask` e2e——run 统一形态/冷启动/log 聚合/status 表格/restart 四策略与迁移场景；既有 `start`、`status` JSON、`autostart/autorestart` 相关断言全部迁移。
- **风险**：三处 BREAKING（`start` 移除、`status` 默认输出、配置字段统一——含「级联到底默认从 always 变 no」的行为翻转，靠模板与迁移告警兜底）；两条平台路径均需覆盖。
