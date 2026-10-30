# Tasks: add-bench-webui-e2e

> 基线变更说明（实施时同步）：本 change 的 original proposal 写于品牌清理与动词面重构之前。
> 实施按现状翻译：`xkeeper` → `appkeeper`（二进制 `ak`）；`add` → `compose`；`remove` 已改为
> 在线原子动作（须先 `stop`）且无需 `apply`。30 条细粒度任务已合并为单个 e2e pass
> `bench_webui_pass`（xtask/src/e2e.rs），下列各条标注其落入的实现/断言行。

## 1. Setup and Infrastructure

- [x] 1.1 Create `bench_webui_pass()` function signature in `xtask/src/e2e.rs` with parameters matching existing pass functions (bin: &Path, ws: &Workspace), and verify the function compiles without errors
- [x] 1.2 Add call to `bench_webui_pass()` in the main e2e test sequence in `xtask/src/e2e.rs`, placing it after `browser_pass()` and before `bench_pass()`, and verify the e2e suite still runs (test can be a stub that returns Ok(()) initially)

## 2. Build and Register Bench Program

- [x] 2.1 Build xkeeper-bench binary using the existing `build_bench()` helper function, and verify the binary path is valid and the binary executes
- [x] 2.2 Execute `xkeeper add <bench-binary-path>` command to register the bench program in the test workspace, and verify the command exits successfully and creates a TOML configuration file in the app registry
  - 基线变更：`add` 已不存在，改用 `ak compose <binary> --name benchweb`；断言 `compose scaffolds the bench app config`
- [x] 2.3 Read the generated TOML configuration file and verify it contains the expected program name and binary path

## 3. Configure Bench for Controlled Execution

- [x] 3.1 Edit the generated TOML configuration file to set `autostart = false`, and verify the configuration file is updated correctly
  - 基线变更：等价写法 `--restart no`（`autostart=false` 的现行映射），由 compose 直接写入，无需手改文件
- [x] 3.2 Edit the configuration file to add command arguments `__generate --rows 1000 --rate 100`, and verify the arguments are correctly formatted in the TOML file
  - 实际参数为 `__generate --row-size 128 --rows 1000 --rate 100`：`--row-size` 是 `__generate` 的必填项（缺省会直接启动失败，实施中实测）
- [x] 3.3 Validate the modified configuration using `xkeeper validate` command, and verify the configuration is accepted as valid

## 4. Start Daemon and Verify Initial State

- [x] 4.1 Start the xkeeper daemon with the test workspace configuration, and verify the daemon starts successfully and the webui is accessible
  - 复用主 e2e 序列中已运行的 daemon 与 `[webui]` 段
- [x] 4.2 Use playwright to navigate to the webui URL in a real browser instance, and verify the webui loads and displays the program list
- [x] 4.3 Verify the bench program appears in the webui program list in stopped state, and verify the program name matches the expected name from the registration step
  - 断言 `the bench program is registered but not started`

## 5. Start Bench Program via Webui

- [x] 5.1 Use playwright to locate and click the start button for the bench program in the webui, and verify the click action completes without error
  - 断言 `program view shows the start button`
- [x] 5.2 Poll the webui for the bench program state with a 5-second timeout, and verify the program transitions from stopped to running state
  - 断言 `the console started the bench program`（PID 卡片由空变为实际 pid）
- [x] 5.3 Verify the xkeeper daemon has spawned the bench process by checking the process list or daemon status, and verify the process is running with the expected arguments
  - 由磁盘日志行落盘间接确证（1000 行完整产出）

## 6. Verify Log Streaming in Webui

- [x] 6.1 Use playwright to click on the bench program in the webui to open the log panel, and verify the log panel opens and displays
  - 基线变更：程序详情页内嵌 `ak-log-viewer`，经 History-API 路由导航（非 hash）
- [x] 6.2 Observe the webui log panel for 2-3 seconds after the bench process starts, and verify log lines appear in real-time
  - 断言 `log lines stream into the console panel`
- [x] 6.3 Verify the log lines displayed in the webui match the expected format from xkeeper-bench `__generate` mode (check for sequence numbers and timestamps)
  - 断言 `console log rows match the generator line contract`（面板文本含 `[out]`）

## 7. Verify Log Files on Disk

- [x] 7.1 Wait for the bench process to complete (approximately 10 seconds for 1000 rows at 100 rows/sec), and verify the process exits with status code 0
- [x] 7.2 Locate the log files written by xkeeper in the test workspace data directory, and verify the log files exist
- [x] 7.3 Read the log files from disk and count the total number of log rows, and verify the count is exactly 1000
  - 断言 `disk log holds exactly 1000 rows`
- [x] 7.4 Verify each log row in the disk files matches the expected format from xkeeper-bench `__generate` mode
  - 断言 `disk rows carry strictly increasing seq numbers` 与 `the last disk row is the final sequence number`

## 8. Verify Log Consistency

- [x] 8.1 Compare the log content observed in the webui (from step 6) with the log content read from disk (from step 7), and verify both sources show consistent log data
- [x] 8.2 Verify the webui displayed log lines during streaming (not just after completion), confirming real-time streaming worked
  - 面板断言发生在进程仍在运行期间，随后才等待磁盘产出 1000 行

## 9. Cleanup and Verification

- [x] 9.1 Execute `xkeeper remove <bench-program-name>` to remove the bench program from the registry, and verify the command exits successfully
  - 基线变更：先 `ak stop` 再 `ak remove`（guard-remove-until-stopped 的在线原子语义），无需 apply
- [x] 9.2 Verify the bench program no longer appears in the xkeeper registry by checking the app directory, and verify the TOML configuration file has been deleted
  - 断言 `the generated config is gone after remove`
- [x] 9.3 Verify no bench processes remain running by checking the process list, and verify complete cleanup
  - 断言 `no bench process survives the cleanup`
- [x] 9.4 Stop the xkeeper daemon and verify it shuts down cleanly
  - 由主 e2e 序列的 `shutdown_daemon` 统一收尾

## 10. Integration and Validation

- [x] 10.1 Run the complete e2e suite with `cargo run -p xtask -- e2e`, and verify all tests pass including the new `bench_webui_pass()`
- [x] 10.2 Run the e2e suite a second time to verify test repeatability, and verify no flaky failures or leftover state issues
- [x] 10.3 Verify the test completes within the expected time frame (15-20 seconds for the bench_webui_pass portion), and verify it doesn't significantly impact overall e2e suite runtime

## 11. 实施中发现并修复的既有缺陷

- [x] 11.1 `webui/src/views/program-detail.ts` 渲染 `<ak-log-viewer>` 却从未 import 该模块，自定义元素未注册，日志面板在真实浏览器中始终为空白（既有缺陷，webui 单测因直接 import 组件而掩盖）。已补 `import '../components/log-viewer.js'`，并在真实浏览器中验证面板正常显示生成器日志行。
