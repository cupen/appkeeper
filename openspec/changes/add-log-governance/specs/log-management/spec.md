## MODIFIED Requirements

### Requirement: 按大小轮转

每个日志文件 SHALL 默认启用按大小轮转：阈值缺省 `50MB`、保留份数缺省 `2`（每流盘上最多 3 个文件：当前 + 2 份轮转归档），out 与 err 两个流 SHALL 各自独立轮转、互不影响。程序 SHALL 通过 `log` 表配置：`log = { max_size = "…", keep = N }`（等价 section 写法 `[program.<name>.log]`），`log.max_size = "0"` 显式禁用轮转。超过阈值时当前文件 SHALL 被移出写入路径并立即新开当前文件继续追加；轮转归档 SHALL 以 gzip 压缩写成 `.1.gz`、`.2.gz`…，更早的归档依次顺延，超出保留份数的最旧归档被删除。压缩 MUST 在 pump 线程之外执行，MUST NOT 使管道排空产生无界阻塞窗口；压缩失败 SHALL 降级保留未压缩归档（`.1`）并告警，MUST NOT 丢失已写入内容。`keep = 0` 表示轮转时直接删除当前文件、不保留归档。轮转 SHALL 发生在写入路径上，不丢失已写入的行；升级前遗留的未压缩归档（`.1`、`.2`…）SHALL 在后续轮转顺延时被删除，不长期残留。

#### Scenario: 缺省配置下默认轮转

- **WHEN** 程序未声明 `log` 表，某流当前日志文件大小超过 50MB 后程序继续输出
- **THEN** 当前文件被移出并压缩为 `.1.gz`，新输出从新的当前文件继续，无需任何配置

#### Scenario: 超过阈值触发轮转

- **WHEN** 程序声明 `log = { max_size = "10MB" }`，该流日志超过阈值后继续输出
- **THEN** 旧内容被压缩为 `.1.gz`，当前文件从新内容继续

#### Scenario: 保留份数受限

- **WHEN** 轮转归档数量超过 `log.keep`
- **THEN** 最旧的归档被删除，盘上最多保留 `keep` 份归档

#### Scenario: 两流独立轮转

- **WHEN** 程序的 err 流已轮转多次而 out 流从未超过阈值
- **THEN** 仅 err 流发生轮转，out 流保持单一文件继续追加

#### Scenario: 显式禁用轮转

- **WHEN** 程序声明 `log = { max_size = "0" }` 且日志持续增长
- **THEN** 任何流都不发生轮转与压缩，日志持续追加到同一文件

#### Scenario: 轮转归档自动压缩

- **WHEN** 任一流触发一次轮转
- **THEN** 盘上出现 `.1.gz` 且不存在同名的 `.1`，归档内容可用 gzip 解压还原

#### Scenario: 压缩失败降级

- **WHEN** 压缩归档时磁盘满或临时目录不可写
- **THEN** 归档以未压缩的 `.1` 保留并记录告警，已写入内容不丢失，pump 继续排空

#### Scenario: 压缩不阻塞排空

- **WHEN** 程序以高速率持续输出且恰逢一次大文件轮转
- **THEN** 子进程写入不因压缩而阻塞，管道排空持续进行

#### Scenario: keep 为零时轮转即删除

- **WHEN** 程序声明 `log = { keep = 0 }` 且某流触发轮转
- **THEN** 当前文件被删除并新开空文件，不产生任何归档

### Requirement: 守护进程自身日志

appkeeper 自身的生命周期事件（启动/关闭、程序启停、退出原因、健康状态变化、reload 结果）SHALL 以带时间戳的行输出到 stderr，级别可通过 `RUST_LOG` 或配置的 `log_level` 调整。后台 detached 模式（`ak daemon start`）下，日志 SHALL 落盘到 `{log_dir}/daemon.out.log` 并按与程序日志相同的默认策略轮转：阈值 `50MB`、保留 `2` 份、归档 gzip（`.1.gz`、`.2.gz`），MUST NOT 引入独立配置键；压缩失败 SHALL 降级保留未压缩归档并告警。前台模式与由 systemd / Windows 服务托管的模式 SHALL 继续写 stderr，不启用文件轮转。

#### Scenario: 程序启停事件可见

- **WHEN** 守护进程启动一个程序或该程序退出
- **THEN** 守护进程日志中出现带时间戳、程序名、pid 与原因的记录

#### Scenario: 后台守护日志按大小轮转

- **WHEN** 后台模式运行且 `daemon.out.log` 超过 50MB
- **THEN** 该文件被轮转为 `daemon.out.log.1.gz`，守护进程继续写入新的 `daemon.out.log`，盘上最多保留 2 份归档

#### Scenario: 前台模式不写日志文件

- **WHEN** 以 `ak daemon start --foreground` 运行
- **THEN** 生命周期日志输出到终端 stderr，不创建或轮转 `daemon.out.log`
