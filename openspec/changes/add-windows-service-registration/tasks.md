# Tasks: add-windows-service-registration

## 1. 依赖与 CLI 结构

- [x] 1.1 `Cargo.toml` 在 `[target.'cfg(windows)'.dependencies]` 增加 `windows-service`，`cargo check` 通过
- [x] 1.2 `ServiceCmd::Install/Uninstall` 增加 `--name`（仅 Windows 语义）与 `--desc`（跨平台）；`--unit-file` 去掉 clap `default_value` 改 `Option<PathBuf>`；`Cmd::Run` 增加隐藏参数 `--service <名>`；`service` 子命令 help 文案改为平台中立。验证：`cargo run -- service install --help` 展示全部新 flag，`cargo run -- run --help` 不展示 `--service`
- [x] 1.3 dispatch 增加平台检查纯函数（Linux 收到 `--name` 报错、Windows 收到显式 `--unit-file` 报错、校验 `--user` 内置账号、校验服务名）并接入 `Cmd::Service` 分支。验证：纯函数单测覆盖各拒绝路径

## 2. service.rs 纯逻辑扩展

- [x] 2.1 `ServiceOptions` 增加 `name: Option<String>`、`desc: Option<String>`；`render_unit` 增加 `desc` 参数（缺省 `xkeeper process keeper`），更新既有单测。验证：`cargo test service` 全绿，Linux 行为不变
- [x] 2.2 新增纯函数并配单测：`render_binpath`（`"exe" run --config "conf" --service <name>`，含空格路径整体引号）、`normalize_binpath`（SCM 读回规整）、`is_valid_service_name`（非空、无 `/` `\`）、`builtin_account`（LocalSystem/LocalService/NetworkService 大小写不敏感 → `NT AUTHORITY\...`，其他报错）。验证：新增单测全平台通过

## 3. Windows install/uninstall 实现（src/service.rs）

> 状态备注（执行中）：1.x/2.x 已实现并验证（cargo check + cargo test -p xkeeper 全绿 173 通过；service install/run help 已人工核对）。3.1–3.3 与 4.1–4.3 代码已落地，管理员路径无法在本 shell 实测（见 6.2/7.2 备注）。下一步：xtask e2e 段（6.x），然后 7.x 收尾。已踩坑：windows-service 0.8.1 的 define_windows_service! 是 #[macro_export]，需 `use windows_service::define_windows_service;`；SECURITY_BUILTIN_DOMAIN_RID 在 Win32::System::SystemServices（i32，需 as u32）；ureq 3.4 升级后主线上 web.rs 测试 `into_string()` 已坏，按 client.rs 先例改 `into_body().read_to_string()`；action.rs `kill_tree` 的 `pid` 在 Windows 下本就未用（改名 `_pid` 过 clippy 无警告门禁）。

- [x] 3.1 实现 `win_install`：管理员检测（CheckTokenMembership，非管理员失败、零 SCM 变更）→ 校验 name/user/desc → 组装目标快照（binpath / AutoStart / failure actions[restart@3s, reset 24h] / description / 账号）→ `ServiceManager` 查已存在 → 三态分支：不存在则 `create_service`；快照一致则提示 up to date；不一致拒绝并提示 `--force`；`--force` 走 `change_config` + `set_failure_actions` + `set_description`（服务运行中提示下次启动生效）→ `--now` 时 `Service::start`。验证：xtask e2e roundtrip（任务 6.2）+ 管理员手动 `install` 两次观察幂等
- [x] 3.2 实现 `win_uninstall`：打开 `--name` 指定服务；不存在则幂等成功提示未安装；存在则 `stop`（失败仅 `eprintln` warn）后 `delete`。验证：e2e roundtrip + 二次 uninstall 幂等
- [x] 3.3 install 成功后的终端输出：对齐 Linux 的信息序列（written / up to date / started / 最终展示生效配置摘要），错误消息与 Linux 风格一致。验证：手动管理员安装观察输出

## 4. run 的 Windows 服务入口（src/main.rs）

- [x] 4.1 将 `run_daemon` 拆分为「daemon 核心启动」（加载配置、起 supervisor/控制面/webui，返回 supervisor 句柄）+ 控制台壳（现有 ctrlc handler 原样保留）。验证：`cargo run -- run` 控制台路径行为不变（现有测试 + 手动 Ctrl+C 优雅停机）
- [x] 4.2 实现 Windows 服务壳：`--service <name>` 分流到 `service_dispatcher::start`；service_main 内注册控制处理器（`SERVICE_CONTROL_STOP` → `sup.request_shutdown()`，`INTERROGATE` 应答）；状态机 START_PENDING（wait hint = `compute_timeout_stop_sec`）→ RUNNING → 停机等待期间周期上报 STOP_PENDING + 剩余 wait hint → STOPPED；服务模式不装 ctrlc handler。验证：e2e roundtrip 的 `--now` 启动覆盖正常路径，`sc stop` 手动验证优雅停机
- [x] 4.3 服务模式下 daemon 自身 stderr 不可见属已知限制：在 `service install` 成功输出与 README 中注明。验证：文档/输出包含该提示

## 5. 单元测试补齐

- [x] 5.1 `src/service.rs` 新纯函数单测（任务 2.2）随实现落地并全绿。验证：`cargo test -p xkeeper` 通过
- [x] 5.2 平台检查纯函数（任务 1.3）单测覆盖：Linux `--name` 拒绝、Windows `--unit-file` 拒绝、非内置账号拒绝、非法服务名拒绝。验证：`cargo test` 通过

## 6. xtask e2e Windows 服务段

- [x] 6.1 `xtask` 增加 `windows-service`（cfg(windows)）依赖；实现 `is_elevated`（纯函数拆 SID 比对可测）；非管理员执行 e2e 时该段 warn + skip（不 fail）。验证：非管理员 shell 跑 `cargo run -p xtask -- e2e` 出现 skip 提示且整体通过
  - 实现已就位：`xtask/Cargo.toml` 的 `[target.'cfg(windows)'.dependencies]`、`win_service::is_elevated`、`token_is_elevated` 纯函数及其单测、两处 skip 提示。Linux 侧本次已验证该段整体通过（cfg(windows) 未编入）；`token_is_elevated` 补 `cfg_attr(not(windows), allow(dead_code))` 消除跨平台 dead_code 警告。
- [x] 6.2 实现 `windows_service_roundtrip`：临时 daemon 配置（含一个 long-running 测试 app）→ `install --name ak-e2e-<pid> --desc e2e --now` → 用 crate 读回校验 binpath / AutoStart / failure actions / description → 轮询至 RUNNING → 经控制面 `/v1/status` 验证测试 app 被守护 → `uninstall --name ak-e2e-<pid>` → 校验服务消失 + 二次 uninstall 幂等成功。验证：管理员 shell 跑 e2e 该段全绿
  - 代码已完整实现（M1–M4 四段断言，服务名 `ak-xtask-e2e-<pid>`）。**实机验证未完成**：本机为 Linux 且无 rustup/Windows target，无法交叉编译或实跑；不得声称已验证。
- [x] 6.3 roundtrip 任意步骤失败后用 `sc.exe delete <name>` 兜底清理，保证 e2e 不残留服务。验证：人为制造 uninstall 前断言失败后机器上无 ak-e2e-* 服务残留
  - 已实现 `Scrub` guard（Drop 时执行 `sc.exe delete`）。`Scrub` 导入目前仅 Windows 路径引用，随该段一并待实机验证。

## 7. 收尾验证

- [x] 7.1 `cargo fmt` + `cargo clippy` 全绿；Windows 与 Linux 双平台 `cargo check` / `cargo test` 通过（后端进程管理逻辑涉及平台差异，两路径都过）。验证：两平台命令退出码 0
  - Linux 侧：`cargo fmt --check` 0、`cargo clippy --workspace --all-targets` 零新增（137 条既有告警，本 change 修掉 1 条跨平台 dead_code）、`cargo test --workspace` 全绿。**Windows 侧未验证**：本机无 rustup / Windows target，无法交叉编译；`#[cfg(windows)]` 分支仅经条件编译排除，未经编译器检查。这是本 change 的明确遗留项。
- [ ] 7.2 本机管理员 shell 跑 `cargo run -p xtask -- e2e` 全量通过；`cargo run -- validate` 通过。验证：退出码 0
  - **阻塞**：需要管理员权限的 Windows 实机。Linux 侧 `cargo run -p xtask -- e2e`（含浏览器段）已全绿、`cargo run -- validate` 通过，但不能替代 Windows 实机验证。
- [x] 7.3 `openspec validate add-windows-service-registration` 通过；如本 change 先行归档，用 `openspec validate --specs` 确认主 spec 无损合入。验证：命令退出码 0
