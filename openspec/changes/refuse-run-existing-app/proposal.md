## Why

`ak run` 对同名已注册应用采用「全量再生/upsert」：定义有变化时会先停掉旧进程再拉起新进程。对已经在运行的 app 来说，这条路径会在用户无明确意图的情况下打断服务，且与「run 是新增注册动词」的直觉相悖。需要把同名视为冲突并默认拦截，优先保护既有（尤其是正在运行的）app；同时提供一个显式的 `--force` 逃生口，让用户在知情前提下安全地覆盖。若覆盖请求的新旧配置完全一致，则不应产生任何写盘、重启或其他副作用。

## What Changes

- **BREAKING** `ak run`（ad-hoc 两种形态）在应用名已存在且**未带 `--force`** 时一律拒绝执行：不写盘、不启动新进程、MUST NOT stop/restart 既有进程；返回结构化冲突错误，CLI 非零退出。
- 新增 `ak run --force`：显式允许覆盖同名已注册应用。
  - 新渲染配置与磁盘现有记录**有差异**：原子重写记录；运行中的旧进程被停止，新定义立即生效并重新判定启动（`--restart=no` 时只重写、不拉起）。
  - 新渲染配置与磁盘现有记录**无差异**：不写盘、不重启、不停止、不拉起，仅提示「配置无修改，不做任何动作」并以 0 退出（`--json` 返回 `unchanged: true`）。
- 删除 ad-hoc 的隐式同名 upsert 路径：同名行为完全由「默认拒绝 / `--force` 覆盖」决定，不再存在「无变化静默幂等成功」的默认放行。
- 「已存在」的判定覆盖注册表记录与内存中的同名应用（含已注册但未运行的程序），在守护进程命令队列内做权威判定，避免与巡检/apply 竞态。
- `--force` 不放行其他既有关卡：跨应用程序名冲突、非脚手架/外部部署配置的写保护（`guard_scaffold_overwrite`）仍然生效。
- 保留 residual（已启动但未落盘）流程：该场景不属于「app 已存在」，仍按既有确认后仅补落盘、不重启进程的语义处理；`--force` 不改变该流程。
- 保留 `ak compose` 目录/`.toml` 来源的 upsert 语义，不受本变更影响。
- 无 `--force` 的冲突错误给出可操作指引：改用其他 `--app` 名称，或显式加 `--force`（详见下条）。`--force` 自身不自动执行 stop/remove。
- 同步更新 e2e/单元测试用例、AGENTS.md 与相关规范场景。

## Capabilities

### New Capabilities

（无）

### Modified Capabilities

- `control-plane`: 「注册并启动」端点新增 `force` 入参语义：默认对同名冲突拒绝，`force` 时按差异覆盖或幂等空操作。
- `app-registry`: 删除 run ad-hoc 来源的隐式同名全量再生，改为「默认拒绝 + `--force` 显式覆盖」；compose 的 upsert 语义保持不变。

## Impact

- `src/supervisor.rs`：`cmd_adhoc_up` 增加同名存在性预检；`--force` 分支复用「渲染 → 字节比对 → 原子写盘 → 停旧启新」流程；`AdhocUpResult` 需要表达「强制覆盖但内容无差异」。
- `src/server.rs`：按 `force`/`unchanged` 分支回传终端帧；恢复「记录已落盘、无需 commit」的路径。
- `src/main.rs`：`Run` 子命令新增 `--force`（clap 旗标与 `--` trailing 解析器都要支持），并渲染「配置无修改，不做任何动作」。
- `xtask/src/e2e.rs`：新增/改写覆盖、无差异空操作、拒绝、跨应用冲突、外部配置保护等场景。
- `AGENTS.md`：补充 `run` 同名默认拒绝与 `--force` 语义。
