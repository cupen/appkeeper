## 1. Implementation

- [x] 1.1 在 `src/webui_open.rs` 建立 plan/execute 边界：纯函数 `plan(status, mode, gui) -> Plan`（`Unavailable { reason }` 不带 URL 字段 / `Print { line }` / `Open { url, gui }`）；`execute(gui, probe, launch) -> (code, notes)` 只产出 stderr 提示与退出码，看不到 URL；以 `cargo test -p appkeeper webui_open` 编译通过为准
- [x] 1.2 `main.rs::webui_cmd` 改为消费 `Plan`：`Unavailable` 打 error 并退 1（无 stdout）；`Print` 打印其 line；`Open` 先 `println!("{url}")` 并 flush，再调 `execute` 打印提示、按 code 退出——打印 MUST 早于探测与启动
- [x] 1.3 降级分支统一为单行 `ak: warning:`：无图形会话、探测失败、启动失败各一行（启动失败把「手动打开 stdout 上的 URL」并入同一行）；删除 `opening ` 追加行与旧的两行 `ak: error:` + `hint:` 组合；成功路径与输出模式 stderr 为空
- [x] 1.4 改写单测（`plan` 侧）：`Unavailable` 无 URL、`Print` 的 `--print-url` / `--json` 行形状（`opened == false`）、`Open` 的 URL 与 `gui`；probe/launch 用 panic 闭包断言「决策不产生副作用」
- [x] 1.5 改写单测（`execute` 侧）：headless（不探测，panic 闭包）、探测失败（code 0、一行 warning、URL 未被抑制）、启动失败（launcher 缺失与返回非 0 都 code 0、一行 warning）、`auto` 成功路径（stdout 恰为裸 URL，无 `opening ` 前缀）；以 `cargo test -p appkeeper webui_open` 全绿为准
- [x] 1.6 确认未变路径：`unavailable_console_fails_with_the_repair_command`（退 1、无 stdout）、`json_mode_reports_the_documented_fields`（`opened == false`、不探测不启动）、`print_url_carries_the_token_only_in_auto_mode` 照旧通过
- [x] 1.7 更新描述旧契约的注释：`src/main.rs::webui_cmd`（「failed launch are exit 1」「token never by the regular success line」两处已假）与 `src/shell.rs::open_webui`（「Same contract as `ak webui`」改为「共享 URL 构造」）；以 `grep -rn "opening \|WebuiReport\|report.code" src xtask` 与 `cargo check --workspace` 通过为准

## 2. Verification

- [x] 2.1 `cargo test --workspace` 全绿（含 `webui_open` 单测与既有 shell/server 测试）
- [x] 2.2 `xtask/src/e2e.rs` 新增可自定义 env 且分离 stdout/stderr 的 helper（如 `cli_try_env`），并补两步断言：已伺服控制台下清空 `DISPLAY`/`WAYLAND_DISPLAY` 跑 `ak webui`（退 0、stdout 恰一行 = URL、stderr 一行 warning）；`PATH=/nonexistent DISPLAY=:0` 跑 `ak webui`（退 0、stdout 恰一行 URL、stderr 一行 warning）。W1b/W4 补「stdout 为空」断言
- [x] 2.3 `cargo run -p xtask -- e2e --no-browser` 全绿，重点确认 W1b（控制台未启用仍退 1 且不打印 URL）、W6b（`--print-url`/`--json` 的 `opened == false`、空 stderr）、G0c2（daemon 离线退 3）未变化
- [x] 2.4 手工冒烟：控制台已伺服时 `PATH=/nonexistent DISPLAY=:0 ak webui` 应 stdout 裸 URL、stderr 一行 warning、退出码 0；无 `[webui]` 段时退 1 且 stdout 为空；观察 URL 在探测/启动之前出现（可把 `[webui]` 指向转发黑洞复现探测失败）
- [x] 2.5 `openspec validate "webui-open-best-effort" --strict` 通过（归档留待实施验收后）
