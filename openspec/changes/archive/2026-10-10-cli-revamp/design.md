# Design: cli-revamp

## Context

现有 CLI 的分层是本设计的地基：顶层 `xkeeper <verb>` 面向脚本——单一规范动词、全库零别名、`status` 为 `/v1/status` 的 JSON 透传、注册动词（add/remove/list）永不要求 daemon 在线、控制动词永不写盘；`xkeeper shell` 面向人——status 表格（src/shell.rs:589）、短别名（st/?/q）、`open`。`add` 已是「注册+生效」复合动词（脚手架引擎 + `--apply`）；`log_follow` 是单连接 HTTP 流式读循环（src/client.rs:173）。配置模型现状：`autorestart`（never|on-failure|always，程序级可级联）+ `autostart`（布尔，应用级）两个字段分担「崩溃重启」与「守护启动拉起」。

## Goals / Non-Goals

**Goals:**

- 「跑」收敛到 `run` 一个动词：daemon（裸/--detach）、已注册程序（`<name>`/`--app`）、ad-hoc 命令行（`--`/`--cmd`，冷启动自动拉起 daemon 后立即生效）。
- 启动/重启策略收敛到单一 `restart` 字段，docker 字面量、按字面语义实现（含 `unless-stopped` 的「用户停止」记录持久化）。
- `run`/`stop`/`restart` 构成自然动词三元组；`add` 保持纯注册（离线可用）。
- `log` 多程序聚合 + `--stream both`；`status` 人读表格（列与 shell 一致）+ `--json` 兜底。
- unix 与 Windows 两条平台路径同等覆盖。

**Non-Goals:**

- `run` 接受路径形态（目录/.toml/可执行文件）——注册仍走 `add`（避免裸 token 的程序名/路径歧义，用户已拍板）。
- `start` 兼容别名（用户已拍板直接移除）；新增任何顶层别名。
- `/v1` API 端点、状态投影、环形缓冲的改动（含日志行时间戳）。
- 停止记录状态文件的丰富化（只存「用户停止」程序名集合，不存退出码/时间等运行历史）。
- webui、前台聚合 dev runner、`daemon.out.log` 轮转。

## Decisions

### D1: `run` 统一动词模型（用户拍板）

「跑 = run」：裸 `run`（daemon，既有）| `run <name>` / `run --app`（吸收 start）| run ad-hoc 形态（注册+生效）。docker/pm2 的先例证明单动词承载「新建并跑 / 跑已有的」是可学习的心理模型，且 `run`/`stop`/`restart` 天然成对。

备选否决：①pm2 完全体（`run <path>` 也吃路径注册）——裸 token 是程序名还是 cwd 下同名目录产生歧义，需裁决规则，且把 add 的离线注册语义搅进在线动词；②保留 `start` 为兼容别名——顶层零别名是既有惯例，同义动词长期共存比一次性 BREAKING 更贵。`start` 移除的迁移面：脚本 `xkeeper start web` → `xkeeper run web`，报错信息给出改用建议；shell 内置动词同步改名。

### D2: 命令行注册唯一入口是 run 的 ad-hoc 形态，`add` 不新增形态

run ad-hoc = 解析 → 脚手架注册 → 立即 app 范围 apply。add 保持既有两路径形态不变（纯注册、离线可用）。「只注册命令行、暂不启动」由 `--restart=no` 覆盖（注册落地不拉起）。

命令入口两种等价形态：`-- <cmd...>`（trailing）与 `--cmd "<命令行字符串>"`（shell 词法切分，适配不便传递 `--` 的调用场景，风格对齐 add 的 `--args`）。二者互斥，落盘结果完全一致。

备选否决：`add -- <cmd>` 与 run ad-hoc 并存——同一注册两个入口，且 add 的语义应是「注册」而非「跑」，apply 隐含的形态放 add 上语义错位。

### D3: `restart` 字面量模型——单字段取代 `autorestart` + `autostart`（用户拍板）

字段域 `no | on-failure | always | unless-stopped`，docker 字面量、按字面语义实现：`no` 退出不重启且守护启动不拉起；`on-failure` 非期望退出码重启、守护启动拉起；`always` 任何退出重启、守护启动无条件拉起；`unless-stopped` 任何退出重启、守护启动无「用户停止」记录才拉起。宽容解析：`false` ≡ `"no"`；四层级联到底未设置 ≡ `"no"`（即内置默认 `no`，docker 对齐）；开箱守护体验由 `config --init` 模板 `[app-default] restart = "always"` 引导。`restart` 为程序级字段，`[app]`/`[app-default]` 层可作默认（原 `autostart` 的应用级职责由级联覆盖）。CLI flag 面收敛：`--autostart/--no-autostart/--autorestart` 移除，统一 `--restart=<字面量>`（run ad-hoc 与 add 共用，直书进配置）。

备选否决：①CLI 侧组合映射（`--restart=always` → 写 `autorestart=always`+`autostart=true` 两个旧字段）——本变更前一版方案，因用户要求「按字面量的含义实现」而废；②保留 `autostart` 独立字段——`unless-stopped` 的字面语义无处安放，两字段模型与字面量值域不可调和。

### D4: `--apply` 离线自动拉起（所有 add 路径形态 + run ad-hoc 形态，用户拍板统一）

任一「立即生效」入口在 health 探测失败时自动后台拉起守护进程，就绪后继续 apply。统一而非按形态分裂，理由：`--apply` / run ad-hoc 的语义就是「我要它现在跑起来，条件不满足就创造条件」——同一意图不该有两种离线行为。备选否决：保持「报错+补救」（与目标直接冲突）；仅新形态拉起（flag 语义分裂）。

### D5: `run --detach` 显式原语 + 隐式拉起复用（用户拍板）

后台拉起沉淀为独立模块（detach spawn + 就绪轮询），两个入口：`xkeeper run --detach`（就绪后打印监听地址与日志路径返回 0，超时非零并指明日志位置）与立即生效入口内部调用。headless/容器/无 systemd 场景获得正路。备选否决：纯隐式（能力存在却不可见，发现性差）。

### D6: detach 与就绪协议——std 进程组分离 + health-only 判定

`Command::new(current_exe()).args(["run", "-c", config])`：unix `process_group(0)`、Windows `creation_flags(CREATE_NEW_PROCESS_GROUP | DETACHED_PROCESS)`，stdout/stderr 追加重定向 `log_dir/daemon.out.log`；以 ~200ms 间隔轮询 `/v1/health`，总超时 ~10s。竞态不设锁：后 bind 的实例按既有「启动冲突处理」自灭，调用方只认 health 探测。

备选否决：unix 双 fork daemonize（libc 依赖、Windows 另写一套）；pidfile/锁文件单实例裁决（新状态、跨平台，与「绑端口即单例」模型重复）。

### D7: 命令首 token 解析为绝对路径，不原样写入

含路径分隔符 → canonicalize（含存在性校验）；裸名 → unix 遍历 `PATH` 取首个 X_OK 匹配、Windows 先原样再按 `PATHEXT` 扩展。失败退出码 2，无注册产物。维持「生成配置中 `command` 为绝对路径」不变量；守护进程 spawn 环境的 `PATH` 可能与调用方不同，注册时固化才可复现。

### D8: `log` 扩展元数与双流，不动动词名

`log` 接受多个程序名：>1 名时行前缀 `[name] `，=1 名时输出与既有完全一致；`--stream both` = 同程序两条连接。follow 读取重构为行回调（`follow_lines(name, stream, on_line)`），聚合器行缓冲 + stdout 锁串化；非 follow 按参数顺序逐程序 `log_tail`。备选否决：新动词 `logs`/别名（顶层零别名惯例）；daemon 侧多路聚合端点。

### D9: `status` 表格默认，复用 shell 渲染器（用户拍板）

顶层 `status` 默认渲染与 shell status **同列同格式**的表格（NAME/APP/STATE/PID/RESTARTS/UNHEALTHY），实现上把 shell.rs 渲染器提为共享函数。`--json` 分支输出与本变更前逐字节等价的 pretty JSON。这是对既有分层的一次有意调整（顶层 status 默认受众从脚本改为人），由用户拍板。

### D10: clap 的 `run` 过载分派与互斥

`Run { name, app, detach, cmd, cmd_str }`：`cmd` 为 trailing 定位参数（`allow_hyphen_values`，`--` 引导），`cmd_str` 为 `--cmd "<字符串>"`（二者互斥，归一为同一命令行）。分派规则：命令行非空 → ad-hoc（与 `name`/`app`/`detach` 互斥；策略 flag 仅 `--restart=<no|always|unless-stopped|on-failure>`，旧 `--autostart/--autorestart` 不存在故无互斥条款）；`name` 或 `app` → 启动（互斥关系沿用原 start：name xor app）；全空 → daemon（`--detach` 仅此形态合法）。`log <names...>`（≥1）与 `status --json` 为普通扩展。

### D11: shell 同步改名（一致性收尾）

shell 内置 `start` → `run`（解析、help 文本、`-e` 单命令模式同步），无别名残留。REPL 内不需要 ad-hoc 形态与 `--detach`，shell 的 `run` 仅承接启动语义。

### D12: `unless-stopped` 的「用户停止」记录由守护进程持久化

守护进程维护一个小状态文件（`{log_dir}/state.json`，仅存「用户停止」程序名集合，原子写）：用户经控制面的显式 stop 写入记录；显式启动清除；正常关机（shutdown 停全部）不写入——docker 对齐：守护关机不算用户停止，重启后 `unless-stopped` 程序照常拉起。守护启动时对 `unless-stopped` 程序查询记录决定拉起与否；`always` 不受记录影响（手动停止仅在守护不重启期间生效）。

分层说明：CLI 控制动词依旧永不写盘（`stop` 经 HTTP 下发，落盘者是守护进程自身的状态文件）——与既有分层不冲突，但这是守护侧新增的持久化状态，本变更因此从纯 CLI 扩展为含守护语义（用户以「按字面量含义实现」授权）。

备选否决：①`stop` 由 CLI 直改生成配置的 `restart` 字段——控制动词写盘，破坏分层且污染部署事实；②不持久化、`unless-stopped` 退化为「守护重启后一律不拉起」（本变更前一版方案）——与字面语义不符，被用户否决。

### D13: 旧字段兼容——弃用别名归并读取，写入一律新字段

`autorestart`（never→no，on-failure/always 原值）与 `autostart`（布尔）继续可读，归并规则：`autostart = false` → `no`；`autostart = true`/缺省 + `autorestart = X` → X；`autostart = true` + `autorestart = "never"` → `no` 并输出迁移警告（「守护启动拉起 + 不重启」组合在新模型不存在）。同层新旧字段同现 → 校验拒绝（歧义）。写入路径（add 微调、run ad-hoc、脚手架、`config --init` 与 conf/ 模板）只写 `restart`。

备选否决：硬切不兼容（现存全部 app 配置立即失效，迁移成本全压用户）；长期双字段并存（值域重叠、语义分裂，正是本次要消除的）。

## Risks / Trade-offs

- [`start` 移除破坏既有脚本] → 报错信息直接给出 `run` 替代写法；e2e/README 全量迁移；xkeeper 未到 1.0，仓库近期已有同类 BREAKING 先例（`--name` → `--unit-file`）。
- [status 默认输出变更破坏既有脚本] → `--json` 逐字节保留旧输出；e2e 与 README 显式迁移提示。
- [级联默认从 always 翻转为 no] → 现存未设 `[app-default]` 的部署行为变化（崩溃不再自动拉起）；靠弃用别名兼容（旧 autorestart=always 读作 restart=always）+ `config --init` 模板显式 always + README 变更说明高亮。仅在「既无旧字段又无 app-default」的部署上发生翻转。
- [迁移歧义组合（autostart=true + autorestart=never）] → 归并为 no 并告警，文档说明新模型无此组合。
- [`run` 一词三义可能误触] → `--`/`--cmd` 强制分隔命令行（二者互斥）；裸 token 只可能是程序名（找不到即 404 + 提示 ad-hoc 形态）；`--detach` 仅 daemon 形态合法。
- [状态文件损坏/丢失] → 记录仅影响 `unless-stopped` 的守护启动拉起，丢失的最坏后果是多拉起一次（用户再 stop 即可）；JSON 解析失败按空集合处理并告警。
- [后台守护进程成为无主进程（非 systemd 场景）] → 语义即全局单例：`xkeeper shutdown` 统一管理；README 写明。
- [`daemon.out.log` 无轮转无限增长] → Non-Goal，后续变更处理；V1 追加写。
- [PATH 解析与守护进程环境不一致] → 注册时固化绝对路径是既有语义，属可复现性特性，文档说明。
- [并发拉起瞬间两实例竞争] → 既有端口冲突快速失败 + health-only 判定（D6）。
- [聚合 follow 的行交错撕裂] → 行缓冲 + 单一 stdout 锁（D8）。
- [Windows `PATHEXT`/分离标志差异] → tasks 单列平台验证项。

## Migration Plan

无数据迁移脚本。三处 BREAKING 的迁移路径：`start <name>` → `run <name>`（报错信息自带建议）；`status` 脚本消费 → `--json`；配置字段——旧 `autorestart`/`autostart` 继续可读（弃用别名，D13），用户可渐进改写为新字段，模板与文档引导。曾依赖 `add --apply` 离线报错的自动化需意识到冷启动现在会真的拉起守护进程。回滚 = revert 提交（状态文件为新增产物，回滚后残留无害）。

## Open Questions

- `daemon.out.log` 是否复用日志轮转——可后续变更，不改本次契约。
- `log --stream both` 非 follow 形态下两流 tail 的精确排序——实现细节，行为级只要求两流可见。
- 弃用别名的移除时间表（何时起校验拒绝旧字段）——依发布节奏另定，本次只保证「读兼容 + 写新字段」。
