# process-management Specification Delta

## MODIFIED Requirements

### Requirement: 重启策略

每个程序 SHALL 支持 `no`、`on-failure`、`always`、`unless-stopped` 四种 `restart` 策略（docker 字面量语义，字段域与兼容规则见 configuration 能力），并支持 `exit_codes` 期望退出码列表：`on-failure` 在退出码不在期望列表（默认仅 0）时重启；`always` 与 `unless-stopped` 无论退出码如何都重启；`no` 不重启。稳态崩溃重启 SHALL 使用指数退避（`restart_backoff` 起步、`max_restart_backoff` 封顶），进程连续运行超过 `backoff_reset_after` 秒后重置退避。

守护启动与 apply 引入新程序时的拉起语义：`no` 不拉起；其余三种策略拉起，其中 `unless-stopped` 存在「用户停止」记录时保持停止。

「用户停止」记录（支撑 `unless-stopped` 的字面语义）SHALL 由守护进程持久化维护，且 MUST 落在持久状态目录——即 `configuration` 能力中 `daemon.state_dir` 的生效值（State 类目录，跨重启保留、停服不删、卸载不删）；MUST NOT 落在日志目录或运行时目录。记录的文件名与格式为实现细节。守护进程 SHALL 在启动时把旧位置（`log_dir`）下的记录一次性迁移到生效状态目录，迁移后 `log_dir` 的改变或清理 MUST NOT 影响记录。用户对程序的显式 stop SHALL 写入该程序的记录；对该程序的显式启动 SHALL 清除记录；守护进程正常关机（shutdown 停止全部程序）SHALL NOT 写入记录（不属于用户停止）；`always` 策略的守护启动拉起 SHALL NOT 受记录影响。

#### Scenario: always 对干净退出也重启

- **WHEN** 策略为 `always` 的程序以退出码 0 退出
- **THEN** 程序按退避计划重启

#### Scenario: on-failure 对期望退出码不重启

- **WHEN** 策略为 `on-failure`、`exit_codes` 为 `[0]` 的程序以退出码 0 退出
- **THEN** 程序进入 `exited`，不再重启

#### Scenario: on-failure 对非期望退出码重启

- **WHEN** 策略为 `on-failure`、`exit_codes` 为 `[0]` 的程序以退出码 3 退出
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

- **WHEN** `unless-stopped` 程序随 `ak daemon shutdown` 被停止后守护进程重新启动
- **THEN** 无「用户停止」记录，该程序被自动拉起

#### Scenario: 记录不随日志目录变化丢失

- **WHEN** 用户 stop 一个 `unless-stopped` 程序后修改 daemon 配置的 `log_dir`（或清理日志目录），随后守护进程重启
- **THEN** 该程序仍保持 `stopped`，记录的落位与内容不受日志目录影响

#### Scenario: 旧位置记录一次性迁移

- **WHEN** 升级前 `log_dir/state.json` 含某程序的用户停止记录，升级后守护进程首次启动
- **THEN** 记录被迁移到生效状态目录并继续生效（该程序不被拉起），旧文件被清理

#### Scenario: 卸载服务保留停止记录

- **WHEN** 用户执行 `ak daemon uninstall` 卸载服务（状态目录中有停止记录），随后重新安装并启动
- **THEN** 状态目录未被卸载流程删除，`unless-stopped` 的记录仍然生效
