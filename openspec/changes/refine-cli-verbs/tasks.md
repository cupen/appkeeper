# Tasks: refine-cli-verbs

## 1. CLI 面（`src/main.rs`、`src/client.rs`）

- [x] 1.1 新增 `Cmd::Start` 并统一 `Start|Stop|Restart` 为 `<app> [<program>]`
  双位置参数（镜像 Apply）：裸 `<app>` 整 app 按序扇出，第二参数收窄单程序；
  三者 `--app` 旗标移除。Start 承接原 run 启动路径，输出逐程序
  `{程序名, 配置路径, PID}`，`--json` 单对象/扇出数组；409/404/离线 3 不变。
  验证：单测覆盖 app 级/程序级/未知 app/未知程序，手动验证 `ak start <app>` 与
  `ak start <app> <program>`
- [x] 1.2 `Cmd::Run` 收窄：删除 positional `name` 与 `--app` 扇出参数（ad-hoc
  `--`/`--cmd` 之外无形态），`--name` 改名 `--app`（ad-hoc 应用命名）；裸 `ak run`
  用法错误列出 ad-hoc 两形态（退出码 2）；删除 `reject_removed_start` 拦截器及其
  单测（`start` 合法化）。验证：单测覆盖新用法错误与 `--name` 拒绝
- [x] 1.3 `reload` 迁入 config：删除顶层 `Cmd::Reload` 分派，`ak config` 增
  `--reload` 旗标（单独使用，或与 `--set`/`--delete`/`--edit` 组合——本地动作
  成功后随即触发重载；与 `--get`/`--init` 组合用法错误退出 2），经既有
  `client.reload()` 调 `/v1/reload`，离线退出码 3、daemon 上报失败非零退出；
  用户可见提示中的 `ak reload` 文案改为 `ak config --reload`（config 写后在线
  提示、compose 在线同步失败补救提示等）。验证：`grep -rn "ak reload" src/`
  无命中；单测覆盖组合用法错误与离线 3
- [x] 1.4 帮助简洁化（design D7）：顶层 `ak --help` 用 clap `after_help` 附三行
  基本用法示例（`ak daemon start` 启动守护进程 / `ak start <app>` 启动应用 /
  `ak stop <app>` 停止应用）；全部动词 about 与旗标帮助文案收紧为短句。
  验证：`ak --help` 末三行示例可见且各动词帮助为一句话级短句
- [x] 1.5 帮助与错误文案统一 app 为主词：管理动词的参数标为 APP/PROGRAM，说明
  「APP 为应用名；可选 PROGRAM 仅作用于单个程序」；`pid/log/action/signal` 保留
  program 并注明「单程序 app 与 app 同名」。验证：`ak --help` 与各子命令 help 无
  含糊混称，三行示例与双位置语法一致

## 2. `ps` PENDING 列（`src/main.rs`/`src/shell.rs` 渲染器）

- [x] 2.1 共享 status 渲染器加 PENDING 列：命中 `StatusDoc.pending.programs` 的
  程序行标 changed/new 类短标记，未命中为空；表格尾部追加摘要行
  （apps_added/apps_removed/daemon_hints/errors 非空时逐行）；`ps --all` 全量视图
  未加载 app 的行同样标记。验证：构造 pending 磁盘差异后 `ak ps` 见标记、
  `ak apply` 后消失
- [x] 2.2 确认 `ps --out json` 零改动（pending 已在投影内），补单测断言投影含
  pending 字段。验证：`ak ps --out json | jq .pending` 可解析

## 3. shell 词表（`src/shell.rs`）

- [x] 3.1 shell 词表与 CLI 对齐：三元组 `run|stop|restart` 改
  `start|stop|restart`（`<app> [<program>]` 双位置参数），`status`/`st` 改名
  `ps`，删除 `reload` 词与分派，similar-hint 对 `reload` 建议 `pending`/`apply`、
  对 `status`/`st` 建议 `ps`；表格随共享渲染器带 PENDING 列。验证：
  `cargo test -p appkeeper` shell 单测改写后通过（含 `status`/`st`/`reload`
  unknown-command 断言与 app 级/程序级参数）

## 4. 测试与 e2e 同步

- [x] 4.1 `xtask/src/e2e.rs`/`stress.rs`/`tests/` 全量：run 启动/扇出场景改
  `start <app> [<program>]`，stop/restart 的 `--app` 改位置参数，ad-hoc 场景
  `--name`→`--app`、`reload` 调用迁至 `ak config --reload`（webui 热开/热关
  e2e 随之闭环）；补新断言：多程序 app 裸名整 app、第二参数单程序、
  `start <app> <program>` 409、三者 `--app` 旗标拒绝、`ak run --name x` 拒绝
  （退出码 2）、`ak reload` 未知子命令、`ak config --reload` 离线 3 与
  `--get` 组合退出 2、ps PENDING 列标与摘要行、apply 后标记消失；`shell -e`
  调用同步
  （`status`→`ps`、`run`→`start`）。验证：
  `cargo run -p xtask -- e2e --no-browser` 全绿，日志落 /tmp/spec-go-refine-e2e.log
- [x] 4.2 `cargo fmt --check`（本 change 触及 crate）+ `cargo clippy` 零新增 +
  `cargo test --workspace` 全绿。验证：命令退出码 0（HEAD 既有漂移除外）

## 5. 文档与收口

- [x] 5.1 README 命令表/示例更新 + 迁移对照表补管理面映射（`run <name>`→
  `start <app>`、`run --app <app>`→`start <app>`、ad-hoc `--name`→`--app`、
  `reload`→`ak config --reload`；旧 `stop|restart --app`→位置参数）；
  AGENTS.md 动词面描述同步。验证：`grep -n "ak reload\|ak run <\|--app" README.md
  AGENTS.md` 仅迁移表/历史锚点/ad-hoc 命名命中
- [x] 5.2 `openspec validate refine-cli-verbs` 与 `openspec validate --specs`
  通过；归档顺序确认（refactor-cli-verbs 先、本 change 后），本任务不执行归档。
  验证：两条 validate 退出码 0
