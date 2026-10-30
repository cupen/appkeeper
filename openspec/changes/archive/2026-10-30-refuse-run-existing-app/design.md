## Context

`ak run` 的同名处理目前集中在 `src/supervisor.rs` 的 `cmd_adhoc_up` / `adhoc_upsert`，`src/server.rs` 的 `respond_adhoc_run` 再按 `unchanged` / `committed` 分叉。本变更把它改成「默认拒绝 + `--force` 显式覆盖」：

- 未带 `--force`：同名（注册记录或内存应用）→ 409 冲突，零副作用。
- 带 `--force`：渲染新配置并与磁盘现有记录逐字节比较；有差异则原子重写 + 停旧启新，无差异则完全空操作并提示「配置无修改，不做任何动作」。

残留态（`st.staged`，已启动未落盘）不属于「已注册」，保持既有确认流程。

## Goals / Non-Goals

**Goals:**

- 默认路径零副作用，保护既有（尤其运行中）的 app。
- `--force` 提供显式覆盖逃生口，且覆盖前完成校验、覆盖时原子落盘。
- 无差异时真正做到「不写盘、不停止、不重启、不拉起」，只输出提示并以 0 退出。
- `--force` 不放行跨应用程序名冲突与外部部署配置写保护。

**Non-Goals:**

- 不改变 `ak compose` 的 upsert / 全量再生语义。
- 不改变 `ak start|stop|restart|apply|remove` 的语义。
- 不改变 residual（已启动未落盘）流程；`--force` 不接管它。
- 不提供自动 stop/remove；`--force` 只覆盖同名注册，不清理其他资源。

## Decisions

### D1：权威判定在 `Command::AdhocUp` 处理器内

判定放在 `cmd_adhoc_up` 中、residual 检查之后、渲染之前。命令队列是唯一写者，判定与后续 stage/spawn 串行，消除「客户端预检通过后、apply/巡检并发改状态」的 TOCTOU。仅客户端查 `/v1/status` 再提交的方案被否决。

### D2：「已存在」的判定口径

满足任一条件即视为已存在：

- 注册记录在盘：`registry::link_path(...)` 生成的 `target` 存在；
- 内存注册表 `st.apps` 含同名 app；
- `st.programs` 含同名程序且 `def.app == name`（覆盖 daemon 尚未完成重扫的窗口）。

`st.staged` 先于该判定处理：它有内存进程但没有落盘的 `target`，走残留确认而非「已注册」。

### D3：默认拒绝返回 `AdhocUpError::Conflict`（409）

同名是状态冲突而非定义非法；复用 `Validation`（400）会让客户端误读为参数格式错误。新增 `Conflict` 变体映射 409、phase `conflict`，与 `start` 对已运行程序返回 409 的既有约定一致。CLI 的 `exit_code_of` 已把非 3 类错误映射为退出码 1。

### D4：`--force` 走独立覆盖路径，复用并收窄旧 upsert 代码

保留 `adhoc_upsert` 的骨架但只允许 `force` 调用，并重命名为 `adhoc_force`（或等价），避免默认路径误入。顺序：

1. `render_adhoc` 完整校验新定义（失败不触碰任何东西）；
2. `registry::guard_scaffold_overwrite` 校验目标确为脚手架生成文件（`--force` 不豁免）；
3. 读取现有记录文本做逐字节比较；
4. 无差异 → 直接返回 `unchanged=true`，不写盘、不动进程；
5. 有差异 → `stage_placeholder` + `commit_staged` 原子落盘，再在状态锁内 `stop()` 旧程序、`spawn()` 新程序（`register_only` 时不 spawn），最后 `sync_health_tasks()`。

选择「先落盘后停旧」与旧实现一致：若新进程启动失败，磁盘上是新定义（覆盖已生效），由用户修正或 `ak remove`；不引入半回滚状态。

### D5：CLI 与线协议同时支持 `--force`

`Cmd::Run` 增加 `--force` 布尔旗标；`--` trailing 形态由 `split_adhoc_trailing` 的 `AdhocFlags` 解析，也必须识别 `--force`（两种入口语义一致）。JSON body 增加 `"force": bool`，服务端 `AdhocSpec` 解析。帮助文案说明「同名默认拒绝，`--force` 覆盖」。

### D6：无差异的判定与呈现

比较对象是「本次渲染出的完整配置文本」与「磁盘上现有记录的全部字节」；相同才空操作。空操作结果需要让 CLI 与 `--json` 都能识别：

- `AdhocUpResult` 保留窄语义的 `unchanged: bool`（仅 `force` + 字节相同为 true）；
- 服务端回 `{ok:true, unchanged:true, pid:<现有或 null>, config_path}`，不进入启动判定；
- CLI 人类输出固定提示「配置无修改，不做任何动作」并以 0 退出；`--json` 输出该对象。

注意：空操作**不会**把「已注册但未运行」的 app 拉起来——这是用户明确要求的「不做任何动作」。

### D7：有差异的结果需要「记录已落盘」信号

覆盖路径在启动判定前已落盘，服务端不能再次 commit。`AdhocUpResult` 保留 `landed: bool`（记录已在盘上）；`respond_adhoc_run` 对 `landed` 跳过 commit，只在启动判定成功后回终端帧，失败/超时时 `stop` 新程序并回结构化错误。

### D8：保留的关卡

- 跨应用程序名冲突：`resolve_adhoc_name` 的既有检查不变，`--force` 不放行；
- 外部/手工配置：`guard_scaffold_overwrite` 在 force 路径同样执行；
- residual：`st.staged` 分支不读取 `force`，保持原语义；
- `--restart=no`：覆盖有差异时写盘并停止旧进程，不拉起新进程。

## Risks / Trade-offs

- **`--force` 有差异会打断运行中的服务**：这是显式选择；通过先校验、原子落盘降低损坏面，但停旧启新的空窗不可避免。
- **无差异空操作不拉起已停止的 app**：与「不做任何动作」一致，但可能不符合「run 应该让它跑起来」的直觉；文档与提示需明确。
- **手工编辑会被覆盖**：`--force` 的有差异路径按本次命令行全量重写，这是覆盖率语义的预期代价。
- **外部并发写入 app_dir**：判定与写盘的窗口内若有人手工创建同名文件，`target.is_file()` 与内存判定覆盖大多数情形；仍属外部绕过。
- **daemon 内存状态滞后**：`target.is_file()` 分支保证即使未重扫也能拦截或覆盖。
- **兼容性**：默认同名重跑从「静默幂等/再生」变为 409（BREAKING）；`unchanged` 字段从旧语义收窄为「force + 无差异」。

## Migration Plan

无需数据迁移。既有注册与运行中的 app 不受影响；同名重跑需加 `--force`，且无差异时不会再重启。回滚需 revert 本次提交（默认拒绝与 force 路径一并撤销）。
