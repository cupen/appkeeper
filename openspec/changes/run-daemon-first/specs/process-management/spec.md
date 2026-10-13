## MODIFIED Requirements

### Requirement: 程序生命周期状态机

每个受管程序的状态 SHALL 是以下之一，且可从控制平面观察到：`stopped`、`starting`、`running`、`stopping`、`exited`、`backoff`、`fatal`。程序另 SHALL 携带 `unhealthy` 标记（默认 false）。守护进程启动时 SHALL 按 `restart` 策略决定是否拉起程序：`no` 不拉起；`on-failure`、`always` 拉起；`unless-stopped` 在无「用户停止」持久化记录时拉起、有记录时保持 `stopped`（记录语义见「重启策略」）。程序进入 `fatal` 后守护进程 MUST NOT 再自动拉起它，直到收到显式的 start 请求或 reload 改变其定义。

`startsecs`（进程存活满该秒数才算「启动成功」）的**内置默认值** SHALL 为 `3.0` 秒（四层级联均未设置时生效）。该字段语义与「健康监控」的 `unhealthy` 标记相互独立：进程存活满 `startsecs` 即进入 `running`，健康状态由 `unhealthy` 单独承载。

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

#### Scenario: startsecs 内置默认 3 秒

- **WHEN** 四层级联均未声明 `startsecs`，某程序启动后持续运行
- **THEN** 该程序在存活满 3 秒后才被判定为启动成功；若在 3 秒内退出则计为一次失败的启动
