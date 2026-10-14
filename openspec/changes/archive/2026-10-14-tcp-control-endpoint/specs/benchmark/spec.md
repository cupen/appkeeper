## MODIFIED Requirements

### Requirement: 连接既有 daemon 拓扑

指定 `--socket <endpoint>` 时，bench SHALL 经该控制端点连接运行中的 daemon，并通过既有 `/v1` 控制面接口注册名为 `appkeeper-bench-<case>[-<序号>]` 形态的临时 app、apply 启动负载、采集指标、stop 并卸载。端点值 SHALL 与 `[daemon] socket` 同构（本地 IPC 路径或 `tcp://host:port`）：IPC 形态凭端点属主准入，`tcp://` 形态凭 Bearer 凭据准入——`--token <token>` SHALL 被接受且仅对 `tcp://` 形态有意义；对 IPC 形态提供 `--token` 属用户错误，bench SHALL 以非零码退出并说明该形态无凭据。连接失败 MUST 以非零码退出且不在目标 daemon 上留下任何注册痕迹；IPC 形态因权限不足而连接失败时 SHALL 明确报告端点属主与当前用户不一致；`tcp://` 形态凭据被拒（401）时 SHALL 明确报告 token 不被接受。

#### Scenario: 对运行中 daemon 测量

- **WHEN** daemon 的控制端点位于 `/run/appkeeper/1000/ak.sock`，执行 `appkeeper-bench --case firehose --socket /run/appkeeper/1000/ak.sock`
- **THEN** 该 daemon 上出现 `appkeeper-bench-` 前缀的临时 app 并产生日志负载，测量结束后该 app 被卸载

#### Scenario: 对 TCP 形态 daemon 测量

- **WHEN** daemon 以 `socket = "tcp://127.0.0.1:7310"` 与 `auth_token` 运行，执行 `appkeeper-bench --case firehose --socket tcp://127.0.0.1:7310 --token <token>`
- **THEN** 该 daemon 上出现 `appkeeper-bench-` 前缀的临时 app 并产生日志负载，测量结束后该 app 被卸载

#### Scenario: 目标不可达

- **WHEN** 执行 `appkeeper-bench --case firehose --socket /run/appkeeper/1000/ak.sock` 且该端点无守护进程监听
- **THEN** bench 以非零码退出，报错说明连接失败

#### Scenario: 权限不足被明确区分

- **WHEN** 目标 daemon 以 uid 0 运行而 bench 以 uid 1000 执行
- **THEN** bench 以非零码退出并报告端点属主为 uid 0、当前用户为 uid 1000，MUST NOT 表述为「连接不上」

#### Scenario: 旧的 --connect 与 --token 不再生效

> 本变更恢复 `--token`（仅 `tcp://` 形态有意义，标题为既有场景锚点，现行行为以下文为准）。

- **WHEN** 执行 `appkeeper-bench --connect 127.0.0.1:7310`，或对 IPC 路径形态的端点提供 `--token`
- **THEN** 命令以非零码退出并说明：`--connect` 已移除应改用 `--socket <endpoint>`；IPC 路径形态无凭据，`--token` 仅对 `tcp://` 形态有意义

#### Scenario: TCP 形态凭据错误被明确区分

- **WHEN** 目标 daemon 以 `tcp://` 形态运行而 bench 提供了错误的 `--token`
- **THEN** bench 以非零码退出并报告凭据被拒绝（401），MUST NOT 表述为「连接不上」

#### Scenario: IPC 形态下提供 token 属用户错误

- **WHEN** 执行 `appkeeper-bench --case firehose --socket /run/appkeeper/1000/ak.sock --token x`
- **THEN** 非零码退出并说明 IPC 路径形态无凭据，MUST NOT 发起连接
