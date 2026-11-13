## ADDED Requirements

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
