# app-registry Specification

## Purpose

定义应用注册生命周期：`ak compose / remove` 命令（支持从部署目录直接注册、微调 flag 写入 app 配置 `[app]` 表）、以 `app_dir` 链接为注册记录的持久化、注册时校验、离线可用与在线同步；注册表查看由 `ak ps --all` 承接。

## Requirements

### Requirement: 注册应用（compose）

`ak compose [path] [flags]` SHALL 注册一个应用，按 `path` 形态分派（原 `add` 动词的改名，行为等价；帮助文案与生成的脚手架注释 SHALL 按 compose 编排隐喻组织，为将来的声明式编排能力铺垫）：

- `path` 为部署目录（取其中默认名 `appkeeper.toml`，如 `ak compose .`）或直接指向 `.toml` 配置文件时，走既有注册流程：校验（文件存在、格式合法、程序名全局唯一、依赖不成环）；在 `app_dir` 创建以应用名命名的 `<name>.toml` 链接指向配置本体（链接即注册记录）；将微调 flag 幂等写入该 app 配置的 `[app]` 表（`--restart`、`--restart-backoff`、`--priority` 等，作为应用级默认；flag 写入失败 SHALL 回滚整个注册）。
- `path` 为其他存在的普通文件（可执行程序）时，走脚手架生成流程：SHALL 在 `app_dir` 下生成真实配置文件 `<name>.toml`（非链接，注册记录即文件本身），内容为单程序 app：program 名 = 应用名，`[app]` 表与 `[program.<name>]` 表按给定 flag 渲染。`command` SHALL 写绝对路径（canonicalize 解析后）；`work_dir` SHALL 缺省取执行 compose 命令时的当前工作目录（以绝对路径写入），可用 `--workdir <路径>` 覆盖。启动参数 SHALL 以 `--args "<字符串>"` 传入（按 shell 词法切分为 argv），环境变量 SHALL 以 `--env K=V` 传入（可重复）。应用名 SHALL 取 `--name`，缺省取可执行文件名（去扩展名的 stem）。脚手架 SHALL 至少同样支持：`--name`、`--description`、`--restart`、`--restart-backoff`、`--priority`。

命令行注册 SHALL 经由 `ak run` 的 ad-hoc 形态进入（动词面见 `control-plane`，注册落地仍在本能力域），命令行有两种等价入口、MUST 二选一：`--` 之后的整段 trailing 内容，或 `--cmd "<命令行字符串>"`（按 shell 词法切分，适合不便使用 `--` 的调用场景）。命令首 token SHALL 解析为绝对路径后写入 `command`——含路径分隔符时按 canonicalize 解析，裸命令名按调用方 `PATH` 解析（Windows 含 `PATHEXT` 扩展）；解析失败 SHALL 以退出码 2 报错且不产生任何注册产物。其余 token 按 shell 词法作为 `args` 写入。应用名 SHALL 取 `--name`，缺省取命令首 token 的文件名 stem。

命令行注册的**执行主体** SHALL 是守护进程（daemon-first，见 `control-plane`）：CLI SHALL 把命令行与微调 flag 交给守护进程，由守护进程完成「先启动、确认成功、后落盘」；CLI MUST NOT 自行写注册文件。因此命令解析失败、启动失败、健康探测未通过、落盘失败等任一情形下，`app_dir` MUST NOT 出现该注册文件或任何占位残留。

该形态 SHALL 支持脚手架 flag：`--env K=V`（可重复）、`--workdir <路径>`（缺省当前目录，写绝对路径）、`--description`、`--restart-backoff`、`--restart=<no|always|unless-stopped|on-failure>`（docker 字面量直书进生成配置，字段语义见 configuration 与 process-management 能力；缺省不写该字段、沿四层级联取内置默认 `no`）；MUST NOT 提供 `--args`（命令行本身已含参数），亦 MUST NOT 提供 `--autostart`/`--autorestart`（已由 `--restart` 取代）。

注册成功后 SHALL 立即以该 app 范围 apply 生效；`--restart=no` 时注册落地但程序不被拉起（覆盖「只注册命令行、暂不运行」的需求）。

应用名 `all` 为保留字（见 apply-workflow），compose 与 run ad-hoc MUST 拒绝以 `all` 注册（含 `--name all` 与文件名/命令名 stem 推导出的 `all`）。

重复执行：目录/`.toml` 来源的同名 compose 保持既有 upsert 语义（更新链接与 flag，不产生重复）；可执行程序来源的同名 compose SHALL **全量再生**——以本次给定参数重写整个生成文件（未给 flag 的字段回到缺省，文件上手工编辑的内容会被覆盖），再生前 MUST 校验通过才落盘。再生后 SHALL 将新内容与磁盘旧内容比对：有差异时打印变更提示并明确指出需执行 `ak apply <app>`（或全量 `ak apply`）方可生效；无差异时打印「无变更」类提示。run ad-hoc 命令行的同名重复提交**不**走隐式再生：未带 `--force` 时 MUST 按「run ad-hoc 同名应用拦截」要求拒绝，带 `--force` 时 MUST 按「run ad-hoc --force 强制覆盖」要求处理。

`compose --apply` SHALL 在注册/再生成功后立即对该 app 执行一次 apply（仅本 app 范围，MUST NOT 波及其他 app 的 pending）。守护进程离线时，`compose --apply` SHALL 自动后台拉起守护进程后继续应用（拉起协议、竞态容忍与超时语义详见 `background-daemon` 能力规范），MUST NOT 以「守护进程离线」报错退出；run 的 ad-hoc 形态**不再**享有该隐式拉起（见 `background-daemon`）。

注册与再生成功后 SHALL 以非交互纯文本完整打印：生成的配置文件路径、应用名、程序名、command、args、env、work_dir，以及下一步提示（在线时提示 pending 与 apply 命令）。

#### Scenario: 从部署目录注册并指定名字

- **WHEN** 在应用部署目录内执行 `ak compose . --name gateway --restart on-failure`
- **THEN** `app_dir` 出现以 gateway 命名的链接，部署目录 app 配置的 `[app]` 表写入 `restart = "on-failure"`，命令成功

#### Scenario: 未指定 --name 时取目录名

- **WHEN** 在 `/opt/myapp` 目录内执行 `ak compose .`（未给 `--name`）
- **THEN** 应用名为 `myapp`

#### Scenario: 守护进程运行中注册即时生效

- **WHEN** 守护进程运行中执行 compose（任一来源）
- **THEN** 注册持久化，并等效于触发一次 reload（重扫检出）：该应用进入 pending，程序尚未拉起；执行 `ak apply` 后按 `restart` 启动语义生效

#### Scenario: 从可执行程序脚手架生成并注册

- **WHEN** 在 `/opt/srv` 目录内执行 `ak compose ./web-server --name abc --args "--port 8080" --env LOG=debug`
- **THEN** `app_dir` 下生成真实文件 `abc.toml`，其中 program 名为 `abc`，`command` 为 `/opt/srv/web-server` 的绝对路径，`args = ["--port", "8080"]`，`env` 含 `LOG=debug`，`work_dir` 为 `/opt/srv`；命令完整打印上述生成信息并以 0 退出

#### Scenario: 命令行形态注册（PATH 解析）

- **WHEN** 执行 `ak run -- python -m http.server 8000 --name web`
- **THEN** 守护进程先启动该命令并确认成功，随后 `app_dir` 下生成真实文件 `web.toml`，`command` 为 `python` 经 `PATH` 解析出的绝对路径，`args = ["-m", "http.server", "8000"]`，注册后立即生效

#### Scenario: 命令行形态缺省名取命令 stem

- **WHEN** 执行 `ak run -- node server.js`（未给 `--name`）
- **THEN** 应用名与程序名为命令首 token 的文件名 stem（`node`）

#### Scenario: 命令解析失败以配置错误退出

- **WHEN** 执行 `ak run -- definitely-not-a-real-cmd --x`
- **THEN** 命令以退出码 2 报错说明无法解析，`app_dir` 无新增文件

#### Scenario: 命令行形态只注册不启动

- **WHEN** 执行 `ak run --cmd "python -m http.server 8000" --name web --restart=no`
- **THEN** `web` 注册落地（生成配置含 `restart = "no"`）但不被拉起，后续 `ak start web` 可启动它

#### Scenario: --cmd 字符串形态与 -- 等价

- **WHEN** 执行 `ak run --cmd "python -m http.server 8000" --name web`
- **THEN** 生成的注册与 `ak run -- python -m http.server 8000 --name web` 完全一致（同一 command/args/名称）

#### Scenario: --restart 字面量直书进生成配置

- **WHEN** 执行 `ak run -- node server.js --name worker --restart=unless-stopped`
- **THEN** 生成配置含 `restart = "unless-stopped"`，程序按其字面语义运行（崩溃自动重启、守护重启无停止记录则拉起）

#### Scenario: 命令行形态重复执行全量再生

> 标题为既有场景锚点，现行行为以下文为准：ad-hoc 同名重复提交不再隐式再生；未带 `--force` 时拒绝，带 `--force` 时按差异覆盖或空操作。

- **WHEN** 对已由命令行注册的 `web` 再次执行 `ak run -- python -m http.server 9000 --name web`
- **THEN** 注册被拒绝，`app_dir/web.toml` 保持原内容，既有 `web` 进程不被停止或重启

#### Scenario: 同名已在运行时按差异处理

- **WHEN** 对已注册且正在运行的 `web` 再次执行 `ak run -- python -m http.server 8000 --name web`（参数相同）
- **THEN** 注册被拒绝，既有 `web` 进程 PID 保持不变；不存在「幂等成功返回现有 PID」的放行路径

#### Scenario: 未指定 --name 时取可执行文件名

- **WHEN** 执行 `ak compose /opt/srv/web-server`（未给 `--name`）
- **THEN** 应用名为 `web-server`

#### Scenario: 生成配置使用绝对路径

- **WHEN** 以相对路径 `./target/release/proc` 执行脚手架 compose，或以 `run -- python ...` 命令行注册
- **THEN** 生成文件中 `command` 与 `work_dir` 均为绝对路径

#### Scenario: 重复执行无差异提示无变更

- **WHEN** 以完全相同参数对同名 app 再次执行脚手架 compose
- **THEN** 文件内容不变，输出「无变更」类提示，以 0 退出

#### Scenario: 重复执行有差异提示需 apply

- **WHEN** 以不同 `--args` 对同名 app 再次执行脚手架 compose
- **THEN** 生成文件被全量重写（手工编辑内容被覆盖），输出变更提示并指明需 `ak apply <app>` 生效；守护进程在线时该差异已进入 pending

#### Scenario: --apply 在线一步生效

- **WHEN** 守护进程运行中执行 `ak compose ./proc --name abc --apply`
- **THEN** 配置生成后自动以本 app 范围执行 apply，`abc` 的程序按 `restart` 启动语义处理，其他 app 的 pending 不受影响，输出含 apply 结果

#### Scenario: --apply 离线自动拉起

> 本变更将 run 的 ad-hoc 形态移出隐式拉起名单（标题为既有场景锚点，现行行为以下文为准）：`compose --apply` 仍自动拉起守护进程；run 的 ad-hoc 形态离线时改为退出码 3。

- **WHEN** 守护进程未运行时执行 `ak compose ./proc --name abc --apply`
- **THEN** 守护进程被自动后台拉起（详见 `background-daemon` 能力），随后该 app 的程序被拉起，命令成功退出；不出现「守护进程离线」报错

#### Scenario: 保留字 all 不可注册

- **WHEN** 执行 `ak compose ./proc --name all` 或 `ak run -- /path/to/all`
- **THEN** 注册被拒绝并说明 `all` 为 apply 全量保留字

#### Scenario: 注册被校验拒绝

- **WHEN** compose 指向不存在的文件，或生成的/给定的配置校验失败
- **THEN** 注册失败并指明原因，`app_dir` 保持不变

#### Scenario: 重复注册更新而非重复条目

- **WHEN** 对同名应用（目录或 `.toml` 来源）再次 compose 并修改 flag
- **THEN** 链接与 `[app]` 表字段被更新，不产生重复注册

### Requirement: 移除与查看

`ak remove <app>` SHALL 在守护进程在线时一步删除该应用的注册记录并立即从运行投影注销，MUST NOT 要求随后执行 `ak apply`。该命令为独立移除动作，MUST NOT 自动停止程序，MUST NOT 依赖 CLI 本地直接删除注册记录。

移除的前置条件是该 app 的全部程序均不处于 `running`、`starting`、`stopping` 或 `backoff`：前三种状态代表仍有存活进程或停机尚未收敛，`backoff` 代表等待自动重启。`stopped`、`exited` 与 `fatal` SHALL 允许移除，MUST NOT 要求用户曾显式执行 stop。前置条件不满足时 SHALL 以冲突错误拒绝，列出阻塞程序及状态并提示先执行 `ak stop <app>`，注册记录、程序状态与配置 MUST 保持不变。

删除成功后才 SHALL 清除该 app 的守护状态及关联的注销 pending，其他 app 的 pending MUST 保留；结果 SHALL 报告注册记录路径并表明已立即移除、无需 apply。注册记录为链接（外部部署目录注册）时，部署目录中的配置本体 MUST NOT 被删除；注册记录为位于 `app_dir` 顶层或 `app_dir/adhoc/` 内的真实文件（脚手架 / ad-hoc 生成，见「注册表分区与来源标注」）时，SHALL 连带删除该文件并打印被删除的路径。日志文件 MUST NOT 因移除而删除。

可移除目标 SHALL 以注册记录是否存在为准，MUST NOT 因记录内容无法解析而判定为未知 app：链接完好但 TOML 解析失败的 app 未被加载、也无运行程序，SHALL 允许移除该注册记录（外部配置本体保留），否则用户将失去清理损坏注册的唯一途径。

若注册记录已不存在但 app 仍处于守护状态（记录被手工删除），移除 SHALL 视为已满足：清除该 app 的运行状态与注销 pending，并以成功结果回报注册记录已不存在、已无需 apply；MUST NOT 返回服务端错误，亦 MUST NOT 声称 apply 无用或必需。

注册删除在确认成功前失败时，已加载程序与 app 状态 MUST NOT 被清理；响应 SHALL 报告注册删除失败，MUST NOT 建议以 apply 作为补救。若返回错误无法确认磁盘删除是否已生效，SHALL 明确提示核对注册路径与当前状态，MUST NOT 虚称注册必然保留。

注册表查看 SHALL 由 `ak ps` 承接（原顶层 `list` 动词移除）：`ak ps --all` 在线时展示含未运行 app 的全量视图，并按记录位置标注注册来源（顶层记录含链接为 `compose`、`adhoc/` 记录为 `run`，见「注册表分区与来源标注」）；离线时降级输出全部已注册应用（名称、配置本体路径、描述、来源标注，见 `control-plane` 能力规范）。

#### Scenario: 移除运行中的应用

> 本变更将该既有场景改为前置守卫：运行中的 app 不可直接 remove，须先停止。

- **WHEN** 守护进程运行中且 web 仍有 running 程序时执行 `ak remove web`
- **THEN** 命令以冲突错误拒绝，列出阻塞程序与状态并提示 `ak stop web`；进程、注册记录和配置均不变，也未自动停止进程

#### Scenario: 停止后一步移除无需 apply

- **WHEN** web 的全部程序已停止，执行 `ak remove web`
- **THEN** web 立即从守护状态消失，注册记录被清理，输出被删除或保留的配置路径；无需执行 `ak apply`，该 app 无注销 pending，其他 app 的 pending 不变

#### Scenario: 自然退出或 fatal 的应用可移除

- **WHEN** app 的程序处于 exited 或 fatal 且没有显式执行过 stop，执行 `ak remove <app>`
- **THEN** 前置检查通过，注册与运行投影按成功移除契约清理

#### Scenario: 启动中、停止中或退避中的应用被拒绝

- **WHEN** app 的任一程序处于 starting、stopping 或 backoff 时执行 `ak remove <app>`
- **THEN** 移除以冲突错误拒绝，不停机、不删除注册；对 backoff 程序提示先 stop 以取消自动重启

#### Scenario: 移除外部注册保留配置本体

- **WHEN** 对已停止的外部部署目录应用执行 `ak remove web`
- **THEN** 顶层注册链接被删除，部署目录中的 `appkeeper.toml` 仍存在，输出保留的配置本体路径

#### Scenario: 移除脚手架生成的应用删除生成文件

- **WHEN** 对已停止的脚手架生成 app 执行 `ak remove abc`
- **THEN** 顶层生成文件被删除且路径被打印，该 app 立即从注册表与状态投影消失

#### Scenario: 移除 adhoc 记录删除生成文件

- **WHEN** 对已停止、位于 `app_dir/adhoc/` 的 run 产物执行 `ak remove web`
- **THEN** `app_dir/adhoc/web.toml` 被删除且路径被打印，该 app 立即从注册表与状态投影消失

#### Scenario: 注册删除失败不清理守护状态

- **WHEN** 已停止 app 的注册文件因权限或 IO 错误无法确认删除成功
- **THEN** 命令报告删除失败，守护状态仍保留该 app 与程序，错误不以再次 apply 作为补救

#### Scenario: 列出注册表

> 本变更将 `list` 动词并入 `ps --all`（标题为既有场景锚点，现行行为以下文为准）。

- **WHEN** 守护进程未运行时执行 `ak ps --all`
- **THEN** 输出注明 daemon offline 的清单，含每个已注册应用的名称、配置本体路径、描述与来源标注

### Requirement: 离线可用

compose MUST NOT 依赖守护进程运行（注册表查看见 `ak ps --all` 的离线降级）：守护进程离线时 SHALL 直接维护 `app_dir` 链接完成注册（离线时 MUST NOT 因无法同步而报错）；守护进程在线时 SHALL 在持久化后通过控制 API 触发同步（重扫检出），使注册变化进入待应用（pending）状态，由 `ak apply` 落地生效。在线同步失败 SHALL 视为本次命令失败：错误信息 MUST 说明注册已持久化但守护进程未同步及补救方式（等待一个重扫周期（守护进程周期性重扫，见 `apply-workflow`）后重试 `ak apply`，或执行 `ak config --reload` 触发立即重扫后重试），并以非零码退出，MUST NOT 仅以警告带过。

remove MUST 依赖守护进程在线：守护进程离线时 SHALL 输出不可达提示并以退出码 3 退出，MUST NOT 直接删除注册记录、隐式拉起守护进程或留下待 apply 的移除。remove 不适用本需求的离线直写与在线 pending 规则，其原子移除语义见「移除与查看」。

例外：`ak compose up` 为守护进程执行（daemon-first）形态，不适用上述离线直写路径——守护进程离线时 SHALL 依 `background-daemon` 的拉起协议隐式后台拉起后继续（见「声明式编排（compose up）」），MUST NOT 离线写盘、MUST NOT 以离线报错退出。

`ak run` 的 ad-hoc 形态为**守护进程执行**（daemon-first），MUST NOT 适用上述离线直写路径：守护进程离线时该形态 SHALL 以退出码 3 报错，MUST NOT 离线写盘或隐式拉起守护进程。

#### Scenario: 离线批量注册后启动

- **WHEN** 守护进程停止状态下依次 compose 两个应用，随后启动守护进程
- **THEN** 两个应用按 priority 与依赖关系全部自动拉起

#### Scenario: 离线移除拒绝且不修改注册

- **WHEN** 守护进程未运行时执行 `ak remove web`
- **THEN** 命令以退出码 3 报守护进程不可达，不拉起守护进程，注册记录与配置本体均不变

#### Scenario: 在线同步失败不静默

- **WHEN** 守护进程在线但其重扫失败时执行 `ak compose`
- **THEN** 注册已持久化到 `app_dir`，命令以非零码退出，错误信息说明守护进程未同步并提示执行 `ak config --reload`（或等待一个重扫周期）后执行 `ak apply`

#### Scenario: 在线注册进入 pending 而非立即生效

- **WHEN** 守护进程在线时执行 `ak compose`，随后立即执行 `ak ps`
- **THEN** 注册同步成功（命令成功退出），新 app 的程序尚未运行，处于 pending；执行 `ak apply` 后按 `restart` 启动语义拉起

#### Scenario: compose up 离线隐式拉起守护

- **WHEN** 守护进程未运行时执行 `ak compose up`
- **THEN** CLI 隐式后台拉起守护进程后完成注册与启动，不写离线直写产物、不以离线报错退出

#### Scenario: 离线 run ad-hoc 退出 3

- **WHEN** 守护进程未运行时执行 `ak run -- ./server --app web`
- **THEN** 以退出码 3 报守护进程不可达，`app_dir` 不变，守护进程未被拉起

### Requirement: 启动成功后再落盘的注册契约

ad-hoc 注册（run 的 `--`/`--cmd` 形态）的落盘 SHALL 遵循「先确认启动成功、后写入注册记录」的顺序，且写入 SHALL 具备原子性与失败无残留保证：

- 落盘 SHALL 采用**临时文件占位 + 原子 rename**：守护进程先把渲染好的配置写入 `app_dir` 内一个可被扫描器忽略的临时文件名（临时名 MUST NOT 以 `.toml` 结尾，以免被注册扫描当作注册记录），待启动判定成功后再同目录 `rename` 为目标 `<name>.toml`；同目录 rename 为原子操作，扫描器任一时刻要么看不到该注册、要么看到完整注册，MUST NOT 看到半成品。
- 写临时文件失败（权限不足、磁盘错误等）SHALL 在启动判定**之前**暴露：守护进程 SHALL NOT 先启动进程再落盘失败；无法建立占位时 SHALL 直接以错误回传且不启动进程。
- 启动判定失败（进程在 `startsecs` 内退出、`startretries` 耗尽、健康探测未通过）时，守护进程 SHALL 删除占位临时文件并停止已启动的进程，`app_dir` MUST NOT 留下残留。
- 目标注册文件的写入 SHALL 有界重试（应对瞬时 IO 抖动）；重试仍失败时，守护进程 SHALL 保留已启动的进程并回传**部分失败**结果：明确告知「进程已启动但注册未落盘」，并打印完整配置信息（应用名、程序名、command、args、env、work_dir、配置文件目标路径）供用户手动落盘。
- 目标 `<name>.toml` 已存在且内容一致时 SHALL 幂等成功（不重复写盘、不重启进程）。

#### Scenario: 原子落盘对扫描器不可见半成品

- **WHEN** ad-hoc 注册正在进行、启动判定尚未完成时守护进程执行一次重扫
- **THEN** 该应用不出现在注册列表/pending 中，`app_dir` 内无 `.toml` 残留（仅有非 `.toml` 占位）

#### Scenario: 占位建立失败不启动进程

- **WHEN** `app_dir` 不可写（如归属其他用户）时执行 `ak run -- ./server --name web`
- **THEN** 守护进程以写盘错误回传，退出非零，不启动进程，`app_dir` 无残留

#### Scenario: 启动失败清理占位

- **WHEN** ad-hoc 注册的命令在 `startsecs` 内退出并最终判定启动失败
- **THEN** 守护进程删除占位临时文件并停止该进程，`app_dir` 无 `.toml` 文件

#### Scenario: 落盘重试后成功

- **WHEN** 首次 rename/写盘因瞬时错误失败、重试成功
- **THEN** 注册最终落盘成功，命令以 0 退出并返回 PID

#### Scenario: 部分失败保留进程并打印配置

- **WHEN** 启动判定成功但目标注册文件写入在重试后仍失败
- **THEN** 进程继续运行，守护进程回传部分失败（含「已启动但未落盘」说明与完整配置信息），CLI 以非零码退出并打印该配置信息

#### Scenario: 幂等重注册无变化

- **WHEN** 对已注册且内容一致的 `web` 再次执行相同的 ad-hoc 注册
- **THEN** 不重复写盘、不重启进程，幂等成功并返回现有 PID

### Requirement: 残留态的提示与确认

对上次「启动成功但落盘失败」而留下的进程（未注册但同名程序在运行），再次以相同参数执行 ad-hoc 注册时，守护进程 SHALL 回传该残留状况，CLI SHALL 提示当前状况并询问用户是否重新落盘；用户确认后 SHALL 仅完成落盘（复用已在运行的进程，MUST NOT 重复启动或重启）；用户拒绝时 SHALL 以非零码退出且不写盘。

#### Scenario: 检测到残留并询问

- **WHEN** 上次 ad-hoc 注册因落盘失败留下运行中的 `web`（未注册），用户再次执行相同参数的 `ak run -- ./server --name web`
- **THEN** CLI 提示「该进程已启动但未注册」并询问是否重新落盘

#### Scenario: 确认后补落盘不重启

- **WHEN** 用户对上述提示确认重新落盘
- **THEN** 守护进程写入 `app_dir/web.toml`，复用已在运行的进程（PID 不变），命令成功退出

#### Scenario: 拒绝后不写盘

- **WHEN** 用户对上述提示选择不重新落盘
- **THEN** 命令以非零码退出，`app_dir` 不变，进程保持运行

### Requirement: run ad-hoc 同名应用拦截

`ak run` 的 ad-hoc 两种形态（`--` trailing 与 `--cmd`）SHALL 把同名判定交给守护进程；当应用名对应的注册记录已存在（无论位于 `app_dir` 顶层还是 `app_dir/adhoc/`），或内存中已有同名应用（含已注册但未运行的程序），且**未指定 `--force`** 时，守护进程 SHALL 拒绝本次注册并回传结构化冲突错误，MUST NOT 写盘、MUST NOT 启动新进程、MUST NOT 停止或重启既有进程。

拒绝信息 SHALL 明确指出该应用名已存在并给出可操作指引（改用其他 `--app` 名称，或显式加 `--force` 覆盖）。CLI SHALL 以非零退出码退出。

「已启动但未落盘」的残留态不属本要求：该场景仍按 `app-registry` 的残留确认流程处理，且 MUST NOT 重启既有进程。

#### Scenario: 同名已注册且运行中被拒绝

- **WHEN** 对已由 `ak run` 注册且正在运行的 `web` 再次执行 `ak run -- python -m http.server 9000 --app web`
- **THEN** 守护进程回传冲突错误，既有 `web` 进程的 PID 与 `app_dir/adhoc/web.toml` 内容均保持不变，CLI 以非零退出码退出

#### Scenario: 同名已注册但未运行也被拒绝

- **WHEN** 对已注册但当前未运行的 `web` 再次执行 `ak run -- python -m http.server 9000 --app web`
- **THEN** 注册被拒绝，`app_dir/adhoc/web.toml` 不被改写，不产生新进程

#### Scenario: --restart=no 同样受拦截

- **WHEN** 对已存在的 `web` 执行 `ak run --cmd "python -m http.server 9000" --app web --restart=no`
- **THEN** 注册被拒绝，不写盘；`--restart=no` 不构成覆盖既有应用的例外

#### Scenario: 参数完全相同也不再生

- **WHEN** 对已注册的 `web` 以完全相同的命令行与参数再次执行 `ak run`
- **THEN** 仍以冲突错误拒绝，MUST NOT 返回「幂等成功」或现有 PID

#### Scenario: 残留态仍走确认流程

- **WHEN** 上一次 run 已启动同名程序但落盘失败，且注册记录不存在
- **THEN** 守护进程回传残留态而非冲突错误，CLI 询问用户是否仅补落盘；确认后复用已在运行的进程，MUST NOT 重启

### Requirement: run ad-hoc --force 强制覆盖

`ak run --force` SHALL 允许覆盖同名已注册应用。覆盖判定 SHALL 在守护进程内完成：把本次命令行渲染出的完整配置文本与磁盘现有注册记录逐字节比较。覆盖目标 SHALL 以记录位置判定：`--force` 仅可覆盖位于 `app_dir/adhoc/` 的记录（run 自家产物）；同名记录位于 `app_dir` 顶层（compose 生成文件或外部部署链接，见「注册表分区与来源标注」）时 SHALL 拒绝覆盖，错误信息提示先 `ak remove <name>` 或改用 `ak compose up`，`--force` 不放行。

- **有差异**：SHALL 在通过全部校验后原子重写注册记录（占位 + rename），随后停止旧的同名程序并按新定义重新拉起；`--restart=no` 时只重写记录、不拉起（旧进程 SHALL 被停止，避免旧定义继续运行）。返回新 PID（`--restart=no` 时为 `null`）。
- **无差异**：MUST NOT 写盘、MUST NOT 停止或重启进程、MUST NOT 拉起新进程；CLI SHALL 仅提示「配置无修改，不做任何动作」并以退出码 0 退出；`--json` 结果 SHALL 标记 `unchanged: true` 并回传现有 PID（若有）。

`--force` SHALL NOT 绕过其他既有关卡：与**其他应用**的程序名冲突仍被拒绝；目标记录不是本工具生成的文件（如指向外部部署配置的链接或手工文件）时仍被拒绝覆盖。

「已启动但未落盘」的残留态 SHALL NOT 被 `--force` 改写：仍按残留确认流程处理。

#### Scenario: 有差异时覆盖并重启

- **WHEN** 对正在运行且已注册的 `web` 执行 `ak run --force -- python -m http.server 9000 --app web`
- **THEN** `app_dir/adhoc/web.toml` 被原子重写为新命令行，旧进程被停止，新进程启动并通过启动判定，CLI 回传新 PID

#### Scenario: 无差异时不做任何动作

- **WHEN** 对正在运行且已注册的 `web` 以完全相同的新旧配置执行 `ak run --force -- python -m http.server 8000 --app web`
- **THEN** `app_dir/adhoc/web.toml` 字节不变、进程 PID 不变、不发生 stop/spawn，CLI 仅提示「配置无修改，不做任何动作」并以 0 退出

#### Scenario: 已注册未运行时有差异

- **WHEN** 对已注册但当前未运行的 `web` 执行带不同参数的 `ak run --force`
- **THEN** 记录被重写并拉起新进程

#### Scenario: 已注册未运行且无差异

- **WHEN** 对已注册但当前未运行的 `web` 以相同配置执行 `ak run --force`
- **THEN** 不写盘、不拉起进程，仅提示「配置无修改，不做任何动作」并以 0 退出

#### Scenario: --restart=no 强制覆盖

- **WHEN** 对正在运行的 `web` 执行带不同参数的 `ak run --force --restart=no`
- **THEN** 记录被重写，旧进程被停止，新定义不被拉起

#### Scenario: 跨应用程序名冲突不因 --force 放行

- **WHEN** `--force` 的目标应用名与**其他应用**已注册的程序名冲突
- **THEN** 注册仍被拒绝，不写盘、不启动进程

#### Scenario: 顶层 compose 记录不被 run --force 覆盖

- **WHEN** 同名注册记录位于 `app_dir` 顶层（`compose up` 快照或脚手架生成文件），执行 `ak run --force --app <name> …`
- **THEN** 覆盖被拒绝并提示 `ak remove` 或 `ak compose up`，目标文件与进程均不变

#### Scenario: 外部部署配置不被 --force 覆盖

- **WHEN** 同名注册记录是指向外部部署配置的链接或非本工具生成文件
- **THEN** `--force` 覆盖被拒绝并说明原因，目标文件不变

#### Scenario: 强制覆盖失败时的处置

- **WHEN** `--force` 的新定义校验失败，或新进程未通过启动判定
- **THEN** 校验失败时目标文件保持原样且无占位残留；启动判定失败时新进程被停止并以结构化错误回传，已原子落盘的新记录保留（覆盖已生效，不回滚旧记录）

### Requirement: 声明式编排（compose up）

`ak compose up [dir]` SHALL 以项目声明文件 `.appkeeper.toml` 为唯一事实源，把其中声明的整组程序交由守护进程启动并注册（daemon-first，端点族与流式进度见 `control-plane` 能力规范）。

**文件与发现**：

- `.appkeeper.toml` SHALL 为一份合法的 app 部署配置（字段域与校验见 `configuration` 能力）：`[app]` 表可选（承载组级默认值与描述），`[program.<名>]` 表至少一个，未知字段 SHALL 被校验拒绝。本能力不为其新增任何配置字段。
- `dir` 缺省为 `.`；`compose up` SHALL 只在 `dir` 下寻找 `.appkeeper.toml`，MUST NOT 向上级目录递归查找；找不到时 SHALL 以非零码报错并列出查找过的路径。`-f/--file <路径>` SHALL 显式指定声明文件，此时文件名不受限。
- 应用名 SHALL 缺省取声明文件所在目录的目录名（stem），`--name` 显式覆盖；名字 SHALL 满足既有命名规则，保留字 `all` MUST 拒绝。`[app]` 表 MUST NOT 引入 name 字段（名字属于调用现场，同一份声明可以从不同名字部署多份）。
- `compose up` SHALL 仅接受 `-f/--file`、`--name`、`--force`、`--json` 与 `-e/--env NAME=VALUE`（可重复）五类旗标；内容微调旗标（`--restart`、`--priority` 等）MUST 以用法错误拒绝——声明文件是唯一内容事实源。`up` 为 `ak compose` 的保留子命令词，MUST NOT 被当作 path 参与形态分派。
- `-e NAME=VALUE` SHALL 在解析前替换声明文本中的 `${NAME}` 占位符；`${NAME:-default}` 在未传值时才用默认值。未传值且无默认值的 `${NAME}`、以及 `${program.<名>.<字段>}` 等运行期动作变量 SHALL 原样保留（交给 shell / 运行期替换层），MUST NOT 因未知而报错。`$$` SHALL 表示字面 `$`（`$${NAME}` 原样抵达程序）。名字须为环境变量形态（字母或 `_` 开头，仅字母数字与 `_`），否则以校验类错误拒绝。替换仅作用于本次调用的渲染，MUST NOT 持久化变量值（落盘的是替换后的快照）。

**渲染与落盘**：

- 渲染 SHALL 把相对路径以声明文件所在目录为基准重写为绝对路径后写入注册记录；`command` SHALL 沿用既有单行带参写法（按 shell 词法拆分为 argv、不经过 shell、与显式 `args` 互斥，见 `configuration` 能力），拆分后的首 token 为裸命令名时 SHALL 按提交方环境解析为绝对路径（与 run ad-hoc 的命令解析规则一致）。
- 注册记录 SHALL 为渲染快照的真实文件，落位于 `app_dir` 顶层 `<name>.toml`；MUST NOT 以链接指向 `.appkeeper.toml`。声明文件与注册记录允许漂移，漂移收敛由下述撞名语义完成。

**执行流水线（daemon-first）**：

- 守护进程 SHALL 按 priority 与 depends_on 拓扑拉起全部声明的程序（落盘前 staging），逐个等待启动判定（`startsecs`、健康探测）通过；**全部通过后** SHALL 以占位 + 原子 rename 落盘注册记录。
- 任一程序启动判定失败时：SHALL 停止本轮已拉起的程序、清理全部占位残留（`app_dir` 无该注册文件或 pending 残留）、以非零码退出并指明失败的程序与原因。
- 拉起 SHALL 遵循守护启动同款规则：`restart=no` 的程序不拉起；`unless-stopped` 存在用户停止记录的程序保持停止并打印提示（`ak start <app> <程序>` 可拉起）。`compose up` MUST NOT 写入或清除用户停止记录。
- 守护进程离线时，CLI SHALL 依 `background-daemon` 能力的拉起协议隐式后台拉起守护进程后继续执行（与 `compose --apply` 同族），MUST NOT 以守护离线报错退出，MUST NOT 走离线直写路径。
- 进度 SHALL 按 `control-plane`「启动进度流式反馈」语义流式回传（逐程序事件），`--json` 输出机器可读流。

**同名撞名语义**（提交前 SHALL 先经预览比对，端点分类见 `control-plane`）：

- 分类 `identical`（渲染结果与注册记录逐字节相同）：一致且程序按策略在运行 → 无操作，提示后以退出码 0 退出，MUST NOT 触碰运行中的进程；有程序未在跑 → 按守护启动规则拉起（ensure-running），因用户停止记录未拉起的按上段提示。
- 分类 `needs-update`（存在同名 compose 记录且内容有差异）：交互式确认——CLI SHALL 在 TTY 上询问是否强制更新；答「是」（或提交带 `--force`）：停止该 app 既有程序 → 原子重写注册记录 → 按新配置拉起；答「否」：零改动、提示未做任何更改、以非零码退出。
- 分类 `adhoc-takeover`（同名记录位于 `app_dir/adhoc/`，为 run 产物）：走同一交互；确认后停止该程序、删除 adhoc 记录、落 compose 记录并按声明拉起（收编）。
- 分类 `external-refused`（同名记录为外部部署链接）：MUST 拒绝且不进交互，提示先执行 `ak remove <name>`；`--force` 也不放行。
- 交互不可用（非 TTY，如管道 / CI）：`needs-update` 与 `adhoc-takeover` SHALL 默认拒绝——零副作用、非零码退出、提示以 `--force` 显式跳过交互；`--force` 为交互「是」的直通版（行为等同确认后）。

#### Scenario: 全新声明注册并按序拉起

- **WHEN** 项目根有含 `web`（priority 10）与 `worker`（depends_on `web`）的 `.appkeeper.toml`，执行 `ak compose up`
- **THEN** 守护进程先拉起 `web` 再拉起 `worker`，二者均过启动判定后 `app_dir/<目录名>.toml` 落盘，命令以 0 退出并打印逐程序结果

#### Scenario: 找不到声明文件

- **WHEN** 在没有 `.appkeeper.toml` 的目录执行 `ak compose up`
- **THEN** 以非零码报错并列出查找过的路径，不联系守护进程、不产生任何注册产物

#### Scenario: 保留字与非法名拒绝

- **WHEN** 声明文件位于名为 `all` 的目录且未给 `--name`，或 `--name` 不满足命名规则
- **THEN** 以校验错误拒绝，零副作用

#### Scenario: 内容微调旗标被拒绝

- **WHEN** 执行 `ak compose up --restart always`
- **THEN** 以用法错误（非零码）拒绝并说明 `up` 不接受内容微调旗标，配置内容只能改声明文件

#### Scenario: -e 替换声明占位符

- **WHEN** `.appkeeper.toml` 中写有 `${PORT}`、`${MODE:-dev}` 与 `${HOME}`，执行 `ak compose up -e PORT=8080`
- **THEN** 注册记录中 `${PORT}` 渲染为 `8080`、`${MODE:-dev}` 渲染为 `dev`，而 `${HOME}` 原样保留（交给程序的 shell）；变量值不落盘

#### Scenario: 单行 command 与相对路径渲染

- **WHEN** `.appkeeper.toml` 位于 `/opt/shop/`，其中 `work_dir = "./data"`、`command = "python -m http.server 8000"`（未写 `args`）
- **THEN** 注册记录中 `work_dir` 渲染为 `/opt/shop/data`，`command` 为按提交方环境解析出的绝对路径、`args` 为 `["-m", "http.server", "8000"]`

#### Scenario: 同名一致且在运行为无操作

- **WHEN** 对已由 `compose up` 注册且全部程序按策略运行的 app，未改动声明文件再次执行 `ak compose up`
- **THEN** 提示无变化并以 0 退出，注册记录与进程均不变

#### Scenario: 同名一致但未运行则拉起

- **WHEN** 同名注册记录与渲染结果逐字节一致，但部分程序未在运行且无用户停止记录
- **THEN** 未运行的程序按守护启动规则被拉起，注册记录不被改写

#### Scenario: 用户停止记录被尊重

- **WHEN** 声明中某 `unless-stopped` 程序存在用户停止记录，执行 `ak compose up`
- **THEN** 该程序保持停止并打印提示，其余程序正常收敛，记录不被清除

#### Scenario: 有差异时交互确认更新

- **WHEN** 已注册的同名 app 与修改后的 `.appkeeper.toml` 渲染结果有差异，TTY 上执行 `ak compose up` 并确认
- **THEN** 既有程序被停止、注册记录被原子重写、新配置按序拉起，命令以 0 退出

#### Scenario: 有差异时交互拒绝

- **WHEN** 同上场景但用户答「否」
- **THEN** 零改动（不写盘、不启停进程），提示未做任何更改，以非零码退出

#### Scenario: 非交互环境默认拒绝

- **WHEN** 在非 TTY 环境（管道/CI）对有差异的同名 app 执行 `ak compose up`
- **THEN** 零副作用拒绝并以非零码退出，错误信息提示加 `--force` 显式跳过交互；改用 `--force` 后行为等同交互确认「是」

#### Scenario: 收编 run 的 adhoc 记录

- **WHEN** 同名记录位于 `app_dir/adhoc/`（此前由 `ak run` 拉起），执行 `ak compose up` 并确认
- **THEN** 该程序被停止、adhoc 记录被删除、compose 记录落盘于顶层并按声明拉起

#### Scenario: 外部部署链接拒绝

- **WHEN** 同名注册为指向外部部署配置的链接，执行 `ak compose up`（含带 `--force`）
- **THEN** 拒绝并提示先 `ak remove <name>`，不进入交互，目标与进程均不变

#### Scenario: 启动失败零残留

- **WHEN** 声明中某程序在 `startsecs` 内退出导致启动判定失败
- **THEN** 本轮已拉起的程序被停止，`app_dir` 无该注册文件与占位残留，以非零码退出并指明失败的程序

#### Scenario: 守护离线隐式拉起

- **WHEN** 守护进程未运行时执行 `ak compose up`
- **THEN** CLI 后台拉起守护进程后继续完成注册与启动，不以离线报错退出

### Requirement: 注册表分区与来源标注

`app_dir` SHALL 按「注册产物来源」分区，使 run 与 compose 的落盘记录可从路径区分：

- `ak run` ad-hoc 形态产生的注册记录 SHALL 落位于 `app_dir/adhoc/<name>.toml`（真实文件），其占位文件同区域。
- compose 产物（可执行脚手架生成、`compose up` 渲染快照）与外部部署链接注册 SHALL 落位于 `app_dir` 顶层。
- 应用名唯一性 SHALL 跨全 `app_dir`（顶层与 `adhoc/`）判定：同名记录不得两处并存，后到方按各自能力的撞名语义处理。
- 覆盖权限 SHALL 按记录位置判定：`run --force` 仅可覆盖 `adhoc/` 内的记录；顶层真实文件仅可由 compose 家族（脚手架再生、`compose up` 确认）改写；链接（外部部署）永不改写。
- `ak remove` 对 `adhoc/` 内的真实文件记录 SHALL 连带删除该文件（同既有脚手架语义）。
- `ak ps --all` SHALL 按记录位置标注注册来源：顶层记录（含链接）标注 `compose`，`adhoc/` 记录标注 `run`；标注仅为展示，不影响任何命令语义。
- 分区前已存在于 `app_dir` 顶层的 run 生成文件 SHALL 不迁移，继续按既有语义工作（其来源按位置推导归 `compose`）。

#### Scenario: run 产物落位 adhoc 子目录

- **WHEN** 执行 `ak run -- python -m http.server 8000 --name web`
- **THEN** 注册记录为 `app_dir/adhoc/web.toml` 真实文件，`ak ps --all` 中标注来源 `run`

#### Scenario: 跨区同名冲突拒绝

- **WHEN** `app_dir/adhoc/web.toml` 存在（run 产物），执行 `ak compose up --name web`
- **THEN** 按撞名语义进入收编交互（或非交互下拒绝），MUST NOT 出现两处同名并存

#### Scenario: 移除 adhoc 记录删除本体

- **WHEN** 对位于 `app_dir/adhoc/` 的 run 产物执行 `ak remove web`
- **THEN** 记录文件被删除且路径被打印，与顶层脚手架产物同语义

#### Scenario: ps --all 标注来源

- **WHEN** 注册表同时含顶层链接、顶层 `compose up` 快照与 `adhoc/` run 产物，执行 `ak ps --all`
- **THEN** 三类记录分别标注 `compose`、`compose`、`run`

#### Scenario: 既有顶层 run 文件不受影响

- **WHEN** 升级前 `app_dir/web.toml` 已是 run 生成的真实文件，升级后启动守护进程
- **THEN** 该记录继续被识别与守护，不迁移、不报错，来源标注为 `compose`
