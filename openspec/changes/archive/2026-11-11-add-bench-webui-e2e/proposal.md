## Why

The xkeeper project lacks an end-to-end test that validates the complete user workflow of registering a supervised process via CLI, controlling it through the webui, and observing its logs in real-time. Current e2e tests cover CLI operations and webui interactions separately, but not the integrated flow that proves xkeeper's core value proposition: a process supervisor with a web-based control plane and real-time log streaming.

This test fills that gap by exercising xkeeper-bench as a realistic workload, started through the webui, with dual verification of logs in both the webui and on disk. It proves that xkeeper's process management, webui control, and log streaming capabilities work correctly together in a scenario that mirrors actual user workflows.

## What Changes

- Add a new e2e test pass function `bench_webui_pass()` in `xtask/src/e2e.rs` that:
  - Registers xkeeper-bench as a supervised program using `xkeeper add`
  - Configures the bench to run in `__generate` mode with controlled parameters (`--rows 1000 --rate 100`)
  - Starts the program through the webui interface (not CLI)
  - Verifies log streaming in the webui log panel
  - Verifies log files on disk match expected content
  - Cleans up completely with `xkeeper remove`
- Integrate the new test into the e2e suite execution order (after `browser_pass`, before `bench_pass`)
- No changes to xkeeper daemon, xkeeper-bench, or webui code—only test infrastructure

## Capabilities

### New Capabilities
- `bench-webui-integration`: End-to-end test that validates the integrated workflow of CLI-based program registration, webui-based process control, and real-time log streaming with dual verification (webui display and disk files)

### Modified Capabilities
(none—this is a test addition that exercises existing capabilities without changing their requirements)

## Impact

- **Test infrastructure**: `xtask/src/e2e.rs` gains a new pass function and playwright script additions for bench program control
- **Dependencies**: Uses existing xkeeper CLI commands (`add`, `remove`), existing webui infrastructure, existing xkeeper-bench `__generate` mode
- **Runtime**: Adds ~15-20 seconds to e2e suite execution time (bench runs 1000 rows at 100 rows/sec = ~10 seconds, plus setup/teardown)
- **Artifacts**: No new binaries, no new APIs, no configuration changes to production code
