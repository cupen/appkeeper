# Proposal: guard-remove-until-stopped

## Why

当前 `ak remove` 先删除注册，再由后续 `ak apply` 停止并清理进程，使“注销”与“生效”脱节，也允许移除操作间接杀死仍在运行的程序。需要让所有移除入口遵循同一条前置规则：app 已无存活进程才可移除；命令式移除一步完成注册删除与状态清理，不再要求额外执行 apply。

## What Changes

- **BREAKING** `ak remove <app>` 改为 daemon 在线执行的独立原子动作：仅当该 app 没有 `running` / `starting` / `backoff` 程序时才接受；否则以冲突错误拒绝，保留注册与运行状态并提示先执行 `ak stop <app>`。`stopped` / `exited` / `fatal` 均视为可移除，不要求曾显式执行 stop；remove 不自动代为停止。
- **BREAKING** remove 成功后立即清除守护状态与注册，不产生待 apply 的注销项。守护离线时 CLI 退出码 3、不隐式拉起 daemon、不修改注册。`app_dir` 内脚手架生成的真实配置随注册删除；外部部署目录的配置本体继续保留，日志文件不受影响。
- 新增控制面与 Web API 的 app 移除端点，以及 shell `remove` 和 Web App 概况移除操作；全部经 Supervisor 同一个移除命令，不各自实现状态检查或文件删除。Web 操作需确认并呈现成功或失败结果。
- **BREAKING** 手工删除整个 app 注册或 app 内 program 定义后，apply 不得再自动停止对应存活程序。若 apply 范围内包含仍在 `running` / `starting` / `backoff` 的被删除 app/program，则整批拒绝：范围内其他变更也不应用，运行状态不变，磁盘删除不回滚，pending 保留并提示先 stop；停止后可由 apply 完成该移除。
- compose 的注册、变更与 apply 启动/重启语义不变；直接编辑配置的移除仍走 pending/apply，命令式 remove 则不走 pending。

## Capabilities

### New Capabilities

（无）

### Modified Capabilities

- `app-registry`: 「移除与查看」改为无存活进程前置检查、在线原子移除且无需 apply；「离线可用」将 remove 从离线直写范围排除。
- `control-plane`: 新增 app 移除 API 与 CLI daemon-first 行为、离线/冲突错误契约。
- `apply-workflow`: app/program 删除的应用阶段新增存活状态前置检查与整批拒绝语义。
- `shell-client`: shell 内置命令集新增与 CLI 一致的 app 移除入口。
- `webui-api`: Web API 新增复用控制面移除语义的端点。
- `webui-ui`: App 概况新增需确认、可反馈的 app 移除操作。

## Impact

- 后端：`src/supervisor.rs`（Remove 命令、状态守卫、apply 删除预检）、`src/registry.rs`（注册记录删除与外部配置边界）、`src/server.rs` / `src/api.rs` / `src/web.rs`（端点与状态投影）、`src/client.rs` / `src/main.rs`（在线 remove 客户端）。
- 前端与终端：`src/shell.rs`；`webui/` 的 App 概况、API 封装、类型与测试。成功后状态/WS 快照需同步，移除后不遗留已删除 app 的 pending 投影。
- 测试：registry、supervisor、server、client、shell、webui 单测与 xtask e2e；覆盖运行/启动/退避拒绝，自然退出/fatal 可移除，手工 app/program 删除的整批拒绝，文件删除边界及失败一致性。
- 文档：README 与 AGENTS.md 的 remove/apply 生命周期描述。
- 协调：进行中的 `refine-cli-verbs` 同样修改 app-registry 的「离线可用」需求；本增量以其更新后的 `ak config --reload` 文案为基线，只改 remove 相关句子，合入时需避免覆盖该 change。
