## 1. 传输层：TCP 形态

- [x] 1.1 `Endpoint::parse` 识别 `tcp://host:port`（host 非空、端口 1–65535，解析失败返回可读错误），其余非空值走既有 IPC 路径分支；单测覆盖两种形态、缺端口/端口越界/空 host 三类拒绝与缺省推导不变（验证：验证：cargo test --bin ak transport::）
- [x] 1.2 transport 新增 TCP 的 `Conn`（`TcpStream` 包装，读写超时语义与 UDS 一致）与 `Listener`（`TcpListener::bind`）实现；`BindError` 三分支语义对齐（占用→AlreadyRunning、其它→错误）；单测以 `tcp://127.0.0.1:<随机空闲端口>` 覆盖 bind/connect 往返（验证：验证：cargo test --bin ak transport::）
- [x] 1.3 陈旧端点判定对 TCP 形态的语义：bind 撞占用时先试连（连得上→AlreadyRunning）；TCP 无「陈旧文件残留」问题，`Err` 一律为真实占用或系统错误；单测覆盖三分支（验证：验证：cargo test --bin ak transport::）
- [x] 1.4 `Endpoint` 的 `owner_uid()` 对 TCP 形态返回 `None`（无属主概念），`display()` 输出 URL 形态；单测断言（验证：验证：cargo test --bin ak transport::）

## 2. 服务端：形态分派与 token 门

- [x] 2.1 `server.rs` 按配置端点形态构造监听器（IPC/TCP 各一），`serve`/`handle_connection` 主体零改动；单测断言两种形态下 `/v1/status` 均可用（验证：验证：cargo test --bin ak server::）
- [x] 2.2 TCP 形态恢复 Bearer 校验：除 `/v1/health` 外全部 `/v1/*` 要求 `Authorization: Bearer <token>`，缺失/不匹配 401；IPC 形态保持忽略 `Authorization` 头；单测分别覆盖两形态的放行/拒绝/豁免三路（验证：验证：cargo test --bin ak server::）
- [x] 2.3 token 比较使用非常量时间比较；单测断言错误 token 拒绝、正确 token 放行（验证：验证：cargo test --bin ak server::）
- [x] 2.4 启动日志打印端点 URL（形态自明）；陈旧端点清理对 TCP 形态不删除任何文件；单测断言（验证：验证：cargo test --bin ak transport:: server::）

## 3. 配置层：条件键与组合校验

- [x] 3.1 `auth_token` 出 `REMOVED_DAEMON_KEYS`（清单收窄为 `host`/`port`），字段回归 `DaemonSettings`；单测断言加载期接受该键（验证：验证：cargo test --bin ak config::）
- [x] 3.2 组合校验函数（load/validate/config set/get 共用）：`tcp://` + 空 token → 报「tcp:// 形态必须配置 auth_token」；路径形态 + 有 token → 报「auth_token 仅属于 tcp:// 形态」；单测逐分支（验证：验证：cargo test --bin ak config::）
- [x] 3.3 `socket` 校验接入 `tcp://` 解析（空值/无法解析/端口越界拒绝）；`config --set/--get/--delete` 键表加回 `auth_token`，`--get auth_token` 未配置打印空串；单测覆盖键表行为与回滚（`tcp://` 下 `--delete auth_token` 非零退出且文件不变）（验证：验证：cargo test --bin ak config::）
- [x] 3.4 `config --init` 模板：`auth_token` 以注释行回模板并注明仅 `tcp://` 形态生效；`host`/`port` 仍不得出现；单测断言模板内容与 `ak validate` 通过（验证：验证：cargo test --bin ak config::）

## 4. 客户端与调用方

- [x] 4.1 `client.rs`：TCP 形态自动附带 `Authorization: Bearer` 头，IPC 形态不附带；401 作为业务答案透传且提示「tcp:// 形态需要 auth_token」；退出码契约不变；单测以假 TCP 服务端覆盖带/不带凭据两路（验证：验证：cargo test --bin ak client::）
- [x] 4.2 `daemonize.rs` 就绪探测、`shell.rs` 按配置形态拨号（无行为分叉，仅确认零改动成立）；单测以 `tcp://` 端点跑通就绪探测成功与预算耗尽两路（验证：验证：cargo test --bin ak daemonize::）
- [x] 4.3 `appkeeper-bench`：`--socket` 接受双形态、`--token` 恢复（IPC 形态提供 `--token` 报用户错误）；单测覆盖三种命令行分支（验证：验证：cargo test -p appkeeper-bench）
- [x] 4.4 `xtask` e2e：新增 TCP 形态全链路用例（实现于 review 阶段：独立 T 段，T0 validate/daemon 双路拒绝缺 token、T1 离线 exit-3 + add --apply 经 TCP 隐式拉起、T2 全控制子命令、T3 裸 curl 钉 HTTP 契约（health 200 免凭据/匿名 401/错 token 401/对 token 200）、T4 错 token 配置走 CLI 得 exit 3 + 专属提示、T5 shutdown + 端点消亡；`cargo run -p xtask -- e2e --no-browser` PASSED，T 段 27 条 ok）；既有 IPC 用例保持不变

## 5. 文档

- [x] 5.1 README 配置示例新增 `tcp://` 形态与远程管理小节（含「明文 HTTP，跨公网必须自带 TLS 隧道」边界说明）；升级注意中死键清单由三键收窄为 `host`/`port`（验证：验证：cargo test --workspace（config 模板/键表断言覆盖文档常量））
- [x] 5.2 `conf/daemon.toml`、`examples/daemon.toml` 注释同步（`auth_token` 注释行与形态说明）（验证：验证：人工核对 conf/daemon.toml、examples/daemon.toml 注释）
- [x] 5.3 `AGENTS.md` 架构行同步（控制端点双形态、token 条件键）（验证：验证：人工核对 AGENTS.md 架构行）
- [x] 5.4 bench README 的 `--socket` 示例补充 `tcp://` 形态（验证：验证：人工核对 README「性能基准」段）
