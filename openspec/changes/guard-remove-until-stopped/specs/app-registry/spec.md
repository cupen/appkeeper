# app-registry Delta: guard-remove-until-stopped

## MODIFIED Requirements

### Requirement: 移除与查看

`ak remove <app>` SHALL 在守护进程在线时一步删除该应用的注册记录并立即从运行投影注销，MUST NOT 要求随后执行 `ak apply`。该命令为独立移除动作，MUST NOT 自动停止程序，MUST NOT 依赖 CLI 本地直接删除注册记录。

移除的前置条件是该 app 的全部程序均不处于 `running`、`starting`、`stopping` 或 `backoff`：前三种状态代表仍有存活进程或停机尚未收敛，`backoff` 代表等待自动重启。`stopped`、`exited` 与 `fatal` SHALL 允许移除，MUST NOT 要求用户曾显式执行 stop。前置条件不满足时 SHALL 以冲突错误拒绝，列出阻塞程序及状态并提示先执行 `ak stop <app>`，注册记录、程序状态与配置 MUST 保持不变。

删除成功后才 SHALL 清除该 app 的守护状态及关联的注销 pending，其他 app 的 pending MUST 保留；结果 SHALL 报告注册记录路径并表明已立即移除、无需 apply。注册记录为链接（外部部署目录注册）时，部署目录中的配置本体 MUST NOT 被删除；注册记录为位于 `app_dir` 内的真实文件（脚手架生成）时，SHALL 连带删除该文件并打印被删除的路径。日志文件 MUST NOT 因移除而删除。

注册删除在确认成功前失败时，已加载程序与 app 状态 MUST NOT 被清理；响应 SHALL 报告注册删除失败，MUST NOT 建议以 apply 作为补救。若返回错误无法确认磁盘删除是否已生效，SHALL 明确提示核对注册路径与当前状态，MUST NOT 虚称注册必然保留。

注册表查看 SHALL 由 `ak ps` 承接（原顶层 `list` 动词移除）：`ak ps --all` 在线时展示含未运行 app 的全量视图，离线时降级输出全部已注册应用（名称、配置本体路径、描述，见 `control-plane` 能力规范）。

#### Scenario: 移除运行中的应用

> 本变更将该既有场景改为前置守卫：运行中的 app 不可直接 remove，须先停止。

- **WHEN** 守护进程运行中且 web 仍有 running 程序时执行 `ak remove web`
- **THEN** 命令以冲突错误拒绝，列出阻塞程序与状态并提示 `ak stop web`；进程、注册记录和配置均不变，也未自动停止进程

#### Scenario: 停止后一步移除无需 apply

- **WHEN** web 的全部程序已停止，执行 `ak remove web`
- **THEN** web 立即从守护状态消失，`app_dir/web.toml` 注册记录被清理，输出被删除或保留的配置路径；无需执行 `ak apply`，该 app 无注销 pending，其他 app 的 pending 不变

#### Scenario: 自然退出或 fatal 的应用可移除

- **WHEN** app 的程序处于 exited 或 fatal 且没有显式执行过 stop，执行 `ak remove <app>`
- **THEN** 前置检查通过，注册与运行投影按成功移除契约清理

#### Scenario: 启动中、停止中或退避中的应用被拒绝

- **WHEN** app 的任一程序处于 starting、stopping 或 backoff 时执行 `ak remove <app>`
- **THEN** 移除以冲突错误拒绝，不停机、不删除注册；对 backoff 程序提示先 stop 以取消自动重启

#### Scenario: 移除外部注册保留配置本体

- **WHEN** 对已停止的外部部署目录应用执行 `ak remove web`
- **THEN** `app_dir/web.toml` 链接被删除，部署目录中的 `appkeeper.toml` 仍存在，输出保留的配置本体路径

#### Scenario: 移除脚手架生成的应用删除生成文件

- **WHEN** 对已停止的脚手架生成 app 执行 `ak remove abc`
- **THEN** `app_dir/abc.toml` 被删除且路径被打印，该 app 立即从注册表与状态投影消失

#### Scenario: 注册删除失败不清理守护状态

- **WHEN** 已停止 app 的注册文件因权限或 IO 错误无法确认删除成功
- **THEN** 命令报告删除失败，守护状态仍保留该 app 与程序，错误不以再次 apply 作为补救

#### Scenario: 列出注册表

> 本变更将 `list` 动词并入 `ps --all`（标题为既有场景锚点，现行行为以下文为准）。

- **WHEN** 守护进程未运行时执行 `ak ps --all`
- **THEN** 输出注明 daemon offline 的清单，含每个已注册应用的名称、配置本体路径与描述

### Requirement: 离线可用

compose MUST NOT 依赖守护进程运行（注册表查看见 `ak ps --all` 的离线降级）：守护进程离线时 SHALL 直接维护 `app_dir` 链接完成注册（离线时 MUST NOT 因无法同步而报错）；守护进程在线时 SHALL 在持久化后通过控制 API 触发同步（重扫检出），使注册变化进入待应用（pending）状态，由 `ak apply` 落地生效。在线同步失败 SHALL 视为本次命令失败：错误信息 MUST 说明注册已持久化但守护进程未同步及补救方式（等待一个重扫周期（守护进程周期性重扫，见 `apply-workflow`）后重试 `ak apply`，或执行 `ak config --reload` 触发立即重扫后重试），并以非零码退出，MUST NOT 仅以警告带过。

remove MUST 依赖守护进程在线：守护进程离线时 SHALL 输出不可达提示并以退出码 3 退出，MUST NOT 直接删除注册记录、隐式拉起守护进程或留下待 apply 的移除。remove 不适用本需求的离线直写与在线 pending 规则，其原子移除语义见「移除与查看」。

`ak run` 的 ad-hoc 形态为**守护进程执行**（daemon-first），MUST NOT 适用上述离线直写路径：守护进程离线时该形态 SHALL 以退出码 3 报错，MUST NOT 离线写盘或隐式拉起守护进程。

#### Scenario: 离线批量注册后启动

- **WHEN** 守护进程停止状态下依次 compose 两个应用，随后启动守护进程
- **THEN** 两个应用按 priority 与依赖关系全部自动拉起

#### Scenario: 离线移除拒绝且不修改注册

- **WHEN** 守护进程未运行时执行 `ak remove web`
- **THEN** 命令以退出码 3 报守护进程不可达，不拉起守护进程，`app_dir/web.toml` 与配置本体均不变

#### Scenario: 在线同步失败不静默

- **WHEN** 守护进程在线但其重扫失败时执行 `ak compose`
- **THEN** 注册已持久化到 `app_dir`，命令以非零码退出，错误信息说明守护进程未同步并提示执行 `ak config --reload`（或等待一个重扫周期）后执行 `ak apply`

#### Scenario: 在线注册进入 pending 而非立即生效

- **WHEN** 守护进程在线时执行 `ak compose`，随后立即执行 `ak ps`
- **THEN** 注册同步成功（命令成功退出），新 app 的程序尚未运行，处于 pending；执行 `ak apply` 后按 `restart` 启动语义拉起

#### Scenario: 离线 run ad-hoc 退出 3

- **WHEN** 守护进程未运行时执行 `ak run -- ./server --app web`
- **THEN** 以退出码 3 报守护进程不可达，`app_dir` 不变，守护进程未被拉起
