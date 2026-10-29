# Proposal: refactor-cli-verbs

## Why

CLI 动词面经多轮演进后顶层已达 20 个动词，且生命周期操作按「历史来源」而非「对象」分组：
守护进程本体（daemon）、系统服务注册（service）、进程总览（status）、注册表清单（list）、
停机（shutdown）、应用注册（add）平铺在同一层。收拢为按对象分组的小动词面——daemon
子命令族统一承载守护进程生命周期，`ps` 统一总览，`compose` 以编排隐喻取代临时的 `add`——
降低记忆负担，并为将来的声明式编排预留语义落点。

## What Changes

- **BREAKING** `ak service install|uninstall` 并入 `ak daemon install|uninstall`：全部旗标
  （--now/--force/--unit-file/--name/--desc/--user/--restart）原样平移，SCM binPath 与
  systemd unit 的 ExecStart 由 install 重新生成为 `daemon start` 新形态
- **BREAKING** `ak daemon` 裸调用与 `--detach` 改为 `ak daemon start`：缺省后台拉起、就绪
  即返回（原 `--detach` 语义成为缺省，旗标删除）；`ak daemon start --foreground` 承接前台
  运行（systemd ExecStart、调试、re-exec 子进程）；隐藏 `--service <NAME>` 落到 start，
  仍为安装器生成的 binPath 专用；裸 `ak daemon` 报 usage 错误并列出四个子命令
- **BREAKING** `ak shutdown` 并入 `ak daemon shutdown`（客户端操作，走控制面，语义不变）
- **BREAKING** `ak status` 与 `ak list` 合并为 `ak ps`：缺省仅在线（离线退出码 3，脚本契约
  不变），注册表路径/描述作为新列并入在线表格；`ak ps --all` 展示全量视图（含已注册但
  未运行/未 apply 的 app），且离线时降级为本地注册表清单（退出码 0）；新增
  `--out <json|table>`（缺省 table），仅 ps 提供，`run --json` 维持不动
- **BREAKING** `ak add` 改名 `ak compose`：三形态分派、脚手架、`--apply`、`all` 保留字等
  行为零变化；帮助文案与生成的脚手架注释按 compose 隐喻重组，为将来声明式编排铺垫
- 旧动词（service、list、status、shutdown、add）直接删除：无别名、无迁移拦截，
  旧调用得到 clap 的 unrecognized subcommand 报错
- 控制面 `/v1` API 零变化：`ps --all` 的在线全量视图由 CLI 本地读注册表与 `/v1` 投影
  在客户端 join，daemon 侧不动
- 全量引用同步：xtask e2e/stress、README、AGENTS.md、其余 spec 中的散文级旧动词引用

## Capabilities

### New Capabilities

（无——全部为既有能力的动词面重组）

### Modified Capabilities

- `control-plane`: 「CLI 控制命令」需求变化——`status`/`shutdown` 动词移除，由 `ps`
  （含 `--all`/`--out`）与 `daemon shutdown` 承接；`status --json` 的机器可读路径由
  `ps --out json` 承接
- `app-registry`: 注册/清单动词需求变化——`add` 改名 `compose`（行为等价、文案按编排
  隐喻重组），`list` 并入 `ps`/`ps --all`；`remove` 不变
- `service-registration`: 系统服务动词需求变化——`ak service install|uninstall` 改为
  `ak daemon install|uninstall`，生成的 SCM binPath 与 systemd ExecStart 指向
  `daemon start [--service <NAME>|--foreground]` 新形态
- `background-daemon`: daemon 启动形态需求变化——裸 `ak daemon`/`--detach` 由
  `ak daemon start`（缺省后台）与 `--foreground` 取代；后台 re-exec 的 argv 随之更新

## Impact

- 代码：`src/main.rs`（Cmd 枚举重构为 Daemon 子命令树、Ps、Compose）、`src/service.rs`
  （binPath/ExecStart 模板与旗标文案）、`src/daemonize.rs`（后台 re-exec argv 改为
  `daemon start --foreground`）、`src/client.rs`（无 API 变化，仅命令封装调整）
- 测试：`xtask/src/e2e.rs` 与 `stress.rs` 中全部 `add`/`list`/`status`/`shutdown`/
  `daemon --detach` 调用点更新；`src/main.rs`/`service.rs`/`daemonize.rs` 内嵌单测同步
- 文档：README、AGENTS.md、9 个未列 delta 的 spec 文件中的散文级旧动词引用清扫
- 不受影响：webui（走 `/api`）、shell REPL 词表（`ak shell` 内 status/shutdown 保持）、
  `remove`/`run`/`validate`/`config`/`apply`/`action`/`signal`/`pid`/`log`/`reload`
- 顺序约束：`add-windows-service-registration`（进行中，13/19）须先完成剩余 e2e 并归档，
  本变更的 service-registration delta 才能落到干净基线上
