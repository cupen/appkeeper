# process-management Specification

## Purpose

定义 appkeeper 对受管子进程生命周期的行为契约：状态机、启动与重启策略、启动/停止排序、优雅停止与进程树清理、健康监控。所有行为在 Linux 与 Windows 上对齐到同一规格，平台差异仅体现在信号与进程树机制上。

## Requirements

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

### Requirement: 启动与停止排序

程序 SHALL 支持 `priority`（数值越小越先启动）与 `depends_on`（依赖的程序名列表）。守护进程启动与 reload 引入新程序时，SHALL 按 priority 排序启动；被依赖程序未进入 `running` 前，依赖它的程序 MUST NOT 被启动；守护进程关闭时 SHALL 按与启动相反的顺序停止程序。

#### Scenario: 依赖未就绪时等待

- **WHEN** 程序 B `depends_on` 程序 A，且 A 尚未进入 `running`
- **THEN** B 保持 `stopped` 并显示等待原因，直到 A 运行后才被启动

#### Scenario: 依赖失败时放弃启动

- **WHEN** 程序 A 进入 `fatal` 或 `exited`（且不再重启），而 B 依赖 A
- **THEN** B 进入 `fatal`，原因标明依赖不可用

#### Scenario: 关闭时逆序停止

- **WHEN** 守护进程收到关闭信号，A 的优先级先于 B 启动
- **THEN** B 先被停止，A 后被停止

### Requirement: 优雅停止

对运行中的程序发出停止请求时，appkeeper SHALL 先请求优雅终止（Unix 上发送 SIGTERM），等待至多 `stop_timeout` 秒；超时后 SHALL 强制终止（Unix 上 SIGKILL，Windows 上 TerminateProcess）。停止完成后程序 SHALL 进入 `stopped`。

#### Scenario: 进程在超时内自行退出

- **WHEN** 对运行中的程序发出 stop，进程在 `stop_timeout` 内响应 SIGTERM 退出
- **THEN** 程序进入 `stopped`，未使用强制终止

#### Scenario: 进程无视优雅信号被强杀

- **WHEN** 进程在 `stop_timeout` 内未退出
- **THEN** appkeeper 强制终止该进程，程序仍进入 `stopped`，且记录使用了强制终止

### Requirement: 进程树清理

守护进程退出时 MUST NOT 遗留存活的子进程。在 Windows 上，该保证 SHALL 由作业对象（kill-on-close）兜底：即使守护进程被强制杀死，子进程树也被内核回收。在 Unix 上，appkeeper SHALL 将子进程放入独立进程组，停止程序时终止整个进程组。

#### Scenario: 守护进程被强制杀死后无孤儿（Windows）

- **WHEN** 在 Windows 上守护进程被外部强杀
- **THEN** 其子进程树在守护进程死亡后被内核一并终止

#### Scenario: 孙进程不残留

- **WHEN** 被守护的程序又派生了子进程，随后该程序被 stop
- **THEN** 该程序派生的孙进程不继续存活（按平台机制尽力保证）

### Requirement: 健康监控

每个程序 SHALL 支持可选健康检查，类型为 `none`（默认）、`tcp`（连通目标地址端口）、`http`（请求 URL 期待 2xx/3xx）、`exec`（执行命令，退出码 0 为健康），并可配置 `interval`、`timeout`、`retries`、`start_period`。连续失败达到 `retries` 次后程序 SHALL 被标记为 `unhealthy`；配置了 `restart_on_unhealthy = true` 时，unhealthy SHALL 触发与崩溃一致的重启流程。`start_period` 内的失败不计入连续失败次数。

#### Scenario: 健康检查通过

- **WHEN** 配置 http 健康检查的程序其健康端点在 interval 周期内返回 2xx
- **THEN** 程序保持 `running` 且 unhealthy 标记为 false

#### Scenario: 连续失败触发 unhealthy

- **WHEN** 健康检查连续失败达到 retries 次
- **THEN** 程序被标记为 unhealthy；若启用了 restart_on_unhealthy，则按重启策略重启并清除 unhealthy

#### Scenario: 预热期失败不计入

- **WHEN** 程序刚启动仍在 `start_period` 内且健康检查失败
- **THEN** 这些失败不计入连续失败计数

### Requirement: 子进程运行身份

守护进程 SHALL 按程序的 `user`、`group` 与 `umask` 决定子进程的运行身份与文件创建掩码，并在拉起前完成可行性判断。有效身份定义为：运行 uid = `user` 的 uid（未声明时取守护进程 euid）；有效主组 = `group` 的 gid（未声明时取运行 uid 的 passwd 主组，用户也未声明时取守护进程 egid）；附加组 = 运行 uid 的 passwd 附加组（`group` 显式值不在其中时并入）。

- 未声明 `user` 时 SHALL 以守护进程的有效 uid 运行（`getpwuid(geteuid())` 解析，MUST NOT 依赖 `$USER` 环境变量）。
- 运行 uid 与守护进程 euid 相同且有效主组与 egid 相同时，MUST NOT 执行任何身份切换调用，直接拉起。
- 守护进程 euid 为 0 时：目标 uid 非 0 SHALL 在 exec 前按 `setgroups` -> `setgid` -> `setuid` 的顺序降权；目标 uid 为 0 时仅按需执行 `setgid`。附加组列表 SHALL 在配置解析期预取，MUST NOT 在 fork 后的子进程上下文里查询系统账号数据库。
- 守护进程 euid 非 0 且与运行 uid 不同时，SHALL 在执行前拒绝（compose 预检返回校验类错误、守护启动加载时剔除该 app 并告警），MUST NOT 进入 spawn 失败的重启退避循环。
- 守护进程 euid 非 0、运行 uid 与 euid 相同但有效主组不同时，SHALL 仅当守护进程已属于该组（`getgroups` 判定）才允许切换主组，否则在执行前拒绝；此时 MUST NOT 调用 `setgroups`（非 root 无此权限）。
- 声明 `umask` 时，SHALL 在 exec 前调用 `umask(2)` 设置子进程的文件创建掩码；该设置 MUST NOT 影响守护进程自身写入的日志文件，MUST NOT 影响未声明 `umask` 的程序。
- 目标身份为 uid 0（缺省继承或显式声明 root）时，SHALL 产生非致命警告，内容指出程序以 root 运行并建议显式配置 `user`。
- 降权后，`HOME`/`USER`/`LOGNAME`/`SHELL` 环境变量 SHALL 取目标账号的 passwd 记录值，除非 `[program.<name>.env]` 已显式设置同名键（显式值优先）。
- 身份切换 MUST NOT 改变既有进程组语义：子进程仍以自身为进程组组长拉起，停止时整组收到信号。

#### Scenario: root 守护进程降权拉起

- **WHEN** 守护进程以 root 运行，程序声明 `user = "deploy"`（uid 1001）
- **THEN** 子进程以 uid 1001、gid 与 deploy 的附加组运行，`HOME` 等内容为 deploy 的 passwd 值

#### Scenario: 非 root 守护进程拒绝跨用户拉起

- **WHEN** 守护进程以 uid 1000 运行，程序声明 `user = "deploy"`
- **THEN** 守护进程在拉起前拒绝该程序并报告原因，进程不会以错误身份启动，也不会进入重启退避

#### Scenario: 继承身份不做身份切换

- **WHEN** 程序未声明 `user`、`group` 与 `umask`，且守护进程以 uid 1000 运行
- **THEN** 子进程以 uid 1000 拉起，不执行任何 setuid/setgid/umask 调用

#### Scenario: root 下显式覆盖主组

- **WHEN** 守护进程以 root 运行，程序声明 `user = "deploy"` 与 `group = "www-data"`
- **THEN** 子进程以 deploy 的 uid、www-data 的 gid 运行，附加组同时包含 deploy 的组与 www-data

#### Scenario: 非 root 同用户换到已属组

- **WHEN** 守护进程以 uid 1000 运行（属于组 `dev`），程序声明 `group = "dev"` 而未声明 `user`
- **THEN** 子进程主组切换为 dev，不执行 setuid 与 setgroups

#### Scenario: 非 root 同用户换到非属组被拒绝

- **WHEN** 守护进程以 uid 1000 运行且不属于 `www-data`，程序声明 `group = "www-data"`
- **THEN** 守护进程在执行前拒绝该程序，进程不会以错误主组启动

#### Scenario: umask 作用于子进程

- **WHEN** 守护进程 umask 为 `0o022`，程序声明 `umask = "0077"` 并创建一个文件
- **THEN** 子进程创建的文件权限按 `0o077` 掩码生效，而 daemon 写入的日志文件权限不变

#### Scenario: root 身份给出警告

- **WHEN** 程序最终运行身份为 uid 0（未声明 user 且守护进程为 root，或显式声明 root）
- **THEN** daemon 日志与 `ak compose up` 输出中出现非致命警告，程序仍正常拉起

#### Scenario: 显式 env 覆盖账号环境

- **WHEN** 程序声明 `user = "deploy"` 且 `[program.x.env]` 设置 `HOME = "/srv/app"`
- **THEN** 子进程的 `HOME` 为 `/srv/app`，其余未显式声明的身份变量取 deploy 的 passwd 值
