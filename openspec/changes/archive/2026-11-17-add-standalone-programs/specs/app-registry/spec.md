## ADDED Requirements

### Requirement: run ad-hoc 散装程序注册与命名

`ak run` 的 ad-hoc 形态 SHALL 只注册**散装程序（standalone program）**：注册记录
以程序名命名、不属于任何 app，用户无需理解或管理 app。命名参数 SHALL 为
`--name <程序名>`；`--app` MUST 被移除（传入时以退出码 2 报未知参数），MUST NOT
支持 `app/program` 复合名——`/` 文法仅用于管理面寻址（`start|stop|restart|apply|remove`
的目标参数），不属于 run 的命名语法。

散装程序记录 SHALL 落位于 `app_dir/adhoc/<程序名>.toml`（真实文件），配置 SHALL
以一个 `[program.<程序名>]` 表为主，**默认不写 `[app]` 表**；`--restart` /
`--restart-backoff` 等 run flag SHALL 写入该程序表（程序级字段），仅当给出
`--description` 时才写一个只含元数据的 `[app]` 段——该段不构成 app 身份。旧版
run 产物（`adhoc/` 内、含 `[app]` 的记录）SHALL 按记录位置识别为散装程序，不迁移、
继续可启动/停止/移除。

**缺省名自动排重**：未给 `--name` 时基名取命令首 token 的文件名 stem；基名空闲则
直接使用，被占用则顺延为 `<基名>-<N>`，N 取现有 `<基名>-<k>` 中最大 k 加一
（不存在时取 1），MUST NOT 填充中间空号。已删除的最高编号会被复用（无持久计数器）。

「占用」SHALL 覆盖：`app_dir` 顶层与 `adhoc/` 下的注册记录（含悬空链接）、内存中的
同名 app、**其他应用**或散装程序已占用的同名程序、以及暂存 app。候选名恰好对应
「已启动但未落盘」的暂存 app 时 SHALL 走残留确认流程，MUST NOT 顺延另起进程。

**显式名不排重**：`--name <程序名>` 为显式命名，SHALL NOT 自动追加后缀。名字已被
任一注册记录（app 名或程序名）、内存 app 或暂存 app 占用时，守护进程 SHALL 回传
**409 结构化冲突**，MUST NOT 写盘、启动或停启任何进程。散装程序名 SHALL 全局唯一，
且 MUST NOT 与任何 app 名冲突（保证单参数寻址无歧义）。

`--force` SHALL 只覆盖、不排重：覆盖目标为 `--name` 指定的散装程序（未给时取
stem），仅限 `app_dir/adhoc/` 内本工具生成的记录；逐字节相同为空操作；顶层 compose
产物与外部部署链接仍拒绝覆盖。`--force` MUST NOT 放行同名程序名冲突。

程序名 SHALL 通过 `config::is_valid_name` 且 MUST NOT 为保留字 `all`；不合法或保留
时以退出码 2 拒绝并提示 `--name`，自动排重只处理重名，MUST NOT 清洗或改写给定名字。

命名的查重、顺延与残留判定 SHALL 在守护进程命令队列的单次处理内完成，MUST NOT 仅由
客户端预检。成功结果 SHALL 回传最终程序名；因缺省名顺延而改名时人类输出 SHALL 提示
自动命名，`--json` 结果 SHALL 标记 `auto_named: true`。`ak remove <程序名>` SHALL
删除该散装记录（同既有脚手架语义，日志保留）。

#### Scenario: 散装程序直接注册

- **WHEN** 执行 `ak run --name agent -- ./agent --serve`
- **THEN** 注册记录为 `app_dir/adhoc/agent.toml`，记录名与程序名均为 `agent`，不属于任何 app，配置不含 `[app]` 表，`ak start agent` 与 `ak pid agent` 均可用，`auto_named` 为 false

#### Scenario: 缺省名空闲时取命令 stem

- **WHEN** 注册表中不存在 `npx`，执行 `ak run -- npx create-vite app`
- **THEN** 程序名为 `npx`，结果中的程序名为 `npx`，不标记自动顺延

#### Scenario: 缺省名被占用时顺延为 npx-1

- **WHEN** 已注册 `npx`（无论其程序是否在运行），再次执行 `ak run -- npx create-vite app2`
- **THEN** 守护进程以 `npx-1` 注册并启动新进程，既有 `npx` 的记录与进程保持不变，输出提示该名字为自动命名且 `auto_named: true`

#### Scenario: 再次执行严格递增不复用空号

- **WHEN** 已存在 `npx` 与 `npx-2`（`npx-1` 已被 `ak remove`），执行 `ak run -- npx ...`
- **THEN** 新命名为 `npx-3`，MUST NOT 复用 `npx-1`

#### Scenario: 顶层 compose 记录同样计入占用

- **WHEN** `app_dir` 顶层已存在 compose 注册的 `node`，执行 `ak run -- node server.js`
- **THEN** 新命名为 `node-1`，顶层 `node` 记录与进程不受影响

#### Scenario: 与其他应用程序名冲突时顺延

- **WHEN** 程序名 `worker` 已被另一个应用注册，执行 `ak run -- /opt/worker/worker`
- **THEN** 新命名为 `worker-1`，不返回冲突错误

#### Scenario: 暂存残留优先于顺延

- **WHEN** 上一次 `ak run -- npx ...` 已启动进程但落盘失败（暂存未提交），注册记录不存在，用户再次执行相同命令
- **THEN** 守护进程回传残留态并进入补落盘确认流程，MUST NOT 自动改名为 `npx-1` 再启动一个进程

#### Scenario: 显式名重复返回 409

- **WHEN** 已注册散装程序 `agent`，执行 `ak run --name agent -- ./other`
- **THEN** 守护进程回传 409 冲突，不写盘、不启动进程，MUST NOT 改名为 `agent-1`

#### Scenario: 显式名与 app 名冲突返回 409

- **WHEN** 已存在 app `gateway`（compose 产物），执行 `ak run --name gateway -- ./gateway`
- **THEN** 守护进程回传 409 冲突，既有 app 的记录与进程不变，不自动改名

#### Scenario: run flag 落入程序级字段

- **WHEN** 执行 `ak run --name worker --restart=unless-stopped --restart-backoff=3 -- ./worker`
- **THEN** 生成的 `app_dir/adhoc/worker.toml` 在 `[program.worker]` 下含 `restart = "unless-stopped"` 与 `restart_backoff = 3.0`，不含 `[app]` 表

#### Scenario: 非法语法与保留名被拒绝

- **WHEN** 执行 `ak run --name a/b ...`、`--name all ...`、`--name "" ...`，或名字含非法字符（如 `my.tool`）
- **THEN** 以退出码 2 报错并给出 `--name` 语法提示，不写盘、不启动进程

#### Scenario: --app 不再是合法参数

- **WHEN** 执行 `ak run -- python -m http.server --app web`
- **THEN** 以退出码 2 报未知参数，提示使用 `--name`

#### Scenario: 移除散装记录

- **WHEN** 对已停止的散装程序执行 `ak remove agent`
- **THEN** `app_dir/adhoc/agent.toml` 被删除且路径被打印，状态投影不再含该程序，日志保留

## MODIFIED Requirements

### Requirement: run ad-hoc 同名应用拦截

`ak run` 的 ad-hoc 两种形态（`--` trailing 与 `--cmd`）在**显式给出 `--name`**
时 SHALL 把同名判定交给守护进程；当程序名对应的注册记录已存在（无论位于 `app_dir`
顶层还是 `app_dir/adhoc/`）、内存中已有同名 app 或程序（含已注册但未运行的）、或
该名字已被任何 app 占用，且**未指定 `--force`** 时，守护进程 SHALL 拒绝本次注册并
回传 **409 结构化冲突错误**，MUST NOT 写盘、MUST NOT 启动新进程、MUST NOT 停止或
重启既有进程。显式名 MUST NOT 自动追加后缀或改名。未给 `--name` 的缺省命名 SHALL
NOT 适用本要求，而按「run ad-hoc 散装程序注册与命名」自动排重。

拒绝信息 SHALL 明确指出冲突的名字并给出可操作指引（改用其他 `--name`，或显式加
`--force` 覆盖 run 自家记录）。CLI SHALL 以非零退出码退出。

「已启动但未落盘」的残留态不属本要求：该场景仍按 `app-registry` 的残留确认流程处理，
且 MUST NOT 重启既有进程。

#### Scenario: 同名已注册且运行中被拒绝

- **WHEN** 对已由 `ak run --name web` 注册且正在运行的 `web` 再次执行 `ak run --name web -- python -m http.server 9000`
- **THEN** 守护进程回传 409 冲突错误，既有 `web` 进程的 PID 与 `app_dir/adhoc/web.toml` 内容均保持不变，CLI 以非零退出码退出

#### Scenario: 同名已注册但未运行也被拒绝

- **WHEN** 对已注册但当前未运行的 `web` 再次执行 `ak run --name web -- python -m http.server 9000`
- **THEN** 注册被拒绝，`app_dir/adhoc/web.toml` 不被改写，不产生新进程

#### Scenario: --restart=no 同样受拦截

- **WHEN** 对已存在的 `web` 执行 `ak run --cmd "python -m http.server 9000" --name web --restart=no`
- **THEN** 注册被拒绝，不写盘；`--restart=no` 不构成覆盖既有应用的例外

#### Scenario: 参数完全相同也不再生

- **WHEN** 对已注册的 `web` 以完全相同的命令行与参数再次执行 `ak run --name web`
- **THEN** 仍以 409 冲突拒绝，MUST NOT 返回「幂等成功」或现有 PID

#### Scenario: 残留态仍走确认流程

- **WHEN** 上一次显式 `--name web` 的 run 已启动同名程序但落盘失败，且注册记录不存在
- **THEN** 守护进程回传残留态而非冲突错误，CLI 询问用户是否仅补落盘；确认后复用已在运行的进程，MUST NOT 重启

### Requirement: run ad-hoc --force 强制覆盖

`ak run --force` SHALL 允许覆盖同名已注册的散装程序。覆盖目标 SHALL 为 `--name`
指定的程序名（未给时取命令 stem）；`--force` MUST NOT 参与自动排重（不会改写为
`<基名>-<N>`）。覆盖判定 SHALL 在守护进程内完成：把本次命令行渲染出的完整配置文本
与磁盘现有散装记录逐字节比较。覆盖目标 SHALL 以记录位置判定：`--force` 仅可覆盖
位于 `app_dir/adhoc/` 的记录（run 自家产物）；同名记录位于 `app_dir` 顶层（compose
生成文件或外部部署链接，见「注册表分区与来源标注」）时 SHALL 拒绝覆盖，错误信息提示
先 `ak remove <name>` 或改用 `ak compose up`，`--force` 不放行。

- **有差异**：SHALL 在通过全部校验后原子重写注册记录（占位 + rename），随后停止旧的同名程序并按新定义重新拉起；`--restart=no` 时只重写记录、不拉起（旧进程 SHALL 被停止，避免旧定义继续运行）。返回新 PID（`--restart=no` 时为 `null`）。
- **无差异**：MUST NOT 写盘、MUST NOT 停止或重启进程、MUST NOT 拉起新进程；CLI SHALL 仅提示「配置无修改，不做任何动作」并以退出码 0 退出；`--json` 结果 SHALL 标记 `unchanged: true` 并回传现有 PID（若有）。

`--force` SHALL NOT 绕过其他既有关卡：与其他 app 或程序的同名冲突仍被拒绝；目标记录不是本工具生成的文件（如指向外部部署配置的链接或手工文件）时仍被拒绝覆盖。

「已启动但未落盘」的残留态 SHALL NOT 被 `--force` 改写：仍按残留确认流程处理。

#### Scenario: 有差异时覆盖并重启

- **WHEN** 对正在运行且已注册的 `web` 执行 `ak run --force --name web -- python -m http.server 9000`
- **THEN** `app_dir/adhoc/web.toml` 被原子重写为新命令行，旧进程被停止，新进程启动并通过启动判定，CLI 回传新 PID

#### Scenario: 无差异时不做任何动作

- **WHEN** 对正在运行且已注册的 `web` 以完全相同的新旧配置执行 `ak run --force --name web -- python -m http.server 8000`
- **THEN** `app_dir/adhoc/web.toml` 字节不变、进程 PID 不变、不发生 stop/spawn，CLI 仅提示「配置无修改，不做任何动作」并以 0 退出

#### Scenario: 已注册未运行时有差异

- **WHEN** 对已注册但当前未运行的 `web` 执行带不同参数的 `ak run --force --name web`
- **THEN** 记录被重写并拉起新进程

#### Scenario: 已注册未运行且无差异

- **WHEN** 对已注册但当前未运行的 `web` 以相同配置执行 `ak run --force --name web`
- **THEN** 不写盘、不拉起进程，仅提示「配置无修改，不做任何动作」并以 0 退出

#### Scenario: --restart=no 强制覆盖

- **WHEN** 对正在运行的 `web` 执行带不同参数的 `ak run --force --name web --restart=no`
- **THEN** 记录被重写，旧进程被停止，新定义不被拉起

#### Scenario: 未给 --name 时按基名覆盖

- **WHEN** 已注册 `npx`，执行 `ak run --force -- npx create-vite app2`（未给 `--name`）
- **THEN** 覆盖目标为 `npx`（不生成 `npx-1`），按有差异/无差异规则处理

#### Scenario: 分组名覆盖对应的 app 记录

> 标题为既有场景锚点；散装模型不再有 `app/程序` 分组名，本场景按散装记录覆盖语义理解。

- **WHEN** 已注册 `gateway`，执行 `ak run --force --name gateway -- ./gateway --v2`
- **THEN** 覆盖目标为 `app_dir/adhoc/gateway.toml`，按有差异/无差异规则处理

#### Scenario: 跨应用程序名冲突不因 --force 放行

- **WHEN** `--force` 的目标程序名与**其他应用**已注册的程序名冲突
- **THEN** 注册仍被拒绝，不写盘、不启动进程

#### Scenario: 顶层 compose 记录不被 run --force 覆盖

- **WHEN** 同名注册记录位于 `app_dir` 顶层（`compose up` 快照或脚手架生成文件），执行 `ak run --force --name <name> …`
- **THEN** 覆盖被拒绝并提示 `ak remove` 或 `ak compose up`，目标文件与进程均不变

#### Scenario: 外部部署配置不被 --force 覆盖

- **WHEN** 同名注册记录是指向外部部署配置的链接或非本工具生成文件
- **THEN** `--force` 覆盖被拒绝并说明原因，目标文件不变

#### Scenario: 强制覆盖失败时的处置

- **WHEN** `--force` 的新定义校验失败，或新进程未通过启动判定
- **THEN** 校验失败时目标文件保持原样且无占位残留；启动判定失败时新进程被停止并以结构化错误回传，已原子落盘的新记录保留（覆盖已生效，不回滚旧记录）

### Requirement: 注册表分区与来源标注

`app_dir` SHALL 按「注册产物来源」分区，使 run 与 compose 的落盘记录可从路径区分：

- `ak run` ad-hoc 形态产生的**散装程序记录** SHALL 落位于 `app_dir/adhoc/<name>.toml`（真实文件，`<name>` 即程序名），其占位文件同区域；该记录不属于任何 app，`[app]` 表可省略。
- compose 产物（可执行脚手架生成、`compose up` 渲染快照）与外部部署链接注册 SHALL 落位于 `app_dir` 顶层；**只有 compose 建立并管理的进程组才是 app**，顶层记录即 app 注册（历史遗留的顶层 run 文件按位置保留 app 语义，见末条）。
- `ak run` 产物 MUST NOT 写入 `app_dir` 顶层，也 MUST NOT 生成 app 记录或 app 名；因此 app 名空间只由 compose 家族产生。
- 名字唯一性 SHALL 跨全 `app_dir`（顶层与 `adhoc/`）判定，且跨「app 名 / 程序名」两个命名空间：同名记录不得两处并存，后到方按各自能力的撞名语义处理。
- 覆盖权限 SHALL 按记录位置判定：`run --force` 仅可覆盖 `adhoc/` 内的记录；顶层真实文件仅可由 compose 家族（脚手架再生、`compose up` 确认）改写；链接（外部部署）永不改写。
- `ak remove` 对 `adhoc/` 内的真实文件记录 SHALL 连带删除该文件（同既有脚手架语义）。
- `ak ps --all` SHALL 按记录位置标注注册来源：顶层记录（含链接）标注 `compose`，`adhoc/` 记录标注 `run`；标注仅为展示，不影响任何命令语义。
- 分区前已存在于 `app_dir` 顶层的 run 生成文件 SHALL 不迁移，继续按既有语义工作（其来源按位置推导归 `compose`）。

#### Scenario: run 产物落位 adhoc 子目录

- **WHEN** 执行 `ak run --name web -- python -m http.server 8000`
- **THEN** 注册记录为 `app_dir/adhoc/web.toml` 真实文件，`ak ps --all` 中标注来源 `run`

#### Scenario: 跨区同名冲突拒绝

- **WHEN** `app_dir/adhoc/web.toml` 存在（散装记录），执行 `ak compose up --name web`
- **THEN** 按撞名语义进入收编交互（或非交互下拒绝），MUST NOT 出现两处同名并存

#### Scenario: 移除 adhoc 记录删除本体

- **WHEN** 对位于 `app_dir/adhoc/` 的散装记录执行 `ak remove web`
- **THEN** 记录文件被删除且路径被打印，与顶层脚手架产物同语义

#### Scenario: ps --all 标注来源

- **WHEN** 注册表同时含顶层链接、顶层 `compose up` 快照与 `adhoc/` 散装记录，执行 `ak ps --all`
- **THEN** 三类记录分别标注 `compose`、`compose`、`run`

#### Scenario: 既有顶层 run 文件不受影响

- **WHEN** 升级前 `app_dir/web.toml` 已是 run 生成的真实文件，升级后启动守护进程
- **THEN** 该记录继续被识别与守护，不迁移、不报错，来源标注为 `compose`
