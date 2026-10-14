## Why

`ak run` 的 ad-hoc 形态（`--` / `--cmd`）当前由 CLI 先在 `app_dir` 落盘注册文件，再拉起守护进程并 apply，最后才由守护进程启动程序。这个顺序把「写盘」放在「确认能启动」之前：命令解析错、启动即崩、配置非法等任何失败，注册文件都已经留在 `app_dir` 里，需要人工清理——正是「不要因为错误信息造成 app_dir 污染」要消灭的情形。同时 `run <name>` / `run --app` 只回一句文本结果，用户拿不到 PID 与配置路径，无法直接对接脚本或做后续操作。

本变更把 `run` 的所有形态统一为 **daemon-first**：CLI 只负责把意图交给守护进程，守护进程**先启动、确认启动成功、再落盘注册**，并把进程名、配置路径、PID 返回给 CLI；`run` 阻塞等待结果，失败时明确展示错误且不留下任何注册残留。

## What Changes

- **BREAKING** `ak run` 四种形态（`run <name>`、`run --app <app>`、`run -- <cmd...>`、`run --cmd "<string>"`）统一改为经控制面 API 由守护进程执行，并统一返回 `{程序名, 配置路径, PID}`（单程序一行；扇出逐程序一行）；`--json` 提供机器可读结构。
- **BREAKING** 注册落盘顺序反转：守护进程**先启动程序并确认启动成功，再写入 `app_dir`**。启动失败（命令不存在、`startsecs` 内退出、健康探测未通过、`startretries` 耗尽）时 MUST NOT 在 `app_dir` 留下任何注册文件，错误信息完整回传 CLI 展示。
- **BREAKING** `run` 的所有形态在守护进程离线时直接报错退出码 3，MUST NOT 再隐式后台拉起守护进程（`add --apply` 的隐式拉起协议保留不变）。
- 新增控制面端点承载「注册并启动」：守护进程接收临时定义 → spawn → 判定启动成功 → 原子落盘 → 回传结果；落盘采用**临时文件占位 + 原子 rename**（同目录 rename），失败有界重试，仍失败则保留运行中的进程、报部分失败并打印完整配置信息供用户手动落盘。
- 有健康探测时守护进程 SHALL 向 `run` 流式回传进度（当前阶段、预估等待上限），CLI 逐行打印以缓解等待焦虑；无健康探测时仅等待 `startsecs`。
- **BREAKING** `[daemon] app_dir` 默认值由 `apps` 改为 `app.d`（生产默认落位 `/etc/appkeeper/app.d`）。
- **BREAKING** 配置层 `startsecs` 内置默认由 `1.0` 秒改为 `3.0` 秒。
- 残留态处理：上次「启动成功但落盘失败」留下的进程，再次以相同参数 `run` 时 SHALL 提示当前状况并询问是否重新落盘。

## Capabilities

### New Capabilities

（无——「注册并启动」落在既有 control-plane / app-registry 能力域内）

### Modified Capabilities

- `control-plane`: `run` 动词面重写为 daemon-first——四形态统一经 API 执行、统一返回 `{程序名, 配置路径, PID}` 与 `--json`；新增「注册并启动」端点与其流式进度帧；离线退出码 3 不再隐式拉起。
- `app-registry`: ad-hoc 注册的执行主体由 CLI 改为守护进程；注册落盘顺序反转为「启动成功后再落盘」；新增临时文件占位 + 原子 rename 的落盘契约与部分失败语义；残留态再注册的提示与确认。
- `background-daemon`: `run` 从「立即生效入口的隐式拉起」名单中移除（离线退出 3）；`add --apply` 保留自动拉起，协议不变。
- `configuration`: `[daemon] app_dir` 默认值 `apps` → `app.d`。
- `process-management`: `startsecs` 内置默认 `1.0` → `3.0`。

## Impact

- 代码：`src/main.rs`（`run` 分派与 `run_adhoc` 改为客户端调用）、`src/registry.rs`（落盘引擎提取为守护进程可复用：渲染/占位/原子 rename/回滚）、`src/supervisor.rs`（新增「注册并启动」命令与状态流转）、`src/server.rs`（新端点 + 流式进度响应）、`src/client.rs`（新客户端方法、流式读取）、`src/config.rs`（`app_dir` 与 `startsecs` 默认值）、`src/daemonize.rs`（`run` 不再走拉起路径）。
- 测试与工具：`xtask` e2e（run 全形态、失败不留残留、离线退出 3、部分失败路径）、`appkeeper-bench`（`app_dir` 默认值变化）、main.rs / registry.rs 相关单测。
- 文档：README（run 用法、返回格式、`--json`、`app_dir` 新默认、升级说明）、AGENTS.md（架构行与子命令清单）、`conf/daemon.toml` 与 `examples/daemon.toml` 模板（`app_dir` 默认名）。
- 运维影响：`app_dir` 默认名变更属破坏性默认值变更——已装机实例若依赖默认 `apps` 目录，升级后守护进程会在新默认目录 `app.d` 下找不到既有注册，需显式在 daemon 配置写回 `app_dir = "apps"` 或迁移注册文件；升级说明写入 README。`startsecs` 默认延长会推迟所有程序的「启动成功」判定与启动失败重试节奏。
- 无新增依赖；webui 运行时行为零改动。
