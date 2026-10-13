## 1. 核心实现（service.rs）

- [x] 1.1 新增 `RestartPolicy` 枚举（clap `ValueEnum`，七种 systemd 策略）与 `Display` 手写映射；单测 `restart_policy_displays_systemd_names` 断言 `Display` 与 `to_possible_value` 拼写同源
- [x] 1.2 `ServiceOptions` 的 `now: bool` 更名 `start: bool`，新增 `restart: RestartPolicy`；`unix_install` 相应改用 `opts.start`
- [x] 1.3 `render_unit` 增加 `restart: RestartPolicy` 参数替换硬编码 `Restart=always`；`unix_install` 传入 `opts.restart`；既有渲染单测补参并新增 `render_honours_restart_policy` 用例（`OnFailure` 渲染、不再含 `always`）

## 2. CLI（main.rs）

- [x] 2.1 `ServiceCmd::Install` 新增 `--start`（`visible_alias = "now"`）与 `--restart <POLICY>`（`default_value_t = RestartPolicy::Always`）；dispatch 元组与 `ServiceOptions` 构造同步；`cargo run -- service install --help` 确认别名、possible values 全表与 `[default: always]` 均出现
- [x] 2.2 行为验证：`--restart bogus` 报错并列出七个合法值；非 root 下 `--now --restart on-failure` 完整解析并止步于 root 检查（无文件副作用）

## 3. 文档与规范同源

- [x] 3.1 README「作为系统服务运行」段：`--start` 示例（注明 `--now` 等价别名）、`--restart` 可选参数说明与七种取值
- [x] 3.2 specs 增量 `specs/service-registration/spec.md`：MODIFIED「service install 生成并注册 systemd unit」含 `--start`/`--restart` 场景（本文件）

## 4. 回归与收口

- [x] 4.1 `cargo test` 全量通过（216+ 用例，含既有 `renders_unit_without_user` 缺省渲染逐字节不变）；迁移提示文案与 e2e 断言（`service install --force --now`）未改动
- [x] 4.2 `openspec validate --change` 通过；归档后 `openspec validate --specs` 通过
