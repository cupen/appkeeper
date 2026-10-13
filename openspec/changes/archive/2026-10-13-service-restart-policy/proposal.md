## Why

`ak service install` 生成的 systemd unit 中 `Restart=` 硬编码为 `always`，用户无法按运维场景选择重启策略（如 `on-failure`，避免守护进程内 `ak shutdown` 优雅退出后被 systemd 再次拉起）；同时「装完即启」的旗标名是 `--now`，用户心智里更自然的 `--start` 不存在。本变更把重启策略开放为参数，并提供 `--start` 旗标。

## What Changes

- `service install` 新增 `--start`：安装后立即 `systemctl start`（原 `--now` 保留为等价可见别名，历史文档与迁移提示不受影响）。
- `service install` 新增 `--restart <POLICY>`：写入 unit 的 `Restart=` 策略，取值为 systemd 的 restart 策略（`no` / `on-success` / `on-failure` / `on-abnormal` / `on-watchdog` / `on-abort` / `always`），缺省 `always`。
- 全部合法策略值及含义必须在 `ak service install --help` 中列出（clap `ValueEnum`）。
- 不传任何参数时生成的 unit 与既有行为逐字节一致（`Restart=always`），完全向后兼容。

## Capabilities

### New Capabilities

（无）

### Modified Capabilities

- `service-registration`：install 的 unit 内容要求由「`Restart=always` 固定」改为「`Restart=` 缺省 `always`、可经 `--restart <POLICY>` 覆盖且取值在 help 中列全」；`--now 立即启动` 场景更名为 `--start 立即启动`（`--now` 为别名）；新增 `--restart` 场景。

## Impact

- `src/service.rs`：新增 `RestartPolicy`（clap `ValueEnum` + `Display`）；`ServiceOptions` 的 `now` 更名 `start` 并新增 `restart` 字段；`render_unit` 签名加策略参数。
- `src/main.rs`：`ServiceCmd::Install` 新增 `--start`（可见别名 `--now`）与 `--restart`（`default_value_t`）；dispatch 透传。
- 文档：README「作为系统服务运行」段；`openspec/specs/service-registration/spec.md`（随本变更 delta 归档合入）。
- 不改动：`uninstall` 语义、`RestartSec=3`、Windows 平台明确不支持的行为、迁移提示文案中的 `--force --now`（别名仍合法，且被 e2e 断言）。
