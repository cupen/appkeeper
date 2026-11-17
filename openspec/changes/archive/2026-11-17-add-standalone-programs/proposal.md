## Why

`ak run` 的定位是「跑一条临时命令」，但当前每个 run 产物都被迫包一层 app：命名参数
是 `--app`、注册记录是 app 记录、状态投影与侧栏按 app 归组。既然 `ak run` 不支持向
已有 app 追加第二个程序（同一 app 再 run 只能 409 或整份覆盖），这层 app 对 ad-hoc
场景不提供任何分组价值，只带来命名与心智负担。

正确的模型是把 **散装程序（standalone program）** 提升为一等注册对象：它不属于任何
app，注册记录以程序名命名，配置里可以没有 `[app]`，直接用全局唯一的程序名寻址。
`ak run` 只生产这种记录；多程序分组仍由 `compose`（app 注册）承担。

## What Changes

- **收窄 app 的定义**：只有 `compose up`（及 compose 注册）建立并管理的进程组才叫
  app；`ak run` 产物一律是散装程序，永不是 app。app 身份由注册来源/记录位置决定，
  不随 `start`/`stop` 等运行时状态变化。
- **`ak run` 只造散装程序**：移除 `--app`，`--name <程序名>` 可选；不再支持
  `app/program` 复合名。缺省名取命令首 token 的 stem。
- **散装程序是一等记录**：落位 `app_dir/adhoc/<程序名>.toml`，**不属于任何 app**，
  不参与 app 归组；其配置默认不写 `[app]` 表（`restart`/`restart_backoff` 等 run
  flag 写入 `[program.<name>]`；`--description` 存在时才写一个仅含元数据的可选
  `[app]` 段，不构成 app 身份）。
- **寻址**：`start|stop|restart|apply|remove` 接受统一目标文法——`<program>`
  （单 token 一律按程序解析，app 内程序或散装程序均可）、`<app>/<program>`、
  `<app>/all`（整组操作）。程序名全局唯一；`all` 为保留字，不得作程序名。
  仅 `<app>/all` 触发 app 扇出；单 token 只命中 app 名时报错并提示 `<app>/all`。
- **帮助上手**：顶层 `ak --help` 末尾的 `Examples:` 少于 5 行，覆盖 daemon 启动、
  `run` 散装注册、单程序目标与 `<app>/all` 整组，注释提示 `<app>/<program>`。
- **命名与排重**：未给 `--name` 时按 stem 自动命名，被占用则单调递增后缀
  （`npx` → `npx-1` → `npx-2`…）；显式 `--name` 不排重，重复返回 **409**，零副作用。
  **BREAKING**：`--app` 成为未知参数（退出码 2）；缺省名重复不再 409 而是自动顺延；
  管理面裸 app 名不再整组操作，整组须写 `<app>/all`。
- **`--force`**：仅覆盖 `app_dir/adhoc/` 内的散装程序记录；逐字节相同为空操作；
  不排重、不改名；顶层 compose 产物与外部链接仍拒绝覆盖。
- **状态投影与展示**：程序投影新增 `standalone: true`；散装程序的 `app` 字段为空
  （不表示任何 app）。`ak ps` 的 APP 列对散装程序显示 `-`；webui 侧栏与总览把散装
  程序作为顶层程序呈现，不创建 app 分组节点。
- **兼容**：既有 `adhoc/` 记录（含带 `[app]` 的旧 run 产物）按位置识别为散装程序，
  不迁移、继续可启动/停止/移除；compose 产物仍是 app，语义不变。
- 命名解析、占用判定与残留确认仍在守护进程命令队列内完成（无客户端预检竞态）。

## Capabilities

### New Capabilities

（无）

### Modified Capabilities

- `glossary`: 收窄 app 定义为「compose 建立并管理的进程组」，新增「散装程序」：
  不是 app、不要求所属 app、以全局唯一程序名寻址；管理面动词的单参数形态可解析到
  散装程序。
- `app-registry`: run 产物由「单程序 app」改为「散装程序记录」；命名、占用、409、
  `--force`、移除与注册表分区语义同步调整，配置可省略 `[app]`。
- `control-plane`: CLI `run` 的 `--name`（无 app）、`start/stop/restart/apply/remove`
  对散装程序的解析；注册端点与状态投影（`standalone`、`app` 为空）同步。
- `webui-api`: `/api/*` 与 WS 复用控制面投影，程序投影新增 `standalone` 字段，
  散装程序不再带 app 归属；新增 `POST /api/programs/<name>/remove` 与
  `/v1/programs/<name>/remove` 同构。
- `webui-ui`: 左侧导航与全局总览把散装程序作为顶层程序呈现，不建 app 节点/分组；
  单程序操作与详情页对散装程序沿用程序级路径。

## Impact

- 代码：`src/config.rs`（程序归属可空 / run flag 落程序级）、`src/registry.rs`
  （散装记录渲染与识别、`render_scaffold`）、`src/supervisor.rs`（命名/占用/残留/
  状态与命令作用域）、`src/server.rs`（`ProgramInfo.standalone`、端点命名）、
  `src/shell.rs`（`ps` 的 APP 列）、`webui/`（TS 类型、导航、总览）、
  `xtask/src/e2e.rs`（run 与散装程序矩阵）。
- 文档：`README.md` 的 `ak run` / `ak ps` 用法与命名说明。
- 兼容性：`--app` 移除、run 产物不再算 app（见 BREAKING）；注册记录无需迁移。
- 存储：仍为 `app_dir/adhoc/<程序名>.toml`；配置格式去掉 `[app]`（可选保留
  description 元数据），注册表分区不变。