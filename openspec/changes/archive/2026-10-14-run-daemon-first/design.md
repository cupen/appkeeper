## Context

`ak run` 的 ad-hoc 形态（`--`/`--cmd`）当前由 CLI 侧 `run_adhoc()`（`src/main.rs:1057`）先调 `registry::add()` 把注册文件写进 `app_dir`，再 `apply_after_add()`（`src/main.rs:1320`）拉起守护进程并 apply，最后才由守护进程启动程序。注册先于启动，失败即污染 `app_dir`。

守护进程侧的关键约束（已核实）：

- 程序只经「扫描 `app_dir` → `registry::reload_app` → 构造 `ManagedProgram`」进入 `SupervisorState.programs`（`src/supervisor.rs:540`、`src/program.rs:49`）。没有「临时定义」概念。
- 命令循环 `Supervisor::execute`（`src/supervisor.rs:711`）**单线程同步**执行：`cmd_start`（`:748`）只 `spawn()` 并把状态置 `Starting` 就返回；到 `Running` 的跃迁发生在之后的巡检 tick（`src/program.rs:233`，判据 `elapsed >= startsecs`）。因此「等待启动成功」不能写在命令处理器里，否则阻塞整个守护循环。
- 扫描器只认 `extension == "toml"` 的条目（`src/registry.rs:60`），非 `.toml` 文件天然对注册扫描不可见。
- `apply_detection`（`src/supervisor.rs:1171`）会**丢弃**其 app 不在本轮检出 `records` 中的程序（`keep` 集合）。任何未持久化的内存程序若不在检出结果里，下一轮巡检就会被清掉。
- 控制面服务端为每连接起独立线程（`src/server.rs:186`），且 `log --follow` 已在用 chunked 流式响应（`src/server.rs:632`）——流式进度无需新基建。
- `ProgramInfo`（`src/server.rs:35`）当前含 `app`/`name`/`state`/`pid`，**不含**配置路径。

## Goals / Non-Goals

**Goals:**
- 让「启动成功」成为注册落盘的前置条件，且任何失败路径都不在 `app_dir` 留残留。
- 让 `run` 四形态统一经守护进程执行，统一返回 `{程序名, 配置路径, PID}`。
- 在不阻塞守护循环的前提下，向 `run` 提供可感知的等待进度。

**Non-Goals:**
- 不改 `add`（非 run 路径）的注册语义与离线直写路径；`add --apply` 的隐式拉起协议保留。
- 不引入持久化的 job/任务表；进度只在单次请求的连接生命周期内存活。
- 不改 webui 运行时行为（`app_dir` 默认值变化对其透明）。

## Decisions

### D1. 引入「暂存应用」（staged app）作为启动与落盘之间的载体

新增 `SupervisorState.staged: HashMap<String, StagedApp>`，承载「已启动但尚未落盘」的 app（定义 + 目标文件路径 + 渲染文本）。`detect()` 把 staged 与磁盘检出**合并**：staged app 既不被当作未注册而丢弃，也不进入 pending 预览。程序因此能在 `startsecs`/健康窗口内稳定存在于 `programs` 中，不被巡检清掉。

**备选**：先写真实 `<name>.toml` 再启动（回到现状的污染问题）；或写 `.toml` 后缀的临时名让扫描器发现（`.pending-web.toml` 的 stem 非法名，扫描直接报错）。均否。

### D2. 用「非 `.toml` 占位文件 + 原子 rename」实现落盘

守护进程先渲染配置写入 `app_dir/<name>.toml.pending`（后缀 `pending`，扫描器不可见），证明写权限与磁盘可用；启动判定成功后再同目录 `rename` 为 `<name>.toml`。同目录 rename 原子，扫描器任一时刻看到的要么不存在、要么完整。失败（启动判定未过）删除占位；写占位失败则在**启动之前**就报错返回，不启动任何进程。

**备选**：直接写目标文件失败即删（spawn/健康窗口内文件已是正式注册，守护进程被 kill 会留下未确认注册，且并发 reload 会提前扫到）。否。

### D3. 守护循环只做「挂起并 spawn」，等待发生在 HTTP 连接线程

新增命令 `Command::AdhocUp { spec, reply }`：在守护循环内**同步且快速**地完成「解析定义 → 插入 staged 记录 + `ManagedProgram` → `spawn()` → 回复 spawned」。真正的等待由发起请求的 HTTP 连接线程做：轮询 `SupervisorState.programs[name]` 的状态与健康确认信号，直到 `Running`（含健康确认）或终态失败。这样守护循环永不被长等待阻塞。

配套命令：`Command::AdhocCommit { name }`（rename 占位 → 正式文件，清 staged 标记）与 `Command::AdhocAbort { name }`（停止并移除程序、删占位、清 staged）。

**备选**：在 `cmd_start` 内忙等 `startsecs`（阻塞整个守护循环，所有程序停摆）。否。

### D4. 落盘失败的可恢复路径：有界重试 → 部分失败 → 残留态询问

- rename/写盘做**有界重试**（应对瞬时 IO）。
- 重试仍失败：**保留运行中的进程**（不 abort），回传部分失败结果，附完整配置信息（app 名、程序名、command、args、env、work_dir、目标路径）供用户手动落盘。此时该 app 处于 staged 状态。
- 残留态再次 run：`AdhocUp` 前先查同名 staged/未注册在跑程序；命中则回传「已启动未落盘」状况，CLI 提示并询问是否重新落盘。确认 → 仅做 commit（复用现有进程，不重启）；拒绝 → 非零退出、不写盘。

### D5. 用流式 NDJSON 单请求承载进度

新端点对 ad-hoc 注册返回 `Transfer-Encoding: chunked`，逐帧输出 JSON 行：

```
{"phase":"staged","estimate_secs":3}        # 已 spawn，预估等待上限
{"phase":"waiting_startsecs"}               # 等待存活满 startsecs
{"phase":"waiting_health","estimate_secs":35}  # 有健康探测，等待首轮通过
{"ok":true,"program":"web","config_path":"/etc/appkeeper/app.d/web.toml","pid":1234}
```

无健康探测时只发 `staged` + 终帧。预估上限 = `startsecs`（无健康探测）或 `startsecs + health_start_period + health_interval × health_retries`。CLI 逐帧打印人读进度，最终帧决定退出码。复用 `log --follow` 已有的 chunked 写法。

**备选**：两阶段 job id + 轮询（引入 jobs 概念与状态清理，重）；CLI 本地估算（`run <name>` 需自行读盘解析健康配置，且与服务端真值可能漂移）。否。

### D6. 健康确认需要显式的可观测信号

`ProgramInfo` 的 `unhealthy` 初值为 false，无法区分「尚未探测」与「探测通过」。新增每程序标志 `health_confirmed: bool`（健康检查器首轮成功时置位；无健康探测的程序视为立即 confirmed），供 HTTP 线程判据 `Running && health_confirmed`。该字段随状态投影暴露，可空/向后兼容。

### D7. `run <name>` / `run --app` 复用现有启动端点，返回值做增量扩展

- `POST /v1/programs/{name}/start` 与 `POST /v1/apps/{name}/start` 的响应**增量**加入结构化逐程序信息 `{name, config_path, pid}`，保留既有 `result` 文本字段不变（既有消费者与场景不受影响）。
- `ProgramInfo` 增加 `config_path`（app 配置本体路径）与 D6 的 `health_confirmed`；均为新增字段，向后兼容。
- CLI 直接从启动响应取 `{程序名, 配置路径, PID}` 渲染；`--json` 输出同一结构。

**备选**：CLI 启动后再 `GET /v1/programs/{name}` 二次查询（多一次往返、存在 TOCTOU，且扇出要 N 次查询）。否。

### D8. `--restart=no` 的 ad-hoc 无「启动成功」可确认，直接落盘

`--restart=no` 语义为只注册不拉起。此时无启动判定，流程退化为「校验 → 写占位 → rename 落盘」，返回 `pid = null`。进度流只发终帧。保持既有场景「命令行形态只注册不启动」。

### D9. `app_dir` 默认名与 `startsecs` 默认值

`DaemonSettings::default()` 的 `app_dir` 由 `apps` 改为 `app.d`（`src/config.rs:508`）；`startsecs` 内置默认由 `1.0` 改为 `3.0`（`src/config.rs:953`）。二者均为配置层默认值变更，显式配置仍完全覆盖。同步 `conf/daemon.toml`、`examples/daemon.toml` 模板注释。

## Risks / Trade-offs

- [已装机实例依赖默认 `apps` 目录] → 升级后守护进程在新默认 `app.d` 下找不到既有注册，表现为「程序全没了」。缓解：README 升级说明明确要求显式写 `app_dir = "apps"` 或迁移目录；发布说明单列。
- [`startsecs` 默认 1→3 延长全局启动判定窗口] → 所有程序的启动失败重试节奏变慢、`run` 等待更久。缓解：这是用户明确要求；模板注释标注可调，文档说明影响面。
- [staged 应用在守护进程崩溃时丢失] → 进程被守护进程一并回收（Job Object/进程组），无孤儿；但已启动未落盘的信息丢失，用户需重跑 `run`。可接受：与「进程随守护进程退出」的既有契约一致。
- [检测与 staged 合并引入竞态] → `detect()` 与命令执行都在守护循环单线程内，天然串行；staged 的增删只经命令队列，无并发写。
- [健康确认标志影响既有投影] → 纯新增字段，旧消费者忽略即可；`unhealthy` 语义不变。
- [部分失败留下 staged 运行态] → 用户可能忘记补落盘；通过再次 run 的残留态询问收敛，且 `ak status` 可见该程序在运行。

## Migration Plan

1. 配置层默认值变更（`app_dir`、`startsecs`）先行落地并更新模板与 README 升级说明。
2. 守护进程侧新增 staged 机制与命令、`config_path`/`health_confirmed` 投影字段、ad-hoc 端点（含流式）。
3. CLI 侧 `run` 四形态切到新路径，删除 `run_adhoc` 的本地落盘与 `apply_after_add` 在 run 路径的调用（`add --apply` 保留）。
4. 回滚：本变更不改变磁盘格式；回滚二进制后 `app.d` 内的注册文件仍是标准 `<name>.toml`，旧版本以显式 `app_dir` 指向即可继续使用。

## Open Questions

- 残留态询问的交互形式（TTY 确认提示 vs `--yes` 直落盘）在非交互场景下的默认行为，可在实现时按「非 TTY 默认拒绝」定夺，不影响规范。
- 进度帧的具体措辞与刷新频率可在实现时打磨，不影响规范契约（规范只要求含阶段与预估上限）。
