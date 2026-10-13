## Context

`src/service.rs` 的 `render_unit` 硬编码 `Restart=always`、`RestartSec=3`；`ServiceOptions` 携带 `now: bool`（`--now` 装完即启）。CLI 在 `src/main.rs` 的 `ServiceCmd::Install`，dispatch 处手工拆元组构造 `ServiceOptions`。README 与 `openspec/specs/service-registration/spec.md` 描述 unit 内容；`xtask/src/e2e.rs` 断言迁移提示文案含 `service install --force --now`。

## Goals / Non-Goals

**Goals:**

- `Restart=` 策略可配置，合法值在 `--help` 中可见可校验（ValueEnum 单一事实源：CLI 取值拼写与 unit 内渲染拼写同源）。
- `--start` 成为「装完即启」的规范名，`--now` 以可见别名兼容。
- 缺省渲染输出与改动前逐字节一致。

**Non-Goals:**

- 不改 `RestartSec`、`KillSignal`、`TimeoutStopSec` 估算逻辑。
- 不动 `uninstall`；不给 Windows 提供服务注册。
- 不改迁移提示文案与 e2e 断言（`--now` 别名继续覆盖这些引用点）。

## Decisions

- **缺省 `always`（显式 `--restart` 优先）**：与既有硬编码一致，向后兼容；`--start` 的「自动设置重启策略」由该缺省承载——启动的服务必然携带崩溃拉起策略，无需用户额外传参。备选「缺省 `on-failure`（避免 `ak shutdown` 优雅退出后被拉起）」被否：超出本次诉求且改变既有 unit 内容；该诉求可由用户显式 `--restart on-failure` 满足。备选「无 `--start` 时缺省 `no`」被否：开机自启场景失去崩溃拉起，违背能力规范 Purpose。
- **`--now` 保留为可见别名而非删除**：README 历史段落、迁移提示（`main.rs` 守护形态拦截文案）与 e2e 断言均引用 `--force --now`；别名使所有既有引用点继续合法。备选「硬切改名 + 迁移提示」被否：收益不抵破坏面。
- **clap `ValueEnum` 承载七种 systemd 策略**：`--help` 自动列出 possible values 及逐条说明，非法值报错回显全表——直接满足「可以在帮助信息里查看」。`Display` 手写映射并在单测中与 `to_possible_value` 对照，防止两处拼写漂移。
- **策略进 `render_unit` 签名而非全局**：与既有 `user` / `timeout_stop_sec` 参数风格一致，纯函数保持平台无关可测。

## Risks / Trade-offs

- [`Restart=always` 下 `ak shutdown`（非 `systemctl stop` 路径）会被 systemd 再次拉起] → 文档明示可用 `--restart on-failure` 规避；行为与改动前一致，不属本变更引入。
- [delta 与现行正式规范中该 Requirement 内容一致（实现与规范直改已先行完成，本变更为规范化补登）] → 归档合并为幂等替换，`openspec validate --specs` 把关。

## Migration Plan

无迁移：缺省行为不变。已装机的 unit 如需调整策略，`sudo ak service install --force --restart <POLICY>` 重新生成即可。

## Open Questions

（无）
