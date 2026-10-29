# Design: refactor-cli-verbs

## Context

CLI 顶层现有 20 个动词，按历史来源而非对象分组。仓库已有动词移除先例：`start`
与守护形态 `run` 均为 clap 前拦截 + 迁移提示（`reject_removed_start` /
`reject_removed_run`，退出码 2）。`daemon --detach` 的后台化实现是父进程 fork 后
以裸 `daemon` argv re-exec 子进程（`src/daemonize.rs`）；systemd unit 的
`ExecStart` 与 Windows SCM 的 binPath 均由 `service install` 生成，分别指向前台
形态与 `--service <name>` 形态。进行中的 `add-windows-service-registration`
（13/19）刚实现 `ak service`，其 delta 尚未合入主 spec——主 spec 的
service-registration 仍为 Linux-only 文本。

## Goals / Non-Goals

**Goals:**

- `daemon` 子命令族（start / install / uninstall / shutdown）统一承载守护进程生命周期
- `ps` 统一运行态总览与注册表查看；`compose` 以编排隐喻取代 `add`
- 控制面 `/v1` API 零变化，webui 零变化

**Non-Goals:**

- 不做声明式 compose 文件与多程序编排（仅文案铺垫）
- 不改 shell REPL 词表（`ak shell` 内 status/shutdown 保持原词）
- 不动 `remove` / `run --json` / `validate` / `config` / `apply` / `action` /
  `signal` / `pid` / `log` / `reload`
- 不做旧动词兼容层（无别名、无拦截提示）

## Decisions

- **D1 daemon 子命令化**（用户拍板）：`ak daemon start|install|uninstall|shutdown`；
  裸 `ak daemon` 以退出码 2 报 usage 错误并列出四个子命令。备选「旗标三形态
  （--install/--uninstall/--shutdown）」被否——用户评审后转向子命令风格。
- **D2 start 缺省后台、`--detach` 删除**（用户提议方向）：`ak daemon start` = 后台
  拉起就绪即返回（原 `--detach` 语义成为缺省）；`daemon start --foreground` 承接
  前台（systemd ExecStart / 调试 / re-exec 子进程）；re-exec argv 改为
  `ak daemon start --foreground -c <config>`。备选「start=前台保留 --detach」被否
  ——start 不返回与直觉相悖，且用户明确希望删掉 --detach。
- **D3 `--service <NAME>` 落于 start、不与 install 合并**：install 是注册动作（一次性），
  `--service` 是 OS 服务管理器每次拉起 daemon 时的运行模式标记（SCM binPath 内嵌），
  两者是「注册」与「被运行」的两半。SCM binPath 由 install 重新生成为
  `<bin> daemon start --service <name> --config <path>`，与 `--foreground` 互斥。
- **D4 `ps` 仅在线、`--all` 全量 + 离线降级、`--out` 仅 ps**（用户拍板）：缺省等价原
  `status`（离线退出码 3，脚本契约不变），表格增列注册表信息（配置本体路径/描述）；
  `--all` 含已注册未运行/未 apply 的 app，且离线时降级为本地注册表清单（退出码 0）；
  `--out json|table` 缺省 table，仅 ps 提供，`run --json` 维持不动。
- **D5 `ps --all` 在线视图走客户端 join**：CLI 本地读注册表（`registry::list`，与原
  `list` 同源）与 `/v1` 投影按 app 名合并，daemon 与 `/v1` API 零改动。备选「新增
  /v1 注册表端点」被否——为单一视图膨胀控制面 API 面，且 staged 应用仅存在于
  daemon 内存态，服务端合并反而复杂化。
- **D6 `add` → `compose` 纯改名 + 文案铺垫**（用户拍板）：三形态分派、脚手架、
  `--apply`、`all` 保留字等行为零变化；帮助文案与脚手架生成文件的注释按 compose
  编排隐喻重组，为将来声明式编排留语义落点。
- **D7 旧动词直接删除**（用户拍板，知情选择）：`status`/`list`/`shutdown`/`add`/
  `service` 无别名、无迁移拦截，旧调用得到 clap 未知子命令错误。与仓库
  `start`/daemon-`run` 的拦截先例相反；守护形态 `run` 的既有拦截保留，但提示
  文案更新为 `ak daemon start` / `ak daemon install --force --now`。
- **D8 顺序约束**：`add-windows-service-registration`（13/19，剩 e2e+验收）先完成
  归档；随后以 `openspec-update-change` 按合并后的主 spec 重写本 change 的
  service-registration delta——Windows SCM 需求块（原「Windows 平台明确不支持」
  的替代需求）届时以 `ak daemon install|uninstall` 动词与
  `daemon start --service` binPath 形态并入，本 delta 不预写未归档文本。
- **D9 细节假设**（次要，记录在案）：`daemon install` 旗标集与原 `ServiceCmd`
  一字不差平移（`--start`（可见别名 `--now`）/`--force`/`--unit-file`/`--name`/
  `--desc`/`--user`/`--restart`）；`daemon install/uninstall` 执行完即退出，
  与 `start` 互斥由 clap 子命令结构天然保证。

## Risks / Trade-offs

- [旧动词直接删除，既有脚本裸断] → 用户知情拍板；README 迁移对照表一次性列全
- [既有 systemd unit 的 ExecStart 为旧形态，升级后拉起失败] → `run --config` 走既有
  拦截（退出码 2）+ 自救提示；`daemon --config` 形态在裸 `daemon` usage 错误下同样
  失败并提示 `ak daemon install --force --now` 重新生成
- [`ps --all` 客户端 join 时本地注册表与 daemon 视图不一致（staged/pending/损坏
  链接）] → 以 `/v1` 投影为权威、本地注册表仅补充投影缺失的 app 名；损坏记录沿原
  `list` 的过滤语义（`ListedApp::good`）
- [e2e/stress 引用面大（数十处旧动词）] → 机械化替换 + 全量 e2e 门禁
- [两 change 的 service-registration delta 竞争] → D8 顺序约束兜底

## Migration Plan

1. 完成 `add-windows-service-registration` 剩余 e2e/验收任务并归档
   （`openspec validate --specs` 确认无损合入）
2. 按合并后的主 spec 重写本 change 的 service-registration delta
   （`openspec-update-change`）
3. 实施本变更（见 tasks.md），全量 e2e + `cargo test --workspace` 通过后归档
4. 部署侧动作：既有安装以 `ak daemon install --force --now` 重新生成 unit/服务注册

## Open Questions

（无——所有会影响 specs/任务拆分的决策已定；次要细节见 D9 假设记录）
