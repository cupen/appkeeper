# Design: add-windows-service-registration

## Context

- `src/service.rs` 现状：Linux systemd 实现完整（unit 渲染 / `decide_write` 三态幂等 / `compute_timeout_stop_sec` 均平台无关纯函数 + 单测）；Windows 分支 `install`/`uninstall` 直接 `bail!("service registration is not supported on Windows yet")`。
- `src/main.rs` 的 daemon 唯一入口 `run_daemon`：加载配置 → 起 supervisor/控制面/webui → `ctrlc::set_handler` 里调 `sup.request_shutdown()` → `sup.run()` 阻塞至停机。这是服务停机要复用的挂接点。
- `Cargo.toml` 的 `cfg(windows)` 依赖已有 windows-sys（无 Services feature），无 windows-service。
- `windows-service` crate（mullvad）API 面已核实：install 侧 `ServiceManager`（create/open/delete service）+ `Service::query_config/change_config/get_description/set_description/get_failure_actions/set_failure_actions/start/stop/delete`；run 侧 `service_dispatcher::start` + `service_control_handler::register`。description 与 failure actions 均可读回 —— 快照比对可含描述。

## Goals / Non-Goals

**Goals:**

- Windows 上 `service install/uninstall` 完整落地：SCM 注册（auto-start + failure actions）、对称注销、幂等/`--force`、管理员门禁。
- `xkeeper run` 能以 Windows 服务方式运行：SCM 状态上报 + STOP 控制映射到现有 `request_shutdown` 优雅停机。
- CLI 新增 `--name`（仅 Windows）与 `--desc`（跨平台）且不破坏现有 flag 语义。
- xtask e2e 管理员环境完整 roundtrip；非管理员自动跳过。

**Non-Goals:**

- 不改 Linux 路径的任何行为（unit 渲染仅在 Description 行引入 `--desc` 参数，缺省值与现状一致）。
- 不支持带密码的自定义 Windows 服务账号（`--user` 仅免密内置三账号）。
- 不做服务 pause/continue、delayed auto-start、服务失败重置周期调参（reset period 固定 24h）。
- 不处理主 spec 中「自定义 unit 名与运行用户」Requirement 仍写 `--name` 的既有漂移（commit 30d9a7e 改为 `--unit-file` 后未同步）——见 Risks。

## Decisions

### D1: 技术路线 —— `windows-service` crate，不手写 SCM、不走 sc.exe

crate 同时覆盖 install 侧（`ServiceManager` + `Service` 配置/查询/删除/failure actions/description）与 run 侧（dispatcher + 控制处理器），免新增 windows-sys feature 与手写 unsafe 样板。

- 备选「手写 windows-sys SCM」：需自实现 service_main 循环、控制处理器、状态上报，unsafe 面大且已在本 crate 被高质量封装。
- 备选「sc.exe 子进程」：注册外壳而已，二进制内仍须实现 service_main（否则 SCM 按 1053 杀进程），还要解析 sc 文本输出，双份脆弱。

### D2: run 的服务入口 —— binpath 内嵌隐藏 `--service <name>` 显式分流

注册时 binpath 渲染为 `"<exe>" run --config "<abs>" --service <name>`；`--service` 为隐藏 flag（clap `hide`），run 见到它才走 `service_dispatcher::start(name, service_main)`，否则走原控制台路径。

- 备选「无标记、dispatch 失败回退控制台」：每次控制台 run 先产生一次 SCM 失败调用（ERROR_FAILED_SERVICE_CONNECTOR 1063），语义含糊且无法支持自定义服务名（dispatcher 需预知 name）。
- 显式分流与 bench 隐藏 `__generate` 自我重入先例一致；binpath 渲染收进纯函数 `render_binpath`（可单测，含空格路径整体加引号，遵循 SCM 引号规则）。

### D3: 停机集成 —— 复用 `sup.request_shutdown()`

`run_daemon` 拆分为「daemon 核心启动（返回 supervisor 等句柄）」+ 两式套壳：

- 控制台壳：保留现有 `ctrlc::set_handler → request_shutdown`。
- 服务壳（Windows）：service_main 内 `register` 控制处理器 → 上报 START_PENDING（wait hint = `compute_timeout_stop_sec(config)`）→ 跑 daemon 核心 → 上报 RUNNING → 收到 `SERVICE_CONTROL_STOP` 时调 `sup.request_shutdown()`（与 ctrlc 同一入口）→ 停机等待期间周期性上报 STOP_PENDING + 剩余 wait hint → 停机完成后上报 STOPPED。

服务模式不装 ctrlc handler（无 console，装了也不会触发，省一次无谓注册）。

### D4: 已存在语义 —— `decide_write` 的 Windows 同构快照比对

读回 SCM 配置组成快照 `Snapshot { binpath, start_type, account, failure_actions, description }`，与目标快照比对，复用现有三态 `WriteDecision { Write, SkipIdentical, RefuseNeedsForce }`：

- 一致 → `SkipIdentical`（提示已是最新；`--now` 仍 `Service::start`）。
- 不一致且无 `--force` → `RefuseNeedsForce`。
- `--force` → `change_config` + `set_failure_actions` + `set_description` 修正；服务运行中提示「更改将于下次启动生效」。

binpath 比对前经纯函数规范化（SCM 读回可能重整引号/空白）。快照读取失败（如打开被拒）按「不存在」处理路径外的问题显式报错。

### D5: CLI 结构 —— flag 两端可见，dispatch 平台检查

- `--name` / `--desc` 不在 clap 层 `cfg(windows)` 掉：跨平台误用时由 dispatch 给出「`--name` 仅支持 Windows」/「`--unit-file` 仅支持 Linux」的明确报错（clap 的 unknown-argument 报错不友好），且判定逻辑可单测。
- `--unit-file` 的 clap `default_value` 移除、改 `Option<PathBuf>`：Linux 侧 `None` → `DEFAULT_UNIT_FILE`；据此区分「显式给错平台」与「缺省」。
- `--desc` 进 `ServiceOptions`（Linux `render_unit` 增 `desc` 参数，缺省 `xkeeper process keeper`）。
- 校验纯函数（全平台可测）：`is_valid_service_name`（非空、不含 `/` `\`）、`builtin_account`（`LocalSystem`/`LocalService`/`NetworkService` 大小写不敏感 → `NT AUTHORITY\...` 规范形，其他报错）。

### D6: failure actions 参数 —— 对齐 RestartSec=3，reset 24h

`SERVICE_CONFIG_FAILURE_ACTIONS`：reset period = 86400s，actions = [Restart, delay 3000ms]。SCM 语义仅对**异常退出**触发（clean exit 0 不重启）——与 systemd `Restart=always` 的差异已写入 spec（「崩溃自动拉起」语义不变，正常停机不会被拉起）。

### D7: e2e —— xtask 直接依赖 windows-service 做断言，不解析 sc 输出

- `is_elevated()`（CheckTokenMembership，纯函数拆分 SID 比对可测）非管理员 → warn + skip，不 fail。
- 管理员 roundtrip：服务名取 `xk-e2e-<pid>`（不碰默认 `xkeeper`）：`install --name X --desc "e2e" --now` → 用 crate 读回校验 binpath/start_type/failure actions/description → 轮询至 RUNNING（`Service::start` 是异步发起）→ `uninstall --name X` → 校验服务消失 + 二次 uninstall 幂等成功。
- 校验失败路径用 `sc.exe` 兜底清理残留服务，保证 e2e 不污染机器。

## Risks / Trade-offs

- **[failure actions 仅异常退出触发]** → 已在 spec 显式注明（与 systemd `Restart=always` 的 clean-exit 差异）；属 SCM 平台语义，无法绕过。
- **[服务模式 daemon 自身 stderr（env_logger）无 console 附着，日志不可见]** → 与 Linux journal 近似但无从查看；记录为已知限制，后续 change 可考虑 config 指定 daemon 日志文件。
- **[SCM 对 binpath 读回可能重整引号/空白，导致快照误判漂移]** → D4 规范化纯函数缓解；e2e 的重复 install 用例覆盖。
- **[非管理员 CI 跑不到 Windows 服务 roundtrip]** → 纯逻辑单测保判定质量（cfg(windows) 编译），e2e 跳过打 warn；有管理员环境时全量验证。
- **[`--now` 是异步发起]** → 服务进入 RUNNING 需轮询；CLI 语义文档注明（systemctl start 亦近同步，SCM 为异步，属平台差异）。
- **[主 spec 既有漂移]**：「自定义 unit 名与运行用户」Requirement 仍描述旧 `--name`（30d9a7e 改 `--unit-file` 未同步归档）。本 change 不归口修复；归档后两者并存可能误读，建议随后用 `openspec-sync-specs` 或独立 change 修正。
- **[运行中 `--force` 改 binpath 不影响当前进程]** → 提示「下次启动生效」（与 Linux `daemon-reload` 不重启运行中服务的语义近似）。
