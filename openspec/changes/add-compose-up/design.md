# Design: add-compose-up

## Context

- 注册表现状：`registry::list` 只扫 `app_dir` 顶层 `*.toml`；`link_path`/`pending_path` 均为 `app_dir/<name>.toml(.pending)`；`guard_scaffold_overwrite` 用 symlink 探测 +「本体是否在 `app_dir` 内」判可覆盖性；`remove` 的 `deleted_file` 判据为 `real_canon.parent() == app_dir`。run 与 compose 的生成文件同居顶层、无法从位置区分。
- run 的 daemon-first 机制：`Command::AdhocUp/AdhocCommit/AdhocAbort` + supervisor 的 `staged` 表；启动判定（wait_startup）跑在 HTTP 连接线程而非命令处理器；`render_adhoc` 在 daemon 侧渲染单程序 app；`POST /v1/run` chunked NDJSON 进度流（`src/client.rs` 有对应流式读取）。
- compose 家族离线隐式拉起：`compose --apply` 经 `daemonize::ensure_daemon` 拉起守护后继续。
- 配置解析：`AppRaw::load`（deny_unknown_fields）+ `resolve_app(name, path, raw, defaults)`，程序相对路径基准 = 配置文件父目录；`AppRaw` 可序列化（BTreeMap 键序确定）。

## Goals / Non-Goals

**Goals:**

- `.appkeeper.toml` → `compose up` 一条命令完成「声明 → daemon 启动 → 注册」，失败零残留。
- run 与 compose 的注册产物从落位即可区分，覆盖权限由路径判据取代 symlink 探测。
- 撞名交互的确认发生在 CLI（TTY），判定与执行全部在 daemon（防竞态、远程可用）。

**Non-Goals:**

- 不改 app 配置 schema、四层默认级联、apply/pending 语义、webui、控制面既有端点。
- 不做 down/stop/watch/多文件合并/env_file/变量替换/profiles（见 proposal 非目标）。

## Decisions

### D1: 分区落位与路径判据（registry）

`app_dir/adhoc/` 为 run 产物专属区域：run 的 `link_path`/`pending_path` 落 `adhoc/<name>.toml(.pending)`，compose 产物（脚手架、up 快照）与链接留在顶层。`list` 扫两处合并；名唯一性跨两区判定。覆盖判据改为**目标路径所在区域**：`guard_scaffold_overwrite` 演进为「run 只许写 adhoc/，compose 只许写顶层，链接（symlink/多链接硬链）永不写」——symlink 探测保留，仅用于「外部部署」判定。`remove` 的 `deleted_file` 判据扩展为「本体父目录 == 顶层或 adhoc/」。

- 备选按文件形态判（现状）：区分不了 run 与 compose 的生成文件，正是本次要消除的歧义。
- 无迁移：既有顶层 run 文件就地兼容，仅来源标注按位置归 `compose`（既定局限，记录于 spec 场景）。

### D2: 渲染在 daemon 侧，确定性的快照文本

新纯函数 `render_declaration(text, base_dir, name) -> Result<声明快照>`：解析（`AppRaw`，未知字段拒绝）→ 校验（`resolve_app` 同源规则，含单行带参 `command` 与 `args` 互斥——schema 层既有校验，拆分用既有 `split_command`）→ 相对路径以 `base_dir` 绝对化、`command` 拆分后首 token 为裸命令名时按提交方 PATH 解析 → 快照规范化为「首 token 绝对化的 `command` + `args` 数组」（与 run 的渲染产物同构）→ 序列化为 TOML 文本。**同一函数产出比对基准与落盘内容**，BTreeMap 键序保证渲染确定，逐字节比对因此成立。CLI 只传 `{文件内容, 基准目录, 应用名, force}`，不传路径——TCP 远程 CLI 天然可用。

- 备选 CLI 渲染：CLI/daemon 版本不一致时比对基准漂移，且远程场景下 daemon 无法校验；渲染必须是 daemon 单一事实源。
- 跨版本渲染格式变化会触发一次 needs-update（安全方向的误报，交互会展示差异）——可接受。

### D3: 两段协议：preview（无副作用）→ up（执行）

`POST /v1/compose/up/preview` 在 HTTP 线程完成渲染+比对+分类（`fresh/identical/needs-update/adhoc-takeover/external-refused`，附运行态与差异摘要）；`POST /v1/compose/up` 携带 `force` 执行。**preview 仅为 advisory**：submit 在 daemon 命令处理路径重新分类判定（与 run 端点「MUST NOT 仅客户端预检」同源，防与巡检/apply 竞态）。需交互分类未带 force → 状态冲突类错误回传，CLI 据此在非 TTY 下呈现指引。

- 备选单端点长轮询等确认：daemon 无交互面、连接占用不可控。
- 备选复用 `/v1/run` 加字段：载荷形态（多程序声明文本 vs 命令行）与预览语义都不同，混在一起两头都难读。

### D4: staged 多程序——spawn 全部、判定各自计时、全过才 commit

执行复用 staged 机制扩展为多程序：按 priority + depends_on 拓扑**一次性 spawn 全部应拉起的程序**（同守护启动 `start_eligible` 的批量语义，`no`/用户停止记录的程序跳过），随后并行等待各程序启动判定（startsecs/健康，各自计时）——不逐个串行等待，总时长 ≈ max 而非 Σ。全部通过 → 占位 + 原子 rename 落盘一个 app 级注册文件；任一失败 → 停掉本轮已拉起的（逆序）+ 清占位 + 结构化错误（失败阶段+程序名）。

- 备选逐程序 staged commit：注册单元是 app 级文件，逐程序 commit 会产生中间态文件。
- 备选串行逐个判定：慢（Σ startsecs），且与守护启动的批量行为不一致。

### D5: identical 的 ensure-running 复用守护启动规则

`identical` 提交不写盘，对未运行程序按 `start_eligible` 同款规则拉起（`no` 不拉、`unless-stopped` 有记录不拉并回传提示），全部在跑则回 `unchanged`。不触碰用户停止记录——up 是部署收敛，不是程序级显式 start。

### D6: CLI 面——`up` 保留字、隐式拉起、NDJSON 复用

`ak compose` 的第一位置参数 `up` 解析为子命令（`compose up [dir]` + `-f/--name/--force/--json`），其余路径分派不变；目录恰好名为 `up` 时需 `-f` 显式指文件（帮助文案说明）。守护离线复用 `daemonize::ensure_daemon`（compose --apply 同款）。进度复用 `client.rs` 的 NDJSON 流式读取，逐程序事件逐行打印，`--json` 直通帧流。

### D7: 来源标注走注册表层，不动状态投影

`ListedApp` 增 `origin`（由记录父目录推导：`adhoc/` → `run`，否则 `compose`），`ps --all` 表格加标注列。**不进** `StatusDoc`/`/v1` 投影——origin 是注册表事实而非 supervisor 运行态，离线降级路径也要标注；避免触发 `/v1`、`/api`、WS 三处同源修改。

## Risks / Trade-offs

- [preview 与 submit 之间记录被外部改动（手工编辑/并发 compose）] → submit 侧重新分类，preview 结果仅作呈现；分类变化按 submit 时点语义执行。
- [多程序并发判定的资源峰值] → 与守护启动批量拉起同水位；staged 期间不落盘，失败即回收。
- [渲染格式随版本演进导致升级后首次 up 误报 needs-update] → 安全方向（展示差异、人确认），不自动覆盖。
- [回滚到旧版二进制后 `adhoc/` 记录不可见（旧版不扫子目录）] → 迁移段写明：回滚前把仍需要的 `adhoc/` 记录移回顶层；run 产物本质可由命令重建，风险有界。
- [`up` 保留字与名为 `up` 的目录冲突] → `-f` 显式指定文件可绕开；帮助文案写明。
- [Windows 硬链接回退] → 链接只出现在顶层，路径判据平台无关；`adhoc/` 只放真实文件，无链接语义。

## Migration Plan

1. 升级部署新版二进制：无需任何数据动作——顶层既有记录照旧，新 run 产物开始落 `adhoc/`。
2. 用户侧无感；`compose up` 为新命令，旧脚本不受影响；唯一行为收紧是 `run --force` 不再覆盖顶层记录（报错指引 `ak remove` / `ak compose up`）。
3. 回滚：旧版二进制不识别 `adhoc/`；回滚前把仍需要的 `adhoc/<name>.toml` 移动到顶层（或记下命令重新 `ak run`）。

## Open Questions

- 差异摘要的呈现粒度（逐程序 changed 清单 vs 含字段级 diff）——实现期定，不改变端点契约（spec 只约束「附差异摘要」）。
- NDJSON 帧字段命名与帧型集合——实现期随 client/server 一体定。
