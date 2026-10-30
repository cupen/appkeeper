# Tasks: guard-remove-until-stopped

## 1. 共享移除守卫与守护命令

- [x] 1.1 在 `src/program.rs` / `src/supervisor.rs` 增加统一的可移除状态判定，覆盖 running/starting/stopping/backoff 拒绝与 stopped/exited/fatal 接受；以单元测试逐一验证七种状态及 backoff 不可豁免。
- [x] 1.2 在 `src/supervisor.rs` 增加 app 级 `Remove` 命令及结构化结果，串行完成成员解析、活态检查、注册删除、内存状态/关联 pending/用户停止记录清理与健康任务同步；验证删除失败时运行投影保持，成功后只清除目标 app 且其他 pending 不受影响。
- [x] 1.3 保持 `src/registry.rs` 的链接/真实文件删除边界并由共享 Remove 调用，扩展 registry 与 supervisor 测试验证生成配置删除、外部配置保留、日志不动、未知 app 及配置路径结果；不得让 CLI 或 HTTP 线程另写删除逻辑。

## 2. 控制面、CLI 与 shell

- [x] 2.1 在 `src/server.rs` 增加 `POST /v1/apps/<app>/remove`，复用 Supervisor 命令并区分 404、409 与注册删除服务端错误，返回同构结构化结果；用 server 测试验证本地准入、冲突无状态变化、成功状态/注册同步，并检查 TCP 共用路由无需传输层改动。
- [x] 2.2 在 `src/client.rs` 与 `src/main.rs` 将 `ak remove <app>` 切换为纯在线 API 调用，删除本地直删和 remove 后 sync，输出无需 apply 及删除/保留路径；client/CLI 测试验证守护离线退出 3、不隐式拉起、冲突非零且文件不变。
- [x] 2.3 在 `src/shell.rs` 增加 `remove <app>` 与帮助文本，复用控制面客户端；以 shell 单测及 `shell -e` 测试验证冲突提示、停止后成功、会话不退出与离线/未知 app 退出码。

## 3. apply 删除前置检查

- [x] 3.1 在 `src/supervisor.rs` 的 apply 检出后、任何范围内状态修改前，识别手工删除的 app/program 并复用可移除判定；实现范围内冲突整批拒绝、范围外不阻塞与 `--restart` 不豁免，保留磁盘删除和 pending。
- [x] 3.2 增加 supervisor/CLI/API 测试：running app 删除整批拒绝、starting/stopping/backoff program 删除拒绝、范围内其他新增/修改/重启均未执行、exited/fatal/stopped 可 apply、冲突列表完整且 409 映射正确。

## 4. Web API 与控制台

- [x] 4.1 在 `src/web.rs`（必要时整理 `src/api.rs` 的共享类型）增加 `POST /api/apps/<app>/remove`，复用同一 Supervisor Remove 和结果类型，验证 401 会话门、404/409/服务端错误、成功结果及 `/v1` 同构；确认状态与 WS 推送移除目标而不影响其他 pending。
- [x] 4.2 在 `webui/` 增加 app 移除 API 封装、类型与 App 概况操作，确认框准确说明配置删除边界且不自动 stop；用组件测试验证取消不请求、409 显示阻塞原因并保留页面、成功反馈无需 apply。
- [x] 4.3 实现移除后的侧栏/概况状态收敛和当前 app 导航回退，测试 REST/WS 更新后无陈旧可交互项、其他 app 不受影响；完成前端 `pnpm exec tsc --noEmit && pnpm test && pnpm build` 并随后 `cargo build` 验证嵌入。

## 5. 集成验收与文档

- [x] 5.1 扩展 xtask e2e 覆盖 `stop → remove → 无需 apply`、运行/退避拒绝、离线 3、外部/生成配置边界，以及手工 app/program 删除的 apply 整批拒绝；增加真实浏览器 Web 移除成功/冲突/取消路径并验证 API/WS/CLI 状态一致。
  - 已实现并运行（`--no-browser`）：`remove_guard_pass` 的 RG1（`/v1` + `/api` 活态 409 且零状态变化）、RG2（手工删除运行中 program → apply 整批拒绝，含 `--restart`、范围外/program 范围不受阻、停止后 apply 移除）、RG3（手工删除运行中 app → 整批拒绝、`--restart` 不豁免、停止后 apply 完成移除且 pending 清空）、RG4（停止后 `/api` 移除=一步、外部配置保留、无 pending、`/api/overview` 收敛）、RG5（`backoff` 拒绝移除、停止后 `/v1` 移除成功）；离线 `remove` 退出 3 且不改注册（G0d3）；`scaffold_pass` G7 覆盖 live 拒绝 + stop→remove 生成文件删除。
  - 浏览器路径本轮仅登记场景、不执行（e2e 固定 `--no-browser`），待有 playwright 环境时补充：F1 取消确认框 → 0 次 `POST /api/apps/<app>/remove`、页面停留；F2 活态冲突 → 409 消息内联展示、页面保留、无侧栏项消失；F3 停止后成功 → 提示「无需 apply」、跳转到下一个 app（或 `/`）、侧栏与该 app 的程序行消失且其他 app 不变；F4 与 CLI/`/v1` 状态一致（同一 `remove` 后 `/v1/status`、`/api/overview`、WS `apps_gone` 三处同步）。
- [x] 5.2 更新 README 与 AGENTS.md 的生命周期、API、shell/Web 操作及 `remove` 不再离线直写说明；若与 `refine-cli-verbs` 同期合入，保留其 start/run/`ak config --reload` 最新语义，并以文档示例确认无 remove 后 apply 的旧指引。
- [x] 5.3 运行 `cargo fmt --check`、`cargo test --workspace`、前端三连与 `cargo build`、`cargo run -p xtask -- e2e`；审查 Unix/Windows 的注册删除、进程状态与停机路径，无法在当前平台实跑 Windows 时明确记录限制，不声称跨平台实测通过。
  - 本机实跑（Linux）：`cargo fmt --check`（通过）、`cargo clippy --workspace --all-targets`（无新增告警，与基线逐条一致）、`cargo test --workspace`（386 + 61 等全通过）、前端 `tsc --noEmit`/`pnpm test`（79）/`pnpm build` + `cargo build`、`cargo run -p xtask -- e2e --no-browser`（ACCEPTANCE E2E PASSED）。
  - Windows 限制：未在 Windows 实跑。代码路径差异仅在 `registry::remove`（硬链接回退时 `deleted_file=true`，外部文件内容仍保留）与 `link_registration`（symlink 失败回退 hardlink）；e2e 中 RG4/RG5 的 `deleted_file=false` 断言在 Windows 硬链接回退下不成立，属平台已知差异，未宣称跨平台实测通过。
