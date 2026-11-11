# glossary Delta: refine-cli-verbs

## MODIFIED Requirements

### Requirement: 动作词表边界

「动作」SHALL 仅指改变运行状态的执行指令。观察类命令（CLI 与 shell 同名：`ps`、`pid`、`log`、`pending`）MUST NOT 纳入动作词表，自定义动作名无需回避它们。`apply` SHALL 保持 apply-workflow 规范定义的配置应用工作流概念，不属于运行时动作。内置动作名（start、stop、restart、signal、reload、shutdown）SHALL 作为自定义动作的保留字；其中 `reload` 自本变更为配置面动作（`ak config --reload`），不属 daemon/app/program 的运行时动作词表。

#### Scenario: 查询不属于动作

- **WHEN** 用户通过 ps/pid/log/pending 获取信息
- **THEN** 这些命令作为观察面板提供，不占用动作命名空间，自定义动作可使用同名（如动作名 `status`）而不冲突

#### Scenario: apply 独立于动作

- **WHEN** 文档或帮助提及 apply
- **THEN** 其语义为 apply-workflow 规范定义的配置应用工作流，不作为 daemon/app/program 的动作呈现

### Requirement: 标识符不变量

app 名、程序名、自定义动作名 SHALL 遵循同一字符集（仅英文字母、数字、下划线与连字符，校验行为见 configuration 规范）；程序名 SHALL 跨全部已注册应用全局唯一（由 configuration 校验强制）。

CLI 管理面动词 `start`、`stop`、`restart`、`apply` SHALL 使用 `<app> [<program>]` 双位置参数以 app 为首要寻址单位：裸 `<app>` 作用于整个 app，第二位置参数收窄到单程序。API、webui 及 CLI 进程面动词（`pid`、`log`、`action`、`signal`）寻址单个 program 时 SHALL 使用其全局唯一名即可无歧义定位。`app.program` 复合寻址语法 SHALL 保留语法空间，本规范版本不定义其语义。

#### Scenario: 单程序无歧义寻址

- **WHEN** API、webui 或 CLI 进程面动词以程序名寻址操作对象
- **THEN** 该名称全局唯一、无歧义，无需 app 前缀限定

#### Scenario: 管理面以 app 寻址

- **WHEN** 用户执行 `ak stop gateway` 或 `ak stop gateway api`
- **THEN** 前者停止 gateway 的全部程序，后者仅停止其 `api` 程序，不使用 `--app` 旗标

#### Scenario: 复合名不被解析为寻址语法

- **WHEN** 配置或命令中出现含点号的复合名（如 `gateway.api`）
- **THEN** 因程序名不允许点号，该名字只会被校验拒绝，MUST NOT 被解释为 app.program 寻址
