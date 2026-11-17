//! Shared "open the web console" plumbing for `ak webui` and the interactive
//! shell's `open` (add-webui-launch-command D8): URL construction with the
//! auto-mode bootstrap token, the `/api/health` readiness probe, the
//! platform browser launcher, and the exit-code/message contract.
//!
//! `ak webui` is split at the side-effect boundary (webui-open-best-effort
//! D1): [`plan`] is pure and decides everything that depends on the console
//! status, the output mode and the session; [`execute`] runs the probe and the
//! launcher and returns only the stderr notes plus the exit code. The URL is
//! printed by the caller *before* [`execute`] runs, so nothing can delay or
//! suppress it, and `execute` never sees it.
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
/// Process exit code for every path where the console is usable (control-plane
/// spec: convenience degradations never fail the command).
const EXIT_OK: i32 = 0;
/// Prefix for the single stderr note a degradation emits. The command exits 0
/// on every one of these paths, so `error:` would be a lie (and would trip CI
/// log matchers and editor problem panes).
const WARNING_PREFIX: &str = "ak: warning: ";
/// Printed when no browser is launched because the session is headless. The
/// URL is on stdout, so this line points there instead of saying "above"
/// (webui-open-best-effort D5: the streams are independent).
const NO_GUI_HINT: &str =
    "no graphical session (DISPLAY/WAYLAND_DISPLAY unset) — open the URL on stdout manually";

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

/// Everything one `ak webui` invocation outputs, decided without side effects
/// (webui-open-best-effort D1). `Unavailable` deliberately has no URL field:
/// "no URL when the console is unusable" is then structural, not a convention.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Plan {
    /// The daemon reports the console as not enabled / not serving: exit 1
    /// with the operator's repair command, and no URL.
    Unavailable { reason: String },
    /// `--print-url` / `--json`: print this line and exit 0.
    Print { line: String },
    /// Default mode: print `url`, then probe and launch.
    Open { url: String, gui: bool },
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

/// Collapse an error chain onto one line: each degradation is exactly one
/// stderr line (control-plane spec), and `anyhow`'s `{:#}` can still carry
/// newlines from the underlying error.
fn one_line(err: &anyhow::Error) -> String {
    format!("{err:#}").replace('\n', " ").replace('\r', " ")
}

/// Decide everything `ak webui` prints, without any side effect. `gui` is
/// passed in so the decision stays pure and testable (the caller reads
/// [`gui_available`]).
pub(crate) fn plan(status: &WebuiStatus, mode: WebuiMode, gui: bool) -> Plan {
    if let Some(reason) = unavailable_reason(status) {
        return Plan::Unavailable { reason };
    }
    let full = console_url(status);
    match mode {
        WebuiMode::PrintUrl => Plan::Print { line: full },
        WebuiMode::Json => Plan::Print {
            line: serde_json::json!({
                "url": full,
                "listen": status.listen,
                "auth": status.auth,
                // Output modes never launch a browser, so `opened` can only
                // ever be false (webui-open-best-effort D4).
                "opened": false,
            })
            .to_string(),
        },
        WebuiMode::Open => Plan::Open { url: full, gui },
    }
}

/// Run the convenience steps of default mode and report their degradations.
///
/// The caller prints the URL first and flushes; this function only produces
/// the stderr notes and the exit code, so it cannot reorder or suppress the
/// URL (webui-open-best-effort D1). Every degradation exits 0: the console
/// is serving, so the command delivered its artifact.
///
/// - headless: never probes, never launches;
/// - probe failure: the console did not answer `/api/health` in
///   [`HEALTH_BUDGET`], so no browser is launched;
/// - launch failure: the launcher is missing or exited non-zero; no fallback
///   launcher is tried.
pub(crate) fn execute(
    gui: bool,
    probe: impl FnOnce() -> Result<()>,
    launch: impl FnOnce() -> Result<()>,
) -> (i32, Vec<String>) {
    if !gui {
        // No browser to open: probing would only delay the exit.
        return (EXIT_OK, vec![format!("{WARNING_PREFIX}{NO_GUI_HINT}")]);
    }
    if let Err(e) = probe() {
        let note = format!(
            "{WARNING_PREFIX}the web console is not reachable: {} — check the daemon log \
             and whether the listen address is already in use, then `ak config --reload` \
             to retry",
            one_line(&e)
        );
        return (EXIT_OK, vec![note]);
    }
    if let Err(e) = launch() {
        let note = format!(
            "{WARNING_PREFIX}could not launch the system browser: {} — open the URL on \
             stdout manually",
            one_line(&e)
        );
        return (EXIT_OK, vec![note]);
    }
    (EXIT_OK, Vec::new())
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

    // Panic closures: the decision must not reach a side effect.

    fn no_probe() -> Result<()> {
        panic!("must not probe the console");
    }

    fn no_launch() -> Result<()> {
        panic!("must not launch a browser");
    }

    fn ok_probe() -> Result<()> {
        Ok(())
    }

    fn ok_launch() -> Result<()> {
        Ok(())
    }

    /// Not enabled / not serving are exit 1 decisions carrying the repair
    /// command, and `Unavailable` has no URL field to print.
    #[test]
    fn unavailable_console_fails_with_the_repair_command() {
        let mut off = status("auto", None);
        off.enabled = false;
        off.serving = false;
        match plan(&off, WebuiMode::Open, true) {
            Plan::Unavailable { reason } => {
                assert!(reason.contains("not enabled"), "{reason}");
                assert!(
                    reason.contains("ak config --set webui.listen=127.0.0.1:9877"),
                    "{reason}"
                );
                assert!(reason.contains("ak config --reload"), "{reason}");
            }
            other => panic!("expected Unavailable, got {other:?}"),
        }

        let mut broken = status("auto", None);
        broken.serving = false;
        match plan(&broken, WebuiMode::Open, true) {
            Plan::Unavailable { reason } => {
                assert!(reason.contains("not serving on 127.0.0.1:9877"), "{reason}");
                assert!(reason.contains("already in use"), "{reason}");
                assert!(reason.contains("ak config --reload"), "{reason}");
            }
            other => panic!("expected Unavailable, got {other:?}"),
        }
    }

    /// `--json` reports url/listen/auth/opened and never opens (`opened` is
    /// the constant false: output modes never launch).
    #[test]
    fn json_mode_reports_the_documented_fields() {
        let Plan::Print { line } = plan(&status("auto", Some("abc123")), WebuiMode::Json, false)
        else {
            panic!("--json must be side-effect free");
        };
        let doc: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(doc["url"], "http://127.0.0.1:9877?token=abc123");
        assert_eq!(doc["listen"], "127.0.0.1:9877");
        assert_eq!(doc["auth"], "auto");
        assert_eq!(doc["opened"], false);
        assert!(doc.get("token").is_none(), "no separate token field: {doc}");
    }

    /// Auto mode carries `?token=`, fixed mode does not — for the
    /// `--print-url` output mode.
    #[test]
    fn print_url_carries_the_token_only_in_auto_mode() {
        assert_eq!(
            plan(&status("auto", Some("abc123")), WebuiMode::PrintUrl, true),
            Plan::Print {
                line: "http://127.0.0.1:9877?token=abc123".to_string(),
            }
        );
        assert_eq!(
            plan(&status("token", Some("s3cr3t")), WebuiMode::PrintUrl, true),
            Plan::Print {
                line: "http://127.0.0.1:9877".to_string(),
            }
        );
    }

    /// Default mode decides the full URL (token included in auto mode) and
    /// passes `gui` through untouched.
    #[test]
    fn open_plan_carries_the_full_url_and_gui() {
        assert_eq!(
            plan(&status("auto", Some("abc123")), WebuiMode::Open, true),
            Plan::Open {
                url: "http://127.0.0.1:9877?token=abc123".to_string(),
                gui: true,
            }
        );
        assert_eq!(
            plan(&status("token", None), WebuiMode::Open, false),
            Plan::Open {
                url: "http://127.0.0.1:9877".to_string(),
                gui: false,
            }
        );
    }

    /// Headless: no probe, no launch, one `ak: warning:` line and exit 0.
    #[test]
    fn execute_headless_does_not_probe_or_launch() {
        let (code, notes) = execute(false, no_probe, no_launch);
        assert_eq!(code, 0);
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert!(notes[0].starts_with("ak: warning: "), "{:?}", notes[0]);
        assert!(notes[0].contains("no graphical session"), "{:?}", notes[0]);
        assert!(notes[0].contains("stdout"), "{:?}", notes[0]);
        assert!(!notes[0].contains('\n'), "{:?}", notes[0]);
    }

    /// A probe that never answers is a degradation: exit 0, one warning with
    /// the reason, and no browser launch.
    #[test]
    fn execute_probe_failure_exits_zero_with_one_warning() {
        let (code, notes) = execute(
            true,
            || Err(anyhow::anyhow!("webui unreachable: connection refused")),
            no_launch,
        );
        assert_eq!(code, 0);
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert!(notes[0].starts_with("ak: warning: "), "{:?}", notes[0]);
        assert!(notes[0].contains("not reachable"), "{:?}", notes[0]);
        assert!(notes[0].contains("connection refused"), "{:?}", notes[0]);
        assert!(!notes[0].contains('\n'), "{:?}", notes[0]);
    }

    /// A missing launcher (spawn failure) and a launcher that exits non-zero
    /// are the same degradation: exit 0, exactly one warning with the reason
    /// and the manual fallback.
    #[test]
    fn execute_launch_failure_exits_zero_with_one_warning() {
        let (code, notes) = execute(true, ok_probe, || {
            Err(anyhow::anyhow!(
                "could not start the system browser for http://127.0.0.1:9877: \
                     No such file or directory"
            ))
        });
        assert_eq!(code, 0);
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert!(notes[0].starts_with("ak: warning: "), "{:?}", notes[0]);
        assert!(
            notes[0].contains("could not launch the system browser"),
            "{:?}",
            notes[0]
        );
        assert!(
            notes[0].contains("open the URL on stdout manually"),
            "{:?}",
            notes[0]
        );
        assert!(!notes[0].contains('\n'), "{:?}", notes[0]);

        let (code, notes) = execute(true, ok_probe, || {
            Err(anyhow::anyhow!(
                "the browser launcher exited with exit status: 4"
            ))
        });
        assert_eq!(code, 0);
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert!(
            notes[0].contains("the browser launcher exited with"),
            "{:?}",
            notes[0]
        );
        assert!(!notes[0].contains('\n'), "{:?}", notes[0]);
    }

    /// Success is silent: stderr is empty and the code is 0.
    #[test]
    fn execute_success_is_silent() {
        let (code, notes) = execute(true, ok_probe, ok_launch);
        assert_eq!(code, 0);
        assert!(notes.is_empty(), "{notes:?}");
    }

    /// The headless decision reads DISPLAY/WAYLAND_DISPLAY (unix) and is
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
