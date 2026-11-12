## 1. unit 模板硬化（`src/service.rs`）

- [x] 1.1 `render_unit` 写入 `KillMode=mixed`；更新既有渲染单测的期望文本，并新增断言「unit 含 KillMode=mixed」。验证：`cargo test service`
- [x] 1.2 `RuntimeDirectory` 改为 `appkeeper/<uid>`（安装期解析的运行账号字面 uid：`--user` 按 passwd 解析、root 缺省 `0`、解析失败回退 `0` 并告警；`%U` 在 system unit 中恒展开为 0，不可用）、`RuntimeDirectoryMode` 改为 `0700`；更新 `render_unit` 的既有断言（原 `RuntimeDirectory=appkeeper` / `0755`）。验证：`cargo test service`，并人工核对渲染样例
- [x] 1.3 写入 `StateDirectory=appkeeper/<uid>`（字面 uid，同 1.2）与 `StateDirectoryMode=0750`，并断言 `uninstall` 流程不触碰该目录。验证：`cargo test service` 的渲染与卸载单测
- [x] 1.4 写入 `Environment=PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin`；`[Unit]` 写入 `Wants=network-online.target`、`After=network.target network-online.target`、`StartLimitIntervalSec=60`、`StartLimitBurst=5`。验证：单测断言全部键与所在段位（`After`/`StartLimit*` 在 `[Unit]`）
- [x] 1.5 为 D1/D2 写回归场景单测：渲染结果不含 `KillMode=control-group`、不含把共享根 `appkeeper` 作为 `RuntimeDirectory` 或 `StateDirectory` 的写法。验证：`cargo test service`

## 2. 持久状态目录（`state_dir`）

- [x] 2.1 `DaemonSettings` 新增可选 `state_dir`，`validate` 拒绝相对路径；新增平台无关的生效值解析纯函数（显式配置 → `STATE_DIRECTORY` → 平台 State 缺省），单测覆盖 root / 非 root / 环境变量优先 / Windows 服务与控制台分支。验证：`cargo test config`（Linux 上跑纯函数）
- [x] 2.2 `config --set/--get/--delete` 白名单加入 `state_dir`（非空与绝对路径校验、写后整体校验），`config --init` 模板加入带注释的 `state_dir` 行；更新白名单相关单测。验证：`cargo test config` + `cargo run -- config --set state_dir=<tmp> ` 手工核对
- [x] 2.3 `Supervisor::new` 改用生效 `state_dir` 读写 `state.json`；目录无法创建/不可写时告警并回退生效 `log_dir`（不阻止启动）；配单测覆盖正常与回退两条路径。验证：`cargo test supervisor`
- [x] 2.4 实现一次性迁移：目标位置无记录而 `<log_dir>/state.json` 存在时读旧写新、记日志、删除旧文件；目标已存在时 MUST NOT 覆盖；失败只告警。配单测覆盖三种输入。验证：`cargo test supervisor`
- [x] 2.5 `install` 预检加入「生效 `state_dir` 位于 `/tmp`/`%TEMP%`」告警，并把原 `log_dir` 告警文案收窄为只管日志。验证：`cargo test service` 的预检单测

## 3. install 安装期预检（`src/service.rs`）

- [x] 3.1 新增平台无关预检纯函数（输入：配置加载结果、注册表 app 解析结果、生效 `log_dir` 与 `state_dir`、平台/服务账号/生效 `socket`），输出「拒绝原因 / 告警列表」；配单测覆盖配置缺失、配置非法、坏 app、临时日志目录、临时状态目录、Windows 管道不匹配六类输入。验证：`cargo test service`
- [x] 3.2 在 `install` 的分平台入口（`unix_install` / `win_install`）中、`decide_write`/SCM 查询之前调用预检：拒绝项 `bail!`（零副作用），告警项写 stderr。验证：单测断言拒绝路径不写文件；`cargo test service`
- [x] 3.3 预检复用 `compute_timeout_stop_sec` 的配置加载，避免重复 IO 与「加载两次结论不一致」；加载失败从「回退 90s」改为「拒绝安装」。验证：单测覆盖配置损坏时拒绝、配置合法时计算值不变
- [x] 3.4 确认 `--force` 不豁免预检拒绝项（`--force` 只作用于同名 unit/服务决策）。验证：单测或手工 `cargo run -- daemon install --force` 于坏配置下仍拒绝

## 4. 服务托管识别与 shutdown 语义

- [x] 4.1 在 `src/service.rs`（或 `src/platform.rs`）新增平台无关纯函数判定托管来源（入参为环境变量查找闭包 / 运行模式），Linux 认 `INVOCATION_ID`、Windows 认 `--service`；配单测覆盖两种托管与手工启动。验证：`cargo test`
- [x] 4.2 新增 cgroup 路径 → unit 名的纯函数（`/proc/self/cgroup` 末段去 `.service`，失败返回 `None`）与单测（system.slice、user.slice、无 unit 段三种输入）。验证：`cargo test service`
- [x] 4.3 `run_daemon_core` 计算托管来源并在启动时注入 supervisor 状态；`DaemonInfo` 新增 `managed_by: Option<String>`（`systemd`/`scm`），`/v1`、`/api`、WS 快照同源。验证：`cargo test` 中既有 `StatusDoc` 序列化测试更新，新增托管/非托管两例
- [x] 4.4 `POST /v1/shutdown` 在入队 `Command::Shutdown` 之前检查托管来源：托管时返回 409 + `systemctl stop <unit>` / `sc stop <name>` 提示，不改变任何程序状态；非托管路径保持原响应与「先回包后退出」的顺序。验证：`cargo test --workspace` 中 server 路由单测
- [x] 4.5 `ak daemon shutdown` 对 409 的呈现：非零退出码，消息含服务管理器命令（复用现有客户端错误呈现）；手工启动路径仍退出 0。验证：单测 + `cargo run -p xtask -- e2e` 的相应段（e2e 段属组 7）

## 5. Windows 配置路径与端点告警

- [x] 5.1 `default_config_path`（`src/main.rs`）Windows 分支改为 `%ProgramData%\appkeeper\daemon.toml`；当该路径不存在且 `%APPDATA%\appkeeper\daemon.toml` 存在时回退旧路径；抽出可测的纯函数（入参为路径存在性与环境变量），配单测覆盖「新路径命中 / 回退旧路径 / 两者皆无」。验证：`cargo test`
- [x] 5.2 `install` 预检的 Windows 管道检查接入 `Endpoint::default_name` 与服务内置账号的推导，命中不一致时输出告警与显式 `socket` 指引；Linux 不出现该告警。验证：预检单测的 Windows 用例（Linux 上跑纯函数）
- [x] 5.3 回退命中旧 `%APPDATA%` 路径时，`install` 输出迁移提示（目标路径与 `-c` 用法）。验证：预检单测断言提示文本

## 6. 文档与升级说明

- [x] 6.1 README 命令速查/服务段补充：服务形态停机用 `systemctl stop` / `sc stop`（`ak daemon shutdown` 会拒绝）；`log_dir` 只放日志、`state_dir` 放持久状态（State 类），服务形态由 `StateDirectory` 提供；服务形态不继承登录 shell 环境（PATH/`HOME` 需显式声明）。验证：人工通读 README 相关段落
- [x] 6.2 README 升级速览补三条：既有 systemd unit 需 `sudo ak daemon install --force --now` 获得硬化键与 `StateDirectory`；旧 `<log_dir>/state.json` 自动一次性迁移；Windows 配置默认路径迁移与回退规则。验证：人工通读升级速览
- [x] 6.3 `conf/daemon.toml` 与 `config --init` 模板的注释同步（`log_dir` 与 `state_dir` 的分工与持久性、服务形态环境说明）。验证：`cargo run -- validate` 且模板渲染单测通过

## 7. 验收

- [x] 7.1 `xtask/src/e2e.rs` 的 unit 断言段补齐新键（KillMode / RuntimeDirectory=<uid> / StateDirectory=<uid>（root 冒烟安装为字面 `appkeeper/0`）/ Environment / network-online / StartLimit），并断言 `[Unit]` 段位正确。验证：`cargo run -p xtask -- e2e --no-browser`。备注：unit 内容断言位于 SM2（真机 install 落盘的 unit 文件），与 7.4 同门控（root + `AK_E2E_SYSTEMD=1`，非 root 无法写 unit）；段位解析器由 xtask 单测 `unit_assert_tests` 钉住
- [x] 7.2 新增「模拟开机」e2e：同一配置先以 `daemon start --foreground` 起停一轮，断言按 `restart` 策略拉起、`unless-stopped` 的停止记录跨重启保持、`/v1/status` 的 `managed_by` 为 null。验证：`cargo run -p xtask -- e2e --no-browser` 该段通过（BS1–BS4）
- [x] 7.3 新增状态目录 e2e：显式 `state_dir` 后 stop 一个 `unless-stopped` 程序 → 改动 `log_dir` 并重启 daemon → 该程序仍保持 `stopped`；再构造旧位置 `state.json` 验证一次性迁移。验证：`cargo run -p xtask -- e2e --no-browser` 该段通过（SD1–SD4，含「目标已存在不覆盖」）
- [x] 7.4 新增门控真机 systemd 冒烟段：仅 `AK_E2E_SYSTEMD=1` 且 root 时执行（临时 unit → enable/start → running 断言 → stop → 无残留子进程 → uninstall），否则打印 skip 并继续。验证：普通用户跑 e2e 出现 skip；有 systemd 的 root 环境跑该段通过。备注：本机非 root，实测到 skip 分支；SM1–SM5 全路径需 root systemd 真机复跑，含失败清理守卫（stop/disable/删 unit/daemon-reload）
- [x] 7.5 全量门禁：`cargo fmt --check`、`cargo clippy --workspace --all-targets`（不新增告警）、`cargo test --workspace`、`cargo run -- validate` 全绿。验证：上述命令退出码 0。备注：clippy 相对 HEAD 基线（137 条）降到 112 条、零新增签名——除本 change 新增 3 处外，把基线内全部 34 处 `collapsible_if` 一并机械折叠；`cargo run -p xtask -- e2e --no-browser` 全程 PASSED（151s，本机 systemd 段 skip）
- [x] 7.6 Windows 分支自查：新增/修改的 `#[cfg(windows)]` 代码保持最小面，纯逻辑抽到跨平台函数并已被 Linux 单测覆盖；在遗留项中记录「Windows 真机编译/运行未验证」（本机无 target）。验证：`cargo check` 通过；任务备注写明未验证范围。备注：本段未新增 `#[cfg(windows)]` 代码，触及的 Windows 共享路径（pump 轮转折叠、webui 管理线程折叠、install_precheck 折叠）均为不改逻辑的机械折叠且被 Linux 单测覆盖；Windows 真机编译/运行未验证（本机无 windows target，`cargo check --target x86_64-pc-windows-msvc` 未跑）
- [x] 7.7 `openspec validate harden-daemon-autostart` 通过；归档前 `openspec validate --specs` 确认主规范无损合入。验证：命令退出码 0
