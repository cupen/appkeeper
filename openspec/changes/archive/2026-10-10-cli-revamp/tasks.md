# Tasks: cli-revamp

## 1. 配置模型：restart 字段（取代 autorestart/autostart）

- [x] 1.1 `config.rs`：`restart` 字段域（no|on-failure|always|unless-stopped）+ 宽容解析（`false` ≡ no）+ 四层级联（到底未设 ≡ no）；`autorestart`/`autostart` 弃用别名归并（映射规则见 design D13）、同层新旧同现校验拒绝、歧义组合迁移告警。验证：单测覆盖值域/宽容写法/级联/别名映射/同现拒绝/告警分支
- [x] 1.2 `supervisor.rs` + `program.rs`：守护启动按 `restart` 拉起（no 不拉起、always 无条件、on-failure 拉起、unless-stopped 查记录）；程序退出按四策略处理（unless-stopped 会话内同 always）；apply 引入新程序同语义。验证：单测（各策略的拉起/不拉起、退出处理）+ 既有策略测试迁移
- [x] 1.3 `supervisor.rs`：「用户停止」记录持久化（`{log_dir}/state.json`，原子写，仅程序名集合）——显式 stop 写入、显式启动清除、shutdown 不写入、解析失败按空集告警。验证：单测（写/清/关机不写/损坏容错）+ 集成（unless-stopped 停止跨守护重启保持、always 不受影响）
- [x] 1.4 apply/pending 检出适配字段改名（`restart` 字段差异进入 pending 比对，旧字段归并后比较）。验证：单测（改 restart 字段被检出为变更）
- [x] 1.5 模板更新：`conf/daemon.toml`（[app-default] restart = "always"）、`conf/app.toml`、`config --init` 模板、`examples/` 全部改用新字段。验证：validate 对全部模板通过 + `config --init` 产物含 restart

## 2. CLI 动词面（clap）

- [x] 2.1 `run` 升级为统一入口动词：新增 `name`/`app` 定位参数（吸收原 `start` 语义与互斥）、`--` 引导的 trailing 命令行（`allow_hyphen_values`）与等价的 `--cmd "<字符串>"`（二者互斥）、`--detach` flag、`--restart=<no|always|unless-stopped|on-failure>`；分派规则与互斥按 design D10（命令行非空 → ad-hoc；name xor app → 启动；全空 → daemon，`--detach` 仅此形态合法）；移除 `start` 子命令与 add 的 `--autostart/--no-autostart/--autorestart`（统一 `--restart`）。验证：clap 单测覆盖各形态（裸、--detach、name、--app、`-- cmd`、`--cmd "..."`）解析、互斥拒绝、`xkeeper start web` 报错并提示 `run`、add flag 集更新
- [x] 2.2 `log` 定位参数扩展为多个程序名（≥1），`--stream` 值域扩展 `both`。验证：clap 单测（多名字、both、默认值、既有单程序形态不回归）
- [x] 2.3 `status` 新增 `--json` flag。验证：clap 单测

## 3. 命令解析与 run ad-hoc 编排

- [x] 3.1 实现命令首 token 解析器：含路径分隔符 → canonicalize；裸名 → unix 遍历 `PATH` 取首个 X_OK 匹配 / Windows `PATHEXT` 扩展；失败返回可诊断错误。验证：单测覆盖绝对/相对路径、PATH 命中/未命中、Windows PATHEXT（条件编译）
- [x] 3.2 run ad-hoc 编排桥接：`--` trailing 与 `--cmd` 字符串归一为同一命令行；解析结果重建 args → `AddOptions`（`--restart` 字面量直书）→ 复用 `registry::add` 脚手架 → 立即 app 范围 apply（`--restart=no` 时注册落地不拉起）；缺省名取命令 stem；保留字 `all` 沿用拒绝；解析失败退出码 2 且无产物。验证：临时 app_dir 单测（生成文件内容含 restart 字面量、stem 命名、--cmd 与 -- 等价、失败分支）+ fake-daemon 单测（在线 apply 路径）

## 4. 后台拉起与 run --detach

- [x] 4.1 实现后台拉起模块：detach spawn（unix `process_group(0)` / Windows `CREATE_NEW_PROCESS_GROUP | DETACHED_PROCESS`；stdio 追加重定向 `log_dir/daemon.out.log`）+ health 轮询（~200ms 间隔、~10s 总超时）+ 竞态容忍（health-only 判定）。验证：单测（spawn 参数拼装、日志重定向路径、不可满足配置的超时分支）
- [x] 4.2 `run --detach` 接线：就绪后打印监听地址与后台日志路径、退出码 0；超时/端口冲突非零退出并指明日志位置。验证：隔离 workspace + 随机端口的真 daemon 集成测试（后台存活、终端无关、二次启动报端口占用）
- [x] 4.3 立即生效入口统一接入拉起：`add --apply`（两路径形态）与 run ad-hoc 共用同一模块，替代原 `apply_after_add` 的 Offline 报错分支；在线短路。验证：集成测试（冷启动 run ad-hoc 全链路生效、`add <path> --apply` 离线生效、在线不派生第二实例）

## 5. log 聚合与 status 表格

- [x] 5.1 `client.rs` follow 重构为行回调 `follow_lines(name, stream, on_line)`，`log_follow` 基于其实现且行为不变。验证：编译 + 既有 follow 冒烟不回归
- [x] 5.2 `log` 多程序实现：非 follow 按序多程序 `log_tail`；follow 并发连接 + 行缓冲 + stdout 锁 + `[name] ` 前缀（多名字才加）；`both` = 双连接。验证：真 daemon 集成冒烟（两程序交错前缀正确、单程序无前缀、both 双流可见、404 退出码 1）
- [x] 5.3 `status` 表格渲染：shell.rs 渲染器提为共享函数，顶层与 shell 同列同格式（NAME/APP/STATE/PID/RESTARTS/UNHEALTHY）；`--json` 分支保持旧 pretty JSON。验证：固定 StatusDoc 样例的渲染单测 + 真 daemon 冒烟（两形态输出对照）

## 6. shell 同步、验收与文档

- [x] 6.1 shell 内置动词 `start` 改名 `run`（解析、help 文本、`-e` 单命令模式、相近命令提示表同步），无别名残留。验证：shell 解析单测更新 + `shell -e "run web"` 冒烟
  - 状态：完成。`parse_line` 识别 `run`（`start` 报 unknown command，无别名）；`HELP_TEXT` 与 `similar_hint` 命令表均为 `run|stop|restart`；`-e "run web"` 冒烟经控制面下发（离线退出码 3 已核）。`cargo test shell::` 12 项全绿。
- [x] 6.2 xtask e2e 全面更新：`start` 断言迁移为 `run`（含 `xkeeper start web` 报错提示断言）、`status` JSON 断言迁移到 `--json`、`autostart/autorestart` 配置与 flag 断言迁移到 `restart`；新增场景——run ad-hoc 冷启动、`--cmd` 与 `--` 等价、命令行重复再生、restart 四策略（no 只注册 / unless-stopped 停止跨守护重启保持 / always 停止后守护重启仍拉起 / shutdown 不写记录）、旧字段别名读取与同现拒绝、`run --detach`（就绪/端口冲突）、`log` 多程序聚合与 both、`status` 表格。验证：`cargo run -p xtask -- e2e` 全绿
  - 状态：断言迁移与新增场景均已落地（`revamp_pass` R1–R8、`detach_pass` R9、`offline_pass` G0 自动拉起、`cli_pass` 迁移）。`cargo build -p xtask` 通过。**e2e 全量运行按约定留待 review 阶段**，本阶段未跑，未全绿状态未知。
- [x] 6.3 README 使用场景重写：快速上手第一路径改为 `xkeeper run -- <cmd>`（含冷启动自动拉起说明、`--restart=no` 提示、`remove` 清理闭环），`restart` 字段模型与迁移说明（旧字段兼容、级联默认变化高亮）、`run` 统一动词与 `--detach` 入文档；架构图动词面更新。验证：文档演练命令与实际输出一致
  - 状态：完成。架构图动词面、快速上手（路径一冷启动 / 路径二部署）、`restart` 字段模型与弃用别名/级联默认变化、统一 `run` 动词与后台拉起协议、shell 内置命令、升级注意（三处 BREAKING）均已更新。文档演练以真 daemon 复核：`run -- /bin/sleep … --name web`、`status`（表格）、`status --json`、`run web`、`remove`、`run --detach`（就绪 + 二次实例）输出与文档一致。
- [x] 6.4 平台复查：unix 路径全套 e2e 通过；Windows 路径 `cargo check` 通过 + 解析器/detach flags 条件编译单测绿（无 Windows runner 时静态确认）。验证：两条平台路径的检查命令与结果记录
  - 状态：unix 单元层面全绿（`cargo build` + `cargo test` 210 项）；unix e2e 按约定留待 review。Windows：本机**未安装 `x86_64-pc-windows-msvc` target**（`cargo check --target` 报 `can't find crate for core`），交叉检查不可行，改为静态确认——`daemonize.rs` 的 `process_group(0)`（unix）/`creation_flags(CREATE_NEW_PROCESS_GROUP | DETACHED_PROCESS)`（windows）、`registry.rs` 的 `resolve_on_path` 双平台实现、`default_config_path` 双平台分支均在，条件编译单测（`resolve_command_*` 等）存在。**Windows 实机验证列为欠账**。

