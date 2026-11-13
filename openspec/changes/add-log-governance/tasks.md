## 1. 配置模型与解析

- [x] 1.1 新增 `LogOptions { max_size: Option<String>, keep: Option<u32> }`，挂到 `ProgramRaw`、`AppMeta`、`LevelOptions` 的 `log` 字段；`ResolvedProgram` 保持扁平字段不变；验证：`cargo build` 通过，既有配置测试不受影响
- [x] 1.2 `resolve_program` 改为按字段合并：`max_size` 与 `keep` 分别取 program > app > app-default > 内置（50MB / 2）；`AppMeta::as_defaults` 同步携带 `log`；验证：单元测试覆盖「程序只写 max_size、app 提供 keep」的逐字段合并
- [x] 1.3 旧键硬破坏：三个结构体保留 `log_max_size` / `log_rotate_keep` 拒绝哨兵，解析到即报错，错误信息给出 `log = { max_size = "…", keep = N }` 改写片段；验证：单元测试断言错误文本包含旧键名与目标片段
- [x] 1.4 解析层测试：内联表与 `[program.x.log]` section 等价、`log` 内未知键被拒绝、`log = { max_size = "0" }` 解析为禁用轮转；验证：`cargo test --workspace` 新增用例通过

## 2. 轮转与压缩

- [x] 2.1 新增日志文件模块：`RotatingWriter`（持有当前文件、字节计数、阈值/份数）与单线程压缩工作队列（FIFO、有界、gzip 用 `flate2`）；验证：单元测试覆盖 `.1.gz` 命名、顺延、最旧删除
- [x] 2.2 `pump::flush_batch` 接入：pump 线程只做「rename 当前 -> 唯一 tmp、重开当前、入队」，`keep = 0` 维持删除语义；验证：既有 pump 测试通过，新增测试断言轮转后出现 `.1.gz` 且不存在 `.1`
- [x] 2.3 失败与边界：压缩失败降级保留未压缩 `.1` 并告警；队列满时该次任务同样降级、不阻塞 pump；升级遗留的 `.1`/`.2` 在顺延时被删除；验证：单元测试覆盖压缩失败、队列满、遗留文件清理三条路径
- [x] 2.4 零反压回归：高速输出叠加一次大文件轮转，断言 pump 排空不停顿、子进程不被阻塞；验证：pump 集成测试或 bench rotation case 通过

## 3. 守护进程自身日志

- [x] 3.1 `daemonize::spawn_detached` 给子进程设置 `AK_DAEMON_LOG_FILE=<绝对路径>`（仅后台 detached 路径）；验证：单元测试断言命令环境包含该变量
- [x] 3.2 `run_daemon_core` 在变量存在时把 `env_logger` target 设为 `Target::Pipe(RotatingWriter)`（50MB / 2 / gzip）；不存在时保持 stderr；打开失败回退 stderr 并告警；验证：单元测试覆盖「有变量走 pipe、无变量走 stderr、打开失败回退」
- [x] 3.3 后台守护日志轮转验收：超过 50MB 后出现 `daemon.out.log.1.gz`、当前文件继续写入、最多 2 份归档；验证：集成测试或 xtask e2e 断言

## 4. 对账与既有测试适配

- [x] 4.1 `appkeeper-bench` 的 `integrity::rotation_chain` 与 `count_rotated` 支持 `.N.gz`（用 `flate2` 解压后校验序号连续性）；验证：`cargo run -p appkeeper-bench -- --case rotation` 报告的行完整性与 rotation_count 正确
- [x] 4.2 清理 bench/e2e 中依赖未压缩 `.N` 文件名的断言；验证：`cargo test --workspace` 与 `cargo run -p xtask -- e2e` 通过
- [x] 4.3 确认生成器路径：脚手架与 compose 快照序列化 `log` 表输出为 `[program.x.log]` section，且再次加载等价；验证：registry/config 相关测试与 `ak compose` 手工 roundtrip

## 5. 收尾与归档

- [ ] 5.1 归档顺序：确认 `add-program-user` 已先归档；用 `openspec-update-change` 把本变更的 `configuration` delta 与最新规范对齐（补 `user`/`group`/`umask` 字段与场景）；验证：`openspec validate "add-log-governance" --strict` 通过且无遗漏场景
- [x] 5.2 全量验证：`cargo test --workspace`、`cargo run -- validate`、`cargo run -p xtask -- e2e`、`cargo run -p appkeeper-bench -- --case firehose --duration 10` 均退出 0；验证：命令输出与退出码
- [x] 5.3 文档一致性：`openspec/specs/log-management` 与 `configuration` 的 delta 覆盖全部 SHALL，无未实现的行为；验证：`openspec validate --specs` 通过
