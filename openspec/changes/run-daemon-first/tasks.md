## 1. 配置层默认值

- [ ] 1.1 `DaemonSettings::default()` 的 `app_dir` 由 `apps` 改为 `app.d`（`src/config.rs:508`）；单测断言缺省解析结果为 `app.d`，显式 `app_dir = "apps"` 仍覆盖
- [ ] 1.2 `startsecs` 内置默认由 `1.0` 改为 `3.0`（`src/config.rs:953`）；单测断言四层级联均未设置时 `ResolvedProgram.startsecs == 3.0`，显式声明仍覆盖
- [ ] 1.3 同步模板注释与文档：`conf/daemon.toml`、`examples/daemon.toml` 的 `app_dir` 注释改 `app.d`；README 增补升级说明（默认名变更、显式写回 `app_dir = "apps"` 的迁移方式、`startsecs` 默认变更影响）
- [ ] 1.4 `cargo run -- validate` 在示例配置下通过；`ak config --init` 产出的模板渲染 `app_dir` 为 `app.d` 且可被 `DaemonConfig` load

## 2. 落盘引擎（registry 可复用）

- [ ] 2.1 从 `add_scaffold`（`src/registry.rs:305`）提取纯渲染函数，使「给定定义 → 渲染文本」可被守护进程复用而不依赖 CLI 侧路径；单测断言渲染结果与既有脚手架输出逐字节一致
- [ ] 2.2 实现「占位 + 原子 rename」落盘原语：写入 `app_dir/<name>.toml.pending` → `fsync` → `rename` 为 `<name>.toml`；单测覆盖成功路径、占位名不以 `.toml` 结尾（扫描器 `list()` 不将其计入注册）
- [ ] 2.3 落盘原语失败语义：写占位失败立即返回错误（不启动进程）；rename 有界重试（可配置次数），重试仍失败返回「部分失败 + 目标路径」而不删除已写占位供诊断；单测以只读目录/注入 rename 失败覆盖
- [ ] 2.4 幂等：目标 `<name>.toml` 已存在且内容一致时跳过写盘并报告无变化；单测断言文件 mtime 不变、无重复写入

## 3. 守护进程：staged 应用与命令

- [ ] 3.1 `SupervisorState` 增 `staged: HashMap<String, StagedApp>`（定义、目标文件路径、渲染文本、是否已落盘）；`StagedApp` 结构与构造单测
- [ ] 3.2 `detect()`（`src/supervisor.rs:939`）合并 staged：staged app 不被 `apply_detection` 的 `keep` 集合丢弃，也不进入 `PendingDoc`；单测断言 staged 程序在连续多轮巡检后仍在 `programs` 中且 pending 为空
- [ ] 3.3 新增 `Command::AdhocUp { spec, reply }`：在守护循环内解析定义 → 校验（名称合法/非 `all`/程序名不冲突/命令可解析）→ 插入 staged + `ManagedProgram` → `spawn()` → 快速回复「已 spawn + 预估等待上限」；单测覆盖校验失败不 spawn、成功 spawn 后状态为 `Starting`
- [ ] 3.4 新增 `Command::AdhocCommit { name, reply }`：rename 占位 → 正式 `<name>.toml`、从 staged 移除、更新 `apps` 记录；单测断言 commit 后 `registry::list` 可见该 app、staged 清空
- [ ] 3.5 新增 `Command::AdhocAbort { name, reply }`：停止并移除程序、删占位、清 staged；单测断言 abort 后 `programs` 无该程序、`app_dir` 无残留文件
- [ ] 3.6 `--restart=no` 分支：无启动判定，直接「校验 → 写占位 → rename 落盘」，返回 `pid = null`；单测断言该路径不 spawn 进程且注册文件生成
- [ ] 3.7 残留态查询：`AdhocUp` 前检测「同名未注册但进程在运行 / staged 未落盘」，命中则回传残留态标识而非重新启动；单测覆盖命中与未命中两分支

## 4. 守护进程：启动判定与健康确认

- [ ] 4.1 `ManagedProgram` 增 `health_confirmed: bool`：无健康探测的程序 spawn 即置 true，有探测的在健康检查器首轮成功时置位（`src/health.rs` 回调路径）；单测覆盖两类程序
- [ ] 4.2 启动判定等待逻辑（置于 HTTP 连接线程，不阻塞守护循环）：轮询目标程序状态，`Running && health_confirmed` 判成功；进入 `fatal`/`Exited`/超时判失败；单测以 fake 时钟或短 `startsecs` 覆盖成功、startsecs 内退出、健康超时三条路径
- [ ] 4.3 超时上限按 D5 推导：无健康探测 = `startsecs`；有健康探测 = `startsecs + health_start_period + health_interval × health_retries`；单测断言两式取值
- [ ] 4.4 启动判定失败路径调用 `AdhocAbort` 清理（停进程、删占位、清 staged），并回传结构化失败（阶段 + 原因）；单测断言失败后 `app_dir` 无任何文件

## 5. 守护进程：控制面端点与投影字段

- [ ] 5.1 `ProgramInfo` 增 `config_path: Option<String>`（app 配置本体路径）与 `health_confirmed: bool`；`/v1/status`、`/v1/programs/{name}` 投影带出；单测断言字段存在且旧字段不变
- [ ] 5.2 新增 ad-hoc 注册端点（`POST /v1/run` 或等价路径，路径与请求体在实现时定）：接受临时定义，串联 3.3/4.2/4.4/3.4，成功回传 `{program, config_path, pid}`；单测覆盖成功与各失败阶段
- [ ] 5.3 该端点流式响应：`Transfer-Encoding: chunked`，按 D5 输出 NDJSON 进度帧与终帧（复用 `log --follow` 的 chunked 写法，`src/server.rs:632`）；无健康探测时仅发 `staged` + 终帧；单测以原始 TCP 读帧断言帧序与内容
- [ ] 5.4 启动端点响应增量扩展：`POST /v1/programs/{name}/start` 与 `/v1/apps/{name}/start` 在保留既有 `result` 文本的同时，加入结构化逐程序 `{name, config_path, pid}`；单测断言既有消费者读取 `result` 仍成功、新字段可用
- [ ] 5.5 鉴权与错误码：新端点遵循既有 Bearer 鉴权；校验失败 400/409、未知 app 404、daemon 侧超时 500，与既有约定一致；单测覆盖 401 与各错误码

## 6. CLI：run 四形态切到 daemon-first

- [ ] 6.1 新增 `client` 方法：调用 ad-hoc 端点并流式读取 NDJSON（逐帧回调），返回终帧结构；单测以本地假服务端覆盖帧解析与终帧提取
- [ ] 6.2 `run_adhoc`（`src/main.rs:1057`）改为：解析命令行与 flag → 解析 command 绝对路径（沿用 `registry::resolve_command`，失败退出 2）→ 调 daemon 端点；删除本地 `registry::add` 与 `print_add_result` 路径；单测断言 CLI 不再写 `app_dir`
- [ ] 6.3 `run <name>`（`start_or_hint`，`src/main.rs:1230`）与 `run --app`（`fanout_cmd`，`src/main.rs:1292`）改为从启动响应渲染 `{程序名, 配置路径, PID}`（单程序一行；扇出逐程序一行，全部成功才 0）；单测覆盖单程序与扇出渲染
- [ ] 6.4 `--json` 输出：单程序对象 / 扇出数组，字段含 `name`、`config_path`、`pid`、`status`；单测断言 JSON 结构与退出码
- [ ] 6.5 离线行为：`run` 全形态在守护不可达时输出提示并以 3 退出，MUST NOT 调 `daemonize::ensure_daemon`；单测/集成断言离线时无守护进程被派生
- [ ] 6.6 进度打印：有健康探测时逐帧打印人读进度（阶段 + 预估上限），等待期间用户可见进展；单测以假服务端发多帧断言逐行输出
- [ ] 6.7 残留态交互：收到残留态标识时提示「已启动未落盘」并询问是否重新落盘；确认 → 调 commit（复用进程、不重启），拒绝 → 非零退出不写盘；单测覆盖确认/拒绝两分支与「非 TTY 默认拒绝」
- [ ] 6.8 部分失败展示：落盘重试仍失败时以非零码退出并打印完整配置信息（app、程序名、command、args、env、work_dir、目标路径）；单测断言输出含全部字段

## 7. 规范同步与清理

- [ ] 7.1 `apply_after_add`（`src/main.rs:1320`）保留给 `add --apply`；确认 run 路径不再引用它，`add --apply` 的隐式拉起行为不变；集成断言 `add --apply` 离线仍自动拉起
- [ ] 7.2 全仓搜索并更新以 `run` 起 daemon 或依赖旧 ad-hoc 语义的调用点：`xtask` e2e/stress、`appkeeper-bench` topology、shell 提示文案；`cargo build` 通过
- [ ] 7.3 `cargo test --workspace` 在 unix 通过；Windows 目标 `cargo check`（平台差异路径）通过

## 8. 验收 e2e

- [ ] 8.1 xtask e2e 增 run-daemon-first 场景段：`run <name>` 返回 name/config/pid；`run --app` 扇出逐程序信息；`run -- --json` 结构正确；单程序 `run` 阻塞至启动成功
- [ ] 8.2 e2e 失败不留残留：命令不存在（退出 2）、`startsecs` 内退出（启动失败）、名称冲突、保留字 `all` 各场景后断言 `app_dir` 无 `.toml` 与 `.pending` 残留
- [ ] 8.3 e2e 原子落盘：ad-hoc 注册进行中触发 `ak reload`/重扫，断言该 app 不在列表与 pending 中、无半成品 `.toml`
- [ ] 8.4 e2e 离线退出 3：守护进程未运行时 `run <name>` 与 `run -- <cmd>` 均退出 3，且断言未派生守护进程、`app_dir` 不变
- [ ] 8.5 e2e 健康探测进度与判定：配置健康探测的程序，断言进度帧含预估上限、启动成功返回 PID；探测始终失败时以启动失败退出且无残留
- [ ] 8.6 e2e 部分失败与残留态：模拟落盘失败（如临时将 `app_dir` 置为只读）断言保留进程并打印配置；恢复后再次 run 触发残留态询问，确认后补落盘且 PID 不变
- [ ] 8.7 `cargo run -p xtask -- e2e` 全绿（含 playwright 浏览器段与 bench 冒烟），确认 `app_dir` 默认名变化后控制台与注册列表无回归
