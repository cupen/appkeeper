# shell-client Specification

## Purpose
定义 `appkeeper shell` 交互式客户端能力：作为根进程（`ak daemon start` 守护）的终端入口，
在 REPL 中浏览被守护 app/程序的清单与状态、执行控制动作，并通过 `open` 动词
辅助用户打开（配置驱动的）Web 控制台。shell 不改变守护进程行为，全部经现有控制面 API 通信；
shell 的内置词表（`status`、`shutdown` 等）独立于 CLI 动词，不受 CLI 改名影响。

## Requirements

### Requirement: shell 入口与 REPL

`appkeeper shell` SHALL 启动一个交互式行式 REPL：显示提示符，读取一行作为一条 shell 命令，
执行后继续读取，直至用户输入 `exit`、`quit` 或发送 EOF（Ctrl+D）。REPL SHALL 提供基础
行编辑（光标移动、退格、历史上下翻）并按会话维护命令历史。Ctrl+C SHALL 仅中断当前输入行，
不退出 shell。

#### Scenario: 进入并退出 shell

- **WHEN** 执行 `appkeeper shell`，输入 `status` 后再输入 `exit`
- **THEN** shell 执行一次状态查询后退出，进程退出码为 0

#### Scenario: Ctrl+C 不退出

- **WHEN** 在提示符处按下 Ctrl+C
- **THEN** 当前输入行被放弃并显示新提示符，shell 进程不退出

#### Scenario: EOF 退出

- **WHEN** 在提示符处按下 Ctrl+D
- **THEN** shell 退出，退出码为 0

### Requirement: shell 内置命令集

shell SHALL 支持以下内置命令，语义与对应单发 CLI 子命令一致，全部经控制面 `/v1` API 通信：

- `ps`：以对齐的表格输出全部程序的名称、所属 app、状态、pid、重启次数、unhealthy 标记与待应用变更标记（PENDING 列，与 `ak ps` 共享渲染）；
- `start|stop|restart <app> [<program>]`：执行对应动作；裸 `<app>` 作用于整个 app，第二位置参数收窄到单程序（与对应 CLI 的双位置范围参数一致），未知 app/程序报错；
- `action <program> <action>`：执行自定义动作，输出退出码与输出尾部，CLI 退出码语义一致；
- `signal <program> <SIGNAL>`：投递白名单信号，语义与 CLI 一致；
- `pid <name>`：输出程序 pid 或 `not running`；
- `log <name> [-f] [--tail N] [--stream out|err]`：查看日志，`-f` 跟随输出；
- `pending`：输出当前待应用变更清单（逐程序 changed、app 级注册变化、检出错误）；
- `apply [<app> [<program>]] [--restart]`：应用待应用变更，输出与 `ak apply` 一致的逐程序结果表格；
- `shutdown`：请求守护进程优雅退出，随后 shell SHOULD 因守护不可达提示并可用 `exit` 离开；
- `open`：探测 webui 可达性（`GET /api/health`），可达则调用系统默认浏览器打开控制台，
  不可达时输出明确提示且不启动浏览器；
- `help`（或 `?`）：列出内置命令与简短用法；
- `exit` / `quit`：离开 shell。

`reload` 与 `status`（含 `st` 别名）词 SHALL 移除（立即重扫的 CLI 入口为 `ak config --reload`，shell 不设 config 词面；`status` 改名 `ps`，词面与 CLI 对齐）；输入二者 SHALL 得到 unknown command 提示并建议相近命令。空行 SHALL 忽略；未知命令 SHALL 输出 `unknown command` 提示并列出相近命令（若有），不退出。

#### Scenario: status 表格输出

> 本变更起观察词为 `ps`（标题为既有场景锚点，现行行为以下文为准）。

- **WHEN** shell 中输入 `ps` 且守护进程有两个程序（一 running 一 stopped）
- **THEN** 输出包含两个程序行的对齐表格，含名称、状态、pid 与重启次数列

#### Scenario: 未知命令容错

- **WHEN** shell 中输入 `sttaus`
- **THEN** 输出 unknown command 提示并建议相近命令，shell 继续接受下一条输入

#### Scenario: 在 shell 中启动程序

> 本变更起启动词为 `start`（标题为既有场景锚点，现行行为以下文为准）。

- **WHEN** shell 中输入 `start web api` 且 `web` 的 `api` 为已停止程序
- **THEN** 仅启动 `web` 的 `api`，程序状态迁移与 `ak start web api` 一致

#### Scenario: 在 shell 中重启程序

- **WHEN** shell 中输入 `restart web`
- **THEN** 请求经控制面 API 下发，程序状态迁移与 `ak restart web` 一致

#### Scenario: 在 shell 中应用变更

- **WHEN** 配置存在 pending 时 shell 中输入 `apply myapp`
- **THEN** 输出与 `ak apply myapp` 一致的逐程序结果，仅作用于该 app

#### Scenario: 在 shell 中查看 pending

- **WHEN** 配置存在 pending 时 shell 中输入 `pending`
- **THEN** 输出变更程序与 app 注册变化清单；无变更时输出「无变更」

#### Scenario: open 打开 webui

- **WHEN** webui 在默认端口可达且输入 `open`
- **THEN** 系统默认浏览器被调起并指向控制台地址，shell 不退出

#### Scenario: open 在 webui 未启动时

- **WHEN** webui 未启动且输入 `open`
- **THEN** 输出"webui 未启动"的提示（含启动方式提示），不启动浏览器，shell 不退出

#### Scenario: 在 shell 中执行自定义动作

- **WHEN** shell 中输入 `action api upgrade` 且动作命令以退出码 3 结束
- **THEN** 输出动作的输出尾部，shell 显示的成败语义与 `appkeeper action api upgrade` 一致，shell 不退出

#### Scenario: 在 shell 中扇出启动

> 本变更起扇出词为 `start <app>`（标题为既有场景锚点，现行行为以下文为准）。

- **WHEN** shell 中输入 `start gateway`
- **THEN** gateway 全部程序按排序规则启动，输出与 `ak start gateway` 一致的逐程序结果

#### Scenario: reload 词已移除

- **WHEN** shell 中输入 `reload`
- **THEN** 输出 unknown command 提示（可建议 `pending` 或 `apply`），shell 继续接受下一条输入

#### Scenario: status 词已改名 ps

- **WHEN** shell 中输入 `status` 或 `st`
- **THEN** 输出 unknown command 提示并建议 `ps`，shell 继续接受下一条输入

### Requirement: shell 单命令模式

`appkeeper shell -e "<命令>"` SHALL 执行该单条 shell 命令后立即退出，退出码遵循控制面
CLI 的同一约定（0 成功；1 一般错误；3 守护进程不可达）。该模式 SHALL 不进入交互界面。

#### Scenario: 脚本化调用

- **WHEN** 执行 `appkeeper shell -e "status"` 且守护进程运行中
- **THEN** 输出状态表格后立即退出，退出码 0

#### Scenario: 单命令模式守护不可达

- **WHEN** 守护进程未启动时执行 `appkeeper shell -e "status"`
- **THEN** 输出守护进程不可达提示，退出码为 3

### Requirement: shell 应用移除

shell SHALL 支持 `remove <app>`，经控制面 app 移除端点执行，语义与 `ak remove <app>` 一致：仅允许无 `running`、`starting`、`stopping`、`backoff` 程序的 app，成功后立即删除注册并清理状态，无需 apply，且不自动停止程序。shell MUST NOT 本地直接修改注册文件。

拒绝时 SHALL 显示控制面返回的阻塞程序及状态，并提示先执行 `stop <app>`；守护不可达、未知 app 与失败的退出码遵循 shell 单命令模式的既有约定。移除成功后 shell 可继续接受命令，后续状态查询不再显示该 app。

#### Scenario: shell 中移除运行应用被拒绝

- **WHEN** 在 shell 中对含 running 程序的 web 输入 `remove web`
- **THEN** 显示冲突原因与先执行 `stop web` 的提示，web 继续运行且注册不变，shell 不退出

#### Scenario: shell 中停止后移除

- **WHEN** 在 shell 中执行 `stop web`，待全部程序停止后输入 `remove web`
- **THEN** web 的注册与状态立即清除，输出移除结果且不要求输入 apply，后续状态查询不含 web
