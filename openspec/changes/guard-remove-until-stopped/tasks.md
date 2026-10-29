# Tasks: guard-remove-until-stopped

## 1. 共享移除守卫与守护命令

- [ ] 1.1 在 `src/program.rs` / `src/supervisor.rs` 增加统一的可移除状态判定，覆盖 running/starting/stopping/backoff 拒绝与 stopped/exited/fatal 接受；以单元测试逐一验证七种状态及 backoff 不可豁免。
- [ ] 1.2 在 `src/supervisor.rs` 增加 app 级 `Remove` 命令及结构化结果，串行完成成员解析、活态检查、注册删除、内存状态/关联 pending/用户停止记录清理与健康任务同步；验证删除失败时运行投影保持，成功后只清除目标 app 且其他 pending 不受影响。
- [ ] 1.3 保持 `src/registry.rs` 的链接/真实文件删除边界并由共享 Remove 调用，扩展 registry 与 supervisor 测试验证生成配置删除、外部配置保留、日志不动、未知 app 及配置路径结果；不得让 CLI 或 HTTP 线程另写删除逻辑。

## 2. 控制面、CLI 与 shell

- [ ] 2.1 在 `src/server.rs` 增加 `POST /v1/apps/<app>/remove`，复用 Supervisor 命令并区分 404、409 与注册删除服务端错误，返回同构结构化结果；用 server 测试验证本地准入、冲突无状态变化、成功状态/注册同步，并检查 TCP 共用路由无需传输层改动。
- [ ] 2.2 在 `src/client.rs` 与 `src/main.rs` 将 `ak remove <app>` 切换为纯在线 API 调用，删除本地直删和 remove 后 sync，输出无需 apply 及删除/保留路径；client/CLI 测试验证守护离线退出 3、不隐式拉起、冲突非零且文件不变。
- [ ] 2.3 在 `src/shell.rs` 增加 `remove <app>` 与帮助文本，复用控制面客户端；以 shell 单测及 `shell -e` 测试验证冲突提示、停止后成功、会话不退出与离线/未知 app 退出码。

## 3. apply 删除前置检查

- [ ] 3.1 在 `src/supervisor.rs` 的 apply 检出后、任何范围内状态修改前，识别手工删除的 app/program 并复用可移除判定；实现范围内冲突整批拒绝、范围外不阻塞与 `--restart` 不豁免，保留磁盘删除和 pending。
- [ ] 3.2 增加 supervisor/CLI/API 测试：running app 删除整批拒绝、starting/stopping/backoff program 删除拒绝、范围内其他新增/修改/重启均未执行、exited/fatal/stopped 可 apply、冲突列表完整且 409 映射正确。

## 4. Web API 与控制台

- [ ] 4.1 在 `src/web.rs`（必要时整理 `src/api.rs` 的共享类型）增加 `POST /api/apps/<app>/remove`，复用同一 Supervisor Remove 和结果类型，验证 401 会话门、404/409/服务端错误、成功结果及 `/v1` 同构；确认状态与 WS 推送移除目标而不影响其他 pending。
- [ ] 4.2 在 `webui/` 增加 app 移除 API 封装、类型与 App 概况操作，确认框准确说明配置删除边界且不自动 stop；用组件测试验证取消不请求、409 显示阻塞原因并保留页面、成功反馈无需 apply。
- [ ] 4.3 实现移除后的侧栏/概况状态收敛和当前 app 导航回退，测试 REST/WS 更新后无陈旧可交互项、其他 app 不受影响；完成前端 `pnpm exec tsc --noEmit && pnpm test && pnpm build` 并随后 `cargo build` 验证嵌入。

## 5. 集成验收与文档

- [ ] 5.1 扩展 xtask e2e 覆盖 `stop → remove → 无需 apply`、运行/退避拒绝、离线 3、外部/生成配置边界，以及手工 app/program 删除的 apply 整批拒绝；增加真实浏览器 Web 移除成功/冲突/取消路径并验证 API/WS/CLI 状态一致。
- [ ] 5.2 更新 README 与 AGENTS.md 的生命周期、API、shell/Web 操作及 `remove` 不再离线直写说明；若与 `refine-cli-verbs` 同期合入，保留其 start/run/`ak config --reload` 最新语义，并以文档示例确认无 remove 后 apply 的旧指引。
- [ ] 5.3 运行 `cargo fmt --check`、`cargo test --workspace`、前端三连与 `cargo build`、`cargo run -p xtask -- e2e`；审查 Unix/Windows 的注册删除、进程状态与停机路径，无法在当前平台实跑 Windows 时明确记录限制，不声称跨平台实测通过。
