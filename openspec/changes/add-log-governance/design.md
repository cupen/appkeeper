## Context

- 子进程日志由 `src/pump.rs` 接管：`flush_batch` 在写入路径上按大小轮转，落盘失败降级为
  内存环，永不反压子进程；`ak log` / WS / webui 都读内存环，**不读轮转文件**。
- 守护进程自身日志没有走 pump：`src/daemonize.rs` 用 append 打开
  `{log_dir}/daemon.out.log`，把 fd 重定向给子进程的 stdout/stderr，之后没有任何轮转。
- 日志配置当前是两个扁平键 `log_max_size` / `log_rotate_keep`，在 `[program.*]`、
  `[app]`、`[app-default]` 三层逐字段合并。
- 仓库的 `toml` crate 是 `1.1.6+spec-1.1.0`（支持多行内联表），`flate2` 已在依赖里
  （HTTP gzip 用），gzip 不引入新 crate。

## Goals / Non-Goals

**Goals:**

- 两条日志路径（程序日志、守护自身日志）都有可预期的磁盘上界。
- 配置面不增加旋钮，只把两个既有键收进 `log` 子表；旧键给出可执行的迁移提示。
- 压缩与轮转不破坏「输出排空零反压」这条硬约束。

**Non-Goals:**

- 不提供压缩开关、时间保留、总量上限、截断模式、时间戳前缀、ANSI 剥离。
- 不改 `ak log` / webui 的读取路径（继续读内存环）。
- 不做日志外送（syslog/OTLP）或归档解压查询。

## Decisions

### D1 `log` 子表替代两个扁平键

- 结构：`LogOptions { max_size: Option<String>, keep: Option<u32> }`，在 `ProgramRaw`、
  `AppMeta`、`LevelOptions` 三处复用；`ResolvedProgram` 保持扁平字段
  （`log_max_size: Option<u64>`、`log_rotate_keep: u32`），`program.rs` 的 spawn 路径不动。
- 合并：`resolve_program` 对 `max_size` 与 `keep` **分别**取
  program > app > app-default > 内置。备选「整表替换」被否决：程序只写 `max_size` 会吞掉
  应用层的 `keep`。
- 旧键：在三个结构体里保留 `log_max_size` / `log_rotate_keep` 两个**拒绝哨兵**字段
  （`Option<toml::Value>`），解析后若存在即 `bail!`，错误信息给出
  `log = { max_size = "…", keep = N }` 的目标片段。备选「交给 `deny_unknown_fields`
  的通用 unknown field 报错」被否决：提示不够可执行。
- 写法：内联表与 `[program.x.log]` section 天然等价（TOML 语义），解析只认表；生成器
  （`toml::to_string_pretty`）会输出 section 形式，属于 TOML 1.0 兼容写法。

### D2 归档压缩：`flate2` gzip + 单工作线程

```
pump 线程（不得阻塞）
  rename current -> path.<seq>.tmp
  open new current, 继续写
  enqueue(path.<seq>.tmp) --------+
                                   |
压缩工作线程（单线程，全局一个）     |
  pop 任务（FIFO）                 |
  删除 path.<keep>.gz 等最旧       |
  shift .N.gz -> .N+1.gz          |
  gzip tmp -> .1.gz.tmp -> rename .1.gz
  失败: rename tmp -> .1（未压缩）+ warn
```

- 轮转的**重命名与重开**留在 pump 线程（两次文件系统调用，有界）；**压缩与归档顺延**
  全部在 worker 完成，保证 FIFO 顺序，避免两次轮转的压缩任务互相覆盖。
- 选择单 worker 而不是 per-rotation 线程：顺序天然正确，且不需要对 `.N.gz` 加锁。
- 队列有界（如 1024）；满时该次任务直接降级为未压缩 `.1`，MUST NOT 阻塞 pump。
- `keep = 0`：pump 直接删除当前文件并重开，不入队、不产生归档（保持既有语义）。
- 旧版本的未压缩归档 `.1`、`.2`… 在 worker 顺延时一并删除，不长期残留。
- 备选「在 pump 线程内联 gzip」被否决：50MB 压缩会让管道排空停顿数百毫秒，违反零反压。

### D3 守护进程日志：env_logger 的 Pipe writer，而不是 dup2

- `spawn_detached` 给子进程设置 `AK_DAEMON_LOG_FILE=<绝对路径>`；只有后台 detached 模式
  带这个变量。
- `run_daemon_core` 读该变量：存在时把 `env_logger` 的 target 设为
  `Target::Pipe(Box::new(RotatingWriter::open(path, 50MB, 2)))`，复用 D2 的轮转/压缩
  实现；不存在时保持 stderr（前台、systemd、Windows 服务模式）。
- 父进程仍把子进程 stdout/stderr 重定向到同一文件：初始化前的错误与 panic 有落点。
  两条 fd 都是 append，日志行不会互相覆盖；轮转后旧 fd 指向已归档 inode，最多让极少数
  早期错误落在归档里，可接受。
- 打开 rotator 失败时回退 stderr 并 `eprintln!` 一条告警，不让日志故障阻止 daemon 启动。
- 备选「周期性检查大小 + dup2 重开 fd」被否决：平台分支多（Windows 无 dup2），且与
  env_logger 的写入无同步。

### D4 默认值不变

`50MB` / `keep = 2` 保持不变。压缩后每流磁盘占用从约 150MB 降到约 60MB，已经达到
「开箱即用」的目标；改默认值会让现网程序多一次轮转，收益不抵噪声。

### D5 明确拒绝的旋钮

| 候选 | 否决理由 |
|---|---|
| `log_retention`（按时间） | `max_size × keep` 已有上界；按年龄删会在低频程序上误删 |
| `log_max_total_size` | 与 `max_size × keep` 等价，纯增字段 |
| `log_compress = false` | 压缩对读取路径透明（zcat/zgrep），开关只制造选择负担 |
| `log_mode = "truncate"` | append 更安全；要干净日志由子进程自己管 |
| `log_timestamps` / `log_ansi` | 改变内容语义，属于加工而非治理 |
| per-stream / per-app 子表 | 现有两项已够，扩大结构面 |

## Risks / Trade-offs

- [归档顺序冲突] 本变更与 `add-program-user` 都修改 `configuration` 的「app 配置」。
  → 先实现并归档 `add-program-user`，再在归档前把本变更的 configuration delta 与最新
  规范对齐（补 `user`/`group`/`umask` 字段与场景）。任务清单里显式包含这一步。
- [临时文件堆积] 写入速率长期高于压缩吞吐时，`.tmp` 会排队。→ 有界队列 + 满时降级为
  未压缩归档，磁盘仍受 `max_size × (keep+1)` 约束（未压缩时短暂超出，下轮顺延回收）。
- [压缩失败] 磁盘满/权限问题导致 gzip 失败。→ 保留未压缩 `.1`，告警一次，不丢数据。
- [双 fd 写同一文件] logger 与其被重定向的 stderr 各自持有 fd。→ 都是 O_APPEND，行级
  原子；轮转后旧 fd 可能把 panic 写进归档，属可接受的边缘情况。
- [TOML 1.1 可移植性] 多行内联表在 TOML 1.0 解析器（如 Python `tomllib`）里不合法。
  → 生成器只输出 section 形式；文档注明手写多行内联表需要 1.1 解析器。
- [Windows 重命名] 轮转依赖 rename 打开中的文件。std 默认 share mode 含
  `FILE_SHARE_DELETE`，现有 pump 已按此工作；压缩只处理已重命名、无打开句柄的 `.tmp`。

## Migration Plan

1. 先落地 `add-program-user`（含其归档）。
2. 本变更合入后，旧配置在加载期报错并给出改写片段；没有静默行为变化。
3. 回滚：恢复两个扁平键的解析即可；已生成的 `.N.gz` 归档不影响回滚（旧代码不读它们，
   但也不会删除——需手工清理或等下一次升级）。
