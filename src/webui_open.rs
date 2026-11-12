//! Shared "open the web console" plumbing for `ak webui` and the interactive
//! shell's `open` (add-webui-launch-command D8): URL construction with the
//! auto-mode bootstrap token, the `/api/health` readiness probe, the
//! platform browser launcher, and the exit-code/message contract.
//!
//! Nothing here reads the daemon config's `[webui]` section: the console's
//! address and token come from the control plane (`GET /v1/webui`), which is
//! the only way to learn an `auto` token (control-plane spec: daemon-first,
//! MUST NOT read or infer the in-process token).

use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

use crate::client::WebuiStatus;
use crate::config::DEFAULT_WEBUI_LISTEN;

/// Console base URL used when no status is known yet (the daemon was
/// unreachable), matching the built-in `webui.listen` default.
pub(crate) const DEFAULT_WEBUI_URL: &str = "http://127.0.0.1:9877";
/// Total budget for the pre-open readiness probe (control-plane spec: about
/// 5 seconds, covering a console that is still binding right after a reload).
pub(crate) const HEALTH_BUDGET: Duration = Duration::from_secs(5);
/// Delay between readiness probes inside [`HEALTH_BUDGET`].
const HEALTH_POLL: Duration = Duration::from_millis(150);
/// Printed when no browser is launched because the session is headless.
const NO_GUI_HINT: &str =
    "no graphical session (DISPLAY/WAYLAND_DISPLAY unset) — open the URL above manually";

/// How `ak webui` was invoked: the two output-only modes never launch a
/// browser (mutually exclusive at the CLI layer).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WebuiMode {
    /// Default: probe, then launch the browser.
    Open,
    /// `--print-url`: print the full URL (token included in auto mode).
    PrintUrl,
    /// `--json`: print `{url, listen, auth, opened}`.
    Json,
}

/// The decided outcome of one `ak webui` invocation: the process exit code
/// and the lines for each stream. Keeping the decision pure makes the
/// exit-code/message contract unit-testable without a live daemon or a real
/// browser.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct WebuiReport {
    pub code: i32,
    pub stdout: Vec<String>,
    pub stderr: Vec<String>,
}

/// The URL the browser should open. In `auto` mode the bootstrap token rides
/// in `?token=` so the SPA can exchange it for a session cookie and strip it
/// from the address bar (webui-api: 引导 URL); a fixed token MUST NOT be put
/// in a URL.
pub(crate) fn console_url(status: &WebuiStatus) -> String {
    match (status.auth.as_str(), status.token.as_deref()) {
        ("auto", Some(token)) if !token.is_empty() => {
            format!("{}?token={token}", status.url)
        }
        _ => status.url.clone(),
    }
}

/// Why the console cannot be opened at all, with the operator's fix
/// (control-plane spec: not enabled → the enable command; not serving →
/// daemon log / address occupancy). `None` = ready to probe/open.
pub(crate) fn unavailable_reason(status: &WebuiStatus) -> Option<String> {
    if !status.enabled {
        return Some(format!(
            "the web console is not enabled — enable it with \
             `ak config --set webui.listen={}` and then `ak config --reload`",
            status.listen
        ));
    }
    if !status.serving {
        return Some(format!(
            "the web console is enabled but not serving on {} — check the daemon log \
             and whether that address is already in use, then `ak config --reload` to retry",
            status.listen
        ));
    }
    None
}

/// The hint shown when the console is on but does not answer `/api/health`.
fn unreachable_message(base: &str, err: &anyhow::Error) -> String {
    format!(
        "the web console is not reachable at {base}: {err:#} — check the daemon log and \
         whether that address is already in use, then `ak config --reload` to retry"
    )
}

/// Decide everything `ak webui` outputs. `probe` and `launch` are injected so
/// tests can assert the contract without sockets or a browser.
pub(crate) fn run(
    status: &WebuiStatus,
    mode: WebuiMode,
    gui: bool,
    probe: impl FnOnce(&str) -> Result<()>,
    launch: impl FnOnce(&str) -> Result<()>,
) -> WebuiReport {
    let mut report = WebuiReport::default();
    if let Some(reason) = unavailable_reason(status) {
        report.code = 1;
        report.stderr.push(format!("ak: error: {reason}"));
        return report;
    }
    let full = console_url(status);
    match mode {
        WebuiMode::PrintUrl => {
            report.stdout.push(full);
            return report;
        }
        WebuiMode::Json => {
            report.stdout.push(
                serde_json::json!({
                    "url": full,
                    "listen": status.listen,
                    "auth": status.auth,
                    "opened": false,
                })
                .to_string(),
            );
            return report;
        }
        WebuiMode::Open => {}
    }
    // Probe the base URL (no token): `/api/health` is public and the token
    // must not travel in a request line more than necessary.
    if let Err(e) = probe(&status.url) {
        report.code = 1;
        report.stderr.push(format!(
            "ak: error: {}",
            unreachable_message(&status.url, &e)
        ));
        return report;
    }
    if !gui {
        // Headless: never try to launch; hand the operator the full URL.
        report.stdout.push(full);
        report.stdout.push(NO_GUI_HINT.to_string());
        return report;
    }
    match launch(&full) {
        Ok(()) => {
            // Regular success output never contains the token.
            report.stdout.push(format!("opening {}", status.url));
        }
        Err(e) => {
            report.code = 1;
            report.stdout.push(full);
            report.stderr.push(format!(
                "ak: error: could not launch the system browser: {e:#}"
            ));
            report
                .stderr
                .push("hint: open the URL above manually".to_string());
        }
    }
    report
}

/// Whether a graphical session is available for launching a browser. Unix
/// (except macOS) has no built-in desktop session: with neither `DISPLAY`
/// nor `WAYLAND_DISPLAY` set, `xdg-open` cannot work, so no launch is
/// attempted (control-plane spec: 无图形会话). macOS and Windows always have
/// a launcher (`open` / `cmd /C start`) and do not use these variables.
pub(crate) fn gui_session(display: Option<&str>, wayland_display: Option<&str>) -> bool {
    if cfg!(all(unix, not(target_os = "macos"))) {
        let set = |v: Option<&str>| v.is_some_and(|v| !v.trim().is_empty());
        set(display) || set(wayland_display)
    } else {
        true
    }
}

/// [`gui_session`] from this process's environment.
pub(crate) fn gui_available() -> bool {
    gui_session(
        std::env::var("DISPLAY").ok().as_deref(),
        std::env::var("WAYLAND_DISPLAY").ok().as_deref(),
    )
}

/// `GET /api/health` on the console port, short timeout. Public endpoint, no
/// token involved (webui auth is independent of `/v1`).
pub(crate) fn webui_health(base: &str) -> Result<()> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_connect(Some(Duration::from_secs(2)))
        .timeout_per_call(Some(Duration::from_secs(2)))
        .build()
        .into();
    let resp = agent
        .get(&format!("{base}/api/health"))
        .call()
        .map_err(|e| anyhow::anyhow!("webui unreachable: {e}"))?;
    if (200..300).contains(&resp.status().as_u16()) {
        Ok(())
    } else {
        bail!("webui health returned {}", resp.status().as_u16())
    }
}

/// Poll [`webui_health`] until the console answers or `budget` elapses.
pub(crate) fn wait_ready(base: &str, budget: Duration) -> Result<()> {
    let started = Instant::now();
    loop {
        match webui_health(base) {
            Ok(()) => return Ok(()),
            Err(e) => {
                let elapsed = started.elapsed();
                if elapsed >= budget {
                    return Err(e);
                }
                std::thread::sleep(HEALTH_POLL.min(budget.saturating_sub(elapsed)));
            }
        }
    }
}

/// Platform browser launcher. Returns the command for diagnostics/tests.
pub(crate) fn browser_command(url: &str) -> std::process::Command {
    #[cfg(target_os = "macos")]
    {
        let mut cmd = std::process::Command::new("open");
        cmd.arg(url);
        cmd
    }
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        let mut cmd = std::process::Command::new("cmd");
        cmd.args(["/C", "start"]);
        // raw arg: `start` mangles extra quotes otherwise
        cmd.raw_arg(format!("\"{url}\""));
        cmd
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let mut cmd = std::process::Command::new("xdg-open");
        cmd.arg(url);
        cmd
    }
}

/// Launch the system browser and wait for the launcher to exit. A launcher
/// that starts but fails (e.g. no handler) is an error too.
pub(crate) fn launch_browser(url: &str) -> Result<()> {
    let mut cmd = browser_command(url);
    let status = cmd
        .status()
        .with_context(|| format!("could not start the system browser for {url}"))?;
    if !status.success() {
        bail!("the browser launcher exited with {status}");
    }
    Ok(())
}

/// The shell's `open` target: the full launch URL on success, or the hint to
/// print while keeping the shell alive.
pub(crate) fn shell_target(status: &WebuiStatus) -> std::result::Result<String, String> {
    match unavailable_reason(status) {
        Some(reason) => Err(format!("{}\n{reason}", shell_hint(&status.url))),
        None => Ok(console_url(status)),
    }
}

/// The pre-existing shell wording for an unreachable console (shell-client
/// spec: `open` keeps its hint, including how to enable the console).
pub(crate) fn shell_hint(base: &str) -> String {
    format!(
        "webui not reachable at {base} — enable it in the daemon config \
         (`ak config --set webui.listen={DEFAULT_WEBUI_LISTEN}`) and run \
         `ak config --reload`, then retry"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(auth: &str, token: Option<&str>) -> WebuiStatus {
        WebuiStatus {
            enabled: true,
            serving: true,
            listen: "127.0.0.1:9877".to_string(),
            url: "http://127.0.0.1:9877".to_string(),
            auth: auth.to_string(),
            token: token.map(str::to_string),
        }
    }

    fn no_launch(_: &str) -> Result<()> {
        panic!("must not launch a browser");
    }

    fn ok_launch(_: &str) -> Result<()> {
        Ok(())
    }

    fn ok_probe(_: &str) -> Result<()> {
        Ok(())
    }

    /// 4.3: auto mode carries `?token=`, fixed mode does not — for both URL
    /// output modes.
    #[test]
    fn print_url_carries_the_token_only_in_auto_mode() {
        let auto = run(
            &status("auto", Some("abc123")),
            WebuiMode::PrintUrl,
            true,
            ok_probe,
            no_launch,
        );
        assert_eq!(auto.code, 0);
        assert_eq!(
            auto.stdout,
            vec!["http://127.0.0.1:9877?token=abc123".to_string()]
        );

        let fixed = run(
            &status("token", Some("s3cr3t")),
            WebuiMode::PrintUrl,
            true,
            ok_probe,
            no_launch,
        );
        assert_eq!(fixed.code, 0);
        assert_eq!(fixed.stdout, vec!["http://127.0.0.1:9877".to_string()]);
    }

    /// 4.3: `--json` reports url/listen/auth/opened and never opens.
    #[test]
    fn json_mode_reports_the_documented_fields() {
        let report = run(
            &status("auto", Some("abc123")),
            WebuiMode::Json,
            false,
            ok_probe,
            no_launch,
        );
        assert_eq!(report.code, 0);
        let doc: serde_json::Value = serde_json::from_str(&report.stdout[0]).unwrap();
        assert_eq!(doc["url"], "http://127.0.0.1:9877?token=abc123");
        assert_eq!(doc["listen"], "127.0.0.1:9877");
        assert_eq!(doc["auth"], "auto");
        assert_eq!(doc["opened"], false);
        assert!(doc.get("token").is_none(), "no separate token field: {doc}");
    }

    /// 4.2: not enabled / not serving are exit 1 with the repair command,
    /// before any probe or browser launch.
    #[test]
    fn unavailable_console_fails_with_the_repair_command() {
        let mut off = status("auto", None);
        off.enabled = false;
        off.serving = false;
        let report = run(&off, WebuiMode::Open, true, ok_probe, no_launch);
        assert_eq!(report.code, 1);
        assert!(report.stdout.is_empty());
        let text = report.stderr.join("\n");
        assert!(text.contains("not enabled"), "{text}");
        assert!(
            text.contains("ak config --set webui.listen=127.0.0.1:9877"),
            "{text}"
        );
        assert!(text.contains("ak config --reload"), "{text}");

        let mut broken = status("auto", None);
        broken.serving = false;
        let report = run(&broken, WebuiMode::Open, true, ok_probe, no_launch);
        assert_eq!(report.code, 1);
        let text = report.stderr.join("\n");
        assert!(text.contains("not serving on 127.0.0.1:9877"), "{text}");
        assert!(text.contains("already in use"), "{text}");
        assert!(text.contains("ak config --reload"), "{text}");
    }

    /// 4.2: a failed readiness probe is exit 1 and never launches.
    #[test]
    fn unreachable_console_fails_without_launching() {
        let report = run(
            &status("auto", Some("t")),
            WebuiMode::Open,
            true,
            |_| Err(anyhow::anyhow!("webui unreachable: connection refused")),
            no_launch,
        );
        assert_eq!(report.code, 1);
        assert!(report.stdout.is_empty());
        let text = report.stderr.join("\n");
        assert!(
            text.contains("not reachable at http://127.0.0.1:9877"),
            "{text}"
        );
        assert!(text.contains("connection refused"), "{text}");
    }

    /// 4.4: headless (no DISPLAY/WAYLAND_DISPLAY) prints the full URL with
    /// the token and exits 0 without launching.
    #[test]
    fn headless_prints_the_full_url_and_exits_zero() {
        let report = run(
            &status("auto", Some("abc123")),
            WebuiMode::Open,
            false,
            ok_probe,
            no_launch,
        );
        assert_eq!(report.code, 0);
        assert_eq!(report.stdout[0], "http://127.0.0.1:9877?token=abc123");
        assert!(report.stdout[1].contains("no graphical session"));
        assert!(report.stderr.is_empty());
    }

    /// 4.4 + success output: the default open path reports the token-free
    /// address; a launcher failure prints the full URL and exits 1.
    #[test]
    fn launch_success_hides_the_token_and_failure_exits_one() {
        let ok = run(
            &status("auto", Some("abc123")),
            WebuiMode::Open,
            true,
            ok_probe,
            ok_launch,
        );
        assert_eq!(ok.code, 0);
        let text = ok.stdout.join("\n");
        assert_eq!(text, "opening http://127.0.0.1:9877");
        assert!(!text.contains("abc123"), "success output hides the token");

        let failed = run(
            &status("auto", Some("abc123")),
            WebuiMode::Open,
            true,
            ok_probe,
            |_| Err(anyhow::anyhow!("no handler")),
        );
        assert_eq!(failed.code, 1);
        assert_eq!(
            failed.stdout,
            vec!["http://127.0.0.1:9877?token=abc123".to_string()]
        );
        let text = failed.stderr.join("\n");
        assert!(
            text.contains("could not launch the system browser"),
            "{text}"
        );
        assert!(text.contains("no handler"), "{text}");
    }

    /// 4.4: the headless decision reads DISPLAY/WAYLAND_DISPLAY (unix) and is
    /// always GUI-capable elsewhere.
    #[test]
    fn gui_session_reads_display_and_wayland() {
        if cfg!(all(unix, not(target_os = "macos"))) {
            assert!(!gui_session(None, None));
            assert!(!gui_session(Some(""), Some("   ")));
            assert!(gui_session(Some(":0"), None));
            assert!(gui_session(None, Some("wayland-0")));
        } else {
            assert!(gui_session(None, None));
        }
    }

    /// 5.1 (shared): the shell target follows the same auto/fixed rule, and a
    /// not-serving console yields the enable/unreachable hint.
    #[test]
    fn shell_target_follows_the_token_mode_and_refuses_when_down() {
        assert_eq!(
            shell_target(&status("auto", Some("abc123"))).unwrap(),
            "http://127.0.0.1:9877?token=abc123"
        );
        assert_eq!(
            shell_target(&status("token", Some("s3cr3t"))).unwrap(),
            "http://127.0.0.1:9877"
        );
        let mut down = status("auto", None);
        down.serving = false;
        let hint = shell_target(&down).unwrap_err();
        assert!(hint.contains("webui not reachable at http://127.0.0.1:9877"));
        assert!(hint.contains("ak config --set webui.listen=127.0.0.1:9877"));
        assert!(hint.contains("ak config --reload"));
    }

    /// The offline hint is the shell's pre-existing wording.
    #[test]
    fn shell_hint_names_the_enable_path_and_default_address() {
        let hint = shell_hint(DEFAULT_WEBUI_URL);
        assert!(hint.contains("webui not reachable at http://127.0.0.1:9877"));
        assert!(hint.contains("ak config --set webui.listen=127.0.0.1:9877"));
        assert!(hint.contains("ak config --reload"));
    }

    /// The readiness budget is bounded: with a zero budget one failed attempt
    /// is enough (no infinite loop), and an unreachable loopback port fails
    /// fast.
    #[test]
    fn wait_ready_is_bounded() {
        let err = wait_ready("http://127.0.0.1:1", Duration::ZERO).unwrap_err();
        assert!(format!("{err:#}").contains("unreachable"), "{err:#}");
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn browser_command_uses_xdg_open_with_the_url() {
        let cmd = browser_command("http://127.0.0.1:9877");
        assert_eq!(cmd.get_program(), "xdg-open");
        let args: Vec<_> = cmd.get_args().map(|a| a.to_string_lossy()).collect();
        assert_eq!(args, vec!["http://127.0.0.1:9877"]);
    }
}
