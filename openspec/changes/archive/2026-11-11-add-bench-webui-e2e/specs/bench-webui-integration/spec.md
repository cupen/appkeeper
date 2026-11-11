## Purpose

Validates the integrated workflow of CLI-based program registration, webui-based process control, and real-time log streaming, proving that appkeeper's core supervision capabilities work correctly together in a realistic user scenario.

## ADDED Requirements

### Requirement: Test registers bench program via CLI
The e2e test SHALL register appkeeper-bench as a supervised program using the `appkeeper compose` command with the bench binary path, producing a valid app configuration in the registry.

#### Scenario: Successful CLI registration
- **WHEN** the test executes `appkeeper compose <bench-binary-path>`
- **THEN** the bench program appears in the appkeeper registry with a generated TOML configuration file

### Requirement: Test configures bench for controlled log generation
The e2e test SHALL edit the generated app configuration to set `autostart = false` and add command arguments `__generate --rows 1000 --rate 100`, ensuring the bench runs as a supervised process with predictable log output.

#### Scenario: Configuration enables manual start and controlled output
- **WHEN** the test edits the app configuration
- **THEN** the configuration specifies `autostart = false` and command arguments that produce exactly 1000 log rows at 100 rows per second

### Requirement: Test starts bench program through webui
The e2e test SHALL start the bench program by clicking the start button in the webui interface, proving that the webui can control supervised processes.

#### Scenario: Webui initiates process start
- **WHEN** the test clicks the start button for the bench program in the webui
- **THEN** the bench program transitions from stopped to running state in the webui
- **AND** the appkeeper daemon spawns the bench process with the configured arguments

### Requirement: Test verifies log streaming in webui
The e2e test SHALL observe real-time log output in the webui log panel, proving that log streaming works correctly for the supervised bench process.

#### Scenario: Logs appear in webui in real-time
- **WHEN** the bench process is running and producing log output
- **THEN** the webui log panel displays log lines as they are generated
- **AND** the log lines match the expected format from appkeeper-bench `__generate` mode

### Requirement: Test verifies log files on disk
The e2e test SHALL read the log files written by appkeeper to disk and verify their content matches the expected output from the bench process.

#### Scenario: Disk logs match expected content
- **WHEN** the bench process completes and appkeeper flushes log buffers
- **THEN** log files exist at the expected location in the appkeeper data directory
- **AND** the log files contain exactly 1000 rows
- **AND** each row matches the expected format from appkeeper-bench `__generate` mode

### Requirement: Test waits for bench completion
The e2e test SHALL wait for the bench process to complete its 1000-row generation (approximately 10 seconds at 100 rows/sec) and verify clean exit.

#### Scenario: Bench completes successfully
- **WHEN** the bench process finishes generating all 1000 rows
- **THEN** the bench process exits with status code 0
- **AND** the webui shows the program in exited/stopped state

### Requirement: Test cleans up completely
The e2e test SHALL remove the bench program from the registry using `appkeeper stop` + `appkeeper remove` and verify no leftover processes or configuration files remain.

#### Scenario: Complete cleanup after test
- **WHEN** the test executes `appkeeper stop <app>` then `appkeeper remove <app>`
- **THEN** the bench program is removed from the appkeeper registry
- **AND** no bench processes remain running
- **AND** the generated configuration file is deleted

### Requirement: Test uses real browser interactions
The e2e test SHALL use playwright to drive real browser interactions with the webui, not mocked or simulated browser behavior.

#### Scenario: Playwright drives actual browser
- **WHEN** the test executes webui interactions
- **THEN** playwright launches a real browser instance
- **AND** all clicks, navigation, and observations occur through the actual webui interface

### Requirement: Test performs dual log verification
The e2e test SHALL verify log output through both webui display (visual confirmation) and disk file inspection (content verification), ensuring consistency between the two views.

#### Scenario: Webui and disk logs are consistent
- **WHEN** the test verifies logs in both webui and on disk
- **THEN** the log content observed in the webui matches the content in the disk files
- **AND** both sources show the same number of log rows

### Requirement: Test is deterministic and repeatable
The e2e test SHALL produce consistent results across multiple runs, with no race conditions or timing-dependent failures.

#### Scenario: Test passes reliably
- **WHEN** the test is executed multiple times in succession
- **THEN** each execution completes successfully
- **AND** no flaky failures occur due to timing or state issues
