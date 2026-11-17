## 1. 配置模型：程序归属可空

- [x] 1.1 `config::ResolvedProgram.app` 改为 `Option<String>`（散装为 `None`），并为 `ResolvedProgram` 提供 `is_standalone()` / `owner_label()` 辅助；编译驱动修正全部 `def.app` 使用点（预期约 30 处）
- [x] 1.2 `config::ResolvedApp` 增加 `standalone: bool`；新增 `resolve_standalone(path, raw, defaults)`，复用 `resolve_app` 的字段合并但把程序归属置 `None`、`standalone = true`
- [x] 1.3 散装记录加载兼容：`adhoc/` 内旧记录若含 `[app]`，只取 `description` 与未显式配置的 restart/backoff 缺省值，MUST NOT 生成 AppRecord；单测覆盖旧格式（含 `[app].restart`）与新格式（无 `[app]`）
- [x] 1.4 `validate_all` 保持并补齐不变量：程序名跨 app 与散装全局唯一，散装记录名 MUST NOT 与 app 名冲突；单测覆盖冲突矩阵

## 2. 注册表：散装记录渲染与分类

- [x] 2.1 `registry::render_scaffold` 增加散装模式：默认不写 `[app]`；`--description` 才写仅含该字段的 `[app]`；`restart`/`restart_backoff` 等 run flag 写入 `[program.<name>]`；compose 调用点保持旧输出（单测 snapshot 对比）
- [x] 2.2 `registry::list()` 的返回项增加 `standalone`（`adhoc/` 为 true、顶层为 false），按位置分类、与文件内容无关；保持 `Origin::{Run,Compose}` 与顶层优先去重
- [x] 2.3 散装记录的 `description` 解析：优先 `[app].description`，否则 `None`；`ps --all` 与投影的 description 字段同源
- [x] 2.4 单测：同名跨区（顶层 + `adhoc/`）不并存、悬空链接、`adhoc/` 旧格式、顶层历史 run 文件按 compose 分类

## 3. 守护进程：命名、暂存与命令作用域

- [x] 3.1 `resolve_adhoc_name` 收紧为「程序名」：显式名不排重、重复回 409；缺省名取 stem，占用则 `<stem>-N`（N = 现存最大值 + 1，不填空号）；`/`、空值、非法字符、`all` 退出 2 / 校验错误；单测矩阵
- [x] 3.2 占用判定收敛为 `Free | Taken | Staged` 单谓词：顶层/`adhoc/` 记录（含悬空链接）、内存 app、任意程序名、暂存 app；缺省循环遇 `Staged` 立即走残留确认；表驱动单测
- [x] 3.3 `cmd_adhoc_up` / `adhoc_force` 适配散装记录：落盘 `app_dir/adhoc/<program>.toml`、不注册 AppRecord；`--force` 仅覆盖 `adhoc/` 记录，逐字节相同为空操作，顶层/链接/手工文件拒绝；单测覆盖 409、覆盖、空操作、拒绝矩阵
- [x] 3.4 `Command::Remove` 增加散装分支：`Program` 命中散装记录时删记录、清内存/pending/用户停止记录，返回 `RemoveOutcome { standalone: true, ... }`；`AppAll` 走既有 app 移除；app 内程序单独 remove 用法错误；`apply_detection` 整批预检覆盖散装
- [x] 3.5 实现统一目标解析器 `Target`：`<program>` / `<app>/<program>` / `<app>/all`；空段/多斜杠/裸 `all` 退出 2；单 token 按程序解析，只命中 app 名时 404 并提示 `<app>/all`；`app/program` 校验程序归属；双位置 `<app> <program>` 归一化为同义；单测覆盖全部分支
- [x] 3.5a `start/stop/restart/apply/remove` 接入 `Target` 路由：程序目标走 `/v1/programs`，`app/all` 走 `/v1/apps`；`remove` 的散装/app 分支与错误提示按 D6；单测覆盖 app 扇出（`app/all`）、app 单程序、散装单名、裸 app 名报错、`all` 报错
- [x] 3.6 `AdhocUpResult` 增加 `standalone: bool` 与 `auto_named: bool`，穿透成功 / `unchanged` / `residual` / `register_only` 全部构造点
- [x] 3.7 回归断言「app 仅由 compose 产生」：run 的任何路径（成功/覆盖/残留/补落盘）都不写 `app_dir` 顶层、不创建 AppRecord、不产生 app 名；compose 产物（含只注册未启动）仍按 app 处理

## 4. 控制面投影与服务端

- [x] 4.1 `server::ProgramInfo` 增加 `standalone: bool`；散装条目 `app` 为空字符串、`config_path` 指向 `app_dir/adhoc/<name>.toml`；集中在单一投影函数产生，确保 `/v1/status`、`/v1/programs`、`/api/*`、WS 同源
- [x] 4.2 `/v1/run` 的 `parse_adhoc_spec` 去掉 `app/program` 拆分；终端 NDJSON 帧与 `--json` 携带 `standalone`/`auto_named`；端点测试覆盖显式 409、缺省顺延、旧格式兼容、非法名
- [x] 4.3 `/v1/apps/<name>/*` 不把散装程序当 app 展开，app 移除端点对散装名返回 404；新增 `POST /v1/programs/<name>/remove`（仅散装，app 内程序 400）并复用 `Command::Remove`；`/api/programs/<name>/remove` 与 `/api/apps/<app>/remove` 同构；`/v1/apply` 的活态删除预检覆盖散装
- [x] 4.4 WS 增量帧的 `removed`/`apps_gone` 对散装移除的收敛语义：散装只发 `removed`（程序名），不发 `apps_gone`；补 server 端测试

## 5. CLI 与 shell 输出

- [x] 5.1 `main.rs`：`Cmd::Run` 移除 `--app`/`scaffold_name` 的 app 语义，`--name` 为程序名；`--name` 含 `/` 直接退出 2；`AdhocFlags`/`split_adhoc_trailing` 的 `FLAGS` 列表同步；`run --help` 只描述单程序名
- [x] 5.2 `run_adhoc` 的 JSON body 去掉 app 维度，终端输出增加 `standalone` 与自动命名提示；`--json` 只增字段
- [x] 5.3 `shell.rs` 的 `ps` 表格：散装程序 APP 列显示 `-`；`ps --all` 的本地注册表清单标注 `run`/`compose` 来源；表格测试覆盖
- [x] 5.4 `remove`/`start`/`stop`/`restart`/`apply` 的帮助文案改为统一目标文法 `<program> | <app>/<program> | <app>/all`，说明单 token 是程序、整组用 `<app>/all`、`all` 为保留字；错误信息对 `--app`、裸 app 名给出迁移提示（`app` → `app/all`）
- [x] 5.5 顶层 `after_help` 的 `Examples:` 改为少于 5 行：覆盖 daemon 启动、`run` 散装注册、单程序目标、`<app>/all` 整组，注释提示 `<app>/<program>`；单测断言三种写法均出现、无 `--app`、无裸 app 名整组示例

## 6. Web 控制台

- [x] 6.1 `webui/src/lib/api.ts` 的 `ProgramInfo` 类型增加 `standalone?: boolean`（缺失视为 false）；store/overview 的 app 分组逻辑跳过散装程序
- [x] 6.2 左侧导航：散装程序渲染为顶层程序节点，不建 app 节点；选中走程序详情路由；样式与 app 内程序节点一致
- [x] 6.3 全局总览：散装程序独立「散装程序」分区，行字段与应用内程序一致；故障/退避排序规则沿用
- [x] 6.4 App 概况/详情对散装程序不显示「应用」归属或显示 `-`；移除操作说明删除边界
- [x] 6.5 `pnpm exec tsc --noEmit && pnpm test && pnpm build` 通过，并补一个 standalone 渲染测试

## 7. 文档与验收

- [x] 7.1 更新 `README.md`：`ak run` 只造散装程序、`--name` 可选、缺省自动排重、显式重复 409、`--force` 只覆盖 `adhoc/`、`--app` 已移除、`ps` 的 `-` 列
- [x] 7.2 `xtask/src/e2e.rs` 增加矩阵：散装注册/启动/停止/移除、连续两次 run 得 `<stem>`/`<stem>-1`、显式冲突 409 零副作用、`--app` 退出 2、`--force` 覆盖与空操作、旧格式 `adhoc/` 记录可启动、`start <program>` 单程序、`start <app>/all` 整组、`start <app>/<program>` 指定程序、裸 app 名报错并提示 `<app>/all`、`all` 退出 2、散装程序不出现在 `/v1/apps` 扇出
- [x] 7.3 全量回归：`openspec validate add-standalone-programs`、`cargo test --workspace`、`cargo build --release`、`cargo run -p xtask -- e2e --no-browser` 全绿
