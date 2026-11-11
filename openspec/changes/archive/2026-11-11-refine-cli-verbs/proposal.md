# Proposal: refine-cli-verbs

## Why

动词面重构后遗留几处语义不顺：`run` 身兼「启动已注册」与「注册并跑新的」两义（docker run 心智在守护场景里反直觉，与 `stop` 无对偶）；`ak reload` 占据顶层动作位但本质是 daemon 配置面操作（守护周期检出已使其非必需）；pending 状态持续存在于 `/v1` 投影却无 CLI 可见面，变更只能靠 `ak apply` 盲推或 shell 内 `pending` 查看。收尾：`start` 与 `stop` 对偶、`ps` 直显 pending、`reload` 收编为 `ak config --reload`、`run` 专注 ad-hoc 注册。

## What Changes

- **新增 `ak start`**：与 `stop` 完全镜像——`ak start <app> [<program>]` 双位置
  参数（镜像 `apply` 的范围语法）：裸 `<app>` 按既定顺序扇出启动整个 app，第二
  参数收窄到单程序；输出契约沿用原 run 启动形态：逐程序 `{程序名, 配置路径,
  PID}`，支持 `--json`（单对象/扇出数组）；对已 running 程序 409、未知 404、
  守护离线退出码 3 且不隐式拉起
- **BREAKING** app/program 命名语义统一（管理面 app 优先）：`stop|restart` 随
  `start` 一致改为 `<app> [<program>]` 双位置参数，三者原有的 `--app` 旗标移除
  （裸名从程序级改为 app 级——单程序 app 行为不变，多程序 app 停整 app 需显式
  第二参数停单程序）；`pid|log|action|signal` 等进程面动词保留程序名寻址（程序名
  跨应用全局唯一），帮助文案注明「单程序 app 与 app 同名」；帮助文本全面以 app
  为主词（47 处 program 措辞收紧），多程序区分处保留 program 说明
- **BREAKING** `ak run` 收窄为纯 ad-hoc 注册动词：仅剩 `run -- <cmd...>` 与
  `run --cmd "<字符串>"` 两形态；positional `<name>` 启动形态与 `--app` 扇出形态
  删除（由 `start` 承接）；ad-hoc 命名参数 `--name` 改名 `--app`（如
  `ak run -- python -m http.server --app web`）；`run --json` 维持
- **BREAKING** `ak reload` 顶层动词移除，重载入口迁至 **`ak config --reload`**
  （config 动作族第六个旗标，与 `--set/--get/--delete/--edit/--init` 同族）：可
  单独触发守护进程重读 daemon 配置并立即重扫（webui 热开/热关由此外闭环），
  也可与写动作组合一步生效（`ak config --set webui.listen=... --reload`）；离线
  退出码 3；`/v1/reload` 端点保留（控制面 API 零变化）；相关补救提示从「重跑
  `ak reload` 与 `ak apply`」改为「`ak config --reload` 后 `ak apply`（或等待
  一个重扫周期）」
- **BREAKING** `reject_removed_start` 拦截器删除：`start` 重新成为合法动词
  （旧「start 已移除、改用 run」的需求与提示整体反转）
- `ak ps` 表格增列 **PENDING**：程序行标注待应用变更（changed/new 等），app 级
  注册增删与 daemon 级 hints/检出错误以表格尾部摘要行呈现；`ps --all` 全量视图
  中未加载的已注册 app 行同样标注；`ps --out json` 零变化（`StatusDoc.pending`
  已含全部信息）
- shell REPL 词表与 CLI 全面对齐：`run` 改为 `start`（三元组
  `start|stop|restart`，`<app> [<program>]` 双位置参数）、`reload` 词删除
  （`pending` 详情命令保留）、`status`（含 `st` 别名）改名 `ps`；表格随共享
  渲染器带 PENDING 列
- 帮助信息简洁化：顶层 `ak --help` 与子命令帮助文案收紧为短句；顶层帮助末尾
  SHALL 给出三行基本用法示例（启动 daemon → 启动 app → 停止 app）
- webui 行为零变化（仅规范中 `ak reload` 引用随入口迁移改写；WS 快照已携带
  pending）；`/v1` 端点零增删

## Capabilities

### New Capabilities

（无——`start` 为既有控制面端点 `/v1/apps/<app>/start` 与程序级启动动作的 CLI 承接）

### Modified Capabilities

- `control-plane`: 「CLI 控制命令」需求变化——新增 start 动词契约；run 收窄为
  ad-hoc 两形态（--name→--app）；reload 顶层动词移除（入口迁至
  `ak config --reload`）；ps 表格 PENDING 列；裸 run 用法错误与 start 反转相关
  文案/场景改写
- `configuration`: 「config 子命令」需求变化——动作族新增 `--reload` 旗标
  （单独或与写动作组合、离线 3、纯本地例外与写后提示）；「按应用粒度的热更新」
  的 `ak reload` 引用改为 `ak config --reload`（并校正已移除键 `port` 场景为
  `socket`）
- `apply-workflow`: 「pending 检出」需求变化——「`ak reload` 触发立即重扫」句
  改为「经 `/v1/reload` 端点触发，CLI 入口 `ak config --reload`」
- `app-registry`: 「离线可用」需求变化——在线同步失败的补救提示改为
  `ak config --reload`（或等待重扫周期）
- `shell-client`: 「shell 内置命令集」需求变化——`run` 词改 `start`、`reload`
  词删除、status 表格描述增 pending 标记列
- `webui-api`: 「配置驱动的控制台生命周期」需求变化——`ak reload` 引用随入口
  迁移改为 `ak config --reload`（热生效语义不变）
- `glossary`: 「命名与寻址」需求变化——CLI 管理面动词（start/stop/restart/apply）
  以 `<app> [<program>]` 双位置参数 app 优先寻址；API/webui/进程面动词维持程序名
  全局唯一无前缀寻址；reload 注明为配置面动作仍属保留字

## Impact

- 代码：`src/main.rs`（Cmd 枚举：Start 新增、Run 收窄、Reload 删除、config 增
  `--reload` 旗标与分派、ps 渲染 PENDING 列）、`src/shell.rs`（词表 run→start、
  删 reload、status 表格列）、`src/client.rs`（start 封装、ps 渲染；reload 封装
  复用）
- 删除面：`reject_removed_start` 拦截器及其单测；run 的 positional/--app 扇出
  分派；`Cmd::Reload` 顶层分派
- 测试：xtask e2e 的 run 启动/扇出场景改 `start`（含 `--json`）、reload 调用迁
  `ak config --reload`（webui 热开/热关 e2e 闭环）、ad-hoc 场景 `--name`→`--app`；
  shell 单测词表断言
- 文档：README 命令表/示例/迁移对照表（run→start、reload→`ak config --reload`、
  --name→--app）、AGENTS.md 动词面描述
- 不受影响：webui 行为、`/v1` API、`stop|restart|apply|action|signal|pid|log|ps`
  其余参数、`compose --name`（注册命名，不随 ad-hoc 改名）
