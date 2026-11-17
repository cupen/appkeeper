## Context

现行 `ak run` 把每次 ad-hoc 启动都包装成一个 app：命名参数是 `--app`（后改为
`--name`），注册记录是 app 粒度的 `app_dir/adhoc/<name>.toml`，`[program.<name>]`
外面总有一个 `[app]` 段；`Supervisor` 用 `apps: Vec<AppRecord>` 归组，`ProgramInfo`
的 `app` 字段恒非空，`ps`/webui 按 app 分组。由于 run 不支持向已有 app 追加第二个
程序，这层 app 对 ad-hoc 场景没有分组价值，只增加命名与心智负担。

本变更把 **散装程序（standalone program）** 提升为一等注册对象：它不属于任何 app，
注册记录以程序名命名，配置可省略 `[app]`，用全局唯一的程序名直接寻址；`ak run`
只生产这种记录。同时把 **app 的定义收窄为「由 `compose up`（及 compose 注册）
建立并管理的进程组」**——app 身份由注册来源/记录位置决定，不随 `stop`/`start`
变化；多程序分组只由 compose 承担。

约束：

- 程序名跨全部已注册应用与散装程序全局唯一（`config::validate_all`）；app 名与
  程序名共享同一字符集 `[A-Za-z0-9_-]`（`config::is_valid_name`）；`all` 是 app
  名保留字；
- 守护进程命令队列是唯一写者，命名解析、暂存与落盘必须在同一命令处理路径内完成；
- 日志按程序名落盘（`log_dir/<name>.out.log`），名字复用会让新旧运行混写；
- 状态投影被 `/v1`、`/api`、WS 三处共用，字段语义必须同源。

## Goals / Non-Goals

**Goals:**

- 术语收敛：只有 compose 建立并管理的进程组是 app；run 产物一律散装程序；
- `ak run` 只创建散装程序：`--name <程序名>` 可选，缺省按命令 stem 自动排重；
- 散装程序不伪装成 app：记录位于 `app_dir/adhoc/`、配置可省略 `[app]`、投影用
  `standalone` 明确标识、寻址走单程序名；
- 显式命名可预测：重复一律 409、零副作用；`--force` 只覆盖 run 自家记录；
- 既有 `adhoc/` 记录（含旧版带 `[app]` 的）按位置识别为散装程序，无需迁移。

**Non-Goals:**

- 不支持把第二个程序追加进已存在记录（多程序 app 由 `compose` 承担）；
- 不改变 compose 产物的 app 语义，不迁移顶层历史 run 文件；
- 不引入持久命名计数器（已删除的最高编号仍会复用）；
- 不做名字清洗或路径式解析（`/` 不是合法程序名字符，不解释为路径或分组）。

## Decisions

### D1. `run` 只接受单程序名，不再有 app 维度

CLI 移除 run 的 `--app` 与 `--name app/program` 命名形态；`--name` 的值就是程序名，
`/` 在该参数中直接按非法字符拒绝（退出码 2）。未给 `--name` 时基名取命令首 token
的 `file_stem()`；
`file_stem` 非法或为空时回退到 `program` 或 `app`，仍非法则报错要求 `--name`。

`AdhocSpec.name` 由 `Option<String>` 保留，但含义收紧为「程序名」；服务端不再有
拆分函数。备选：保留 `app/program` 语法并让上层忽略 app——会产生「写了但不生效」
的假功能，且与「散装程序不属于 app」的术语矛盾；放弃。

### D2. 散装记录是一等对象，但复用现有单程序记录格式

记录仍落 `app_dir/adhoc/<程序名>.toml`（真实文件 + 同区域 `*.toml.pending` 占位），
文件内容为一个 `[program.<程序名>]` 表；默认**不写** `[app]`。`--description`
存在时写一个只含 `description` 的 `[app]` 段作为元数据，`--restart` /
`--restart-backoff` 等 run flag 写入 `[program.<程序名>]`（程序级字段），避免把
运行参数塞进「app 元数据」造成语义错位。

模型改动：

- `config::ResolvedProgram.app: String` → `Option<String>`（散装为 `None`）；
- `config::ResolvedApp` 增加 `standalone: bool`（散装记录名 = 程序名，`programs`
  通常只有一项）；
- `config::resolve_standalone(path, raw, defaults)` 复用 `resolve_app` 的字段合并，
  但把每个程序归属置为 `None`、`standalone = true`；`AppRaw::load` 仍要求至少一个
  程序；
- 旧版 adhoc 记录的 `[app]` 段在散装加载时只作为元数据/缺省值来源（`description`
  与程序字段未显式配置时的 restart/backoff），MUST NOT 生成 `AppRecord`。

备选：合成一个 `_adhoc` app 承载全部散装程序——会让 `remove`/扇出/寻址退化为整组
操作，还要程序级 upsert；放弃。备选：新建独立 `standalone/` 目录——既有 `adhoc/`
记录就无法「按位置识别」，需要迁移；放弃。

### D3. 注册表按位置分类，不新增文件类型

`registry::list()` 仍先扫顶层（compose）再扫 `adhoc/`（run）；`ListedApp` 扩展为
`ListedRecord { name, path, description, broken, origin, standalone }`（或保留类型名
并加字段）。`adhoc/` 内的记录一律 `standalone = true`，顶层一律 `false`，与文件
内容是否有 `[app]` 无关。`Origin::Run` 与 `Origin::Compose` 语义不变，`ps --all`
仍按 origin 标注。

`registry::render_scaffold` 增加 `standalone`（或 `ScaffoldKind`）参数：散装模式不
写 `[app]`（除非 description），并把 restart/backoff 写进程序表；compose 调用点
传 `standalone = false`，输出与现在一致。

### D4. 命名占用是「app 名 ∪ 程序名」的单一谓词

`candidate_status(name) -> Free | Taken | Staged`：

- `Taken`：`app_dir` 顶层与 `adhoc/` 的记录路径 lstat（含悬空链接）、内存中的
  app 名、任意程序的程序名、暂存 app 名；
- `Staged`：`state.staged` 中存在该名字；
- 其余 `Free`。

缺省名循环：`Free` 采用、`Taken` 顺延 `<base>-N`、`Staged` 立即返回该候选交给
残留确认流程。显式名不循环：`Taken` → 409，`Staged` → 残留确认。N =
现存 `<base>-<k>` 的最大 k + 1，不填中间空号；已删除的最高编号会复用（无持久
计数器），写入 spec 作为已知边界。

### D5. 显式名重复 409，`--force` 只覆盖不排重

`--name` 是显式意图：命中任何占用即 409，MUST NOT 追加后缀或改名；`--force`
以同一名字为覆盖目标（未给 `--name` 时取 stem），不参与排重。覆盖仍限
`app_dir/adhoc/` 内本工具生成的记录：顶层 compose 产物、外部部署链接与手工文件
拒绝覆盖；逐字节相同为空操作；`--restart=no` 只重写记录并停止旧进程、不拉起。

### D6. 统一目标文法：单 token 是程序，`app/all` 才是整组

管理面动词先解析出一个 `Target`：

```text
<target> := <program> | <app>/<program> | <app>/all
```

- 按 `/` 切分；空段（`/p`、`app/`）或两个以上 `/` → 用法错误（退出码 2）；
- 无 `/`：值等于 `all` → 用法错误（`all` 为保留字）；否则为 `Program(value)`；
- `app/all` → `AppAll(app)`；`app/program` → `AppProgram { app, program }`；
- 第二位置 `<app> <program>` 归一化为 `AppProgram`（旧形式别名）；斜杠形式与双位置
  形式同时出现时视为多余参数，用法错误。

解析与路由：

- `Program`：按全局唯一程序名查 app 内程序或散装程序；查不到但存在同名 app 时
  返回未知程序，并提示 `<app>/all`，MUST NOT 隐式扇出；
- `AppProgram`：先查 app 再校验程序归属；程序存在但属于别的 app 时返回 404，
  不跨 app 操作；
- `AppAll`：只查 app；散装程序永远不匹配（它不是 app）。
- 线上映射：`Program`/`AppProgram` → `/v1/programs/<program>/*`（`remove` 为
  `/v1/programs/<program>/remove`）；`AppAll` → `/v1/apps/<app>/*`。

`Command::Remove` 的散装分支：`Program` 命中散装记录时删除 `adhoc/<name>.toml`、
清内存程序与 pending/用户停止记录，返回 `RemoveOutcome { standalone: true, ... }`；
活态检查复用 `ProgramState::removable()`。`AppAll` 走既有 app 移除。app 内程序
不接受单独 remove（`Program` 命中 app 内程序时用法错误，提示改 compose 声明）。
`apply_detection` 的整批预检同样覆盖散装记录。

### D7. 投影显式标记 `standalone`，`app` 为空

`ProgramInfo` 增加 `standalone: bool`；散装程序的 `app` 为空字符串（不伪造 app 名），
`config_path` 由记录路径推导（`app_config_path` 对散装返回 `adhoc/<name>.toml`，
或由 `ResolvedApp.path` 直接携带）。该字段在 `server.rs` 的单一投影函数中产生，
`/v1`、`/api`、WS 三处自动同源。

`ak ps` 的 APP 列对散装程序显示 `-`；`--out json` 零新增顶层结构（仅程序条目多
一个字段）。webui 侧栏不再为散装程序建 app 节点，总览增加「散装程序」顶层分区；
前端以 `standalone` 判断，不靠 `app === name` 猜测。

### D8. 成功结果与 `auto_named`

成功结果保持 `{程序名, 配置路径, PID}`，并增加 `standalone: true`（所有 run 产物）
与 `auto_named`（仅缺省名发生顺延时为 true）。人类输出顺延时提示「自动命名：<最终
名>（基名 <stem> 已被占用）」。`--json` 字段只增不改，旧脚本仍可读。

### D9. 顶层 help 的 Examples 是上手入口

`Cli::after_help` 保持少于 5 行，固定覆盖四件事：`ak daemon start`、`ak run --
<cmd>`（散装注册）、`ak start <program>`（单程序）、`ak stop <app>/all`（整组）；
最后一行注释提示 app 内程序写 `<app>/<program>`。不放 `--app`、不把裸 app 名示例成
整组操作。示例随目标文法同步修改，e2e 断言行数与三种写法均出现。

## Risks / Trade-offs

- [`--app` 与 run 的 `app/program` 命名移除是 BREAKING] → 版本 0.1.0 未发布，
  接受；help 与 README 第一屏写明 `--name`，`--app` 报未知参数并提示迁移。管理面
  新增 `app/program` 寻址文法，与 run 命名是两套入口，互不影响。
- [`ResolvedProgram.app` 变为 `Option` 波及面大] → 用编译器驱动（31 处 `def.app`
  使用点逐一改为 `owner()` 或显式分支）；投影层收口到 `standalone`，避免上层到处
  判空。
- [旧 adhoc 记录带 `[app]` 可能被误当 app] → 分类只看位置；加载器按散装路径调用
  `resolve_standalone`，`[app]` 只取元数据，绝不注册 AppRecord；e2e 覆盖旧记录。
- [`--force` 覆盖后 `[app]` 消失导致旧元数据丢失] → 覆盖是整文件重写，符合既有
  「有差异即重写」语义；description 若未重新给出则不保留，spec 明示。
- [日志混写：最高编号删除后复用] → 已知边界，spec 记录；如需严格历史唯一可后续
  引入计数器状态。
- [webui 旧数据无 `standalone` 字段] → 前端把缺失视为 `false`（兼容旧守护进程），
  新守护进程恒发该字段。

## Migration Plan

无强制迁移：`adhoc/` 内既有记录（含 `[app]`）按位置识别为散装程序，继续可启动/
停止/移除；其 `[app].restart` 等值作为缺省被读取，直到下次 `--force` 重写。
compose 产物与顶层历史 run 文件语义不变。回滚只需恢复 `--app`、复合名与「假 app」
投影行为；记录无需回改。

CLI 迁移：`ak run --app X` → `ak run --name X`；`ak run --name app/prog` → 改为
`ak run --name prog`（需要分组请用 `compose`）。管理面整组操作由裸 app 名改为
`<app>/all`：`ak start|stop|restart <app>` → 加 `/all`；`ak remove <app>` →
`ak remove <app>/all`；`ak apply <app>` → `ak apply <app>/all`。双位置
`<app> <program>` 仍可用，等价于 `<app>/<program>`。不设 `--app` 别名，避免两套语义。

## Open Questions

（无）
