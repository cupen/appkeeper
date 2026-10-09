## MODIFIED Requirements

### Requirement: 程序生命周期状态机

每个受管程序的状态 SHALL 是以下之一，且可从控制平面观察到：`stopped`、`starting`、`running`、`stopping`、`exited`、`backoff`、`fatal`。程序另 SHALL 携带 `unhealthy` 标记（默认 false）。守护进程启动时 SHALL 按 `restart` 策略决定是否拉起程序：`no` 不拉起；`on-failure`、`always` 拉起；`unless-stopped` 在无「用户停止」持久化记录时拉起、有记录时保持 `stopped`（记录语义见「重启策略」）。程序进入 `fatal` 后守护进程 MUST NOT 再自动拉起它，直到收到显式的 start 请求或 reload 改变其定义。

#### Scenario: 守护进程启动后自动拉起

- **WHEN** 守护进程以包含 `restart = "always"` 程序的配置启动
- **THEN** 该程序被启动并进入 `running`，其 pid 与运行时长可从状态查询中获得

#### Scenario: unless-stopped 有停止记录时不拉起

- **WHEN** 程序 `web` 声明 `restart = "unless-stopped"`，曾被用户显式 stop 后守护进程重启
- **THEN** `web` 保持 `stopped`，不被自动拉起；执行显式启动后清除记录并进入 `running`

#### Scenario: 稳定运行前退出计入启动失败

- **WHEN** 进程在 `startsecs` 秒内退出
- **THEN** 该次记录为一次失败的启动（消耗 `startretries` 预算），而不是一次普通退出，程序在退避后重新启动

#### Scenario: 稳定运行后退出按重启策略处理

- **WHEN** 进程连续运行超过 `startsecs` 后退出
- **THEN** 启动重试计数清零，退出被记录（退出码、存活时长、累计退出次数），并按重启策略决定后续状态

### Requirement: 重启策略

每个程序 SHALL 支持 `no`、`on-failure`、`always`、`unless-stopped` 四种 `restart` 策略（docker 字面量语义，字段域与兼容规则见 configuration 能力），并支持 `exit_codes` 期望退出码列表：`on-failure` 在退出码不在期望列表（默认仅 0）时重启；`always` 与 `unless-stopped` 无论退出码如何都重启；`no` 不重启。稳态崩溃重启 SHALL 使用指数退避（`restart_backoff` 起步、`max_restart_backoff` 封顶），进程连续运行超过 `backoff_reset_after` 秒后重置退避。

守护启动与 apply 引入新程序时的拉起语义：`no` 不拉起；其余三种策略拉起，其中 `unless-stopped` 存在「用户停止」记录时保持停止。

「用户停止」记录（支撑 `unless-stopped` 的字面语义）SHALL 由守护进程持久化维护（如状态文件，位置与格式为实现细节）：用户对程序的显式 stop SHALL 写入该程序的记录；对该程序的显式启动 SHALL 清除记录；守护进程正常关机（shutdown 停止全部程序）SHALL NOT 写入记录（不属于用户停止）；`always` 策略的守护启动拉起 SHALL NOT 受记录影响。

#### Scenario: always 对干净退出也重启

- **WHEN** 策略为 `always` 的程序以退出码 0 退出
- **THEN** 程序按退避计划重启

#### Scenario: on-failure 对期望退出码不重启

- **WHEN** 策略为 `on-failure`、`exit_codes` 为 `[0]` 的程序以退出码 0 退出
- **THEN** 程序进入 `exited`，不再重启

#### Scenario: on-failure 对非期望退出码重启

- **WHEN** 策略为 `on-failure` 的程序以退出码 3 退出
- **THEN** 程序按退避计划重启

#### Scenario: 启动重试耗尽进入 fatal

- **WHEN** 程序连续失败的启动次数超过 `startretries`（例如命令不存在，每次都在 startsecs 内退出）
- **THEN** 程序进入 `fatal` 并在状态中给出原因，守护进程不再自动拉起它

#### Scenario: unless-stopped 崩溃后自动重启

- **WHEN** 策略为 `unless-stopped` 的程序稳定运行后异常退出
- **THEN** 程序按退避计划重启（会话内行为与 always 一致）

#### Scenario: unless-stopped 手动停止跨守护重启保持

- **WHEN** 策略为 `unless-stopped` 的程序被用户 stop 后守护进程重启、随后再次重启
- **THEN** 每次守护启动该程序均保持 `stopped`（记录持久），直至用户显式启动清除记录

#### Scenario: always 手动停止后守护重启仍拉起

- **WHEN** 策略为 `always` 的程序被用户 stop 后守护进程重启
- **THEN** 该程序在守护启动时被重新拉起（docker always 语义：手动停止仅在守护进程不重启期间生效）

#### Scenario: 守护正常关机不写停止记录

- **WHEN** `unless-stopped` 程序随 `xkeeper shutdown` 被停止后守护进程重新启动
- **THEN** 无「用户停止」记录，该程序被自动拉起
