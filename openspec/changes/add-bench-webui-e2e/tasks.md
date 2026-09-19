## 1. Setup and Infrastructure

- [ ] 1.1 Create `bench_webui_pass()` function signature in `xtask/src/e2e.rs` with parameters matching existing pass functions (bin: &Path, ws: &Workspace), and verify the function compiles without errors
- [ ] 1.2 Add call to `bench_webui_pass()` in the main e2e test sequence in `xtask/src/e2e.rs`, placing it after `browser_pass()` and before `bench_pass()`, and verify the e2e suite still runs (test can be a stub that returns Ok(()) initially)

## 2. Build and Register Bench Program

- [ ] 2.1 Build xkeeper-bench binary using the existing `build_bench()` helper function, and verify the binary path is valid and the binary executes
- [ ] 2.2 Execute `xkeeper add <bench-binary-path>` command to register the bench program in the test workspace, and verify the command exits successfully and creates a TOML configuration file in the app registry
- [ ] 2.3 Read the generated TOML configuration file and verify it contains the expected program name and binary path

## 3. Configure Bench for Controlled Execution

- [ ] 3.1 Edit the generated TOML configuration file to set `autostart = false`, and verify the configuration file is updated correctly
- [ ] 3.2 Edit the configuration file to add command arguments `__generate --rows 1000 --rate 100`, and verify the arguments are correctly formatted in the TOML file
- [ ] 3.3 Validate the modified configuration using `xkeeper validate` command, and verify the configuration is accepted as valid

## 4. Start Daemon and Verify Initial State

- [ ] 4.1 Start the xkeeper daemon with the test workspace configuration, and verify the daemon starts successfully and the webui is accessible
- [ ] 4.2 Use playwright to navigate to the webui URL in a real browser instance, and verify the webui loads and displays the program list
- [ ] 4.3 Verify the bench program appears in the webui program list in stopped state, and verify the program name matches the expected name from the registration step

## 5. Start Bench Program via Webui

- [ ] 5.1 Use playwright to locate and click the start button for the bench program in the webui, and verify the click action completes without error
- [ ] 5.2 Poll the webui for the bench program state with a 5-second timeout, and verify the program transitions from stopped to running state
- [ ] 5.3 Verify the xkeeper daemon has spawned the bench process by checking the process list or daemon status, and verify the process is running with the expected arguments

## 6. Verify Log Streaming in Webui

- [ ] 6.1 Use playwright to click on the bench program in the webui to open the log panel, and verify the log panel opens and displays
- [ ] 6.2 Observe the webui log panel for 2-3 seconds after the bench process starts, and verify log lines appear in real-time
- [ ] 6.3 Verify the log lines displayed in the webui match the expected format from xkeeper-bench `__generate` mode (check for sequence numbers and timestamps)

## 7. Verify Log Files on Disk

- [ ] 7.1 Wait for the bench process to complete (approximately 10 seconds for 1000 rows at 100 rows/sec), and verify the process exits with status code 0
- [ ] 7.2 Locate the log files written by xkeeper in the test workspace data directory, and verify the log files exist
- [ ] 7.3 Read the log files from disk and count the total number of log rows, and verify the count is exactly 1000
- [ ] 7.4 Verify each log row in the disk files matches the expected format from xkeeper-bench `__generate` mode

## 8. Verify Log Consistency

- [ ] 8.1 Compare the log content observed in the webui (from step 6) with the log content read from disk (from step 7), and verify both sources show consistent log data
- [ ] 8.2 Verify the webui displayed log lines during streaming (not just after completion), confirming real-time streaming worked

## 9. Cleanup and Verification

- [ ] 9.1 Execute `xkeeper remove <bench-program-name>` to remove the bench program from the registry, and verify the command exits successfully
- [ ] 9.2 Verify the bench program no longer appears in the xkeeper registry by checking the app directory, and verify the TOML configuration file has been deleted
- [ ] 9.3 Verify no bench processes remain running by checking the process list, and verify complete cleanup
- [ ] 9.4 Stop the xkeeper daemon and verify it shuts down cleanly

## 10. Integration and Validation

- [ ] 10.1 Run the complete e2e suite with `cargo run -p xtask -- e2e`, and verify all tests pass including the new `bench_webui_pass()`
- [ ] 10.2 Run the e2e suite a second time to verify test repeatability, and verify no flaky failures or leftover state issues
- [ ] 10.3 Verify the test completes within the expected time frame (15-20 seconds for the bench_webui_pass portion), and verify it doesn't significantly impact overall e2e suite runtime
