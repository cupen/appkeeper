## Context

See proposal.md for motivation. The xkeeper e2e suite currently tests CLI operations and webui interactions in isolation. This design adds an integrated test that exercises the complete workflow: CLI registration → webui control → log verification.

The test leverages existing infrastructure:
- `xkeeper add` / `xkeeper remove` for program registration
- xkeeper-bench's hidden `__generate` mode as a realistic workload
- Playwright for browser automation (already used in `browser_pass`)
- Existing webui log streaming and display capabilities

## Goals / Non-Goals

**Goals:**
- Prove that xkeeper's process supervision, webui control, and log streaming work correctly together
- Validate the user workflow of registering a program via CLI and controlling it through the webui
- Verify log consistency between webui display and disk files
- Ensure complete cleanup with no leftover state

**Non-Goals:**
- Performance testing or benchmarking (the bench runs at controlled, slow rate)
- Testing edge cases like process crashes, restart loops, or network failures
- Modifying xkeeper daemon, xkeeper-bench, or webui code
- Testing CLI-based process control (that's already covered by other tests)

## Decisions

**Decision 1: Use xkeeper-bench `__generate` mode as the test workload**

Rationale: The `__generate` mode produces predictable, formatted log output at a controlled rate. Using `--rows 1000 --rate 100` generates logs over ~10 seconds, providing enough time to observe streaming without making the test too long. This is a real workload that exercises the full supervision path.

Alternatives considered:
- Simple echo/sleep script: Too trivial, doesn't exercise log formatting or sustained output
- Full bench case (firehose/rotation): Too fast, hard to observe streaming; too much output for disk verification
- External workload (e.g., Python script): Adds dependencies, less reproducible

**Decision 2: Set autostart=false and start via webui click**

Rationale: This proves the webui can control process lifecycle, which is a core value proposition. Starting via CLI would only test CLI functionality (already covered). The webui start action must traverse the same control plane path as CLI, so it validates the integration.

Alternatives considered:
- Set autostart=true: Doesn't test webui control capability
- Start via CLI after webui verification: Redundant, doesn't prove webui can initiate

**Decision 3: Dual verification (webui + disk)**

Rationale: Verifying logs in both places ensures consistency between the webui's view (ring buffer + WebSocket streaming) and the actual disk files (pump rotation + flush). This catches bugs where one path works but the other doesn't.

Alternatives considered:
- Webui only: Misses disk-level bugs (flush, rotation, file I/O)
- Disk only: Misses webui-specific bugs (WebSocket, ring buffer, rendering)
- API only (log_tail endpoint): Doesn't verify actual disk files or webui display

**Decision 4: Place test after browser_pass, before bench_pass**

Rationale: The test uses webui infrastructure (like browser_pass) but is conceptually about bench supervision (like bench_pass). Placing it between them groups related tests logically. It also ensures the webui is already built and working before this test runs.

Alternatives considered:
- Before browser_pass: Webui might not be fully validated yet
- After bench_pass: Breaks logical grouping (bench-related tests separated)

**Decision 5: Use playwright for all webui interactions**

Rationale: Playwright drives real browser instances, ensuring the test validates actual user experience. Mocked browser interactions would miss rendering bugs, WebSocket issues, and UI state problems.

Alternatives considered:
- Direct HTTP API calls: Bypasses webui entirely, doesn't test the UI
- Puppeteer instead of playwright: Project already uses playwright for browser_pass, consistency matters

**Decision 6: Clean up with `xkeeper remove` after test**

Rationale: Using the CLI remove command validates that cleanup works correctly and leaves no leftover state. This is important for test repeatability and for running the e2e suite multiple times.

Alternatives considered:
- Manual file deletion: Bypasses xkeeper's cleanup logic, doesn't test remove command
- Leave state for next test: Breaks test isolation, causes flaky failures

## Risks / Trade-offs

**Risk: Playwright test may be flaky due to timing**
→ Mitigation: Use explicit waits for state transitions (e.g., wait for program to appear in webui, wait for log lines to appear). Avoid hardcoded sleeps where possible. The 10-second bench duration provides a comfortable window.

**Risk: Log streaming observation may miss early lines**
→ Mitigation: Start observing the webui log panel immediately after clicking start. The bench produces logs at 100 rows/sec, so even if we miss the first few, we'll capture plenty for verification. Disk verification catches any that were missed.

**Risk: Webui may not update state immediately after start click**
→ Mitigation: Poll the webui for state changes with a timeout (e.g., 5 seconds). The daemon reports state changes quickly, so this should be reliable.

**Risk: Test adds 15-20 seconds to e2e suite**
→ Mitigation: Acceptable trade-off for the coverage gained. The bench duration (10 seconds) is the minimum needed to observe streaming reliably. Could be reduced to `--rows 500 --rate 100` (5 seconds) if needed, but 1000 rows provides better verification.

**Trade-off: Test complexity vs. coverage**
The test is more complex than existing passes because it exercises multiple integration points. This complexity is justified by the coverage gained: it's the only test that validates the complete user workflow.

**Trade-off: Test depends on webui being functional**
If the webui is broken, this test will fail even if xkeeper's core supervision works. This is intentional—the test validates the integrated experience, not just the daemon in isolation.
