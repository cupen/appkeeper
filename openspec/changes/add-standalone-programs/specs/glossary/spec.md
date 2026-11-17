## MODIFIED Requirements

### Requirement: 核心术语定义

系统文档与规范 SHALL 以下列定义为准：**daemon（守护进程）**——全局唯一的 appkeeper 监督进程本体，持有 daemon 配置与 `app_dir`，唯一执行监督循环的实体；**app（应用）**——由 `compose up`（及 compose 注册）建立并管理的**进程组**，配置本体为部署目录中的 `.appkeeper.toml`（兼容 `appkeeper.toml`），注册记录位于 `app_dir` 顶层（外部链接、脚手架生成文件或 `compose up` 渲染快照）；app 自身不是进程，是程序的容器；**只有 compose 建立并管理的进程组才是 app**，`ak run` 产物一律为散装程序；**program（程序）**——app 配置 `[program.<name>]` 定义的被守护子进程，拥有独立状态机与日志流，其名称跨全部已注册应用全局唯一；**standalone program（散装程序）**——由 `ak run` 注册、**不属于任何 app** 的程序，注册记录为 `app_dir/adhoc/<程序名>.toml`，配置以 `[program.<程序名>]` 为根、可省略 `[app]` 表，以全局唯一的程序名直接寻址；散装程序与 app 内的程序是同一运行对象类型（同为 program），区别仅在注册记录的归属与位置；**action（动作）**——面向 daemon、app 或 program 发出的执行指令，分为内置动作（实现于 appkeeper 自身）与自定义动作（用户在配置中声明、映射为 shell 命令执行）。复合词：**daemon 配置**（全局唯一根配置）、**app 配置**（应用部署目录中的配置文件）、**app_dir**（应用注册目录）、**注册**（建立 `app_dir` 链接或生成脚手架文件的动作）、**控制平面**（daemon 暴露的本地 HTTP API 及其 CLI/Shell 映射）。

app 身份 SHALL 由「compose 建立并管理」这一归属关系决定，MUST NOT 由运行时状态（当前是否运行、是否由某次 `compose up` 拉起）决定：`stop` 一个 app 不会使其失去 app 身份，`start` 一个已注册的 compose app 也不需要重新 `compose up`。散装程序不是 app：它不参与 app 归组、不构成 app 名、也不要求用户理解 app 概念；多程序分组 SHALL 由 `compose` 承担。`ak run` SHALL 只产生散装程序记录。

#### Scenario: 状态输出中的术语指称

- **WHEN** `ak ps` 输出程序列表
- **THEN** 每行的主名称为全局唯一的程序名；app 内的程序在 APP 列展示归属，散装程序的 APP 列显示 `-`

#### Scenario: 文档用词一致

- **WHEN** 规范、CLI 帮助或错误信息指称被守护的子进程
- **THEN** 使用「程序/program」（散装程序亦为程序），不以「应用/app」混称（app 特指部署单元）

### Requirement: 标识符不变量

app 名、程序名、自定义动作名 SHALL 遵循同一字符集（仅英文字母、数字、下划线与连字符，校验行为见 configuration 规范）；程序名 SHALL 跨全部已注册应用**及全部散装程序**全局唯一（由 configuration 校验强制）。散装程序名额外 MUST NOT 与任何 app 名相同，以保证单参数寻址无歧义。

CLI 管理面动词 `start`、`stop`、`restart`、`apply`、`remove` SHALL 接受统一的目标文法 `<target>`：

- `<program>`：全局唯一的程序名（app 内程序或散装程序）；单 token 一律按**程序**解析，MUST NOT 因同名 app 存在而改为整组操作；
- `<app>/<program>`：该 app 下的指定程序；
- `<app>/all`：该 app 的全部程序（整组操作）；
- 空值、空段（`/program`、`app/`）、两个以上 `/`、或裸 `all`：用法错误（退出码 2）。

`all` SHALL 作为保留字：MUST NOT 作为程序名（app 名亦不得为 `all`）。`/` 不在名称字符集内，切分无歧义。第二位置参数 `<app> <program>` MAY 作为 `<app>/<program>` 的等价旧形式保留（同一语义；`remove` 除外，它只接受单个 `<target>`），两种形态不得混用。仅 `<app>/all` 触发 app 级扇出；单 token 命中的名字若只是 app 名（无同名程序），SHALL 报未知程序并提示改用 `<app>/all`。

API、webui 及 CLI 进程面动词（`pid`、`log`、`action`、`signal`）寻址单个 program 时 SHALL 使用其全局唯一名即可无歧义定位。`app.program` 复合寻址语法 SHALL 保留语法空间，本规范版本不定义其语义。

#### Scenario: 单程序无歧义寻址

- **WHEN** API、webui 或 CLI 进程面动词以程序名寻址操作对象
- **THEN** 该名称全局唯一、无歧义，无需 app 前缀限定

#### Scenario: 管理面以 app 寻址

- **WHEN** 用户执行 `ak stop gateway/all`、`ak stop gateway api` 或 `ak stop gateway`
- **THEN** `gateway/all` 停止 gateway 的全部程序，`gateway api` 仅停止其 `api` 程序，`gateway` 按程序名解析（不存在同名程序时报错并提示 `gateway/all`），不使用 `--app` 旗标

#### Scenario: 单 token 按程序解析

- **WHEN** 同时存在 app `api`（无同名程序）与散装程序 `worker`，用户执行 `ak start worker` 与 `ak start api`
- **THEN** 前者启动散装程序 `worker`；后者报未知程序并提示 `api/all`，MUST NOT 隐式启动整个 app `api`

#### Scenario: 复合名不被解析为寻址语法

- **WHEN** 配置或命令中出现含点号的复合名（如 `gateway.api`）
- **THEN** 因程序名不允许点号，该名字只会被校验拒绝，MUST NOT 被解释为 app.program 寻址

#### Scenario: 散装程序以单名寻址

- **WHEN** 执行 `ak stop agent` 且 `agent` 是散装程序（无同名 app）
- **THEN** 解析为散装记录并停止该程序；`ak stop agent agent` 之类的双位置调用不适用散装程序
