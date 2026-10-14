## MODIFIED Requirements

### Requirement: 自带隔离环境拓扑

未指定 `--socket` 时，bench SHALL 自建隔离测量环境：在临时 workspace 目录中生成 daemon 配置与 bench app 配置，为该 daemon 分配一个位于临时目录内的**随机 socket 路径**并写入生成的 daemon 配置，拉起真实 `appkeeper` daemon 进程，经该 socket 注册并 apply 启动负载程序；测量结束后经 socket 停止 daemon 并删除临时 workspace（含 socket 端点）。`--keep` 指定时 SHALL 保留 workspace 供检查。daemon 二进制位置 SHALL 可用 `--daemon <path>` 指定，缺省在工作区 target 目录发现。

隔离环境的 socket 路径 SHALL 选用足够短的临时目录前缀以不超出 unix socket 路径长度上限；超限时 bench SHALL 以清晰错误信息退出，MUST NOT 静默截断路径。

#### Scenario: 缺省自跑完整周期

- **WHEN** 在仓库目录执行 `appkeeper-bench --case firehose`
- **THEN** bench 拉起自己的 daemon 与负载程序完成测量，退出后该临时 workspace 不存在，系统内无遗留 bench 进程与遗留 socket 端点

#### Scenario: --keep 保留现场

- **WHEN** 执行 `appkeeper-bench --case firehose --keep` 且测量完成
- **THEN** bench 退出时打印保留的 workspace 路径，daemon 已停止，workspace 内容仍在磁盘上

#### Scenario: 隔离环境断言 socket 权限

- **WHEN** bench 在隔离环境启动自己的 daemon
- **THEN** bench 校验该控制 socket 的权限位为 `0600`、其所在目录为 `0700`，不符则以非零码退出并报告实际权限

### Requirement: 连接既有 daemon 拓扑

指定 `--socket <path>` 时，bench SHALL 经该控制 socket 端点连接运行中的 daemon，并通过既有 `/v1` 控制面接口注册名为 `appkeeper-bench-<case>[-<序号>]` 形态的临时 app、apply 启动负载、采集指标、stop 并卸载。控制面 SHALL 不再需要 Bearer 凭据（准入由 socket 属主判定），因此 bench SHALL 移除 `--token` 选项；MUST NOT 保留旧 `--connect <addr>` 与 `--token` 的兼容路径或回退逻辑。连接失败 MUST 以非零码退出且不在目标 daemon 上留下任何注册痕迹；因权限不足而连接失败时 SHALL 明确报告端点属主与当前用户不一致。

#### Scenario: 对运行中 daemon 测量

- **WHEN** daemon 的控制端点位于 `/run/appkeeper/1000/ak.sock`，执行 `appkeeper-bench --case firehose --socket /run/appkeeper/1000/ak.sock`
- **THEN** 该 daemon 上出现 `appkeeper-bench-` 前缀的临时 app 并产生日志负载，测量结束后该 app 被卸载

#### Scenario: 目标不可达

- **WHEN** 执行 `appkeeper-bench --case firehose --socket /run/appkeeper/1000/ak.sock` 且该端点无守护进程监听
- **THEN** bench 以非零码退出，报错说明连接失败

#### Scenario: 权限不足被明确区分

- **WHEN** 目标 daemon 以 uid 0 运行而 bench 以 uid 1000 执行
- **THEN** bench 以非零码退出并报告端点属主为 uid 0、当前用户为 uid 1000，MUST NOT 表述为「连接不上」

#### Scenario: 旧的 --connect 与 --token 不再生效

- **WHEN** 执行 `appkeeper-bench --connect 127.0.0.1:7310` 或 `--token <token>`
- **THEN** 命令以非零码退出并说明这些选项已移除、应改用 `--socket <path>`
