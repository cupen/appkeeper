## 1. 注册表分区（`src/registry.rs`）

- [ ] 1.1 新增 `adhoc` 子区域常量与惰性创建；run 侧 `link_path`/`pending_path` 改为落 `app_dir/adhoc/<name>.toml(.pending)`（仅真实文件，不产生链接）。验证：`cargo test registry` 覆盖落位与占位路径的新单测
- [ ] 1.2 `list` 扫描顶层 + `adhoc/` 合并返回，`adhoc/` 不存在时视为空；应用名唯一性判定跨两区（`is_registered` 等存在性探测同步两处）。验证：`cargo test registry` 覆盖混合布局 / 无 adhoc 目录 / 顶层旧 run 文件三类
- [ ] 1.3 `guard_scaffold_overwrite` 判据路径化：run 目标仅许 `adhoc/`、compose 目标仅许顶层、symlink 与多链接硬链接（外部部署）永不放行。验证：`cargo test registry` 六类目标单测（adhoc 可写 / 顶层生成文件 / symlink / 硬链接 / 不存在 / 悬挂链接）
- [ ] 1.4 `remove` 的 `deleted_file` 判据扩展为「本体父目录 == 顶层或 `adhoc/`」。验证：`cargo test registry` 对 `adhoc/` 记录的移除单测（删本体并打印路径）
- [ ] 1.5 `ListedApp` 增 `origin` 字段（父目录推导：`adhoc/` → `run`，否则 `compose`），`ak ps --all` 表格与离线降级清单输出来源标注。验证：`cargo test registry` + `cargo test` 的 ps 展示单测；标注不进 `/v1`/`/api`/WS 投影（StatusDoc 无新字段）

## 2. 声明渲染（`src/config.rs` / supervisor 基础）

- [ ] 2.1 新增纯函数 `render_declaration(文本, 基准目录, 应用名, 调用方 PATH)`：`AppRaw` 解析（未知字段拒绝）→ `resolve_app` 同源校验（含保留字/命名规则/依赖成环、单行带参 `command` 与 `args` 互斥——既有校验）→ 相对 `work_dir` 以基准目录绝对化、`command` 拆分后首 token 为裸命令名时按调用方 PATH 解析为绝对路径 → 序列化为落盘 TOML 文本（规范化为首 token 绝对化 + `args` 数组，与 run 渲染产物同构）。验证：`cargo test config` 覆盖相对路径 / 单行带参 command / 裸命令 / 未知字段 / 非法名 / 保留字
- [ ] 2.2 渲染确定性：同一输入重复渲染逐字节一致（BTreeMap 键序）；渲染文本即比对基准与落盘内容。验证：`cargo test config` 确定性单测

## 3. 服务端端点与 staged 多程序（`src/server.rs` / `src/supervisor.rs`）

- [ ] 3.1 `POST /v1/compose/up/preview`：HTTP 线程完成渲染 + 与注册记录逐字节比对 + 分类（`fresh`/`identical`/`needs-update`/`adhoc-takeover`/`external-refused`），附各程序运行态与差异摘要，零副作用；请求体携带文件内容、基准目录、调用方 PATH 与应用名（MUST NOT 传文件路径）。验证：`cargo test server` 五分类 + 校验失败路由单测
- [ ] 3.2 新增 `Command::ComposeUp`（载荷：应用名、渲染文本、force）：按 priority + depends_on 一次性 spawn 全部应拉起程序（`restart=no` 与有用户停止记录者跳过并回传提示），并行等待各程序启动判定（startsecs/健康各自计时），全部通过 → 占位 + 原子 rename 落盘顶层注册文件；任一失败 → 逆序停止本轮已拉起程序、清理占位、回传结构化错误（失败阶段 + 程序名）。验证：`cargo test supervisor` 覆盖成功 / 失败回收 / 跳过规则 / 用户停止记录保持四组
- [ ] 3.3 `identical` 提交语义：不写盘；未运行程序按守护启动规则拉起（ensure-running），全部在跑回 `unchanged`。验证：`cargo test supervisor` 三分支单测（全在跑 / 部分未跑 / 因停止记录不拉）
- [ ] 3.4 `needs-update`/`adhoc-takeover` 分支：未带 force → 状态冲突类错误（附预览详情）；带 force → 停既有程序 →（收编分支先删 `adhoc/` 记录）→ 原子落盘 → 按声明拉起。`external-refused` 无论 force 一律拒绝并附 `ak remove` 指引。验证：`cargo test server` + `cargo test supervisor` 各分支单测
- [ ] 3.5 执行进度按「启动进度流式反馈」chunked NDJSON 逐程序回传（复用 `/v1/run` 的传输模式与帧约定风格）。验证：`cargo test server` 帧序列单测（starting/判定通过/失败/落盘结果）

## 4. CLI（`src/main.rs` / `src/client.rs`）

- [ ] 4.1 `compose up [dir]` 子命令解析：`up` 为 `compose` 第一位置参数保留字（不参与路径分派）；`dir` 缺省 `.`；仅收 `-f/--file`、`--name`、`--force`、`--json`，内容微调旗标以用法错误拒绝；声明文件缺失报错并列出查找路径；应用名缺省取目录 stem、`--name` 覆盖、`all` 拒绝；不向上递归查找。验证：`cargo test` 的 parse 单测覆盖以上各分支（含名为 `up` 的目录需 `-f` 的提示文案）
- [ ] 4.2 客户端两段流：读声明文件 → preview → TTY 交互「是否强制更新」（needs-update/adhoc-takeover）→ 提交（force=确认结果）；非 TTY 需交互分类直接拒绝（非 0、零副作用）并提示 `--force`；`--force` 跳过交互直通；进度逐行打印 / `--json` 直通帧流；守护离线经 `daemonize::ensure_daemon` 隐式拉起后继续。验证：`cargo test client` 单测 + 手工 `cargo run -- compose up` 核对交互文案与流式输出
- [ ] 4.3 退出码：无操作（identical 全在跑）0；交互答「否」/非 TTY 拒绝/校验失败 非 0；启动失败 非 0。验证：单测断言各路径退出码语义

## 5. 验收与文档

- [ ] 5.1 `xtask/src/e2e.rs` 新增 compose up 段：全新声明注册并按序拉起；同名一致无操作退出 0；一致但未运行被拉起；`unless-stopped` 用户停止记录保持并提示；有差异 TTY 交互「是/否」两分支；非 TTY 拒绝 + `--force` 直通；收编 `adhoc/` 记录（顶层落位）；外部链接拒绝（含 `--force`）；启动失败零残留；守护离线隐式拉起；`ps --all` 来源标注；既有顶层 run 文件与 `adhoc/` 并存兼容；`run --force` 对顶层记录拒绝（BREAKING 断言）。验证：`cargo run -p xtask -- e2e --no-browser` 该段通过
- [ ] 5.2 README：`compose up` 与 `.appkeeper.toml` 用法（发现规则、命名、四旗标、非交互形态）、`app_dir/adhoc/` 分区说明、`run --force` 收窄的迁移提示（顶层旧记录如何处理）；新增「开发场景速览」小节——一条命令 dev 流程（`ak compose up` 守护离线时自动后台拉起）、防复活提示（dev 声明用 `unless-stopped`/`no`，或收工 `ak stop <app>`——用户停止记录跨重启生效）、Windows 前台形态提示（`ak daemon start --foreground` 后再 `compose up`，关终端不影响后台守护；前台守护自身不接 SIGHUP 的既有语义）、webui 控制台一次性配置提示（`[webui]` 段 + `config --reload` 热开启，127.0.0.1 绑定）。验证：人工通读相关段落
- [ ] 5.3 全量门禁：`cargo fmt --check`、`cargo clippy --workspace --all-targets`（不新增告警）、`cargo test --workspace`、`cargo run -- validate` 全绿。验证：上述命令退出码 0
- [ ] 5.4 `openspec validate add-compose-up` 通过；归档前 `openspec validate --specs` 确认主规范无损合入。验证：命令退出码 0
