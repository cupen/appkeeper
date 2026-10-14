## MODIFIED Requirements

### Requirement: 注册应用（add）

`ak add [path] [flags]` SHALL 注册一个应用，按 `path` 形态分派：

- `path` 为部署目录（取其中默认名 `appkeeper.toml`，如 `ak add .`）或直接指向 `.toml` 配置文件时，走既有注册流程：校验（文件存在、格式合法、程序名全局唯一、依赖不成环）；在 `app_dir` 创建以应用名命名的 `<name>.toml` 链接指向配置本体（链接即注册记录）；将微调 flag 幂等写入该 app 配置的 `[app]` 表（`--restart`、`--restart-backoff`、`--priority` 等，作为应用级默认；flag 写入失败 SHALL 回滚整个注册）。
- `path` 为其他存在的普通文件（可执行程序）时，走脚手架生成流程：SHALL 在 `app_dir` 下生成真实配置文件 `<name>.toml`（非链接，注册记录即文件本身），内容为单程序 app：program 名 = 应用名，`[app]` 表与 `[program.<name>]` 表按给定 flag 渲染。`command` SHALL 写绝对路径（canonicalize 解析后）；`work_dir` SHALL 缺省取执行 add 命令时的当前工作目录（以绝对路径写入），可用 `--workdir <路径>` 覆盖。启动参数 SHALL 以 `--args "<字符串>"` 传入（按 shell 词法切分为 argv），环境变量 SHALL 以 `--env K=V` 传入（可重复）。应用名 SHALL 取 `--name`，缺省取可执行文件名（去扩展名的 stem）。脚手架 SHALL 至少同样支持：`--name`、`--description`、`--restart`、`--restart-backoff`、`--priority`。

命令行注册 SHALL 经由 `ak run` 的 ad-hoc 形态进入（动词面见 `control-plane`，注册落地仍在本能力域），命令行有两种等价入口、MUST 二选一：`--` 之后的整段 trailing 内容，或 `--cmd "<命令行字符串>"`（按 shell 词法切分，适合不便使用 `--` 的调用场景）。命令首 token SHALL 解析为绝对路径后写入 `command`——含路径分隔符时按 canonicalize 解析，裸命令名按调用方 `PATH` 解析（Windows 含 `PATHEXT` 扩展）；解析失败 SHALL 以退出码 2 报错且不产生任何注册产物。其余 token 按 shell 词法作为 `args` 写入。应用名 SHALL 取 `--name`，缺省取命令首 token 的文件名 stem。

命令行注册的**执行主体** SHALL 是守护进程（daemon-first，见 `control-plane`）：CLI SHALL 把命令行与微调 flag 交给守护进程，由守护进程完成「先启动、确认成功、后落盘」；CLI MUST NOT 自行写注册文件。因此命令解析失败、启动失败、健康探测未通过、落盘失败等任一情形下，`app_dir` MUST NOT 出现该注册文件或任何占位残留。

该形态 SHALL 支持脚手架 flag：`--env K=V`（可重复）、`--workdir <路径>`（缺省当前目录，写绝对路径）、`--description`、`--restart-backoff`、`--restart=<no|always|unless-stopped|on-failure>`（docker 字面量直书进生成配置，字段语义见 configuration 与 process-management 能力；缺省不写该字段、沿四层级联取内置默认 `no`）；MUST NOT 提供 `--args`（命令行本身已含参数），亦 MUST NOT 提供 `--autostart`/`--autorestart`（已由 `--restart` 取代）。

注册成功后 SHALL 立即以该 app 范围 apply 生效；`--restart=no` 时注册落地但程序不被拉起（覆盖「只注册命令行、暂不运行」的需求）。

应用名 `all` 为保留字（见 apply-workflow），add 与 run ad-hoc MUST 拒绝以 `all` 注册（含 `--name all` 与文件名/命令名 stem 推导出的 `all`）。

重复执行：目录/`.toml` 来源的同名 add 保持既有 upsert 语义（更新链接与 flag，不产生重复）；可执行程序与 run ad-hoc 命令行来源的同名注册 SHALL **全量再生**——以本次给定参数重写整个生成文件（未给 flag 的字段回到缺省，文件上手工编辑的内容会被覆盖），再生前 MUST 校验通过才落盘。再生后 SHALL 将新内容与磁盘旧内容比对：有差异时打印变更提示并明确指出需执行 `ak apply <app>`（或全量 `ak apply`）方可生效（run ad-hoc 因立即生效而当场应用）；无差异时打印「无变更」类提示。

同名 ad-hoc 注册且该程序**已在运行**时：定义无变化 SHALL 幂等成功并返回现有 PID；定义有变化 SHALL 重启并返回新 PID。

`add --apply` SHALL 在注册/再生成功后立即对该 app 执行一次 apply（仅本 app 范围，MUST NOT 波及其他 app 的 pending）。守护进程离线时，`add --apply` SHALL 自动后台拉起守护进程后继续应用（拉起协议、竞态容忍与超时语义详见 `background-daemon` 能力规范），MUST NOT 以「守护进程离线」报错退出；run 的 ad-hoc 形态**不再**享有该隐式拉起（见 `background-daemon`）。

注册与再生成功后 SHALL 以非交互纯文本完整打印：生成的配置文件路径、应用名、程序名、command、args、env、work_dir，以及下一步提示（在线时提示 pending 与 apply 命令）。

#### Scenario: 从部署目录注册并指定名字

- **WHEN** 在应用部署目录内执行 `ak add . --name gateway --restart on-failure`
- **THEN** `app_dir` 出现以 gateway 命名的链接，部署目录 app 配置的 `[app]` 表写入 `restart = "on-failure"`，命令成功

#### Scenario: 未指定 --name 时取目录名

- **WHEN** 在 `/opt/myapp` 目录内执行 `ak add .`（未给 `--name`）
- **THEN** 应用名为 `myapp`

#### Scenario: 守护进程运行中注册即时生效

- **WHEN** 守护进程运行中执行 add（任一来源）
- **THEN** 注册持久化，并等效于触发一次 reload（重扫检出）：该应用进入 pending，程序尚未拉起；执行 `ak apply` 后按 `restart` 启动语义生效

#### Scenario: 从可执行程序脚手架生成并注册

- **WHEN** 在 `/opt/srv` 目录内执行 `ak add ./web-server --name abc --args "--port 8080" --env LOG=debug`
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
- **THEN** `web` 注册落地（生成配置含 `restart = "no"`）但不被拉起，后续 `ak run web` 可启动它

#### Scenario: --cmd 字符串形态与 -- 等价

- **WHEN** 执行 `ak run --cmd "python -m http.server 8000" --name web`
- **THEN** 生成的注册与 `ak run -- python -m http.server 8000 --name web` 完全一致（同一 command/args/名称）

#### Scenario: --restart 字面量直书进生成配置

- **WHEN** 执行 `ak run -- node server.js --name worker --restart=unless-stopped`
- **THEN** 生成配置含 `restart = "unless-stopped"`，程序按其字面语义运行（崩溃自动重启、守护重启无停止记录则拉起）

#### Scenario: 命令行形态重复执行全量再生

- **WHEN** 对已由命令行注册的 `web` 再次执行 `ak run -- python -m http.server 9000 --name web`
- **THEN** 生成文件按本次命令行全量重写并立即生效（端口变更当场应用），行为与可执行程序来源的全量再生一致

#### Scenario: 同名已在运行时按差异处理

- **WHEN** 对已注册且正在运行的 `web` 再次执行 `ak run -- python -m http.server 8000 --name web`（参数相同）
- **THEN** 幂等成功，返回现有 PID，不重启进程

#### Scenario: 未指定 --name 时取可执行文件名

- **WHEN** 执行 `ak add /opt/srv/web-server`（未给 `--name`）
- **THEN** 应用名为 `web-server`

#### Scenario: 生成配置使用绝对路径

- **WHEN** 以相对路径 `./target/release/proc` 执行脚手架 add，或以 `run -- python ...` 命令行注册
- **THEN** 生成文件中 `command` 与 `work_dir` 均为绝对路径

#### Scenario: 重复执行无差异提示无变更

- **WHEN** 以完全相同参数对同名 app 再次执行脚手架 add
- **THEN** 文件内容不变，输出「无变更」类提示，以 0 退出

#### Scenario: 重复执行有差异提示需 apply

- **WHEN** 以不同 `--args` 对同名 app 再次执行脚手架 add
- **THEN** 生成文件被全量重写（手工编辑内容被覆盖），输出变更提示并指明需 `ak apply <app>` 生效；守护进程在线时该差异已进入 pending

#### Scenario: --apply 在线一步生效

- **WHEN** 守护进程运行中执行 `ak add ./proc --name abc --apply`
- **THEN** 配置生成后自动以本 app 范围执行 apply，`abc` 的程序按 `restart` 启动语义处理，其他 app 的 pending 不受影响，输出含 apply 结果

#### Scenario: --apply 离线报错

> 本变更将 run 的 ad-hoc 形态移出隐式拉起名单（标题为既有场景锚点，现行行为以下文为准）：`add --apply` 仍自动拉起守护进程；run 的 ad-hoc 形态离线时改为退出码 3。

- **WHEN** 守护进程未运行时执行 `ak add ./proc --name abc --apply`
- **THEN** 守护进程被自动后台拉起（详见 `background-daemon` 能力），随后该 app 的程序被拉起，命令成功退出；不出现「守护进程离线」报错

#### Scenario: 保留字 all 不可注册

- **WHEN** 执行 `ak add ./proc --name all` 或 `ak run -- /path/to/all`
- **THEN** 注册被拒绝并说明 `all` 为 apply 全量保留字

#### Scenario: 注册被校验拒绝

- **WHEN** add 指向不存在的文件，或生成的/给定的配置校验失败
- **THEN** 注册失败并指明原因，`app_dir` 保持不变

#### Scenario: 重复注册更新而非重复条目

- **WHEN** 对同名应用（目录或 `.toml` 来源）再次 add 并修改 flag
- **THEN** 链接与 `[app]` 表字段被更新，不产生重复注册

### Requirement: 离线可用

add / remove / list MUST NOT 依赖守护进程运行：守护进程离线时 SHALL 直接维护 `app_dir` 链接完成操作（离线时 MUST NOT 因无法同步而报错）；守护进程在线时 SHALL 在持久化后通过控制 API 触发同步（重扫检出），使注册变化进入待应用（pending）状态，由 `ak apply` 落地生效。在线同步失败 SHALL 视为本次命令失败：错误信息 MUST 说明注册已持久化但守护进程未同步及补救方式（`ak reload` 重扫后 `ak apply`），并以非零码退出，MUST NOT 仅以警告带过。

`ak run` 的 ad-hoc 形态为**守护进程执行**（daemon-first），MUST NOT 适用上述离线直写路径：守护进程离线时该形态 SHALL 以退出码 3 报错，MUST NOT 离线写盘或隐式拉起守护进程。

#### Scenario: 离线批量注册后启动

- **WHEN** 守护进程停止状态下依次 add 两个应用，随后启动守护进程
- **THEN** 两个应用按 priority 与依赖关系全部自动拉起

#### Scenario: 在线同步失败不静默

- **WHEN** 守护进程在线但其重扫失败时执行 `ak add`
- **THEN** 注册已持久化到 `app_dir`，命令以非零码退出，错误信息说明守护进程未同步并提示重跑 `ak reload` 与 `ak apply`

#### Scenario: 在线注册进入 pending 而非立即生效

- **WHEN** 守护进程在线时执行 `ak add`，随后立即执行 `ak status`
- **THEN** 注册同步成功（命令成功退出），新 app 的程序尚未运行，处于 pending；执行 `ak apply` 后按 `restart` 启动语义拉起

#### Scenario: 离线 run ad-hoc 退出 3

- **WHEN** 守护进程未运行时执行 `ak run -- ./server --name web`
- **THEN** 以退出码 3 报守护进程不可达，`app_dir` 不变，守护进程未被拉起

## ADDED Requirements

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
