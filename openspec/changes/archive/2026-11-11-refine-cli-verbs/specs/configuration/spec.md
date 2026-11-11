# configuration Delta: refine-cli-verbs

## MODIFIED Requirements

### Requirement: config 子命令

`ak config` SHALL 作为 daemon 配置文件的统一本地管理入口，动作由 flag 指定：`--set`（写入，可重复）、`--get`（读取）、`--delete`（删除，可重复）、`--edit`（编辑器编辑）、`--init`（初始化）、`--reload`（触发守护进程重载，本变更新增）；本地动作一次调用 SHALL 只执行一类（`--set`/`--get`/`--delete` 可各自多键，`--edit` 与 `--init` 不与其他本地动作同用），无任何动作 flag 时 SHALL 打印用法帮助并以 0 退出。目标文件为解析后的 daemon 配置路径（全局 `-c/--config` 可覆盖，缺省平台位置）。

除 `--reload` 外，config SHALL 保持纯本地文件操作：MUST NOT 依赖运行中的 daemon，MUST NOT 直接改变运行态（写入生效经由 `--reload` 触发的重载与既有 apply 流程）。`--reload` SHALL 经控制端点调用既有 `/v1/reload` 端点（端点形态由目标配置的 `socket` 值决定，`tcp://` 形态自动携带 `auth_token` 凭据），使守护进程重读 daemon 配置并触发一次立即重扫（语义见本能力「按应用粒度的热更新」与 `apply-workflow`）；`--reload` 可单独使用，也可与写动作（`--set`/`--delete`/`--edit`）组合——本地动作成功后随即触发重载，任一步失败即以非零码退出；与 `--get`/`--init` 组合 SHALL 为用法错误（退出码 2）。重载成功 SHALL 输出确认与 pending 摘要（daemon 提示、app 级增删与程序级变更），语义与原顶层 `ak reload` 一致。守护进程离线时 `--reload` SHALL 输出不可达提示并以退出码 3 退出，MUST NOT 拉起守护进程；守护进程上报重载失败（如 `[webui]` 段非法）SHALL 以非零码退出并展示回传错误。daemon 在线且本次本地写动作改写了文件而未附 `--reload` 时，SHALL 在输出中提示执行 `ak config --reload`（或附 `--reload` 重跑本命令）方可生效；在线检测 SHALL 经控制端点进行，MUST NOT 依赖任何硬编码端口。

#### Scenario: 无动作打印帮助

- **WHEN** 执行 `ak config`
- **THEN** 打印六个动作 flag 的用法说明，以 0 退出

#### Scenario: 离线可用

- **WHEN** daemon 未运行，执行 `ak config --set log_buffer_lines=2000`
- **THEN** 文件被写入并校验通过，输出中说明 daemon 当前离线、下次启动生效

#### Scenario: 动作互斥

- **WHEN** 同时给定 `--init` 与 `--get log_level`
- **THEN** 以非零码退出并说明一次只能执行一类动作

#### Scenario: --reload 触发立即重扫

- **WHEN** daemon 在线时执行 `ak config --reload`
- **THEN** 守护进程重读 daemon 配置并立即重扫，重载输出含 pending 摘要（`ak ps` 的 PENDING 列与摘要行同见），命令以 0 退出

#### Scenario: --reload 离线退出 3

- **WHEN** daemon 未运行时执行 `ak config --reload`
- **THEN** 输出守护进程不可达提示，以退出码 3 退出，MUST NOT 拉起守护进程

#### Scenario: 写动作与 --reload 一步生效

- **WHEN** daemon 在线且配置无 `[webui]` 段，执行 `ak config --set webui.listen=127.0.0.1:9877 --reload`
- **THEN** 文件写入成功后守护进程随即重载，控制台立即在该地址伺服（见 `webui-api`），命令以 0 退出

#### Scenario: --reload 与读取动作互斥

- **WHEN** 执行 `ak config --get log_level --reload`
- **THEN** 以退出码 2 报用法错误，不发起任何控制面请求

#### Scenario: 在线写后提示重载入口

- **WHEN** daemon 在线时执行 `ak config --set log_level=debug`（未附 `--reload`）
- **THEN** 文件写入成功，输出提示执行 `ak config --reload`（或附 `--reload` 重跑）后方生效

### Requirement: 按应用粒度的热更新

reload SHALL 重扫 `app_dir` 注册并重读全部 app 配置本体与 daemon 配置，但仅将差异记为**待应用（pending）**变更并输出预览，MUST NOT 停止、重启或拉起任何程序（应用由 apply 触发，见 apply-workflow 能力）。**唯一例外**：`[webui]` 段的启用/关闭/`listen` 变更 SHALL 由 reload 立即热应用（见 webui-api「配置驱动的控制台生命周期」），MUST NOT 进入 pending。守护进程 SHALL 周期性自动执行该重扫，`ak config --reload`（见「config 子命令」）触发一次立即重扫。单个 app 文件非法时 SHALL 隔离失败——该 app 不进入 pending、保持旧定义继续运行并上报错误，其余 app 照常检出。跨应用校验失败时本轮 SHALL 整体不形成 pending。daemon 配置中守护进程自身字段变更（`[webui]` 段之外）SHALL 尽量热应用（如日志级别），无法热更的字段（如控制端点 `socket`）SHALL 在 pending 预览中明确提示需要重启守护进程。

#### Scenario: 单个 app 文件损坏不影响其他

- **WHEN** 重扫时 app B 的配置存在语法错误，app A 的配置正常且已修改
- **THEN** A 的变更进入 pending，B 保持旧定义继续运行，错误信息被上报

#### Scenario: reload 只检出不应用

> 本变更后 CLI 触发入口为 `ak config --reload`（标题为既有场景锚点，现行行为以下文为准）。

- **WHEN** app A 的程序 `web` 配置已修改且正在运行，执行 `ak config --reload`
- **THEN** 输出包含 `web` 的 pending 预览，进程未被停止或重启

#### Scenario: 不可热更字段提示

> `port` 为已移除键，本变更按现行 `socket` 单键端点表述（标题为既有场景锚点，现行行为以下文为准）。

- **WHEN** daemon 配置的 `socket` 被修改为另一端点，执行 `ak config --reload`
- **THEN** pending 预览提示控制端点变更需重启守护进程方可生效，控制面仍监听旧端点

#### Scenario: 移除注册立即生效

- **WHEN** 守护进程运行中执行 `ak remove <app>`
- **THEN** `app_dir` 链接即时清理，该 app 的移除进入 pending，其程序在
  apply 前继续运行
