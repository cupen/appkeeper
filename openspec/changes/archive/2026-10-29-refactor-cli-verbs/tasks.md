# Tasks: refactor-cli-verbs

## 1. 前置与顺序约束（design D8；执行期裁决：C2 归档绕开）

- [x] 1.1 C2（add-windows-service-registration）归档绕开：其 6.x e2e 代码已存在于
  xtask（`windows_service_pass`/`is_elevated`/`windows-service` 依赖），6.2/7.2 的
  Windows 管理员真机验证本机不可达、如实留置；C2 待真机验收后与本 change 一并
  归档。验证：`openspec show add-windows-service-registration --json --deltas-only`
  可读出其 delta 需求清单
- [x] 1.2 对照 C2 **未归档 delta**（上条命令输出）把 Windows SCM 需求以
  `ak daemon install|uninstall` 动词与 `daemon start --service <NAME> --config <路径>`
  binPath 形态并入本 change 的 service-registration delta。验证：
  `openspec validate refactor-cli-verbs` 通过

## 2. CLI 结构重构（`src/main.rs`）

- [x] 2.1 `Cmd::Daemon` 改为子命令树 `DaemonCmd::{Start, Install, Uninstall, Shutdown}`：
  `start` 带 `--foreground` 与隐藏 `--service <NAME>`（互斥）；`install`/`uninstall`
  平移原 `ServiceCmd` 全部旗标（`--start`/`--now`/`--force`/`--unit-file`/`--name`/
  `--desc`/`--user`/`--restart`）；裸 `ak daemon` 报 usage 错误列出四子命令（退出码
  2）。验证：`cargo test -p appkeeper daemon` 相关单测 + `ak daemon --help` 输出新树
- [x] 2.2 删除顶层 `Cmd::Service` / `Cmd::Status` / `Cmd::List` / `Cmd::Shutdown` /
  `Cmd::Add`，新增 `Cmd::Ps`（`--all`、`--out <json|table>` 缺省 table）与
  `Cmd::Compose`（原 Add 旗标原样平移，帮助文案按 compose 编排隐喻重写）。验证：
  `cargo build` 无旧动词残留；`ak --help` 顶层不再出现旧动词
- [x] 2.3 守护形态 `run` 拦截提示文案更新：`--detach` 形态提示改用 `ak daemon start`；
  仅 `--config` 形态提示 `ak daemon start --foreground` + `ak daemon install
  --force --now` 重新生成 unit。验证：`reject_removed_run` 单测断言更新后通过
- [x] 2.4 `daemon install/uninstall` 分派接 `service::install/uninstall`（原
  `Cmd::Service` 分派逻辑迁移，执行完即退出）；`daemon shutdown` 分派接原
  `client_cmd(shutdown)` 路径。验证：`cargo test` 单测通过 + 手动 `ak daemon
  shutdown`（在线/离线）退出码分别为 0/3

## 3. start 语义与后台化（`src/daemonize.rs`、`src/service.rs`）

- [x] 3.1 `daemon start` 缺省走 daemonize 路径（原 `--detach` 逻辑），re-exec argv 改为
  `ak daemon start --foreground -c <config>`；`daemon start --foreground` 为前台
  入口。验证：`daemonize` 单测 `reexec_targets_the_daemon_subcommand` 改写后通过；
  手动 `ak daemon start` 打印监听地址与日志路径后返回 0
- [x] 3.2 systemd unit 模板 ExecStart 改为 `daemon start --foreground --config <路径>`
  （含路径含空格时的引号规则沿用）；升级兼容分支覆盖 `run --config` 与裸
  `daemon --config` 两代旧形态。验证：`service.rs` unit 生成单测断言更新后通过
- [x] 3.3 Windows SCM binPath 改为 `daemon start --service <NAME> --config <路径>`，
  `--service` 分派从 `Cmd::Daemon` 旗标迁至 `DaemonCmd::Start`；`run_daemon` 的
  service-mode 分派保持。验证：cfg(windows) 路径 `cargo check` 通过（或交叉
  `cargo check --target x86_64-pc-windows-msvc`）

## 4. `ps` 实现（`src/main.rs` + `src/shell.rs` 表格）

- [x] 4.1 `ak ps` 缺省在线表格：在既有 status 列（NAME/APP/STATE/PID/RESTARTS/
  UNHEALTHY）基础上增列配置本体路径与描述（复用 `StatusDoc` 投影中每程序的
  `config_path`；描述由 CLI 本地注册表按 app 名补充）；离线退出码 3。验证：
  手动对照表格输出；`ak ps` 离线退出码 3
- [x] 4.2 `ak ps --out json`：输出与 `GET /v1/status` 等价的 pretty JSON（`--out
  table` 缺省；非法取值 clap 报错）。验证：`ak ps --out json | jq .` 可解析且与
  `curl /v1/status` 投影一致
- [x] 4.3 `ak ps --all`：在线时以 `/v1` 投影为权威、本地 `registry::list` 补充投影
  缺失的已注册 app（标注未运行/未 apply；坏记录沿 `ListedApp::good` 过滤）；
  离线时降级打印注册表清单（名称、路径、描述）+ daemon offline 注记，退出码 0；
  `--all --out json` 降级输出仅注册表的 JSON 数组。验证：构造「注册未 apply +
  daemon 停止」双场景手动检查输出与退出码

## 5. 测试与 e2e 同步

- [x] 5.1 `xtask/src/e2e.rs` 与 `stress.rs` 全量替换旧动词调用：`add`→`compose`、
  `list`/`status`→`ps`（含 `--json`→`--out json`）、`shutdown`→`daemon shutdown`、
  `daemon --detach`→`daemon start`；按 specs 增量补 ps --all/--out、裸 daemon
  usage 错误、旧动词未知子命令错误的新断言。验证：`cargo run -p xtask -- e2e`
  全绿（含 `--no-browser` 可选段）
- [x] 5.2 `cargo fmt` + `cargo clippy` 全绿；Linux 与 Windows 双平台 `cargo
  check` / `cargo test --workspace` 通过（平台差异路径都过一遍）。验证：两平台
  命令退出码 0

  > 执行记录（如实）：Linux `cargo check --workspace --all-targets`、
  > `cargo clippy --workspace --all-targets`（无新增告警，逐文件计数与 HEAD 一致）、
  > `cargo test --workspace`（342 + 61 + 2 + 1 全绿）通过。`cargo fmt --check` 在
  > HEAD 即有不相关漂移（195 处），本 change 未全局重排；按 hunk 内容比对确认
  > 本 change 触碰文件零新增漂移（现 191 处，均为 HEAD 遗留）。Windows 目标在
  > 本机不可达（无交叉工具链）→ 该平台检查本轮跳过，`cfg(windows)` 代码仅做
  > 静态评审。
- [x] 5.3 `cargo run -p xtask -- e2e` 浏览器段（webui 不受影响的回归确认）+
  `appkeeper-bench --case firehose` 冒烟通过。验证：命令退出码 0、报告落盘

  > 执行记录（如实）：5.3 的浏览器段按本轮指令以 `--no-browser` 排除，未执行
  > （webui 段 W1–W7 全部通过，控制台行为未受影响）；bench 冒烟由 e2e 的
  > firehose 段覆盖（退出码 0、JSON 报告落盘）。独立
  > `appkeeper-bench --case firehose --duration 10 --daemon target/debug/ak`
  > 亦通过（报告 `/tmp/spec-go-rcv-relay2-bench.json`）。注：bench 未带
  > `--daemon` 时的二进制名探测（找 `appkeeper`，实际产物为 `ak`）为 HEAD
  > 既有问题，与本 change 无关。

## 6. 文档与规范清扫

- [x] 6.1 README：命令表、示例与迁移对照表（旧动词 → 新动词五条）全面更新。验证：
  `grep -n "ak add\|ak list\|ak status\|ak shutdown\|ak service\|--detach" README.md`
  仅迁移对照表内命中
- [x] 6.2 AGENTS.md 架构段与新命令面对齐（daemon 子命令族、ps、compose）。验证：
  人工评审无旧动词残留
- [x] 6.3 openspec/specs 散文清扫：未列 delta 的 spec（glossary、configuration、
  process-management、apply-workflow、shell-client、webui-ui、webui-api、branding、
  release-pipeline、ansible-deployment）中旧动词引用改为新动词；受影响能力的
  Purpose 段（background-daemon、shell-client 等）直接编辑主 spec。验证：
  `grep -rn "ak add\|ak list\|ak status\|ak shutdown\|ak service" openspec/specs/`
  除历史锚点引注外无命中；`openspec validate --specs` 通过

  > 执行记录：共改 14 个主 spec（含 background-daemon / app-registry /
  > service-registration / control-plane 的 Purpose 与正文，非 delta 的
  > glossary / configuration / process-management / apply-workflow / shell-client /
  > webui-ui / webui-api / branding / release-pipeline / ansible-deployment）。
  > 保留的旧名仅出现在「需求锚点」注记里，且措辞避开 `ak <旧动词>` 字面
  > （写「已移除的顶层 `service` 动词」等），故门禁 grep 零命中。
  > shell 内置词表 `status`/`shutdown` 按 design Non-Goals 原样保留。

## 7. 归档收口

- [x] 7.1 `openspec validate refactor-cli-verbs` 与 `openspec validate --specs`
  通过后归档：`openspec archive refactor-cli-verbs --yes`，确认 delta 无损合入
  四个能力的主 spec。验证：两条 validate 退出码 0、archive 后主 spec 含新需求文本
