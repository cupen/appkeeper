## MODIFIED Requirements

### Requirement: 注册应用（add）

`xkeeper add [path] [flags]` SHALL 注册一个应用，按 `path` 形态分派：

- `path` 为部署目录（取其中默认名 `xkeeper.toml`，如 `xkeeper add .`）或直接指向 `.toml` 配置文件时，走既有注册流程：校验（文件存在、格式合法、程序名全局唯一、依赖不成环）；在 `app_dir` 创建以应用名命名的 `<name>.toml` 链接指向配置本体（链接即注册记录）；将微调 flag 幂等写入该 app 配置的 `[app]` 表（`--restart`、`--restart-backoff`、`--priority` 等，作为应用级默认；flag 写入失败 SHALL 回滚整个注册）。
- `path` 为其他存在的普通文件（可执行程序）时，走脚手架生成流程：SHALL 在 `app_dir` 下生成真实配置文件 `<name>.toml`（非链接，注册记录即文件本身），内容为单程序 app：program 名 = 应用名，`[app]` 表与 `[program.<name>]` 表按给定 flag 渲染。`command` SHALL 写绝对路径（canonicalize 解析后）；`work_dir` SHALL 缺省取执行 add 命令时的当前工作目录（以绝对路径写入），可用 `--workdir <路径>` 覆盖。启动参数 SHALL 以 `--args "<字符串>"` 传入（按 shell 词法切分为 argv），环境变量 SHALL 以 `--env K=V` 传入（可重复）。应用名 SHALL 取 `--name`，缺省取可执行文件名（去扩展名的 stem）。脚手架 SHALL 至少同样支持：`--name`、`--description`、`--restart`、`--restart-backoff`、`--priority`。

命令行注册 SHALL 经由 `xkeeper run` 的 ad-hoc 形态进入（动词面见 `control-plane`，注册落地仍在本能力域），命令行有两种等价入口、MUST 二选一：`--` 之后的整段 trailing 内容，或 `--cmd "<命令行字符串>"`（按 shell 词法切分，适合不便使用 `--` 的调用场景）。命令首 token SHALL 解析为绝对路径后写入 `command`——含路径分隔符时按 canonicalize 解析，裸命令名按调用方 `PATH` 解析（Windows 含 `PATHEXT` 扩展）；解析失败 SHALL 以退出码 2 报错且不产生任何注册产物。其余 token 按 shell 词法作为 `args` 写入。应用名 SHALL 取 `--name`，缺省取命令首 token 的文件名 stem。

该形态 SHALL 支持脚手架 flag：`--env K=V`（可重复）、`--workdir <路径>`（缺省当前目录，写绝对路径）、`--description`、`--restart-backoff`、`--restart=<no|always|unless-stopped|on-failure>`（docker 字面量直书进生成配置，字段语义见 configuration 与 process-management 能力；缺省不写该字段、沿四层级联取内置默认 `no`）；MUST NOT 提供 `--args`（命令行本身已含参数），亦 MUST NOT 提供 `--autostart`/`--autorestart`（已由 `--restart` 取代）。

注册成功后 SHALL 立即以该 app 范围 apply 生效；`--restart=no` 时注册落地但程序不被拉起（覆盖「只注册命令行、暂不运行」的需求）。

应用名 `all` 为保留字（见 apply-workflow），add 与 run ad-hoc MUST 拒绝以 `all` 注册（含 `--name all` 与文件名/命令名 stem 推导出的 `all`）。

重复执行：目录/`.toml` 来源的同名 add 保持既有 upsert 语义（更新链接与 flag，不产生重复）；可执行程序与 run ad-hoc 命令行来源的同名注册 SHALL **全量再生**——以本次给定参数重写整个生成文件（未给 flag 的字段回到缺省，文件上手工编辑的内容会被覆盖），再生前 MUST 校验通过才落盘。再生后 SHALL 将新内容与磁盘旧内容比对：有差异时打印变更提示并明确指出需执行 `xkeeper apply <app>`（或全量 `xkeeper apply`）方可生效（run ad-hoc 因立即 apply 而当场生效）；无差异时打印「无变更」类提示。

`add --apply` SHALL 在注册/再生成功后立即对该 app 执行一次 apply（仅本 app 范围，MUST NOT 波及其他 app 的 pending）。守护进程离线时，`add --apply` 与 run 的 ad-hoc 形态（`--`/`--cmd`）SHALL 自动后台拉起守护进程后继续应用（拉起协议、竞态容忍与超时语义详见 `background-daemon` 能力规范），MUST NOT 再以「守护进程离线」报错退出。

注册与再生成功后 SHALL 以非交互纯文本完整打印：生成的配置文件路径、应用名、程序名、command、args、env、work_dir，以及下一步提示（在线时提示 pending 与 apply 命令）。

#### Scenario: 从部署目录注册并指定名字

- **WHEN** 在应用部署目录内执行 `xkeeper add . --name gateway --restart on-failure`
- **THEN** `app_dir` 出现以 gateway 命名的链接，部署目录 app 配置的 `[app]` 表写入 `restart = "on-failure"`，命令成功

#### Scenario: 未指定 --name 时取目录名

- **WHEN** 在 `/opt/myapp` 目录内执行 `xkeeper add .`（未给 `--name`）
- **THEN** 应用名为 `myapp`

#### Scenario: 守护进程运行中注册即时生效

- **WHEN** 守护进程运行中执行 add（任一来源）
- **THEN** 注册持久化，并等效于触发一次 reload（重扫检出）：该应用进入 pending，程序尚未拉起；执行 `xkeeper apply` 后按 `restart` 启动语义生效

#### Scenario: 从可执行程序脚手架生成并注册

- **WHEN** 在 `/opt/srv` 目录内执行 `xkeeper add ./web-server --name abc --args "--port 8080" --env LOG=debug`
- **THEN** `app_dir` 下生成真实文件 `abc.toml`，其中 program 名为 `abc`，`command` 为 `/opt/srv/web-server` 的绝对路径，`args = ["--port", "8080"]`，`env` 含 `LOG=debug`，`work_dir` 为 `/opt/srv`；命令完整打印上述生成信息并以 0 退出

#### Scenario: 命令行形态注册（PATH 解析）

- **WHEN** 执行 `xkeeper run -- python -m http.server 8000 --name web`
- **THEN** `app_dir` 下生成真实文件 `web.toml`，`command` 为 `python` 经 `PATH` 解析出的绝对路径，`args = ["-m", "http.server", "8000"]`，注册后立即生效

#### Scenario: 命令行形态缺省名取命令 stem

- **WHEN** 执行 `xkeeper run -- node server.js`（未给 `--name`）
- **THEN** 应用名与程序名为命令首 token 的文件名 stem（`node`）

#### Scenario: 命令解析失败以配置错误退出

- **WHEN** 执行 `xkeeper run -- definitely-not-a-real-cmd --x`
- **THEN** 命令以退出码 2 报错说明无法解析，`app_dir` 无新增文件

#### Scenario: 命令行形态只注册不启动

- **WHEN** 执行 `xkeeper run --cmd "python -m http.server 8000" --name web --restart=no`
- **THEN** `web` 注册落地（生成配置含 `restart = "no"`）但不被拉起，后续 `xkeeper run web` 可启动它

#### Scenario: --cmd 字符串形态与 -- 等价

- **WHEN** 执行 `xkeeper run --cmd "python -m http.server 8000" --name web`
- **THEN** 生成的注册与 `xkeeper run -- python -m http.server 8000 --name web` 完全一致（同一 command/args/名称）

#### Scenario: --restart 字面量直书进生成配置

- **WHEN** 执行 `xkeeper run -- node server.js --name worker --restart=unless-stopped`
- **THEN** 生成配置含 `restart = "unless-stopped"`，程序按其字面语义运行（崩溃自动重启、守护重启无停止记录则拉起）

#### Scenario: 命令行形态重复执行全量再生

- **WHEN** 对已由命令行注册的 `web` 再次执行 `xkeeper run -- python -m http.server 9000 --name web`
- **THEN** 生成文件按本次命令行全量重写并立即生效（端口变更当场应用），行为与可执行程序来源的全量再生一致

#### Scenario: 未指定 --name 时取可执行文件名

- **WHEN** 执行 `xkeeper add /opt/srv/web-server`（未给 `--name`）
- **THEN** 应用名为 `web-server`

#### Scenario: 生成配置使用绝对路径

- **WHEN** 以相对路径 `./target/release/proc` 执行脚手架 add，或以 `run -- python ...` 命令行注册
- **THEN** 生成文件中 `command` 与 `work_dir` 均为绝对路径

#### Scenario: 重复执行无差异提示无变更

- **WHEN** 以完全相同参数对同名 app 再次执行脚手架 add
- **THEN** 文件内容不变，输出「无变更」类提示，以 0 退出

#### Scenario: 重复执行有差异提示需 apply

- **WHEN** 以不同 `--args` 对同名 app 再次执行脚手架 add
- **THEN** 生成文件被全量重写（手工编辑内容被覆盖），输出变更提示并指明需 `xkeeper apply <app>` 生效；守护进程在线时该差异已进入 pending

#### Scenario: --apply 在线一步生效

- **WHEN** 守护进程运行中执行 `xkeeper add ./proc --name abc --apply`
- **THEN** 配置生成后自动以本 app 范围执行 apply，`abc` 的程序按 `restart` 启动语义处理，其他 app 的 pending 不受影响，输出含 apply 结果

#### Scenario: --apply 离线报错

> 本变更将其行为翻转为自动拉起（标题为既有场景锚点，现行行为以下文为准）；run 的 ad-hoc 形态同此语义。

- **WHEN** 守护进程未运行时执行 `xkeeper add ./proc --name abc --apply`，或 `xkeeper run -- ./server --name abc`
- **THEN** 守护进程被自动后台拉起（详见 `background-daemon` 能力），随后该 app 的程序被拉起，命令成功退出；不再出现「守护进程离线」报错

#### Scenario: 保留字 all 不可注册

- **WHEN** 执行 `xkeeper add ./proc --name all` 或 `xkeeper run -- /path/to/all`
- **THEN** 注册被拒绝并说明 `all` 为 apply 全量保留字

#### Scenario: 注册被校验拒绝

- **WHEN** add 指向不存在的文件，或生成的/给定的配置校验失败
- **THEN** 注册失败并指明原因，`app_dir` 保持不变

#### Scenario: 重复注册更新而非重复条目

- **WHEN** 对同名应用（目录或 `.toml` 来源）再次 add 并修改 flag
- **THEN** 链接与 `[app]` 表字段被更新，不产生重复注册

### Requirement: 离线可用

add / remove / list MUST NOT 依赖守护进程运行：守护进程离线时 SHALL 直接维护 `app_dir` 链接完成操作（离线时 MUST NOT 因无法同步而报错）；守护进程在线时 SHALL 在持久化后通过控制 API 触发同步（重扫检出），使注册变化进入待应用（pending）状态，由 `xkeeper apply` 落地生效。在线同步失败 SHALL 视为本次命令失败：错误信息 MUST 说明注册已持久化但守护进程未同步及补救方式（`xkeeper reload` 重扫后 `xkeeper apply`），并以非零码退出，MUST NOT 仅以警告带过。

#### Scenario: 离线批量注册后启动

- **WHEN** 守护进程停止状态下依次 add 两个应用，随后启动守护进程
- **THEN** 两个应用按 priority 与依赖关系全部自动拉起

#### Scenario: 在线同步失败不静默

- **WHEN** 守护进程在线但其重扫失败时执行 `xkeeper add`
- **THEN** 注册已持久化到 `app_dir`，命令以非零码退出，错误信息说明守护进程未同步并提示重跑 `xkeeper reload` 与 `xkeeper apply`

#### Scenario: 在线注册进入 pending 而非立即生效

- **WHEN** 守护进程在线时执行 `xkeeper add`，随后立即执行 `xkeeper status`
- **THEN** 注册同步成功（命令成功退出），新 app 的程序尚未运行，处于 pending；执行 `xkeeper apply` 后按 `restart` 启动语义拉起
