## Why

`log-management` 现在只对子进程日志做按大小轮转，轮转文件不压缩；守护进程自身的
`daemon.out.log` 则是 append 直写、**完全没有轮转**，是唯一真正无界增长的日志。配置面
上两个扁平键 `log_max_size` / `log_rotate_keep` 也不成组。本变更把日志治理补齐，同时把
两个键收进 `log` 子表，且不新增任何旋钮。

## What Changes

- **配置收口（BREAKING）**：`log_max_size` / `log_rotate_keep` 替换为
  `log = { max_size = "…", keep = N }`（等价 section 写法 `[program.<name>.log]`）。
  三层（`[program.*]` / `[app]` / daemon `[app-default]`）都支持，表内**逐字段合并**。
  旧扁平键 SHALL 被校验拒绝，错误信息直接给出改写后的片段。
- **轮转文件自动压缩**：轮转归档写成 `{name}.out.log.1.gz`、`.2.gz`；压缩在 pump 线程
  之外执行，MUST NOT 阻塞管道排空（对齐「输出排空零反压」）。压缩失败降级为未压缩
  归档并告警；`keep = 0` 维持「轮转即删除」语义。
- **守护进程自身日志纳入轮转**：后台 detached 模式下 `daemon.out.log` 按与程序日志
  相同的默认策略（50MB × 保留 2 份 + gzip）轮转，无新增配置；前台与 systemd/journald
  模式继续写 stderr，行为不变。
- **默认值不变**：单文件阈值仍为 50MB、保留份数仍为 2；压缩后每流磁盘占用约从
  150MB 降到 60MB 量级。
- **不新增任何配置键**：不提供压缩开关、时间保留、截断模式、时间戳前缀等旋钮。

## Capabilities

### New Capabilities

（无）

### Modified Capabilities

- `log-management`: 轮转命名改为 `.N.gz`、压缩不阻塞排空；守护进程自身日志新增按大小
  轮转与压缩（后台模式）。
- `configuration`: 日志配置由两个扁平键收为 `log` 子表，旧键硬破坏并给迁移提示；
  「字段优先级」明确 `log` 表内逐字段合并。

## Impact

- **配置/解析**：`src/config.rs`（`ProgramRaw`/`AppMeta`/`LevelOptions` 增加 `log` 表，
  `resolve_program` 逐字段合并，旧键拒绝并给改写提示）。
- **日志实现**：`src/pump.rs`（轮转后 gzip、后台压缩队列、旧 `.N` 兼容清理）、
  `src/daemonize.rs`（子进程环境变量标记后台日志路径）、`src/main.rs`（后台模式下
  `env_logger` 接 rotator）、新增日志轮转/压缩的共享小模块。
- **测试**：pump 单测（压缩、keep=0、压缩失败降级、旧 `.N` 清理、排空不阻塞）、
  config 单测（log 表解析、逐字段合并、旧键报错文案）、daemon 日志轮转测试、
  xtask e2e（日志轮转与 gzip 对账）。
- **不影响**：`ak log` / WS 日志流 / webui 日志视图（都读内存环，不读轮转文件）；
  Windows 的日志路径行为（同一套 std 文件操作）。
- **迁移**：旧配置需把 `log_max_size = "X"` / `log_rotate_keep = N` 改写为
  `log = { max_size = "X", keep = N }`；校验错误直接给出目标片段。
