## ADDED Requirements

### Requirement: run ad-hoc 同名应用拦截

`ak run` 的 ad-hoc 两种形态（`--` trailing 与 `--cmd`）SHALL 把同名判定交给守护进程；当应用名对应的注册记录已存在，或内存中已有同名应用（含已注册但未运行的程序），且**未指定 `--force`** 时，守护进程 SHALL 拒绝本次注册并回传结构化冲突错误，MUST NOT 写盘、MUST NOT 启动新进程、MUST NOT 停止或重启既有进程。

拒绝信息 SHALL 明确指出该应用名已存在并给出可操作指引（改用其他 `--app` 名称，或显式加 `--force` 覆盖）。CLI SHALL 以非零退出码退出。

「已启动但未落盘」的残留态不属本要求：该场景仍按 `app-registry` 的残留确认流程处理，且 MUST NOT 重启既有进程。

#### Scenario: 同名已注册且运行中被拒绝

- **WHEN** 对已由 `ak run` 注册且正在运行的 `web` 再次执行 `ak run -- python -m http.server 9000 --app web`
- **THEN** 守护进程回传冲突错误，既有 `web` 进程的 PID 与 `app_dir/web.toml` 内容均保持不变，CLI 以非零退出码退出

#### Scenario: 同名已注册但未运行也被拒绝

- **WHEN** 对已注册但当前未运行的 `web` 再次执行 `ak run -- python -m http.server 9000 --app web`
- **THEN** 注册被拒绝，`app_dir/web.toml` 不被改写，不产生新进程

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

`ak run --force` SHALL 允许覆盖同名已注册应用。覆盖判定 SHALL 在守护进程内完成：把本次命令行渲染出的完整配置文本与磁盘现有注册记录逐字节比较。

- **有差异**：SHALL 在通过全部校验后原子重写注册记录（占位 + rename），随后停止旧的同名程序并按新定义重新拉起；`--restart=no` 时只重写记录、不拉起（旧进程 SHALL 被停止，避免旧定义继续运行）。返回新 PID（`--restart=no` 时为 `null`）。
- **无差异**：MUST NOT 写盘、MUST NOT 停止或重启进程、MUST NOT 拉起新进程；CLI SHALL 仅提示「配置无修改，不做任何动作」并以退出码 0 退出；`--json` 结果 SHALL 标记 `unchanged: true` 并回传现有 PID（若有）。

`--force` SHALL NOT 绕过其他既有关卡：与**其他应用**的程序名冲突仍被拒绝；目标记录不是本工具生成的脚手架文件（如指向外部部署配置的链接或手工文件）时仍被拒绝覆盖。

「已启动但未落盘」的残留态 SHALL NOT 被 `--force` 改写：仍按残留确认流程处理。

#### Scenario: 有差异时覆盖并重启

- **WHEN** 对正在运行且已注册的 `web` 执行 `ak run --force -- python -m http.server 9000 --app web`
- **THEN** `app_dir/web.toml` 被原子重写为新命令行，旧进程被停止，新进程启动并通过启动判定，CLI 回传新 PID

#### Scenario: 无差异时不做任何动作

- **WHEN** 对正在运行且已注册的 `web` 以完全相同的新旧配置执行 `ak run --force -- python -m http.server 8000 --app web`
- **THEN** `app_dir/web.toml` 字节不变、进程 PID 不变、不发生 stop/spawn，CLI 仅提示「配置无修改，不做任何动作」并以 0 退出

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

#### Scenario: 外部部署配置不被 --force 覆盖

- **WHEN** 同名注册记录是指向外部部署配置的链接或非脚手架生成文件
- **THEN** `--force` 覆盖被拒绝并说明原因，目标文件不变

#### Scenario: 强制覆盖失败时的处置

- **WHEN** `--force` 的新定义校验失败，或新进程未通过启动判定
- **THEN** 校验失败时目标文件保持原样且无占位残留；启动判定失败时新进程被停止并以结构化错误回传，已原子落盘的新记录保留（覆盖已生效，不回滚旧记录）

## MODIFIED Requirements

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
