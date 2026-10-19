//! Acceptance e2e for the apply-workflow + app-registry + actions changes
//! (`cargo run -p xtask -- e2e`).
//!
//! Drives the REAL daemon binary end-to-end, in two passes:
//!
//! - CLI pass — the acceptance checklist as executable scenarios (spec:
//!   apply-workflow, configuration, control-plane, shell-client,
//!   app-registry, actions, glossary):
//!   A. detect does not touch processes (periodic + reload preview),
//!   B. scoped apply leaves other apps untouched,
//!   C. --restart restarts unchanged programs but never a user-stopped one,
//!   D. online `add` enters pending and starts on apply,
//!   E. no-change apply is idempotent,
//!   G. add-process scaffold: offline `--apply` fails loudly (exit 3) and
//!      `--env` parse errors exit 2 (G0, daemon not yet running) → generate →
//!      apply → no-change re-add → changed re-add + `apply all` → `--apply`
//!      one-step (app-scoped) → reserved `all` rejected → remove deletes the
//!      generated file and stops the program.
//!   H. actions (actions/glossary change): offline declaration + rejection
//!      cases (charset / unknown variable / reserved word / timeout range),
//!      then online: `--app` fan-out order + user_stopped coverage, custom
//!      action success with variable substitution, work_dir resolution,
//!      timeout tree kill, same-action mutual exclusion, chained
//!      `ak restart` inside an action, stopped-program pid→empty,
//!      signal whitelist in/out + not-running (unix).
//!   K. config subcommand (add-config-subcommand): init (defaults + comments
//!      + app dir + example.toml.sample), get (defaults / file values /
//!      multi-key), strong-typed set (comments and key order preserved),
//!      rejection paths (unknown key, type violation, missing file, init
//!      overwrite), delete (default fallback, idempotent, unknown table),
//!      and `--edit` through a fake $EDITOR (valid + invalid). The webui
//!      keys ride the same engine (K16: set/get/delete on webui.listen and
//!      the whole [webui] table).
//!   RDF. run-daemon-first (run-daemon-first change): its own workspace with
//!      an EMPTY app_dir. Offline every `run` form exits 3 without deriving a
//!      daemon (RDF0). Online: `run --` / `run <name>` / `run --app` return
//!      `{program, config_path, pid}` and `--json` is machine-readable
//!      (RDF1-5), a single-program `run` blocks for `startsecs` (RDF6);
//!      failure paths (unresolvable command, early exit, name collision,
//!      reserved `all`) leave no `.toml`/`.toml.pending` residue (RDF7-10);
//!      a mid-flight reload sees only the non-`.toml` placeholder (RDF11);
//!      health-probed runs stream progress and either return a pid or fail
//!      with no residue (RDF12-13); a failed rename keeps the process and
//!      prints the full config, and a re-run detects the residual, refuses
//!      non-interactively, then re-lands on confirm without restarting
//!      (RDF14-16); the built-in `app_dir` default is `app.d` (RDF17).
//! - webui lifecycle pass (config-driven-webui): the daemon boots with
//!   `ak run` and a `[webui]` section in daemon.toml; W1 deletes the
//!   section + reload → connection refused, W2 sets a new listen + reload →
//!   serving on the new address, W3 restores the listen + reload → rebind,
//!   W4/W5 occupy then free the listen → reload degrades with a note and
//!   recovers, W7 boots a second daemon with a bare `[webui]` → serves the
//!   built-in default 127.0.0.1:9877 and the console exits with the daemon.
//!   Offline: G0c2 pins the removed `webui`/`system webui` subcommands,
//!   G0c3 covers the shell exit-3 contract, G0d2 rejects an unknown
//!   `[webui]` key through `validate`.
//! - webui pass — a real browser (playwright, chromium) against the embedded
//!   console: the pending badge appears after a disk edit, "Apply 此应用"
//!   (app-scope) confirms and applies, the result text renders, and a second
//!   app's pending is untouched by the scoped apply.
//!
//! Fast correctness tests stay in `cargo test` + `pnpm test`; this harness
//! exists because these paths span process boundaries (child processes, HTTP,
//! WS, a browser) and take ~20-40s.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

pub(crate) struct Args {
    pub keep: bool,
    /// Skip the browser pass (no node/playwright on the machine).
    pub no_browser: bool,
}

// -- workspace -------------------------------------------------------------

struct Workspace {
    root: PathBuf,
    /// The control endpoint the daemon config pins: a filesystem path on unix,
    /// a pipe name on Windows (unix-socket-control-plane: the control plane
    /// serves a local IPC endpoint, not a TCP port).
    control_socket: PathBuf,
    webui_port: u16,
}

impl Workspace {
    fn config_arg(&self) -> String {
        format!("{}", self.root.join("daemon.toml").display())
    }
}

fn free_port() -> Result<u16> {
    let l = std::net::TcpListener::bind("127.0.0.1:0")?;
    Ok(l.local_addr()?.port())
}

/// alpha: web + worker. beta: job. Both restart = "always".
fn setup_workspace(tag: &str) -> Result<Workspace> {
    let root = std::env::temp_dir().join(format!("xk-xtask-e2e-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    for d in ["apps", "logs", "app-alpha", "app-beta"] {
        std::fs::create_dir_all(root.join(d)).context("create workspace dirs")?;
    }
    let webui_port = free_port()?;
    // Forward slashes in the TOML basic string: a raw Windows path would
    // put invalid `\U`-style escapes into the config.
    let log_dir = root.join("logs").to_string_lossy().replace('\\', "/");
    // The web console is config-driven: the `[webui]` section enables it and
    // pins its port for the browser pass. `[app-default] restart = "always"`
    // mirrors what `config --init` renders, so the scaffolded apps in this
    // workspace get the supervised experience (the built-in cascade default is
    // `no`). `app_dir = "apps"` keeps this workspace on the explicit name the
    // scenarios below use (run-daemon-first changed the built-in default).
    // The control plane rides a local IPC endpoint inside the workspace
    // (unix-socket-control-plane): a socket path on unix, a pipe name on
    // Windows. The temp-dir prefix is short so `sun_path` (~107 bytes) is
    // never an issue.
    let control_socket = control_socket_in(&root);
    std::fs::write(
        root.join("daemon.toml"),
        format!(
            "[daemon]\nsocket = {socket}\nlog_dir = \"{log_dir}\"\nlog_level = \"warn\"\nmonitor_interval = 0.5\napp_dir = \"apps\"\n\n[app-default]\nrestart = \"always\"\n\n[webui]\nlisten = \"127.0.0.1:{webui_port}\"\n",
            socket = socket_toml_value(&control_socket),
        ),
    )?;
    std::fs::write(
        root.join("app-alpha/appkeeper.toml"),
        "[app]\nrestart = \"always\"\n\n[program.web]\ncommand = \"sleep 100000\"\nstartsecs = 0.2\n\n[program.worker]\ncommand = \"sleep 100000\"\nstartsecs = 0.2\n",
    )?;
    std::fs::write(
        root.join("app-beta/appkeeper.toml"),
        "[app]\nrestart = \"always\"\n\n[program.job]\ncommand = \"sleep 100000\"\nstartsecs = 0.2\n",
    )?;
    link_registration(
        &root.join("app-alpha/appkeeper.toml"),
        &root.join("apps/alpha.toml"),
    )?;
    link_registration(
        &root.join("app-beta/appkeeper.toml"),
        &root.join("apps/beta.toml"),
    )?;
    Ok(Workspace {
        root,
        control_socket,
        webui_port,
    })
}

/// The control endpoint for a workspace: `<root>/ak.sock` on unix (a short
/// temp-dir prefix keeps it inside the ~107-byte `sun_path` budget),
/// `\\.\pipe\ak-e2e-<name>.sock` on Windows.
fn control_socket_in(root: &Path) -> PathBuf {
    #[cfg(unix)]
    {
        let _ = root; // used above
        root.join("ak.sock")
    }
    #[cfg(windows)]
    {
        let name = format!(
            r"\\.\pipe\ak-e2e-{}-{}.sock",
            root.file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "ws".into()),
            std::process::id()
        );
        PathBuf::from(name)
    }
}

/// The TOML value for a `[daemon] socket` entry: a basic string on unix, a
/// literal string on Windows (backslashes must survive unescaped).
fn socket_toml_value(ep: &Path) -> String {
    #[cfg(unix)]
    {
        format!("{:?}", ep.to_string_lossy())
    }
    #[cfg(windows)]
    {
        format!("'{}'", ep.to_string_lossy())
    }
}

/// The current uid (via `id -u`), for asserting the uid-layered default
/// endpoint in the config scenarios. Windows has none (the pipe name carries
/// the user instead), where this returns 0 and the uid assertions are skipped.
fn current_uid_e2e() -> u32 {
    #[cfg(unix)]
    {
        std::process::Command::new("id")
            .arg("-u")
            .output()
            .ok()
            .and_then(|o| String::from_utf8_lossy(&o.stdout).trim().parse().ok())
            .unwrap_or(0)
    }
    #[cfg(windows)]
    {
        0
    }
}

/// Register a config as `app_dir/<name>.toml` — symlink where available,
/// hard-link fallback (unprivileged Windows), mirroring registry::make_link.
fn link_registration(target: &Path, link: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target, link)?;
    }
    #[cfg(windows)]
    {
        if std::os::windows::fs::symlink_file(target, link).is_err() {
            std::fs::hard_link(target, link)?;
        }
    }
    Ok(())
}

// -- daemon lifecycle --------------------------------------------------------

struct Daemon {
    child: Child,
}

/// `curl --unix-socket <ep> http://localhost<path>`: the control plane speaks
/// HTTP/1.1 over the local IPC endpoint, so plain curl is the harness client
/// (and doubles as the operator troubleshooting contract, spec 8.4).
fn curl_socket(endpoint: &Path, args: &[&str], path: &str) -> Command {
    let mut cmd = Command::new("curl");
    cmd.arg("-s");
    // Separate value: `--unix-socket=<path>` is only accepted by curl 8.15+,
    // while Debian bookworm ships 7.88.
    cmd.arg("--unix-socket").arg(endpoint);
    cmd.args(args);
    cmd.arg(format!("http://localhost{path}"));
    cmd
}

/// GET a control-plane path over the socket; returns (status, body).
fn socket_get(endpoint: &Path, path: &str) -> Result<(u16, String)> {
    let out = curl_socket(endpoint, &["-o", "-", "-w", "\n%{http_code}"], path)
        .output()
        .context("curl the control socket")?;
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    let (body, code) = text.rsplit_once('\n').unwrap_or((&text, "0"));
    Ok((code.trim().parse().unwrap_or(0), body.to_string()))
}

/// Is the daemon healthy on this control endpoint?
fn socket_health_ok(endpoint: &Path) -> bool {
    matches!(socket_get(endpoint, "/v1/health"), Ok((c, _)) if c == 200)
}

fn spawn_daemon(bin: &Path, ws: &Workspace) -> Result<Daemon> {
    // The daemon must not inherit our stdio (see stress.rs for the reasons),
    // but its output lands in workspace files so a crash leaves evidence.
    let out = std::fs::File::create(ws.root.join("daemon.out.log"))?;
    let err = out.try_clone()?;
    let mut child = Command::new(bin)
        .arg("--config")
        .arg(ws.root.join("daemon.toml"))
        .arg("daemon")
        .stdout(Stdio::from(out))
        .stderr(Stdio::from(err))
        .spawn()
        .context("spawn daemon")?;
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if Instant::now() > deadline {
            let _ = child.kill();
            bail!("daemon did not become healthy in 20s");
        }
        if let Ok(Some(_)) = child.try_wait() {
            bail!("daemon exited during startup");
        }
        // Readiness rides the control socket — there is no control port to
        // poll (and the console may not even be enabled in a given workspace).
        if socket_health_ok(&ws.control_socket) {
            break;
        }
        std::thread::sleep(Duration::from_millis(150));
    }
    Ok(Daemon { child })
}

fn shutdown_daemon(ws: &Workspace, daemon: &mut Daemon) {
    let _ = curl_socket(&ws.control_socket, &["-m", "10", "-X", "POST"], "/v1/shutdown").output();
    for _ in 0..60 {
        if matches!(daemon.child.try_wait(), Ok(Some(_))) {
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let _ = daemon.child.kill();
    let _ = daemon.child.wait();
}

// -- helpers -------------------------------------------------------------------

fn cli(bin: &Path, ws: &Workspace, args: &[&str]) -> Result<String> {
    let out = Command::new(bin)
        .arg("--config")
        .arg(ws.config_arg())
        .args(args)
        .output()
        .with_context(|| format!("run appkeeper {:?}", args))?;
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    if !out.status.success() {
        bail!(
            "appkeeper {:?} failed ({}): {}{}",
            args,
            out.status,
            stdout,
            String::from_utf8_lossy(&out.stderr)
        );
    }
    Ok(stdout)
}

/// Run the CLI expecting FAILURE; returns (success, exit code, stdout+stderr).
fn cli_try(bin: &Path, ws: &Workspace, args: &[&str]) -> Result<(bool, i32, String)> {
    let out = Command::new(bin)
        .arg("--config")
        .arg(ws.config_arg())
        .args(args)
        .output()
        .with_context(|| format!("run appkeeper {:?}", args))?;
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let code = out.status.code().unwrap_or(-1);
    Ok((out.status.success(), code, combined))
}

/// Run the CLI against an explicit daemon config (for bad-config scenarios
/// where the workspace's own daemon.toml must stay valid), expecting FAILURE.
fn cli_try_cfg(bin: &Path, cfg: &Path, args: &[&str]) -> Result<(bool, i32, String)> {
    let out = Command::new(bin)
        .arg("--config")
        .arg(cfg)
        .args(args)
        .output()
        .with_context(|| format!("run appkeeper {:?} (cfg {})", args, cfg.display()))?;
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let code = out.status.code().unwrap_or(-1);
    Ok((out.status.success(), code, combined))
}

/// Run the CLI with the exact argv given (no `--config` prefix). The removed
/// daemon-form interception happens before clap, so those scenarios exercise
/// the real command line from the spec instead of the helper's global flag.
fn cli_try_raw(bin: &Path, args: &[&str]) -> Result<(bool, i32, String)> {
    let out = Command::new(bin)
        .args(args)
        .output()
        .with_context(|| format!("run appkeeper {:?}", args))?;
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let code = out.status.code().unwrap_or(-1);
    Ok((out.status.success(), code, combined))
}

/// Run the CLI against an explicit daemon config, expecting SUCCESS.
fn cli_cfg(bin: &Path, cfg: &Path, args: &[&str]) -> Result<String> {
    let (ok, _code, combined) = cli_try_cfg(bin, cfg, args)?;
    if !ok {
        bail!(
            "appkeeper {:?} (cfg {}) failed: {combined}",
            args,
            cfg.display()
        );
    }
    Ok(combined)
}

/// HTTP status code + body of a control-plane request (curl, like `api`,
/// but the status code is kept so error mappings can be asserted).
fn api_checked(ws: &Workspace, method: &str, path: &str, body: &str) -> Result<(u16, String)> {
    let mut cmd = curl_socket(&ws.control_socket, &["-o", "-", "-w", "\n%{http_code}"], path);
    cmd.arg("-X").arg(method);
    if !body.is_empty() {
        cmd.args(["-d", body]);
    }
    let out = cmd.output().context("curl control plane")?;
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    let (body, code) = text.rsplit_once('\n').unwrap_or((&text, "0"));
    Ok((code.trim().parse().unwrap_or(0), body.to_string()))
}

/// A `sleep` executable for the scaffold scenarios: standard locations on
/// unix, PATH search elsewhere (Git for Windows ships one).
fn find_sleep() -> Result<PathBuf> {
    if cfg!(unix) {
        for p in ["/usr/bin/sleep", "/bin/sleep"] {
            if Path::new(p).is_file() {
                return Ok(PathBuf::from(p));
            }
        }
    }
    let exe = if cfg!(windows) { "sleep.exe" } else { "sleep" };
    if let Some(paths) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&paths) {
            let cand = dir.join(exe);
            if cand.is_file() {
                return Ok(cand);
            }
        }
    }
    bail!("no `sleep` executable found — needed by the scaffold e2e scenario");
}

fn api(ws: &Workspace, path: &str) -> Result<String> {
    let out = curl_socket(&ws.control_socket, &["-sf"], path)
        .output()
        .context("curl control plane")?;
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

/// pid of a program via the control plane (None = not running or unknown).
fn pid_of(ws: &Workspace, program: &str) -> Result<Option<u32>> {
    let (code, body) = socket_get(
        &ws.control_socket,
        &format!("/v1/programs/{program}"),
    )?;
    if code == 404 {
        return Ok(None);
    }
    if !(200..300).contains(&code) {
        bail!("GET {program} -> {code}: {body}");
    }
    let v: serde_json::Value = serde_json::from_str(&body)?;
    Ok(v.get("pid").and_then(|p| p.as_u64()).map(|p| p as u32))
}

fn wait_running(ws: &Workspace, program: &str) -> Result<u32> {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if let Some(pid) = pid_of(ws, program)? {
            return Ok(pid);
        }
        if Instant::now() > deadline {
            bail!("{program} never reached running");
        }
        std::thread::sleep(Duration::from_millis(150));
    }
}

fn wait_pending_empty(ws: &Workspace) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let v = api(ws, "/v1/pending")?;
        let v: serde_json::Value = serde_json::from_str(&v)?;
        if v.get("programs")
            .and_then(|p| p.as_array())
            .map(|a| a.is_empty())
            .unwrap_or(true)
        {
            return Ok(());
        }
        if Instant::now() > deadline {
            bail!("pending never cleared: {v}");
        }
        std::thread::sleep(Duration::from_millis(150));
    }
}

fn step(name: &str) {
    println!("\n==> {name}");
}

fn expect(cond: bool, what: &str) -> Result<()> {
    if !cond {
        bail!("assertion failed: {what}");
    }
    println!("    ok: {what}");
    Ok(())
}

// -- offline pass ----------------------------------------------------------------

/// Runs BEFORE the daemon exists. The "take effect now" entries (`add --apply`
/// and the run ad-hoc form) auto-pull the daemon up in the background instead
/// of failing (background-daemon spec); the pulled-up daemon is shut down
/// again so the later offline scenarios still see a dead control plane.
/// `--env` parse errors exit with the config code before anything is registered.
fn offline_pass(bin: &Path, ws: &Workspace) -> Result<()> {
    let sleep = find_sleep()?;

    step("G0. offline `add --apply` auto-pulls the daemon up (background-daemon)");
    let (ok, code, combined) = cli_try(
        bin,
        ws,
        &[
            "add",
            sleep.to_str().unwrap(),
            "--name",
            "offline-demo",
            "--apply",
        ],
    )?;
    expect(
        ok,
        &format!(
            "offline add --apply now succeeds by pulling the daemon up, got code {code}: {combined}"
        ),
    )?;
    expect(
        ws.root.join("apps/offline-demo.toml").is_file(),
        "the generated config is on disk",
    )?;
    expect(
        !combined.contains("daemon is offline") && !combined.contains("daemon unreachable"),
        "no more 'daemon offline' error: {combined}",
    )?;
    expect(
        combined.contains("offline-demo"),
        "the report names the new app",
    )?;
    // The pulled-up daemon is serving and knows the new app (its scaffolded
    // `sleep` with no args exits immediately, so only presence is asserted).
    expect(
        api(ws, "/v1/status")?.contains("offline-demo"),
        "the auto-pulled daemon loaded the new app",
    )?;
    // The daemon is now serving; shut it down so the offline scenarios below
    // (and the explicit daemon spawn) see a clean, offline control plane.
    cli(bin, ws, &["shutdown"])?;
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let up = socket_health_ok(&ws.control_socket);
        if !up {
            break;
        }
        if Instant::now() > deadline {
            bail!("the auto-pulled-up daemon did not shut down");
        }
        std::thread::sleep(Duration::from_millis(150));
    }

    step("G0b. `--env` without '=' / with an empty key exits 2 (config error)");
    let (ok, code, combined) = cli_try(
        bin,
        ws,
        &[
            "add",
            sleep.to_str().unwrap(),
            "--name",
            "env-bad",
            "--env",
            "NOEQUALS",
        ],
    )?;
    expect(
        !ok && code == 2,
        &format!("--env missing '=' exits 2, got {code}"),
    )?;
    expect(combined.contains("--env"), "error names the offending flag")?;
    expect(
        !ws.root.join("apps/env-bad.toml").exists(),
        "bad --env registers nothing",
    )?;
    let (ok, code, _) = cli_try(
        bin,
        ws,
        &[
            "add",
            sleep.to_str().unwrap(),
            "--name",
            "env-bad",
            "--env",
            "NOEQUALS",
        ],
    )?;
    expect(
        !ok && code == 2,
        &format!("--env missing '=' exits 2, got {code}"),
    )?;
    expect(combined.contains("--env"), "error names the offending flag")?;
    expect(
        !ws.root.join("apps/env-bad.toml").exists(),
        "bad --env registers nothing",
    )?;
    let (ok, code, _) = cli_try(
        bin,
        ws,
        &[
            "add",
            sleep.to_str().unwrap(),
            "--name",
            "env-bad",
            "--env",
            "=v",
        ],
    )?;
    expect(
        !ok && code == 2,
        &format!("empty --env key exits 2, got {code}"),
    )?;
    expect(
        !ws.root.join("apps/env-bad.toml").exists(),
        "empty --env key registers nothing",
    )?;

    step("G0c. `ak status` with no daemon is unreachable (exit 3)");
    let (ok, code, combined) = cli_try(bin, ws, &["status"])?;
    expect(
        !ok && code == 3,
        &format!("status without a daemon exits 3, got {code}"),
    )?;
    expect(
        combined.contains("unreachable"),
        "the error says the daemon is unreachable",
    )?;

    step("G0c2. removed webui/system subcommands stay removed (BREAKING, config-driven-webui)");
    for bad in [&["webui"][..], &["system", "webui"][..]] {
        let (ok, _code, combined) = cli_try(bin, ws, bad)?;
        expect(
            !ok && combined.contains("unrecognized subcommand"),
            &format!("{bad:?} must be rejected as an unknown subcommand: {combined:?}"),
        )?;
    }

    step("G0c3. `shell -e status` with no daemon exits 3 (shell 纳入退出码约定)");
    let (ok, code, combined) = cli_try(bin, ws, &["shell", "-e", "status"])?;
    expect(
        !ok && code == 3,
        &format!("shell -e status without a daemon exits 3, got {code}"),
    )?;
    expect(
        combined.contains("unreachable"),
        "the shell reports the daemon as unreachable",
    )?;

    step("G0c4. removed daemon forms of `run` are intercepted pre-clap (daemon-subcommand)");
    // The interception reads argv before clap, so the config path is never
    // resolved — the exact spec command lines can be run against a dead daemon.
    let cfg = ws.root.join("daemon-run-form.toml"); // never read, never created
    // `run --detach` → exit 2, hint points at `ak daemon --detach`.
    let (ok, code, combined) = cli_try_raw(bin, &["run", "--detach"])?;
    expect(
        !ok && code == 2,
        &format!("`run --detach` is intercepted with exit 2, got {code}: {combined:?}"),
    )?;
    expect(
        combined.contains("daemon --detach"),
        "the detach-form hint names `ak daemon --detach`",
    )?;
    // Config-only forms (the old systemd ExecStart signature) → exit 2 and the
    // extra unit-regeneration hint, with `--config` in either position.
    for args in [
        &["run", "--config", cfg.to_str().unwrap()][..],
        &["run", &format!("--config={}", cfg.to_str().unwrap())][..],
        &["--config", cfg.to_str().unwrap(), "run"][..],
    ] {
        let (ok, code, combined) = cli_try_raw(bin, args)?;
        expect(
            !ok && code == 2,
            &format!("{args:?} is intercepted with exit 2, got {code}: {combined:?}"),
        )?;
        expect(
            combined.contains("ak daemon") && combined.contains("service install --force --now"),
            &format!("{args:?} hint names `ak daemon` and the unit regeneration command"),
        )?;
    }
    // Bare `run` carries no daemon marker: it reaches dispatch and reports the
    // three-form usage error (design D4), not the migration interceptor.
    let (ok, code, combined) = cli_try_raw(bin, &["run"])?;
    expect(
        !ok && code == 2,
        &format!("bare `run` exits 2 with the usage error, got {code}: {combined:?}"),
    )?;
    expect(
        combined.contains("run <name>")
            && combined.contains("run --app")
            && combined.contains("run --"),
        "the bare-run error lists all three client forms",
    )?;
    // `run <name>` is a client call: with the daemon still down it reports
    // unreachable (exit 3) instead of the interception (exit 2). A dead-port
    // config keeps this hermetic — the bare form would read the machine's
    // default config (/etc/appkeeper/daemon.toml), where a real daemon may be
    // listening and answering 404 instead.
    let dead_cfg = ws.root.join("daemon-dead.toml");
    // A config pointing at an endpoint nothing serves keeps this hermetic —
    // the bare form would read the machine's default config otherwise, where
    // a real daemon of this uid may be listening.
    std::fs::write(
        &dead_cfg,
        format!(
            "[daemon]\nsocket = {}\n",
            socket_toml_value(&ws.root.join("daemon-dead.sock"))
        ),
    )?;
    let (ok, code, combined) = cli_try_cfg(bin, &dead_cfg, &["run", "web"])?;
    expect(
        !ok && code == 3,
        &format!("`run web` stays a client call (unreachable, exit 3), got {code}: {combined:?}"),
    )?;
    expect(
        combined.contains("unreachable") && !combined.contains("daemon form of `run`"),
        "`run <name>` is not caught by the daemon-form guard",
    )?;

    step("G0d. a relative daemon.log_dir fails validate AND refuses daemon startup");
    let bad_cfg = ws.root.join("daemon-bad.toml");
    std::fs::write(&bad_cfg, "[daemon]\nlog_dir = \"logs\"\n")?;
    let (ok, _code, combined) = cli_try_cfg(bin, &bad_cfg, &["validate"])?;
    expect(!ok, "validate with a relative log_dir fails")?;
    expect(
        combined.contains("daemon.log_dir") && combined.contains("absolute"),
        "the error names daemon.log_dir and demands an absolute path",
    )?;
    // The daemon itself must refuse to boot on that config (fast, non-zero).
    let out = std::fs::File::create(ws.root.join("bad-daemon.out.log"))?;
    let err = out.try_clone()?;
    let mut child = Command::new(bin)
        .arg("--config")
        .arg(&bad_cfg)
        .arg("daemon")
        .stdout(Stdio::from(out))
        .stderr(Stdio::from(err))
        .spawn()
        .context("spawn daemon with relative log_dir")?;
    let mut refused = false;
    for _ in 0..50 {
        match child.try_wait()? {
            Some(st) => {
                refused = !st.success();
                break;
            }
            None => std::thread::sleep(Duration::from_millis(100)),
        }
    }
    if !refused {
        let _ = child.kill();
        let _ = child.wait();
    }
    expect(
        refused,
        "the daemon exits non-zero instead of starting (relative log_dir)",
    )?;
    let refusal = std::fs::read_to_string(ws.root.join("bad-daemon.out.log"))?;
    expect(
        refusal.contains("daemon.log_dir") && refusal.contains("absolute"),
        "the startup refusal names daemon.log_dir and the fix",
    )?;

    step("G0d2. an unknown [webui] key fails validate (whole-config rejection)");
    let bad_webui = ws.root.join("daemon-webui-bad.toml");
    std::fs::write(&bad_webui, "[webui]\nauth = true\n")?;
    let (ok, _code, combined) = cli_try_cfg(bin, &bad_webui, &["validate"])?;
    expect(!ok, "validate rejects an unknown [webui] key")?;
    expect(
        combined.contains("auth") && combined.contains("unknown field"),
        "the error names the unknown field: {combined:?}",
    )?;

    // Clean the offline registration so the daemon pass starts clean.
    let out = cli(bin, ws, &["remove", "offline-demo"])?;
    expect(
        out.contains("generated config file removed"),
        "offline remove deletes the generated file",
    )?;
    Ok(())
}

// -- config subcommand pass (add-config-subcommand) -------------------------------

/// Offline scenarios for `ak config` (runs BEFORE the daemon exists, so
/// write actions report the offline note). Uses its own config paths under
/// `cfgcmd/` so the workspace's daemon.toml stays untouched for the daemon
/// pass. Covers: init → get defaults → set → get file values → illegal sets
/// rejected with the file unchanged → delete falls back to defaults →
/// idempotent delete → unknown-key delete rejected → init-overwrite rejected
/// → set on a missing file refused → sample not scanned as an app →
/// `config --edit` through a fake $EDITOR (valid + invalid).
fn config_pass(bin: &Path, ws: &Workspace) -> Result<()> {
    let dir = ws.root.join("cfgcmd");
    let cfg = dir.join("daemon.toml");

    step("K1. config --init creates defaults + comments + app dir + sample");
    let out = cli_cfg(bin, &cfg, &["config", "--init"])?;
    expect(cfg.is_file(), "daemon.toml created")?;
    let text = std::fs::read_to_string(&cfg)?;
    for key in [
        "log_level",
        "log_dir",
        "monitor_interval",
        "socket",
        "log_buffer_lines",
        "app_dir",
    ] {
        expect(
            text.contains(key),
            &format!("the template renders the known key {key}"),
        )?;
    }
    // unix-socket-control-plane removed the bind keys; tcp-control-endpoint
    // brought `auth_token` back as a COMMENTED template line (only the tcp://
    // form takes it) — so it must exist as a comment but never as a live key.
    for removed in ["host", "port = "] {
        expect(
            !text.contains(&format!("{removed} =")),
            &format!("the removed key {removed} is not rendered as a live key"),
        )?;
    }
    expect(
        text.contains("# auth_token = \"change-me\"") && text.contains("ONLY for the tcp:// form"),
        "auth_token is a commented template line with the tcp:// shape note",
    )?;
    expect(
        !text.contains("\nauth_token = "),
        "auth_token is not rendered as a live (uncommented) key",
    )?;
    expect(
        text.contains("socket = \"/run/appkeeper/") && text.contains("/ak.sock"),
        "the socket default is rendered (uid-layered endpoint)",
    )?;
    expect(
        text.contains("[app-default]") && text.contains("restart = \"always\""),
        "the [app-default] template section carries an explicit restart = \"always\"",
    )?;
    expect(
        dir.join("app.d").is_dir(),
        "the app registry dir is created",
    )?;
    let sample = dir.join("app.d/example.toml.sample");
    expect(sample.is_file(), "example.toml.sample is created")?;
    expect(out.contains("next:"), "init prints the next-step hint")?;

    step("K2. config --get: defaults for unset keys, bare value / key=value");
    let out = cli_cfg(bin, &cfg, &["config", "--get", "socket"])?;
    expect(
        out.trim() == format!("/run/appkeeper/{}/ak.sock", current_uid_e2e()),
        &format!("a single key prints the bare (uid-derived) default: {out:?}"),
    )?;
    let out = cli_cfg(bin, &cfg, &["config", "--get", "socket", "--get", "log_level"])?;
    expect(
        out.contains("socket=/run/appkeeper/") && out.contains("log_level=info"),
        &format!("multiple keys print key=value lines: {out:?}"),
    )?;

    step("K3. config --init refuses to overwrite an existing config");
    let before = std::fs::read_to_string(&cfg)?;
    let (ok, code, combined) = cli_try_cfg(bin, &cfg, &["config", "--init"])?;
    expect(
        !ok && code == 2,
        &format!("init over an existing file exits 2, got {code}"),
    )?;
    expect(
        combined.contains("already exists"),
        "the error explains the refusal",
    )?;
    expect(
        std::fs::read_to_string(&cfg)? == before,
        "the existing config was not touched",
    )?;

    step("K4. config --set writes strong-typed keys (offline note printed)");
    // Point the config at an endpoint nothing serves first: the reload-hint
    // probe reads the pre-modification config, and the built-in default
    // (/run/appkeeper/<uid>/ak.sock) may be served by a real daemon of this
    // uid on this host (which would flip the note online).
    let dead_socket = ws.root.join("cfgcmd-dead.sock");
    cli_cfg(
        bin,
        &cfg,
        &[
            "config",
            "--set",
            &format!("socket={}", dead_socket.display()),
        ],
    )?;
    let out = cli_cfg(
        bin,
        &cfg,
        &["config", "--set", "monitor_interval=2.5", "--set", "log_level=debug"],
    )?;
    expect(out.contains("monitor_interval"), "set reports the written keys: {out}")?;
    expect(
        out.contains("offline"),
        "the offline note says the change takes effect at next start",
    )?;
    expect(
        out.contains("set monitor_interval = 2.5") && out.contains("set log_level = debug"),
        "set prints the written keys with their new values",
    )?;
    let out = cli_cfg(bin, &cfg, &["config", "--get", "socket"])?;
    expect(
        out.trim() == dead_socket.display().to_string(),
        "the file value wins over the default",
    )?;
    let out = cli_cfg(bin, &cfg, &["config", "--get", "log_level"])?;
    expect(out.trim() == "debug", "log_level was written")?;

    step("K5. --set keeps comments, key order and untouched values");
    std::fs::write(
        &cfg,
        "# my header\n[daemon]\n# keep me\nlog_level = \"info\"\nlog_buffer_lines = 7 # count\nsocket = \"/tmp/old.sock\" # trailing\n",
    )?;
    cli_cfg(bin, &cfg, &["config", "--set", "socket=/tmp/new.sock"])?;
    let text = std::fs::read_to_string(&cfg)?;
    for snippet in [
        "# my header",
        "# keep me",
        "log_level = \"info\"",
        "log_buffer_lines = 7 # count",
    ] {
        expect(
            text.contains(snippet),
            &format!("untouched content survives: {snippet:?}"),
        )?;
    }
    expect(
        text.contains("socket = \"/tmp/new.sock\" # trailing"),
        "the touched key's trailing comment survives",
    )?;

    step("K6. illegal sets are rejected with the file unchanged");
    let before = std::fs::read_to_string(&cfg)?;
    for bad in [
        "socket=", // empty: the transport gate rejects it
        &format!("socket=/tmp/{}/ak.sock", "x".repeat(200)), // over sun_path
        "foo=1",
        "log_level=verbose",
        "monitor_interval=0",
        "log_dir=relative/logs", // parses, fails whole-config validation
    ] {
        let (ok, code, _combined) = cli_try_cfg(bin, &cfg, &["config", "--set", bad])?;
        expect(
            !ok && code == 2,
            &format!("--set {bad} is rejected with exit 2"),
        )?;
    }
    expect(
        std::fs::read_to_string(&cfg)? == before,
        "rejected sets leave the file byte-identical",
    )?;

    step("K6b. removed keys are refused with the replacement named (D7)");
    for removed in ["port=8080", "host=127.0.0.1"] {
        let (ok, code, combined) = cli_try_cfg(bin, &cfg, &["config", "--set", removed])?;
        expect(
            !ok && code == 2,
            &format!("--set {removed} exits 2 (the key is removed)"),
        )?;
        expect(
            combined.contains("removed") && combined.contains("socket"),
            "the error names the removal and the socket replacement: {combined:?}",
        )?;
    }
    expect(
        std::fs::read_to_string(&cfg)? == before,
        "the removed-key sets left the file byte-identical",
    )?;
    // tcp-control-endpoint: `auth_token` is a conditional key again. On a
    // path-form socket the write parses, but whole-config validation refuses
    // the combination — exit 2, the fix named, file unchanged.
    {
        let (ok, code, combined) = cli_try_cfg(bin, &cfg, &["config", "--set", "auth_token=legacy"])?;
        expect(
            !ok && code == 2,
            &format!("--set auth_token on a path-form socket exits 2, got {code}"),
        )?;
        expect(
            combined.contains("auth_token") && combined.contains("tcp://"),
            &format!("the combination error names auth_token and the tcp:// form: {combined:?}"),
        )?;
        expect(
            std::fs::read_to_string(&cfg)? == before,
            "the refused auth_token set left the file byte-identical",
        )?;
    }
    // A config file that still carries the dead keys fails validate outright.
    let legacy = ws.root.join("cfgcmd-legacy.toml");
    std::fs::write(&legacy, "[daemon]\nhost = \"127.0.0.1\"\nport = 7310\nauth_token = \"x\"\n")?;
    let (ok, _code, combined) = cli_try_cfg(bin, &legacy, &["validate"])?;
    expect(
        !ok && combined.contains("removed"),
        "a legacy config with host/port fails validate: {combined:?}",
    )?;
    expect(
        std::fs::read_to_string(&cfg)? == before,
        "the live config was untouched by the legacy checks",
    )?;

    step("K7. --set on a missing file is refused with the --init hint");
    let fresh = ws.root.join("cfgcmd-fresh.toml");
    let (ok, code, combined) =
        cli_try_cfg(bin, &fresh, &["config", "--set", "socket=/tmp/ak.sock"])?;
    expect(
        !ok && code == 2,
        &format!("set without a config exits 2, got {code}"),
    )?;
    expect(
        combined.contains("config --init"),
        "the error points at `ak config --init`",
    )?;
    expect(!fresh.exists(), "nothing was created by the refused set")?;

    step("K8. --delete removes the key, the default applies again");
    cli_cfg(bin, &cfg, &["config", "--delete", "socket"])?;
    let out = cli_cfg(bin, &cfg, &["config", "--get", "socket"])?;
    expect(
        out.trim() == format!("/run/appkeeper/{}/ak.sock", current_uid_e2e()),
        "the deleted key answers with the uid-derived default",
    )?;
    let text = std::fs::read_to_string(&cfg)?;
    expect(!text.contains("socket ="), "the socket line is gone")?;
    expect(
        text.contains("log_buffer_lines = 7 # count"),
        "untouched keys survive the delete",
    )?;

    step("K9. --delete of an absent key is an idempotent success");
    let before = std::fs::read_to_string(&cfg)?;
    let out = cli_cfg(bin, &cfg, &["config", "--delete", "socket"])?;
    expect(
        out.contains("no change"),
        "the output explains nothing changed",
    )?;
    expect(
        std::fs::read_to_string(&cfg)? == before,
        "the file was not rewritten",
    )?;

    step("K10. unknown keys/tables are refused");
    for bad in ["foo", "daemon.nope", "webui.theme", "webui.listen.x"] {
        let (ok, code, combined) = cli_try_cfg(bin, &cfg, &["config", "--delete", bad])?;
        expect(!ok && code == 2, &format!("--delete {bad} exits 2"))?;
        expect(
            combined.contains("unknown"),
            "the error says the key/table is unknown",
        )?;
    }
    expect(
        std::fs::read_to_string(&cfg)? == before,
        "rejected deletes leave the file unchanged",
    )?;

    // K16: webui keys ride the same engine (config-driven-webui).
    step("K16a. --set webui.listen writes a [webui] section; illegal listen is refused");
    let (ok, code, combined) =
        cli_try_cfg(bin, &cfg, &["config", "--set", "webui.listen=not-an-addr"])?;
    expect(!ok && code == 2, "an illegal listen exits 2 untouched")?;
    expect(
        combined.contains("webui.listen") && std::fs::read_to_string(&cfg)? == before,
        "the error names webui.listen and the file is byte-identical",
    )?;
    let out = cli_cfg(
        bin,
        &cfg,
        &["config", "--set", "webui.listen=127.0.0.1:19877"],
    )?;
    expect(out.contains("webui.listen"), "set reports the written key")?;
    let text = std::fs::read_to_string(&cfg)?;
    expect(
        text.contains("[webui]") && text.contains("listen = \"127.0.0.1:19877\""),
        "the [webui] section was created with the listen value",
    )?;

    step("K16b. --get webui.listen answers the file value; default when unset");
    let out = cli_cfg(bin, &cfg, &["config", "--get", "webui.listen"])?;
    expect(out.trim() == "127.0.0.1:19877", "get reads the section")?;
    let other = dir.join("daemon-empty.toml");
    std::fs::write(&other, "[daemon]\nlog_buffer_lines = 500\n")?;
    let out = cli_cfg(bin, &other, &["config", "--get", "webui.listen"])?;
    expect(
        out.trim() == "127.0.0.1:9877",
        "an absent section answers the built-in default",
    )?;

    step("K16c. --delete webui removes the whole section (console off)");
    let out = cli_cfg(bin, &cfg, &["config", "--delete", "webui"])?;
    let text = std::fs::read_to_string(&cfg)?;
    expect(!text.contains("[webui]"), "the section is gone")?;
    expect(out.contains("webui"), "delete reports the removed table")?;
    // Idempotent second delete of the absent table.
    let (ok, code, _) = cli_try_cfg(bin, &cfg, &["config", "--delete", "webui"])?;
    expect(
        ok && code == 0,
        "an absent table delete is an idempotent success",
    )?;

    step("K11. config --edit through a fake $EDITOR (valid + invalid)");
    if cfg!(unix) {
        let script = dir.join("fake-editor.sh");
        // Valid edit: the config passes validation afterwards.
        std::fs::write(
            &script,
            "#!/bin/sh\nprintf '[daemon]\\nsocket = \"/tmp/xk-e2e-edit.sock\"\\n' > \"$1\"\n",
        )?;
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755))?;
        let out = Command::new(bin)
            .env("EDITOR", &script)
            .arg("--config")
            .arg(&cfg)
            .args(["config", "--edit"])
            .output()
            .context("run config --edit (valid)")?;
        expect(out.status.success(), "a valid edit exits 0")?;
        let out = cli_cfg(bin, &cfg, &["config", "--get", "socket"])?;
        expect(
            out.trim() == "/tmp/xk-e2e-edit.sock",
            "the edited value is effective",
        )?;
        // Invalid edit: exit 2 and the file keeps the editor's content.
        std::fs::write(&script, "#!/bin/sh\nprintf 'bogus_field = 1\\n' > \"$1\"\n")?;
        let out = Command::new(bin)
            .env("EDITOR", &script)
            .arg("--config")
            .arg(&cfg)
            .args(["config", "--edit"])
            .output()
            .context("run config --edit (invalid)")?;
        expect(
            out.status.code() == Some(2),
            &format!(
                "an invalid edit result exits 2, got {:?}",
                out.status.code()
            ),
        )?;
        let text = std::fs::read_to_string(&cfg)?;
        expect(
            text.contains("bogus_field"),
            "the file keeps the editor's content (no rollback)",
        )?;
        // Restore a legal config for the following scenario.
        std::fs::write(&cfg, "[daemon]\nsocket = \"/tmp/xk-e2e-edit.sock\"\n")?;
    } else {
        println!("    (windows: the fake-EDITOR edit scenario requires unix; skipping)");
    }

    step("K12. example.toml.sample is not scanned as an app");
    let out = cli_cfg(bin, &cfg, &["list"])?;
    expect(
        out.contains("no apps registered"),
        "the init-produced app_dir lists no apps (.sample ignored)",
    )?;

    step("K13. bare `ak config` prints the five-action usage and exits 0");
    let out = cli_cfg(bin, &cfg, &["config"])?;
    for flag in ["--set", "--get", "--delete", "--edit", "--init"] {
        expect(
            out.contains(flag),
            &format!("the usage mentions {flag}: {out}"),
        )?;
    }

    step("K14. unknown --get key exits 2; --delete on a missing file points at --init");
    let (ok, code, combined) = cli_try_cfg(bin, &cfg, &["config", "--get", "foo"])?;
    expect(!ok && code == 2, &format!("--get foo exits 2, got {code}"))?;
    expect(
        combined.contains("unknown"),
        "the error names the key as unknown",
    )?;
    let missing = ws.root.join("cfgcmd-missing.toml");
    let (ok, code, combined) = cli_try_cfg(bin, &missing, &["config", "--delete", "socket"])?;
    expect(
        !ok && code == 2,
        &format!("delete without a config exits 2, got {code}"),
    )?;
    expect(
        combined.contains("config --init"),
        "the error points at `ak config --init`",
    )?;
    expect(
        !missing.exists(),
        "nothing was created by the refused delete",
    )?;

    step("K15. $VISUAL wins over $EDITOR for config --edit");
    if cfg!(unix) {
        let visual = dir.join("visual-editor.sh");
        let editor = dir.join("env-editor.sh");
        std::fs::write(
            &visual,
            "#!/bin/sh\nprintf '[daemon]\\nsocket = \"/tmp/xk-e2e-visual.sock\"\\n' > \"$1\"\n",
        )?;
        std::fs::write(
            &editor,
            "#!/bin/sh\nprintf '[daemon]\\nsocket = \"/tmp/xk-e2e-editor.sock\"\\n' > \"$1\"\n",
        )?;
        use std::os::unix::fs::PermissionsExt;
        for s in [&visual, &editor] {
            std::fs::set_permissions(s, std::fs::Permissions::from_mode(0o755))?;
        }
        let out = Command::new(bin)
            .env("VISUAL", &visual)
            .env("EDITOR", &editor)
            .arg("--config")
            .arg(&cfg)
            .args(["config", "--edit"])
            .output()
            .context("run config --edit with both $VISUAL and $EDITOR")?;
        expect(out.status.success(), "the edit through $VISUAL exits 0")?;
        let out = cli_cfg(bin, &cfg, &["config", "--get", "socket"])?;
        expect(
            out.trim() == "/tmp/xk-e2e-visual.sock",
            &format!(
                "$VISUAL won over $EDITOR (visual.sock, not editor.sock): {out:?}"
            ),
        )?;
    } else {
        println!("    (windows: the $VISUAL-precedence scenario requires unix; skipping)");
    }
    Ok(())
}

// -- CLI pass -------------------------------------------------------------------

fn cli_pass(bin: &Path, ws: &Workspace) -> Result<()> {
    // Sanity: both apps up.
    let web_pid = wait_running(ws, "web")?;
    let worker_pid = wait_running(ws, "worker")?;
    wait_running(ws, "job")?;

    step("E. no-change apply is idempotent (exit 0, no restarts)");
    let out = cli(bin, ws, &["apply"])?;
    expect(out.contains("no changes"), "empty apply reports no changes")?;
    expect(pid_of(ws, "web")? == Some(web_pid), "web was not touched")?;

    step("A. disk edit only enters pending (no process action)");
    std::fs::write(
        ws.root.join("app-alpha/appkeeper.toml"),
        "[app]\nrestart = \"always\"\n\n[program.web]\ncommand = \"sleep 100000\"\nstartsecs = 0.2\n\n[program.worker]\ncommand = \"sleep 200000\"\nstartsecs = 0.2\n",
    )?;
    // Periodic detection window (monitor_interval 0.5s).
    std::thread::sleep(Duration::from_secs(2));
    let v: serde_json::Value = serde_json::from_str(&api(ws, "/v1/pending")?)?;
    let progs = v
        .get("programs")
        .and_then(|p| p.as_array())
        .cloned()
        .unwrap_or_default();
    expect(
        progs.len() == 1 && progs[0].get("program").and_then(|p| p.as_str()) == Some("worker"),
        "periodic detect published worker as changed (no manual reload)",
    )?;
    expect(
        pid_of(ws, "worker")? == Some(worker_pid),
        "worker still runs the old definition",
    )?;
    let out = cli(bin, ws, &["reload"])?;
    expect(
        out.contains("worker") && out.contains("run `ak apply`"),
        "reload prints a preview, applies nothing",
    )?;
    expect(
        pid_of(ws, "worker")? == Some(worker_pid),
        "reload left worker alone",
    )?;

    step("B. scoped apply leaves other apps untouched");
    // Give beta a pending change too.
    std::fs::write(
        ws.root.join("app-beta/appkeeper.toml"),
        "[app]\nrestart = \"always\"\n\n[program.job]\ncommand = \"sleep 300000\"\nstartsecs = 0.2\n",
    )?;
    let job_pid = pid_of(ws, "job")?.expect("job running");
    std::thread::sleep(Duration::from_secs(2));
    let out = cli(bin, ws, &["apply", "alpha"])?;
    expect(
        out.contains("update-and-restart"),
        "alpha apply rebuilt+restarted the changed worker",
    )?;
    let new_worker = wait_running(ws, "worker")?;
    expect(new_worker != worker_pid, "worker restarted (pid changed)")?;
    expect(
        pid_of(ws, "job")? == Some(job_pid),
        "beta job untouched by alpha-scoped apply",
    )?;
    let v: serde_json::Value = serde_json::from_str(&api(ws, "/v1/pending")?)?;
    let progs = v
        .get("programs")
        .and_then(|p| p.as_array())
        .cloned()
        .unwrap_or_default();
    expect(
        progs.len() == 1 && progs[0].get("program").and_then(|p| p.as_str()) == Some("job"),
        "beta's pending survived the alpha apply",
    )?;

    step("B2. shell pending + scoped apply mirror the CLI with real pending");
    let out = cli(bin, ws, &["shell", "-e", "pending"])?;
    expect(
        out.contains("pending changes") && out.contains("job"),
        "shell pending lists the changed program",
    )?;
    let out = cli(bin, ws, &["shell", "-e", "apply beta"])?;
    expect(
        out.contains("update-and-restart") && out.contains("job"),
        "shell scoped apply reports the per-program result",
    )?;
    wait_running(ws, "job")?;

    step("C. --restart restarts unchanged programs but keeps user-stopped ones");
    cli(bin, ws, &["stop", "web"])?;
    let out = cli(bin, ws, &["apply", "--restart"])?;
    expect(
        out.contains("restarted (--restart)"),
        "--restart restarted unchanged programs",
    )?;
    expect(
        out.contains("keep-stopped"),
        "--restart reported the user-stopped program as kept",
    )?;
    let v: serde_json::Value = serde_json::from_str(&api(ws, "/v1/programs/web")?)?;
    expect(
        v.get("state").and_then(|s| s.as_str()) == Some("stopped"),
        "user-stopped web stayed stopped under --restart",
    )?;
    // job (unchanged, running) was restarted by --restart; catch its new pid.
    let job2 = pid_of(ws, "job")?;
    expect(
        job2.is_some() && job2 != Some(job_pid),
        "job was restarted by --restart",
    )?;

    step("D. online add enters pending and starts on apply");
    let gamma = ws.root.join("app-gamma");
    std::fs::create_dir_all(&gamma)?;
    std::fs::write(
        gamma.join("appkeeper.toml"),
        "[app]\nrestart = \"always\"\n\n[program.gamma]\ncommand = \"sleep 100000\"\nstartsecs = 0.2\n",
    )?;
    let out = cli(
        bin,
        ws,
        &["add", gamma.to_str().unwrap(), "--name", "gamma"],
    )?;
    expect(
        out.contains("registration is pending"),
        "add sync reports pending (not immediate start)",
    )?;
    expect(
        pid_of(ws, "gamma")?.is_none(),
        "gamma not started before apply",
    )?;
    let out = cli(bin, ws, &["apply"])?;
    expect(out.contains("gamma"), "apply picked the new app up")?;
    wait_running(ws, "gamma")?;

    step("shell: pending + apply single-command mode");
    let out = cli(bin, ws, &["shell", "-e", "pending"])?;
    expect(
        out.contains("no pending changes"),
        "shell pending is clean after full apply",
    )?;
    let out = cli(bin, ws, &["shell", "-e", "apply alpha"])?;
    expect(out.contains("no changes"), "shell scoped apply idempotent")?;

    scaffold_pass(bin, ws)?;

    // Re-sync disk with the running definitions so the browser pass starts
    // from a clean slate (worker currently runs sleep 200000 after step B).
    std::fs::write(
        ws.root.join("app-alpha/appkeeper.toml"),
        "[app]\nrestart = \"always\"\n\n[program.web]\ncommand = \"sleep 100000\"\nstartsecs = 0.2\n\n[program.worker]\ncommand = \"sleep 200000\"\nstartsecs = 0.2\n",
    )?;
    wait_pending_empty(ws)
}

/// Scenario G (add-process-scaffold): generate from an executable, apply,
/// regenerate, `--apply`, the `all` reserved word, and remove-file semantics.
fn scaffold_pass(bin: &Path, ws: &Workspace) -> Result<()> {
    let sleep = find_sleep()?;
    let apps = ws.root.join("apps");

    step("G1. add scaffold generates a real config with absolute paths");
    let out = cli(
        bin,
        ws,
        &[
            "add",
            sleep.to_str().unwrap(),
            "--name",
            "abc",
            "--args",
            "100000",
            "--env",
            "XK_E2E=1",
        ],
    )?;
    expect(out.contains("generated"), "add prints the generated file")?;
    expect(
        out.contains("pending"),
        "online sync reports the pending registration",
    )?;
    expect(
        out.contains("changed"),
        "first generation is announced as changed",
    )?;
    // Full non-interactive summary (app-registry: 注册与再生成功后完整打印).
    expect(
        out.contains("app[abc]") && out.contains("program[abc]"),
        "summary names the app and the program",
    )?;
    expect(
        out.contains("command:") && out.contains(sleep.to_string_lossy().as_ref()),
        "summary prints the absolute command",
    )?;
    expect(
        out.contains("args:") && out.contains("100000"),
        "summary prints the args",
    )?;
    expect(
        out.contains("env:") && out.contains("XK_E2E=1"),
        "summary prints the env",
    )?;
    expect(out.contains("work_dir:"), "summary prints the work_dir")?;
    let abc_file = apps.join("abc.toml");
    expect(abc_file.is_file(), "apps/abc.toml is a real file")?;
    let abc_text = std::fs::read_to_string(&abc_file)?;
    expect(
        abc_text.contains("XK_E2E"),
        "env written into the generated file",
    )?;
    // toml emits Windows paths as single-quoted literal strings; accept both
    // quote styles when extracting the value.
    let wd = abc_text
        .split("work_dir")
        .nth(1)
        .and_then(|rest| {
            let open = rest.find(['"', '\''])?;
            let rest = &rest[open + 1..];
            rest.find(['"', '\'']).map(|end| &rest[..end])
        })
        .unwrap_or_default();
    expect(
        wd.starts_with('/') || (wd.len() > 2 && wd.as_bytes()[1] == b':'),
        &format!("work_dir in the file is absolute: {wd:?}"),
    )?;
    // Round-trip through config load: the generated file must validate and
    // the command must have been written as an absolute path.
    let generated = cli(bin, ws, &["validate", &abc_file.to_string_lossy()])?;
    expect(
        generated.contains("abc") && generated.contains("program(s)"),
        "generated config passes validate",
    )?;
    expect(
        generated.contains(sleep.to_string_lossy().as_ref()),
        "validated command is the absolute executable path",
    )?;
    expect(pid_of(ws, "abc")?.is_none(), "abc not started before apply")?;
    // list scans app_dir and shows the scaffolded record like any other.
    let out = cli(bin, ws, &["list"])?;
    expect(out.contains("abc"), "list shows the scaffolded app name")?;
    expect(
        out.contains("abc.toml"),
        "list shows the scaffolded record path",
    )?;

    step("G2. apply abc starts the scaffolded program");
    let out = cli(bin, ws, &["apply", "abc"])?;
    expect(out.contains("abc"), "scoped apply picked up abc")?;
    let abc_pid = wait_running(ws, "abc")?;

    step("G3. identical re-add reports no change");
    let out = cli(
        bin,
        ws,
        &[
            "add",
            sleep.to_str().unwrap(),
            "--name",
            "abc",
            "--args",
            "100000",
            "--env",
            "XK_E2E=1",
        ],
    )?;
    expect(out.contains("no change"), "identical re-add is a no change")?;
    expect(
        pid_of(ws, "abc")? == Some(abc_pid),
        "no-change re-add does not touch the running program",
    )?;

    step("G4. changed re-add, a second pending app, then `apply all` takes both");
    let out = cli(
        bin,
        ws,
        &[
            "add",
            sleep.to_str().unwrap(),
            "--name",
            "abc",
            "--args",
            "200000",
        ],
    )?;
    expect(
        out.contains("changed") && out.contains("ak apply abc"),
        "changed re-add points at the remedy",
    )?;
    let out = cli(
        bin,
        ws,
        &[
            "add",
            sleep.to_str().unwrap(),
            "--name",
            "fox",
            "--args",
            "100000",
        ],
    )?;
    expect(out.contains("changed"), "fox first generation is changed")?;
    let out = cli(bin, ws, &["apply", "all"])?;
    expect(out.contains("abc"), "`apply all` applied abc's change")?;
    expect(
        out.contains("fox"),
        "`apply all` also applied the other pending app (full-registry scope)",
    )?;
    let abc_pid2 = wait_running(ws, "abc")?;
    expect(abc_pid2 != abc_pid, "abc restarted with the new args")?;
    wait_running(ws, "fox")?;

    step("G5. add --apply is one step and app-scoped (other pending survives)");
    // Give alpha a pending change; delta's app-scoped --apply must not touch it.
    let worker_before = pid_of(ws, "worker")?;
    std::fs::write(
        ws.root.join("app-alpha/appkeeper.toml"),
        "[app]\nrestart = \"always\"\n\n[program.web]\ncommand = \"sleep 100000\"\nstartsecs = 0.2\n\n[program.worker]\ncommand = \"sleep 300000\"\nstartsecs = 0.2\n",
    )?;
    cli(bin, ws, &["reload"])?; // publish the pending preview
    let out = cli(
        bin,
        ws,
        &["add", sleep.to_str().unwrap(), "--name", "delta", "--apply"],
    )?;
    expect(out.contains("generated"), "delta scaffold generated")?;
    expect(out.contains("-> start"), "apply result shows delta started")?;
    wait_running(ws, "delta")?;
    let v: serde_json::Value = serde_json::from_str(&api(ws, "/v1/pending")?)?;
    let progs = v
        .get("programs")
        .and_then(|p| p.as_array())
        .cloned()
        .unwrap_or_default();
    expect(
        progs
            .iter()
            .any(|p| p.get("program").and_then(|x| x.as_str()) == Some("worker")),
        "alpha's pending survived delta's app-scoped --apply",
    )?;
    expect(
        pid_of(ws, "worker")? == worker_before && worker_before.is_some(),
        "delta's --apply did not restart alpha's worker",
    )?;
    // Restore alpha's disk to the applied content so no pending leaks on.
    std::fs::write(
        ws.root.join("app-alpha/appkeeper.toml"),
        "[app]\nrestart = \"always\"\n\n[program.web]\ncommand = \"sleep 100000\"\nstartsecs = 0.2\n\n[program.worker]\ncommand = \"sleep 200000\"\nstartsecs = 0.2\n",
    )?;
    cli(bin, ws, &["reload"])?;
    wait_pending_empty(ws)?;

    step("G6. `--name all` is rejected (reserved apply-all keyword)");
    let (ok, _code, combined) =
        cli_try(bin, ws, &["add", sleep.to_str().unwrap(), "--name", "all"])?;
    expect(!ok, "add --name all fails")?;
    expect(
        combined.contains("reserved"),
        "error explains the reserved word",
    )?;

    step("G7. remove deletes the generated file, keeps external bodies");
    let out = cli(bin, ws, &["remove", "abc"])?;
    expect(
        out.contains("generated config file removed"),
        "remove announces the deleted file",
    )?;
    expect(!abc_file.exists(), "apps/abc.toml is gone")?;
    let out = cli(bin, ws, &["remove", "fox"])?;
    expect(
        out.contains("generated config file removed"),
        "fox's generated file is deleted too",
    )?;
    expect(!apps.join("fox.toml").exists(), "apps/fox.toml is gone")?;
    // A link registration (gamma) is removed without touching the external
    // deployment file — whatever the message says, the body must survive
    // (on Windows the record is a hard link, so the message differs).
    let out = cli(bin, ws, &["remove", "gamma"])?;
    expect(out.contains("unregistered"), "gamma unregistered")?;
    expect(
        ws.root.join("app-gamma/appkeeper.toml").is_file(),
        "gamma's appkeeper.toml still exists",
    )?;
    // Apply the removals: the stopped programs disappear from the status.
    cli(bin, ws, &["apply"])?;
    expect(
        pid_of(ws, "abc")?.is_none(),
        "abc's program stopped and gone",
    )?;
    expect(
        pid_of(ws, "fox")?.is_none(),
        "fox's program stopped and gone",
    )?;
    Ok(())
}

// -- cli-revamp scenario group -------------------------------------------------

/// cli-revamp acceptance: the unified `run` verb (start / ad-hoc), `log`
/// multi-program aggregation, `status` table vs `--json`, the four `restart`
/// policies with the persisted user-stop record, and the legacy-alias rules.
/// Self-contained: it runs in its own workspace with its own daemon (restarting
/// it mid-pass is part of the restart-policy scenarios).
fn revamp_pass(bin: &Path) -> Result<()> {
    let ws = setup_workspace("revamp")?;
    let mut daemon = spawn_daemon(bin, &ws)?;
    let result = revamp_scenarios(bin, &ws, &mut daemon);
    shutdown_daemon(&ws, &mut daemon);
    let _ = std::fs::remove_dir_all(&ws.root);
    result
}

/// The scenarios run against `daemon`, which they may restart in place (the
/// live handle is kept in `daemon` so `revamp_pass` always shuts it down).
fn revamp_scenarios(bin: &Path, ws: &Workspace, daemon: &mut Daemon) -> Result<()> {
    step("R1. `start` is gone (BREAKING) with a `run` hint; `run <name>` starts");
    let (ok, _code, combined) = cli_try(bin, ws, &["start", "web"])?;
    expect(!ok, "`ak start web` is rejected")?;
    expect(
        combined.contains("start") && combined.contains("run"),
        &format!("the error names the removed verb and the replacement: {combined:?}"),
    )?;
    // `run <name>` behaves like the old start: stop then start.
    cli(bin, ws, &["stop", "web"])?;
    wait_state_in(ws, "web", &["stopped"])?;
    let out = cli(bin, ws, &["run", "web"])?;
    expect(
        out.contains("web"),
        &format!("run <name> reports the program: {out:?}"),
    )?;
    wait_running(ws, "web")?;

    step("R2. `run --app` fans out; an unknown program is a 404 → exit 1");
    let out = cli(bin, ws, &["run", "--app", "beta"])?;
    expect(
        out.contains("job"),
        &format!("run --app reports the app's programs: {out:?}"),
    )?;
    let (ok, code, combined) = cli_try(bin, ws, &["run", "nosuch"])?;
    expect(
        !ok && code == 1,
        &format!("unknown program exits 1, got {code}"),
    )?;
    expect(
        combined.contains("404") || combined.contains("unknown program"),
        "the error carries the 404/unknown-program message",
    )?;
    expect(
        combined.contains("run -- <cmd...>"),
        "the 404 points at the ad-hoc `run --` registration form",
    )?;

    step("R3. run ad-hoc cold registration + `--cmd`/`--` equivalence + regenerate");
    let sleep = find_sleep()?;
    let sleep_s = sleep.to_str().unwrap();
    let out = cli(bin, ws, &["run", "--", sleep_s, "123", "--name", "adhoc-a"])?;
    expect(
        out.contains("started") && out.contains("config:") && out.contains("pid:"),
        &format!("run -- registers and reports {{program, config, pid}}: {out:?}"),
    )?;
    expect(
        out.contains("adhoc-a"),
        &format!("the report names the new program: {out:?}"),
    )?;
    let a_cfg = ws.root.join("apps/adhoc-a.toml");
    expect(a_cfg.is_file(), "adhoc-a.toml generated")?;
    let a_text = std::fs::read_to_string(&a_cfg)?;
    expect(
        a_text.contains("command") && a_text.contains("args"),
        "the generated config names the command and args",
    )?;
    // `--cmd` produces an identical command/args/name.
    let out = cli(
        bin,
        ws,
        &[
            "run",
            "--cmd",
            &format!("{sleep_s} 123"),
            "--name",
            "adhoc-b",
        ],
    )?;
    expect(
        out.contains("started"),
        &format!("--cmd registers too: {out:?}"),
    )?;
    let b_text = std::fs::read_to_string(ws.root.join("apps/adhoc-b.toml"))?;
    expect(
        a_text.replace("adhoc-a", "adhoc-X") == b_text.replace("adhoc-b", "adhoc-X"),
        "`--` and `--cmd` produce the same registration modulo the name",
    )?;
    // Re-running the SAME name is a full regeneration (app-registry: 同名
    // ad-hoc 全量再生, immediate effect): the whole file is rewritten from
    // this command line. Identical content is an idempotent success returning
    // the existing pid with no restart.
    let pid_before = pid_of(ws, "adhoc-a")?.expect("adhoc-a is running");
    let out = cli(bin, ws, &["run", "--", sleep_s, "123", "--name", "adhoc-a"])?;
    expect(
        out.contains("started") && out.contains(&pid_before.to_string()),
        &format!("identical re-run is idempotent (same pid {pid_before}): {out:?}"),
    )?;
    expect(
        pid_of(ws, "adhoc-a")? == Some(pid_before),
        "the identical re-run did not restart the process",
    )?;
    // Changed args → regenerated and restarted (new pid).
    let out = cli(bin, ws, &["run", "--", sleep_s, "456", "--name", "adhoc-a"])?;
    expect(
        out.contains("started"),
        &format!("the changed re-run succeeds: {out:?}"),
    )?;
    let a_text2 = std::fs::read_to_string(&a_cfg)?;
    expect(
        a_text2.contains("456") && !a_text2.contains("123"),
        "re-running regenerates the whole file from the new command line",
    )?;
    let pid_after = pid_of(ws, "adhoc-a")?.expect("adhoc-a still running");
    expect(
        pid_after != pid_before,
        "the changed definition restarted the process (new pid)",
    )?;
    // Command resolution failure exits 2 with no artifact.
    let (ok, code, combined) =
        cli_try(bin, ws, &["run", "--", "definitely-not-a-real-cmd", "--x"])?;
    expect(
        !ok && code == 2,
        &format!("an unresolvable command exits 2, got {code}: {combined}"),
    )?;
    expect(
        !ws.root.join("apps/definitely-not-a-real-cmd.toml").exists(),
        "a failed resolution leaves no registration",
    )?;
    // `--restart=no` registers without pulling up.
    cli(
        bin,
        ws,
        &[
            "run",
            "--",
            sleep_s,
            "789",
            "--name",
            "adhoc-quiet",
            "--restart=no",
        ],
    )?;
    let quiet = std::fs::read_to_string(ws.root.join("apps/adhoc-quiet.toml"))?;
    expect(
        quiet.contains("restart = \"no\""),
        "the restart literal is written straight into the config",
    )?;
    expect(
        pid_of(ws, "adhoc-quiet")?.is_none(),
        "--restart=no registers but does not start",
    )?;
    // Reserved `all` is refused.
    let (ok, _code, combined) = cli_try(bin, ws, &["run", "--", sleep_s, "--name", "all"])?;
    expect(!ok, "run --name all is rejected")?;
    expect(
        combined.contains("reserved"),
        "the reserved-word error explains",
    )?;

    step("R4. restart = no stays down; always pull-up after a daemon restart");
    // adhoc-quiet (restart = no) stays stopped across a daemon restart, while
    // a user-stopped `always` program is pulled back up.
    cli(bin, ws, &["stop", "job"])?;
    wait_state_in(ws, "job", &["stopped"])?;
    shutdown_and_wait(bin, ws)?;
    *daemon = spawn_daemon(bin, ws)?;
    wait_running(ws, "job")?;
    expect(
        pid_of(ws, "adhoc-quiet")?.is_none(),
        "restart = no stays stopped across the daemon restart",
    )?;

    step("R5. unless-stopped honors the persisted user-stop record across restart");
    cli(
        bin,
        ws,
        &[
            "run",
            "--cmd",
            &format!("{sleep_s} 999"),
            "--name",
            "adhoc-us",
            "--restart=unless-stopped",
        ],
    )?;
    wait_running(ws, "adhoc-us")?;
    cli(bin, ws, &["stop", "adhoc-us"])?;
    let state_file = ws.root.join("logs/state.json");
    let state_text = std::fs::read_to_string(&state_file).unwrap_or_default();
    expect(
        state_text.contains("adhoc-us"),
        &format!("the user-stop record was persisted: {state_text:?}"),
    )?;
    shutdown_and_wait(bin, ws)?;
    // A plain shutdown must NOT add a record for a program it stops: `job`
    // (restart = always) is pulled back up, proving shutdown wrote nothing.
    *daemon = spawn_daemon(bin, ws)?;
    expect(
        pid_of(ws, "adhoc-us")?.is_none(),
        "unless-stopped stays stopped after the daemon restart (record kept)",
    )?;
    wait_running(ws, "job")?;
    // An explicit start clears the record.
    cli(bin, ws, &["run", "adhoc-us"])?;
    wait_running(ws, "adhoc-us")?;
    let state_text = std::fs::read_to_string(&state_file).unwrap_or_default();
    expect(
        !state_text.contains("adhoc-us"),
        &format!("an explicit start cleared the record: {state_text:?}"),
    )?;

    step("R6. legacy field aliases read; same-layer mixing is rejected");
    let alias_dir = ws.root.join("alias-app");
    std::fs::create_dir_all(&alias_dir)?;
    let alias_file = alias_dir.join("appkeeper.toml");
    std::fs::write(
        &alias_file,
        "[app]\nautostart = true\nautorestart = \"on-failure\"\n[program.p]\ncommand = 'sleep 100'\n",
    )?;
    let out = cli(bin, ws, &["validate", &alias_file.to_string_lossy()])?;
    expect(out.contains("OK"), "legacy aliases still validate: {out}")?;
    std::fs::write(
        &alias_file,
        "[program.p]\ncommand = 'sleep 100'\nrestart = 'always'\nautorestart = 'on-failure'\n",
    )?;
    let (ok, _code, combined) = cli_try(bin, ws, &["validate", &alias_file.to_string_lossy()])?;
    expect(
        !ok,
        "mixing restart and autorestart in one layer is rejected",
    )?;
    expect(
        combined.contains("mixed") || combined.contains("cannot be mixed"),
        &format!("the error explains the mixing: {combined:?}"),
    )?;

    step("R7. status table by default; --json keeps the projection");
    let table = cli(bin, ws, &["status"])?;
    expect(
        ["NAME", "APP", "STATE", "PID", "RESTARTS", "UNHEALTHY"]
            .iter()
            .all(|h| table.contains(h)),
        &format!("status default prints the table header: {table:?}"),
    )?;
    expect(
        !table.contains('{') && !table.contains('}'),
        "the default output is not JSON",
    )?;
    let json = cli(bin, ws, &["status", "--json"])?;
    expect(
        json.trim_start().starts_with('{'),
        "status --json is pretty JSON",
    )?;
    expect(
        json.contains("\"programs\"") && json.contains("\"daemon\""),
        "the JSON carries the daemon + programs projection",
    )?;

    step("R8. log multi-program aggregation: prefixes, both streams, 404 exit 1");
    if cfg!(unix) {
        // Two programs that actually emit output (the `sleep` apps above are
        // silent). `sh` is unix-only.
        let echo_dir = ws.root.join("echo-app");
        std::fs::create_dir_all(&echo_dir)?;
        let echo_file = echo_dir.join("appkeeper.toml");
        std::fs::write(
            &echo_file,
            "[app]\nrestart = \"always\"\n\n[program.echo-a]\ncommand = 'sh'\nargs = ['-c', 'while true; do echo a-line; sleep 0.2; done']\nstartsecs = 0.2\n\n[program.echo-b]\ncommand = 'sh'\nargs = ['-c', 'while true; do echo b-line; sleep 0.2; done']\nstartsecs = 0.2\n",
        )?;
        cli(
            bin,
            ws,
            &["add", echo_file.to_str().unwrap(), "--name", "echo"],
        )?;
        cli(bin, ws, &["apply", "echo"])?;
        wait_running(ws, "echo-a")?;
        wait_running(ws, "echo-b")?;
        // Give the ring buffer a moment to accumulate a line.
        std::thread::sleep(Duration::from_millis(600));
        let multi = cli(bin, ws, &["log", "echo-a", "echo-b", "--tail", "1"])?;
        expect(
            multi.contains("[echo-a]") && multi.contains("[echo-b]"),
            &format!("multi-program log prefixes each line: {multi:?}"),
        )?;
        let single = cli(bin, ws, &["log", "echo-a", "--tail", "1"])?;
        expect(
            !single.contains("[echo-a]"),
            &format!("single-program log adds no prefix: {single:?}"),
        )?;
        // `--stream both` is accepted (both streams visible).
        let _ = cli(
            bin,
            ws,
            &["log", "echo-a", "--stream", "both", "--tail", "1"],
        )?;
    } else {
        println!("    (windows: the multi-program log scenario needs sh; skipping)");
    }
    let (ok, code, _combined) = cli_try(bin, ws, &["log", "ghost", "--tail", "1"])?;
    expect(
        !ok && code == 1,
        &format!("logging an unknown program exits 1, got {code}"),
    )?;

    let _ = &mut *daemon;
    Ok(())
}

// -- run-daemon-first scenario group ------------------------------------------

/// run-daemon-first acceptance (tasks.md §8): its own workspace with an EMPTY
/// `app_dir` so every residue assertion ("no `.toml`, no `.toml.pending`") is
/// exact. The offline half runs before the daemon exists; the rest drives a
/// daemon that this pass owns.
fn run_daemon_first_pass(bin: &Path) -> Result<()> {
    let ws = setup_runfirst_workspace()?;
    let result = runfirst_scenarios(bin, &ws);
    let _ = std::fs::remove_dir_all(&ws.root);
    result
}

/// A minimal isolated workspace: empty `apps/`, its own control + webui ports.
/// No `[app-default]` section, so the built-in `startsecs` default (3.0) is
/// exactly what the ad-hoc startup window uses.
fn setup_runfirst_workspace() -> Result<Workspace> {
    let root = std::env::temp_dir().join(format!("xk-xtask-e2e-runfirst-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    for d in ["apps", "logs", "fan-app", "slow-app", "mixed-app"] {
        std::fs::create_dir_all(root.join(d)).context("create runfirst dirs")?;
    }
    let control_socket = control_socket_in(&root);
    let webui_port = free_port()?;
    let log_dir = root.join("logs").to_string_lossy().replace('\\', "/");
    std::fs::write(
        root.join("daemon.toml"),
        format!(
            "[daemon]\nsocket = {socket}\nlog_dir = \"{log_dir}\"\nlog_level = \"warn\"\nmonitor_interval = 0.5\napp_dir = \"apps\"\n\n[webui]\nlisten = \"127.0.0.1:{webui_port}\"\n",
            socket = socket_toml_value(&control_socket),
        ),
    )?;
    Ok(Workspace {
        root,
        control_socket,
        webui_port,
    })
}

/// The app_dir entries (sorted) — the raw material for residue assertions.
fn app_dir_files(ws: &Workspace) -> Vec<String> {
    let dir = ws.root.join("apps");
    let mut out: Vec<String> = std::fs::read_dir(&dir)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    out.sort();
    out
}

/// Assert no residue for `name`: neither the record nor its placeholder, and
/// no stray `.toml.pending` anywhere in `app_dir` (app-registry spec: 失败
/// 不留残留).
fn expect_no_residue(ws: &Workspace, name: &str) -> Result<()> {
    let dir = ws.root.join("apps");
    expect(
        !dir.join(format!("{name}.toml")).exists(),
        &format!("no {name}.toml residue"),
    )?;
    expect(
        !dir.join(format!("{name}.toml.pending")).exists(),
        &format!("no {name}.toml.pending residue"),
    )?;
    let stray: Vec<String> = app_dir_files(ws)
        .into_iter()
        .filter(|f| f.ends_with(".toml.pending"))
        .collect();
    expect(
        stray.is_empty(),
        &format!("no placeholder residue at all: {stray:?}"),
    )
}

/// POST a JSON body to a control-plane path and return `(status, raw body)`.
/// `-N` disables curl buffering so a chunked NDJSON stream arrives as-is.
fn post_ndjson(ws: &Workspace, path: &str, body: &serde_json::Value) -> Result<(u16, String)> {
    post_ndjson_at(&ws.control_socket, path, body)
}

/// [`post_ndjson`] against an explicit endpoint (secondary workspaces).
fn post_ndjson_at(
    endpoint: &Path,
    path: &str,
    body: &serde_json::Value,
) -> Result<(u16, String)> {
    let out = curl_socket(
        endpoint,
        &[
            "-N",
            "-o",
            "-",
            "-w",
            "\n%{http_code}",
            "-X",
            "POST",
            "-H",
            "Content-Type: application/json",
            "-d",
            &body.to_string(),
        ],
        path,
    )
    .output()
    .context("curl the ndjson endpoint")?;
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    let (body, code) = text.rsplit_once('\n').unwrap_or((&text, "0"));
    Ok((code.trim().parse().unwrap_or(0), body.to_string()))
}

/// Spawn the daemon against an explicit config, waiting for control-socket
/// health. The endpoint the config pins is passed separately for polling.
fn spawn_daemon_at(bin: &Path, cfg: &Path, endpoint: &Path) -> Result<Daemon> {
    let out = std::fs::File::create(cfg.with_extension("out.log"))?;
    let err = out.try_clone()?;
    let mut child = Command::new(bin)
        .arg("--config")
        .arg(cfg)
        .arg("daemon")
        .stdout(Stdio::from(out))
        .stderr(Stdio::from(err))
        .spawn()
        .context("spawn secondary daemon")?;
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if Instant::now() > deadline {
            let _ = child.kill();
            bail!("secondary daemon did not become healthy in 20s");
        }
        if let Ok(Some(_)) = child.try_wait() {
            bail!("secondary daemon exited during startup");
        }
        if socket_health_ok(endpoint) {
            break;
        }
        std::thread::sleep(Duration::from_millis(150));
    }
    Ok(Daemon { child })
}

/// pid of a program on an explicit control endpoint.
fn pid_of_at(endpoint: &Path, program: &str) -> Result<Option<u32>> {
    let (code, body) = socket_get(endpoint, &format!("/v1/programs/{program}"))?;
    if code == 404 {
        return Ok(None);
    }
    if !(200..300).contains(&code) {
        bail!("GET {program} -> {code}: {body}");
    }
    let v: serde_json::Value = serde_json::from_str(&body)?;
    Ok(v.get("pid").and_then(|p| p.as_u64()).map(|p| p as u32))
}

/// Shut down a daemon on an explicit control endpoint and reap it.
fn shutdown_daemon_at(endpoint: &Path, daemon: &mut Daemon) {
    let _ = curl_socket(endpoint, &["-m", "10", "-X", "POST"], "/v1/shutdown").output();
    for _ in 0..60 {
        if matches!(daemon.child.try_wait(), Ok(Some(_))) {
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let _ = daemon.child.kill();
    let _ = daemon.child.wait();
}

/// Parse the terminal (last non-empty) NDJSON frame of a streamed body.
fn last_ndjson_frame(body: &str) -> Option<serde_json::Value> {
    body.lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .next_back()
}

/// Run the daemon-first scenarios: the offline half (8.4) first, then a daemon
/// this pass spawns and shuts down itself.
fn runfirst_scenarios(bin: &Path, ws: &Workspace) -> Result<()> {
    runfirst_offline(bin, ws)?;
    let mut daemon = spawn_daemon(bin, ws)?;
    let result = runfirst_online(bin, ws, &mut daemon);
    shutdown_daemon(ws, &mut daemon);
    result
}

/// 8.4: with the daemon down, every `run` form exits 3, derives no daemon and
/// leaves `app_dir` untouched (background-daemon / control-plane spec).
fn runfirst_offline(bin: &Path, ws: &Workspace) -> Result<()> {
    let sleep = find_sleep()?;
    let sleep_s = sleep.to_str().unwrap();
    let before = app_dir_files(ws);

    step("RDF0. offline `run` forms exit 3 and derive no daemon (background-daemon)");
    expect(
        !socket_health_ok(&ws.control_socket),
        "the runfirst workspace starts with no daemon",
    )?;
    let cmd_form = format!("{sleep_s} 1");
    let forms: [&[&str]; 4] = [
        &["run", "web"],
        &["run", "--", sleep_s, "--name", "cold"],
        &["run", "--cmd", &cmd_form, "--name", "cold2"],
        &["run", "--app", "ghost"],
    ];
    for args in forms {
        let (ok, code, combined) = cli_try(bin, ws, args)?;
        expect(
            !ok && code == 3,
            &format!("offline {args:?} exits 3, got {code}: {combined:?}"),
        )?;
        expect(
            combined.contains("unreachable"),
            &format!("{args:?} reports the daemon unreachable"),
        )?;
    }
    expect(
        !socket_health_ok(&ws.control_socket),
        "no daemon was derived in the background",
    )?;
    expect(
        app_dir_files(ws) == before,
        &format!("app_dir is unchanged: {:?}", app_dir_files(ws)),
    )?;
    Ok(())
}

/// The online half of the daemon-first acceptance (§8.1–8.3, §8.5–8.6).
fn runfirst_online(bin: &Path, ws: &Workspace, daemon: &mut Daemon) -> Result<()> {
    let sleep = find_sleep()?;
    let sleep_s = sleep.to_str().unwrap();
    let apps = ws.root.join("apps");

    // -- 8.1: run <name> / run --app / run -- --json / blocking -------------

    step("RDF1. ad-hoc `run --` registers and returns {program, config, pid}");
    let out = cli(bin, ws, &["run", "--", sleep_s, "300", "--name", "web"])?;
    expect(
        out.contains("web") && out.contains("apps/web.toml") && out.contains("pid:"),
        &format!("run -- reports program/config/pid: {out:?}"),
    )?;
    expect(
        apps.join("web.toml").is_file(),
        "the registration landed in app_dir",
    )?;
    let web_pid = wait_running(ws, "web")?;
    expect(
        out.contains(&web_pid.to_string()),
        &format!("the reported pid is the live one ({web_pid})"),
    )?;

    step("RDF2. `run <name>` returns name/config/pid for a stopped registered program");
    cli(bin, ws, &["stop", "web"])?;
    wait_state_in(ws, "web", &["stopped"])?;
    let out = cli(bin, ws, &["run", "web"])?;
    expect(
        out.contains("web") && out.contains("apps/web.toml") && out.contains("pid="),
        &format!("run <name> renders the three fields: {out:?}"),
    )?;
    let web2 = wait_running(ws, "web")?;
    expect(
        out.contains(&web2.to_string()),
        &format!("the pid in the output is the live one ({web2})"),
    )?;

    step("RDF3. `run <name> --json` is a machine-readable single object");
    cli(bin, ws, &["stop", "web"])?;
    wait_state_in(ws, "web", &["stopped"])?;
    let out = cli(bin, ws, &["run", "web", "--json"])?;
    let v: serde_json::Value = serde_json::from_str(out.trim()).context("run --json parses")?;
    expect(
        v.get("name").and_then(|n| n.as_str()) == Some("web"),
        &format!("json carries name: {out:?}"),
    )?;
    expect(
        v.get("config_path")
            .and_then(|c| c.as_str())
            .map(|c| c.ends_with("web.toml"))
            .unwrap_or(false),
        &format!("json carries config_path: {out:?}"),
    )?;
    expect(
        v.get("pid").and_then(|p| p.as_u64()).is_some(),
        &format!("json carries a numeric pid: {out:?}"),
    )?;
    expect(
        v.get("status").and_then(|s| s.as_str()) == Some("started"),
        &format!("json carries status: {out:?}"),
    )?;

    step("RDF4. ad-hoc `run --json -- <cmd>` is the same single object");
    let out = cli(
        bin,
        ws,
        &["run", "--json", "--", sleep_s, "301", "--name", "json-web"],
    )?;
    let v: serde_json::Value = serde_json::from_str(out.trim()).context("adhoc --json parses")?;
    expect(
        v.get("name").and_then(|n| n.as_str()) == Some("json-web")
            && v.get("pid").and_then(|p| p.as_u64()).is_some(),
        &format!("adhoc --json is a single object with name/pid: {out:?}"),
    )?;

    step("RDF5. `run --app` fans out one line per program; `--json` is an array");
    std::fs::write(
        ws.root.join("fan-app/appkeeper.toml"),
        format!(
            "[app]\nrestart = \"always\"\n\n[program.fa]\ncommand = \"{sleep_s}\"\nargs = [\"300\"]\nstartsecs = 0.2\n\n[program.fb]\ncommand = \"{sleep_s}\"\nargs = [\"300\"]\nstartsecs = 0.2\n"
        ),
    )?;
    cli(
        bin,
        ws,
        &[
            "add",
            ws.root.join("fan-app").to_str().unwrap(),
            "--name",
            "fan",
        ],
    )?;
    cli(bin, ws, &["apply", "fan"])?;
    wait_running(ws, "fa")?;
    wait_running(ws, "fb")?;
    let out = cli(bin, ws, &["run", "--app", "fan"])?;
    let (pa, pb) = (out.find("fa "), out.find("fb "));
    expect(
        matches!((pa, pb), (Some(a), Some(b)) if a < b),
        &format!("fan-out lists fa then fb, one line each: {out:?}"),
    )?;
    expect(
        out.contains("config=") && out.contains("pid="),
        &format!("each fan-out line carries config and pid: {out:?}"),
    )?;
    expect(
        out.contains("appkeeper.toml"),
        &format!("the config_path names the app's config body: {out:?}"),
    )?;
    let json = cli(bin, ws, &["run", "--app", "fan", "--json"])?;
    let arr: serde_json::Value =
        serde_json::from_str(json.trim()).context("fan-out json parses")?;
    let arr = arr.as_array().context("fan-out --json is an array")?;
    expect(arr.len() == 2, &format!("two entries, got {}", arr.len()))?;
    for p in arr {
        for f in ["name", "config_path", "pid", "status"] {
            expect(p.get(f).is_some(), &format!("fan-out json has {f}: {p}"))?;
        }
    }

    step("RDF5b. a partial fan-out failure exits non-zero and lists the failing program");
    // `bad` exits before its (short) startsecs, so the `run --app` wait sees it
    // as not-running and must report the app as failed.
    std::fs::write(
        ws.root.join("fan-app/appkeeper.toml"),
        format!(
            "[app]\nrestart = \"always\"\n\n[program.fa]\ncommand = \"{sleep_s}\"\nargs = [\"300\"]\nstartsecs = 0.2\n\n[program.fb]\ncommand = \"{sleep_s}\"\nargs = [\"300\"]\nstartsecs = 0.2\n"
        ),
    )?;
    std::fs::write(
        ws.root.join("mixed-app/appkeeper.toml"),
        format!(
            "[app]\nrestart = \"no\"\n\n[program.ok]\ncommand = \"{sleep_s}\"\nargs = [\"300\"]\nstartsecs = 0.2\n\n[program.bad]\ncommand = \"{sleep_s}\"\nargs = [\"0.05\"]\nstartsecs = 2.0\n"
        ),
    )?;
    cli(
        bin,
        ws,
        &[
            "add",
            ws.root.join("mixed-app").to_str().unwrap(),
            "--name",
            "mixed",
        ],
    )?;
    cli(bin, ws, &["apply", "mixed"])?;
    let (ok, code, combined) = cli_try(bin, ws, &["run", "--app", "mixed"])?;
    expect(
        !ok && code != 0,
        &format!("a partial fan-out failure exits non-zero, got {code}: {combined:?}"),
    )?;
    expect(
        combined.contains("bad"),
        &format!("the failure lists the failing program: {combined:?}"),
    )?;
    expect(
        combined.contains("ok"),
        &format!("the successful program is still reported: {combined:?}"),
    )?;

    step("RDF6. a single-program `run` blocks until the startup judgement returns");
    std::fs::write(
        ws.root.join("slow-app/appkeeper.toml"),
        format!(
            "[app]\nrestart = \"no\"\n\n[program.slow]\ncommand = \"{sleep_s}\"\nargs = [\"300\"]\nstartsecs = 1.5\n"
        ),
    )?;
    cli(
        bin,
        ws,
        &[
            "add",
            ws.root.join("slow-app").to_str().unwrap(),
            "--name",
            "slow",
        ],
    )?;
    cli(bin, ws, &["apply", "slow"])?;
    // `restart = "no"`: the app is registered but not started, so the `run`
    // below is the first start and its 1.5s startsecs is a pure wait.
    expect(
        pid_of(ws, "slow")?.is_none(),
        "slow is registered but stopped",
    )?;
    let t0 = Instant::now();
    let out = cli(bin, ws, &["run", "slow"])?;
    let elapsed = t0.elapsed();
    expect(
        elapsed >= Duration::from_millis(1400),
        &format!("run blocked for the 1.5s startsecs (took {elapsed:?})"),
    )?;
    expect(
        out.contains("pid=") && out.contains("slow"),
        &format!("the blocked run still reports the result: {out:?}"),
    )?;

    // -- 8.2: failures leave no residue -------------------------------------

    step("RDF7. a command that cannot be resolved exits 2 with no residue");
    let (ok, code, combined) =
        cli_try(bin, ws, &["run", "--", "definitely-not-a-real-cmd", "--x"])?;
    expect(
        !ok && code == 2,
        &format!("unresolvable command exits 2, got {code}: {combined:?}"),
    )?;
    expect_no_residue(ws, "definitely-not-a-real-cmd")?;

    step("RDF8. a program that exits before startsecs fails with no residue");
    let (ok, code, combined) =
        cli_try(bin, ws, &["run", "--", sleep_s, "0.05", "--name", "flaky"])?;
    expect(!ok, &format!("early-exit ad-hoc fails: {combined:?}"))?;
    expect(code != 0, &format!("non-zero exit, got {code}"))?;
    expect(
        combined.contains("startsecs") || combined.contains("startup"),
        &format!("the error names the startup failure: {combined:?}"),
    )?;
    expect_no_residue(ws, "flaky")?;

    step("RDF9. a cross-app program-name collision is refused with no residue");
    // `owner` is a separate app that owns the program name `clash`; an ad-hoc
    // `--name clash` collides with ANOTHER app and must be refused
    // (control-plane spec: 名称冲突被拒绝 — 与其他应用冲突).
    std::fs::create_dir_all(ws.root.join("owner-app"))?;
    std::fs::write(
        ws.root.join("owner-app/appkeeper.toml"),
        format!("[program.clash]\ncommand = \"{sleep_s}\"\nargs = [\"300\"]\nstartsecs = 0.2\n"),
    )?;
    cli(
        bin,
        ws,
        &[
            "add",
            ws.root.join("owner-app").to_str().unwrap(),
            "--name",
            "owner",
        ],
    )?;
    cli(bin, ws, &["apply", "owner"])?;
    let (ok, code, combined) = cli_try(bin, ws, &["run", "--", sleep_s, "302", "--name", "clash"])?;
    expect(
        !ok && code != 0,
        &format!("cross-app colliding --name is refused, got {code}: {combined:?}"),
    )?;
    expect(
        combined.contains("already registered"),
        &format!("the error explains the collision: {combined:?}"),
    )?;
    expect(
        combined.contains("owner"),
        &format!("the error names the owning app: {combined:?}"),
    )?;
    expect_no_residue(ws, "clash")?;

    step("RDF9b. a same-name ad-hoc re-submission regenerates (not refused)");
    // `web` was ad-hoc registered in RDF1; re-submitting it is a full
    // regeneration (app-registry: 同名全量再生), immediate effect.
    let web_pid = pid_of(ws, "web")?.expect("web is running");
    let out = cli(bin, ws, &["run", "--", sleep_s, "303", "--name", "web"])?;
    expect(
        out.contains("started"),
        &format!("a same-name re-submission succeeds: {out:?}"),
    )?;
    let web_text = std::fs::read_to_string(apps.join("web.toml"))?;
    expect(
        web_text.contains("303"),
        "the record was regenerated from the new command line",
    )?;
    expect(
        pid_of(ws, "web")? != Some(web_pid),
        "the changed definition restarted the process",
    )?;

    step("RDF10. the reserved `all` name is refused with no residue");
    let (ok, _code, combined) = cli_try(bin, ws, &["run", "--", sleep_s, "303", "--name", "all"])?;
    expect(!ok, "run --name all is refused")?;
    expect(
        combined.contains("reserved"),
        &format!("the error explains the reserved word: {combined:?}"),
    )?;
    expect_no_residue(ws, "all")?;

    // -- 8.3: atomic landing ------------------------------------------------

    step("RDF11. mid-flight reload sees no half-written registration");
    let atomic_out = ws.root.join("atomic.out");
    let atomic_log = std::fs::File::create(&atomic_out)?;
    let mut atomic = Command::new(bin)
        .arg("--config")
        .arg(ws.config_arg())
        .args(["run", "--", sleep_s, "400", "--name", "atomic"])
        .stdout(Stdio::from(atomic_log))
        .stderr(Stdio::null())
        .spawn()
        .context("spawn the atomic ad-hoc run")?;
    // The startup window is the built-in 3s startsecs: sample it mid-flight.
    std::thread::sleep(Duration::from_millis(800));
    expect(
        !apps.join("atomic.toml").exists(),
        "no .toml record while the startup judgement is still pending",
    )?;
    expect(
        apps.join("atomic.toml.pending").is_file(),
        "the placeholder exists mid-flight (a scanner cannot see it as a record)",
    )?;
    let listed = cli(bin, ws, &["list"])?;
    expect(
        !listed.contains("atomic"),
        &format!("`ak list` excludes the in-flight app: {listed:?}"),
    )?;
    let pending: serde_json::Value = serde_json::from_str(&api(ws, "/v1/pending")?)?;
    expect(
        pending["programs"]
            .as_array()
            .map(|a| a.is_empty())
            .unwrap_or(true),
        "the in-flight app is not in pending",
    )?;
    // A reload mid-flight must not disturb the staging.
    cli(bin, ws, &["reload"])?;
    let status = atomic.wait()?;
    expect(
        status.success(),
        &format!("the ad-hoc run still succeeds after the mid-flight reload ({status})"),
    )?;
    expect(
        apps.join("atomic.toml").is_file() && !apps.join("atomic.toml.pending").exists(),
        "the placeholder was renamed onto the record",
    )?;
    let atomic_text = std::fs::read_to_string(&atomic_out).unwrap_or_default();
    expect(
        atomic_text.contains("atomic") && atomic_text.contains("pid:"),
        &format!("the completed run reported its result: {atomic_text:?}"),
    )?;

    // -- 8.5: health probe progress and judgement ---------------------------

    if cfg!(unix) {
        step("RDF12. a health-probed ad-hoc run streams progress and returns the pid");
        let body = serde_json::json!({
            "name": "healthy",
            "command": sleep_s,
            "args": ["300"],
            "health_check": "/bin/sh -c true",
        });
        let (code, stream) = post_ndjson(ws, "/v1/run", &body)?;
        expect(
            code == 200,
            &format!("the health run answers 200, got {code}"),
        )?;
        expect(
            stream.contains("\"phase\":\"staged\"") && stream.contains("estimate_secs"),
            &format!("the stream carries the staged frame with an estimate: {stream:?}"),
        )?;
        expect(
            stream.contains("waiting_health"),
            &format!("the stream carries the waiting_health frame: {stream:?}"),
        )?;
        let terminal = last_ndjson_frame(&stream).context("a terminal frame exists")?;
        expect(
            terminal.get("ok").and_then(|o| o.as_bool()) == Some(true),
            &format!("the terminal frame reports success: {terminal}"),
        )?;
        let hpid = terminal.get("pid").and_then(|p| p.as_u64());
        expect(
            hpid.is_some(),
            &format!("the terminal frame carries a pid: {terminal}"),
        )?;
        expect(
            apps.join("healthy.toml").is_file(),
            "the health-probed app landed after the first successful probe",
        )?;

        step("RDF13. a health probe that never passes fails with no residue");
        let body = serde_json::json!({
            "name": "sickly",
            "command": sleep_s,
            "args": ["300"],
            "health_check": "/bin/sh -c false",
        });
        let t0 = Instant::now();
        let (code, stream) = post_ndjson(ws, "/v1/run", &body)?;
        let elapsed = t0.elapsed();
        expect(
            code == 200,
            &format!("the failing health run answers 200, got {code}"),
        )?;
        let terminal = last_ndjson_frame(&stream).context("a terminal frame exists")?;
        expect(
            terminal.get("ok").and_then(|o| o.as_bool()) == Some(false),
            &format!("the terminal frame reports failure: {terminal}"),
        )?;
        expect(
            terminal.get("phase").and_then(|p| p.as_str()) == Some("timeout"),
            &format!("the failure is the health window timing out: {terminal}"),
        )?;
        expect(
            elapsed >= Duration::from_secs(20),
            &format!("it waited out the health window (took {elapsed:?})"),
        )?;
        expect_no_residue(ws, "sickly")?;
        expect(
            pid_of(ws, "sickly")?.is_none(),
            "the failed health program was stopped and dropped",
        )?;
    } else {
        println!("    (windows: health-probe scenarios need /bin/sh; skipping)");
    }

    step("RDF13b. without a health probe the endpoint emits no intermediate frame");
    let body = serde_json::json!({
        "name": "plain",
        "command": sleep_s,
        "args": ["300"],
    });
    let (code, stream) = post_ndjson(ws, "/v1/run", &body)?;
    expect(
        code == 200,
        &format!("the plain run answers 200, got {code}"),
    )?;
    let frames: Vec<serde_json::Value> = stream
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();
    expect(
        frames.len() == 1,
        &format!("exactly one (terminal) frame without a health check: {stream:?}"),
    )?;
    expect(
        frames[0].get("ok").and_then(|o| o.as_bool()) == Some(true),
        &format!("the terminal frame reports success: {}", frames[0]),
    )?;
    expect(apps.join("plain.toml").is_file(), "the plain app landed")?;

    step("RDF13c. an unusable app_dir refuses before starting any process");
    // Point a fresh daemon's app_dir underneath a regular file: creating the
    // directory fails deterministically (root- or permission-independent), so
    // the placeholder can never be established. The endpoint must report a
    // stage error and start nothing (app-registry spec: 占位建立失败不启动进程).
    let blocker = ws.root.join("blocker");
    std::fs::write(&blocker, "not a directory")?;
    let ro_apps = blocker.join("apps");
    let ro_cfg = ws.root.join("daemon-ro.toml");
    let ro_endpoint = ws.root.join("daemon-ro.sock");
    let ro_logs = ws.root.join("logs").to_string_lossy().replace('\\', "/");
    std::fs::write(
        &ro_cfg,
        format!(
            "[daemon]\nsocket = {socket}\nlog_dir = \"{ro_logs}\"\nlog_level = \"warn\"\napp_dir = \"{}\"\n",
            ro_apps.display().to_string().replace('\\', "/"),
            socket = socket_toml_value(&ro_endpoint),
        ),
    )?;
    let mut ro_daemon = spawn_daemon_at(bin, &ro_cfg, &ro_endpoint)?;
    let (code, body) = post_ndjson_at(
        &ro_endpoint,
        "/v1/run",
        &serde_json::json!({ "name": "ro", "command": sleep_s, "args": ["300"] }),
    )?;
    expect(
        code == 500 || code == 400,
        &format!("the unusable app_dir answers an error status, got {code}: {body:?}"),
    )?;
    expect(
        body.contains("placeholder") || body.contains("app_dir") || body.contains("create"),
        &format!("the error names the write failure: {body:?}"),
    )?;
    expect(
        !ro_apps.exists(),
        "no app_dir was created under the blocker",
    )?;
    expect(
        pid_of_at(&ro_endpoint, "ro")?.is_none(),
        "no process was started when the placeholder could not be written",
    )?;
    shutdown_daemon_at(&ro_endpoint, &mut ro_daemon);

    // -- 8.6: partial failure + residual state ------------------------------

    if cfg!(unix) {
        step("RDF14. a failed rename keeps the process and prints the full config");
        // Simulate an unwritable TARGET (robust even as root): a directory named
        // `<name>.toml` makes the same-directory rename fail with EISDIR after
        // startup already succeeded — exactly the partial-failure path.
        std::fs::create_dir_all(apps.join("weird.toml"))?;
        let (ok, code, combined) =
            cli_try(bin, ws, &["run", "--", sleep_s, "400", "--name", "weird"])?;
        expect(
            !ok && code != 0,
            &format!("the partial failure exits non-zero, got {code}: {combined:?}"),
        )?;
        expect(
            combined.contains("registration was not written"),
            &format!("it explains the process is running but unregistered: {combined:?}"),
        )?;
        expect(
            combined.contains("command")
                && combined.contains("args")
                && combined.contains("work_dir"),
            &format!("it prints the full configuration to land by hand: {combined:?}"),
        )?;
        let weird_pid = pid_of(ws, "weird")?.expect("the process was kept running");
        expect(
            apps.join("weird.toml.pending").is_file(),
            "the placeholder is kept for diagnosis",
        )?;

        step("RDF15. a re-run detects the residual state and asks (non-TTY refuses)");
        let (ok, code, combined) =
            cli_try(bin, ws, &["run", "--", sleep_s, "400", "--name", "weird"])?;
        expect(
            !ok && code != 0,
            &format!("a non-interactive re-run refuses, got {code}: {combined:?}"),
        )?;
        expect(
            combined.contains("already running") && combined.contains("not registered"),
            &format!("it reports the residual state: {combined:?}"),
        )?;
        expect(
            pid_of(ws, "weird")? == Some(weird_pid),
            "the refusal did not restart the process",
        )?;
        expect(
            !apps.join("weird.toml").is_file(),
            "the refusal wrote no record",
        )?;

        step("RDF16. confirming the residual re-lands the record without restarting");
        std::fs::remove_dir(apps.join("weird.toml"))?;
        let cmd = format!(
            "{} --config {} run -- {} 400 --name weird",
            bin.display(),
            ws.config_arg(),
            sleep_s
        );
        let mut child = Command::new("script")
            .args(["-qec", &cmd, "/dev/null"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("spawn the residual run under a pty")?;
        {
            use std::io::Write;
            child
                .stdin
                .as_mut()
                .expect("piped stdin")
                .write_all(b"y\n")?;
        }
        let confirm = child.wait_with_output()?;
        let confirm_text = format!(
            "{}{}",
            String::from_utf8_lossy(&confirm.stdout),
            String::from_utf8_lossy(&confirm.stderr)
        );
        expect(
            confirm.status.success(),
            &format!("the confirmed residual exits 0: {confirm_text:?}"),
        )?;
        expect(
            confirm_text.contains("registered"),
            &format!("the confirmation reports the registration: {confirm_text:?}"),
        )?;
        expect(
            apps.join("weird.toml").is_file() && !apps.join("weird.toml.pending").exists(),
            "the record landed and the placeholder is gone",
        )?;
        expect(
            pid_of(ws, "weird")? == Some(weird_pid),
            "the confirmed commit reused the running process (pid unchanged)",
        )?;
    } else {
        println!("    (windows: the residual-confirmation scenario needs a pty; skipping)");
    }

    // -- default app_dir name (configuration spec) --------------------------

    step("RDF17. the built-in app_dir default is app.d");
    let defaults = ws.root.join("defaults");
    std::fs::create_dir_all(&defaults)?;
    let dcfg = defaults.join("daemon.toml");
    std::fs::write(&dcfg, "[daemon]\n")?;
    let out = cli_cfg(bin, &dcfg, &["config", "--get", "app_dir"])?;
    expect(
        out.trim() == "app.d",
        &format!("config --get app_dir answers app.d, got {out:?}"),
    )?;

    let _ = &mut *daemon;
    Ok(())
}

/// Ask the daemon to shut down and wait for the endpoint to go quiet.
fn shutdown_and_wait(bin: &Path, ws: &Workspace) -> Result<()> {
    cli(bin, ws, &["shutdown"])?;
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if !socket_health_ok(&ws.control_socket) {
            return Ok(());
        }
        if Instant::now() > deadline {
            bail!("daemon did not shut down within 15s");
        }
        std::thread::sleep(Duration::from_millis(150));
    }
}

/// `daemon --detach` in its own isolated workspace: cold start becomes ready,
/// prints the address + log path, and a second detach reports the
/// already-running instance (no second process).
fn detach_pass(bin: &Path) -> Result<()> {
    step("R9. `daemon --detach` cold start + second instance reports already-running");
    let root = std::env::temp_dir().join(format!("xk-xtask-detach-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("logs"))?;
    let endpoint = control_socket_in(&root);
    let log_dir = root.join("logs").to_string_lossy().replace('\\', "/");
    std::fs::write(
        root.join("daemon.toml"),
        format!(
            "[daemon]\nsocket = {socket}\nlog_dir = \"{log_dir}\"\nlog_level = \"warn\"\n",
            socket = socket_toml_value(&endpoint),
        ),
    )?;
    let cfg = root.join("daemon.toml");
    let (ok, code, combined) = cli_try_cfg(bin, &cfg, &["daemon", "--detach"])?;
    expect(
        ok,
        &format!("daemon --detach exits 0, got {code}: {combined}"),
    )?;
    expect(
        combined.contains(endpoint.display().to_string().as_str()),
        &format!("the ready message prints the control endpoint: {combined:?}"),
    )?;
    expect(
        combined.contains("daemon.out.log"),
        &format!("the ready message prints the background log path: {combined:?}"),
    )?;
    expect(
        root.join("logs/daemon.out.log").is_file(),
        "the background log file exists",
    )?;
    // A second detach sees the healthy instance and does not fail.
    let (ok, _code, combined) = cli_try_cfg(bin, &cfg, &["daemon", "--detach"])?;
    expect(
        ok,
        "a second daemon --detach succeeds against the live instance",
    )?;
    expect(
        combined.contains("already running"),
        &format!("it reports the already-running daemon: {combined:?}"),
    )?;
    // Shut the background daemon down through the control plane.
    let (ok, code, c) = cli_try_cfg(bin, &cfg, &["shutdown"])?;
    expect(
        ok,
        &format!("the detached daemon shuts down via the CLI (code {code}): {c:?}"),
    )?;
    let _ = std::fs::remove_dir_all(&root);
    Ok(())
}

// -- actions scenario group (actions + glossary change) ----------------------

/// Write the scenario app used by the actions pass: one long-running service
/// with a custom-action toolbox, a USR1/USR2-trapping program and a plain
/// sleeper, chained by dependencies so the fan-out order is observable.
/// The `appkeeper` binary path and daemon config path are baked into the
/// chained-restart action (the CLI reads connection info from the config).
fn setup_actions_app(ws: &Workspace, bin: &Path) -> Result<()> {
    let dir = ws.root.join("acts");
    std::fs::create_dir_all(&dir)?;
    let bin_s = bin.display().to_string().replace('\\', "/");
    let cfg_s = ws.config_arg().replace('\\', "/");
    let text = format!(
        r#"[app]
restart = "no"

[program.svc]
command = "sleep 100000"
startsecs = 0.2

[program.svc.action.ping]
command = "echo svc_pid=${{program.svc.pid}} svc_state=${{program.svc.state}}"
timeout = 10

[program.svc.action.workdir_probe]
command = "pwd"
timeout = 5

[program.svc.action.slow]
command = "sleep 31"
timeout = 2

[program.svc.action.ring]
command = "sleep 4"
timeout = 30

[program.svc.action.chained]
command = "echo old_svc_pid=${{program.svc.pid}}; {bin} --config {cfg} restart svc; {bin} --config {cfg} pid svc"
timeout = 60

[program.svc.action.stopped_pid]
command = "echo stopped_svc_pid=[${{program.svc.pid}}]"
timeout = 10

[program.svc.action.multiline]
command = '''
echo ml-one
echo ml-two
'''
timeout = 10

[program.svc.action.fail3]
command = "exit 3"
timeout = 10

[program.signalee]
command = "sleep 100000"
startsecs = 0.2
restart = "no"
depends_on = ["svc"]

[program.trapper]
command = "sh"
args = ["-c", "trap '' USR1 USR2; sleep 100000"]
startsecs = 0.2
restart = "no"
depends_on = ["signalee"]

[program.logger]
command = "sh"
args = ["-c", "while true; do echo tick; sleep 0.2; done"]
startsecs = 0.2
restart = "no"
"#,
        bin = bin_s,
        cfg = cfg_s,
    );
    std::fs::write(dir.join("appkeeper.toml"), text)?;
    link_registration(&dir.join("appkeeper.toml"), &ws.root.join("apps/acts.toml"))?;
    Ok(())
}

/// Offline (daemon not running): declarations validate, bad ones are refused
/// with actionable errors — charset (breaking tighten), unknown action
/// variable, built-in reserved word, timeout range.
fn actions_offline_pass(bin: &Path, ws: &Workspace) -> Result<()> {
    step("H0. action declarations pass validate");
    let acts_file = ws.root.join("acts/appkeeper.toml");
    let out = cli(bin, ws, &["validate", &acts_file.to_string_lossy()])?;
    expect(
        out.contains("OK") && out.contains("program(s)"),
        "the acts config with 7 declared actions validates",
    )?;

    step("H1. illegal identifier names are refused (dots, spaces, non-ASCII)");
    let bad_dir = ws.root.join("badcase");
    std::fs::create_dir_all(&bad_dir)?;
    let bad_file = bad_dir.join("appkeeper.toml");
    std::fs::write(&bad_file, "[program.\"my.web\"]\ncommand = 'sleep 1'\n")?;
    let (ok, code, combined) = cli_try(bin, ws, &["validate", &bad_file.to_string_lossy()])?;
    expect(!ok, "a dotted program name fails validate")?;
    expect(code == 1, &format!("validate failure exits 1, got {code}"))?;
    expect(
        combined.contains("[A-Za-z0-9_-]"),
        "the error states the identifier charset",
    )?;
    std::fs::write(&bad_file, "[program.\"my web\"]\ncommand = 'sleep 1'\n")?;
    let (ok, _code, combined) = cli_try(bin, ws, &["validate", &bad_file.to_string_lossy()])?;
    expect(
        !ok && combined.contains("[A-Za-z0-9_-]"),
        "a spaced program name is refused",
    )?;
    let (ok, _code, combined) = cli_try(
        bin,
        ws,
        &["add", &acts_file.to_string_lossy(), "--name", "my.app"],
    )?;
    expect(
        !ok && combined.contains("[A-Za-z0-9_-]"),
        "`add --name my.app` is refused (dots are not identifiers)",
    )?;
    let (ok, _code, combined) = cli_try(
        bin,
        ws,
        &["add", &acts_file.to_string_lossy(), "--name", "bad name"],
    )?;
    expect(
        !ok && combined.contains("[A-Za-z0-9_-]"),
        "`add --name \"bad name\"` is refused (spaces are not identifiers)",
    )?;

    step("H2. unknown action variables are refused at validate time");
    std::fs::write(
        &bad_file,
        "[program.ok]\ncommand = 'sleep 1'\n\n[program.ok.action.bad]\ncommand = 'curl http://h/?x=${program.ok.hello}'\n",
    )?;
    let (ok, _code, combined) = cli_try(bin, ws, &["validate", &bad_file.to_string_lossy()])?;
    expect(!ok, "an unknown variable field fails validate")?;
    expect(
        combined.contains("unknown action variable") && combined.contains("program.ok.hello"),
        "the error names the offending variable",
    )?;

    step("H3. built-in action names are reserved");
    std::fs::write(
        &bad_file,
        "[program.ok]\ncommand = 'sleep 1'\n\n[program.ok.action.stop]\ncommand = 'echo nope'\n",
    )?;
    let (ok, _code, combined) = cli_try(bin, ws, &["validate", &bad_file.to_string_lossy()])?;
    expect(
        !ok && combined.contains("reserved"),
        "action `stop` is refused as reserved",
    )?;
    // Observe-only command names are NOT reserved (glossary 动作词表边界).
    std::fs::write(
        &bad_file,
        "[program.ok]\ncommand = 'sleep 1'\n\n[program.ok.action.status]\ncommand = 'echo hi'\n",
    )?;
    let out = cli(bin, ws, &["validate", &bad_file.to_string_lossy()])?;
    expect(
        out.contains("OK"),
        "a custom action named `status` is allowed",
    )?;

    step("H4. action timeout must be > 0");
    std::fs::write(
        &bad_file,
        "[program.ok]\ncommand = 'sleep 1'\n\n[program.ok.action.t0]\ncommand = 'x'\ntimeout = 0\n",
    )?;
    let (ok, _code, combined) = cli_try(bin, ws, &["validate", &bad_file.to_string_lossy()])?;
    expect(
        !ok && combined.contains("timeout"),
        "timeout = 0 is refused",
    )?;
    std::fs::write(
        &bad_file,
        "[program.ok]\ncommand = 'sleep 1'\n\n[program.ok.action.tn]\ncommand = 'x'\ntimeout = -1\n",
    )?;
    let (ok, _code, combined) = cli_try(bin, ws, &["validate", &bad_file.to_string_lossy()])?;
    expect(
        !ok && combined.contains("timeout"),
        "a negative timeout is refused",
    )?;
    Ok(())
}

/// Process-tree helper: pids whose argv contains exactly `arg` (unix only;
/// windows runs no tree assertions).
#[cfg(unix)]
fn pids_with_arg(arg: &str) -> Vec<u32> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return out;
    };
    for e in entries.flatten() {
        let Ok(name) = e.file_name().into_string() else {
            continue;
        };
        if name.is_empty() || !name.bytes().all(|b| b.is_ascii_digit()) {
            continue;
        }
        let Ok(cmdline) = std::fs::read(e.path().join("cmdline")) else {
            continue;
        };
        if cmdline.split(|&b| b == 0).any(|a| a == arg.as_bytes()) {
            out.push(name.parse().unwrap());
        }
    }
    out
}

fn state_of(ws: &Workspace, program: &str) -> Result<String> {
    let (_code, body) = socket_get(
        &ws.control_socket,
        &format!("/v1/programs/{program}"),
    )?;
    let v: serde_json::Value = serde_json::from_str(&body)?;
    Ok(v.get("state")
        .and_then(|s| s.as_str())
        .unwrap_or("unknown")
        .to_string())
}

fn wait_state(ws: &Workspace, program: &str, want: &str) -> Result<()> {
    wait_state_in(ws, program, &[want])
}

fn wait_state_in(ws: &Workspace, program: &str, want: &[&str]) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let have = state_of(ws, program)?;
        if want.contains(&have.as_str()) {
            return Ok(());
        }
        if Instant::now() > deadline {
            bail!("{program} never reached {want:?} (state {have})");
        }
        std::thread::sleep(Duration::from_millis(150));
    }
}

/// Online actions pass: fan-out, custom-action contract, signal.
fn actions_online_pass(bin: &Path, ws: &Workspace) -> Result<()> {
    // Everything below addresses the acts app; make sure the daemon loaded it.
    let svc_running = pid_of(ws, "svc")?;
    expect(
        svc_running.is_none(),
        "acts programs are not started (restart = no)",
    )?;
    let acts_dir = ws.root.join("acts");

    step("H5. `run --app acts` fans out in dependency order");
    let out = cli(bin, ws, &["run", "--app", "acts"])?;
    // run-daemon-first re-renders the `run --app` fan-out as one
    // `{program}  config=...  pid=...  status (...)` line per program; the
    // dependency order is asserted on the program name at each line start.
    let pos = |p: &str| out.find(&format!("{p}  config="));
    let (psvc, psig, ptrap) = (pos("svc"), pos("signalee"), pos("trapper"));
    expect(
        matches!((psvc, psig, ptrap), (Some(a), Some(b), Some(c)) if a < b && b < c),
        &format!("fan-out start lists svc < signalee < trapper: {out:?}"),
    )?;
    for name in ["svc", "signalee", "trapper"] {
        wait_running(ws, name)?;
    }

    if cfg!(unix) {
        step("H5b. `ak log -f` streams new lines and stays alive (unix)");
        // logger (started by the fan-out above) echoes `tick` every 0.2s.
        let follow_path = ws.root.join("follow.out");
        let follow = std::fs::File::create(&follow_path)?;
        let mut child = Command::new(bin)
            .arg("--config")
            .arg(ws.config_arg())
            .args(["log", "logger", "--tail", "1", "-f"])
            .stdout(Stdio::from(follow))
            .stderr(Stdio::null())
            .spawn()
            .context("spawn `log -f`")?;
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut len1 = 0usize;
        loop {
            let text = std::fs::read_to_string(&follow_path).unwrap_or_default();
            if text.contains("tick") {
                len1 = text.len();
                break;
            }
            if Instant::now() > deadline {
                let _ = child.kill();
                let _ = child.wait();
                bail!("log follow never showed a tick line");
            }
            std::thread::sleep(Duration::from_millis(150));
        }
        // The stream must keep growing after the initial tail (following,
        // not a one-shot tail), and the client must still be running.
        std::thread::sleep(Duration::from_millis(1500));
        let len2 = std::fs::read_to_string(&follow_path)?.len();
        expect(
            len2 > len1,
            &format!("new lines kept arriving after the tail ({len1} -> {len2} bytes)"),
        )?;
        expect(
            matches!(child.try_wait(), Ok(None)),
            "`log -f` is still following (no self-exit)",
        )?;
        let _ = child.kill();
        let _ = child.wait()?;
    }

    step("H5c. control-plane API: status JSON, 409 transition, 404 unknown targets");
    let (code, body) = api_checked(ws, "GET", "/v1/status", "")?;
    expect(
        code == 200,
        &format!("GET /v1/status answers 200, got {code}"),
    )?;
    expect(
        body.contains("\"daemon\"")
            && body.contains("\"programs\"")
            && body.contains("\"unhealthy\"")
            && body.contains("\"pid\""),
        "status JSON carries daemon info and per-program pid/unhealthy",
    )?;
    let (code, body) = api_checked(ws, "POST", "/v1/programs/svc/start", "{}")?;
    expect(
        code == 409,
        &format!("starting a running program is 409, got {code}"),
    )?;
    expect(
        body.contains("already"),
        "the 409 explains the invalid transition",
    )?;
    let (code, _) = api_checked(ws, "GET", "/v1/programs/ghost", "")?;
    expect(code == 404, &format!("unknown program is 404, got {code}"))?;
    let (code, body) = api_checked(ws, "POST", "/v1/programs/ghost/start", "{}")?;
    expect(
        code == 404,
        &format!("starting an unknown program is 404, got {code}"),
    )?;
    expect(
        body.contains("unknown program"),
        "the 404 names the unknown program",
    )?;
    let (code, _) = api_checked(ws, "POST", "/v1/programs/svc/actions/nosuch", "{}")?;
    expect(code == 404, &format!("unknown action is 404, got {code}"))?;
    let (code, _) = api_checked(ws, "POST", "/v1/apps/ghost/start", "{}")?;
    expect(
        code == 404,
        &format!("unknown app fan-out is 404, got {code}"),
    )?;
    // The CLI surfaces the same answers with exit code 1.
    let (ok, code, combined) = cli_try(bin, ws, &["action", "svc", "nosuch"])?;
    expect(
        !ok && code == 1,
        &format!("unknown action exits 1, got {code}"),
    )?;
    expect(combined.contains("404"), "the CLI error reports the 404")?;
    let (ok, code, combined) = cli_try(bin, ws, &["run", "--app", "ghost"])?;
    expect(
        !ok && code == 1,
        &format!("unknown app fan-out exits 1, got {code}"),
    )?;
    expect(
        combined.contains("unknown app") || combined.contains("404"),
        "the CLI error names the unknown app",
    )?;

    step("H6. custom action runs with variable substitution");
    wait_state(ws, "svc", "running")?;
    let svc_pid = pid_of(ws, "svc")?.expect("svc has a pid");
    let out = cli(bin, ws, &["action", "svc", "ping"])?;
    expect(
        out.contains(&format!("svc_pid={svc_pid}")),
        &format!("${{program.svc.pid}} substituted with the real pid: {out:?}"),
    )?;
    expect(
        out.contains("svc_state=running"),
        "state variable resolves to running",
    )?;

    step("H6b. a multi-line command runs as one shell script");
    let out = cli(bin, ws, &["action", "svc", "multiline"])?;
    expect(
        out.contains("ml-one") && out.contains("ml-two"),
        &format!("both lines of the multi-line command executed: {out:?}"),
    )?;

    step("H6c. `appkeeper action` passes the action's own exit code through");
    let (ok, code, combined) = cli_try(bin, ws, &["action", "svc", "fail3"])?;
    expect(
        !ok && code == 3,
        &format!("an exit-3 action exits 3, got {code}"),
    )?;
    expect(
        combined.contains("exited with code 3"),
        "the failure explains the action's exit code",
    )?;

    step("H7. the action cwd is the program's work_dir");
    let out = cli(bin, ws, &["action", "svc", "workdir_probe"])?;
    expect(
        out.trim() == acts_dir.to_str().unwrap(),
        &format!("pwd == work_dir ({}): {out:?}", acts_dir.display()),
    )?;

    step("H8. timeout kills the action process tree");
    let t0 = Instant::now();
    let (ok, code, combined) = cli_try(bin, ws, &["action", "svc", "slow"])?;
    let elapsed = t0.elapsed();
    expect(!ok, "a timed-out action fails the call")?;
    expect(code == 1, &format!("timeout exits 1, got {code}"))?;
    expect(
        combined.contains("timed out"),
        "the error says the action timed out",
    )?;
    expect(
        elapsed < Duration::from_secs(20),
        &format!("returned near the 2s timeout, took {elapsed:?} (sleep 31 was killed)"),
    )?;
    expect(
        pid_of(ws, "svc")? == Some(svc_pid),
        "the supervised program is unaffected by the action timeout",
    )?;
    #[cfg(unix)]
    {
        let deadline = Instant::now() + Duration::from_secs(6);
        while Instant::now() < deadline && !pids_with_arg("31").is_empty() {
            std::thread::sleep(Duration::from_millis(100));
        }
        expect(
            pids_with_arg("31").is_empty(),
            "no `sleep 31` survives the tree kill (/proc scan)",
        )?;
    }

    step("H9. same (program, action) is mutually exclusive, no queueing");
    // Fire the first ring call in the background (sleep 4, timeout 30),
    // then race a second one in: it must hit the 409, not queue.
    let handle = std::thread::spawn({
        let bin = bin.to_path_buf();
        let cfg = ws.config_arg();
        move || {
            let out = Command::new(&bin)
                .arg("--config")
                .arg(&cfg)
                .args(["action", "svc", "ring"])
                .output()
                .expect("run first ring");
            (
                out.status.success(),
                out.status.code().unwrap_or(-1),
                format!(
                    "{}{}",
                    String::from_utf8_lossy(&out.stdout),
                    String::from_utf8_lossy(&out.stderr)
                ),
            )
        }
    });
    std::thread::sleep(Duration::from_millis(700));
    let (ok2, code2, combined2) = cli_try(bin, ws, &["action", "svc", "ring"])?;
    expect(!ok2, "the concurrent second call is refused")?;
    expect(
        code2 == 1,
        &format!("conflict exits 1, got {code2}: {combined2:?}"),
    )?;
    expect(
        combined2.contains("409") || combined2.contains("already running"),
        "the error says the action is already running",
    )?;
    let (ok1, code1, _) = handle.join().unwrap();
    expect(
        ok1 && code1 == 0,
        "the first ring call completed normally (exit 0)",
    )?;
    expect(
        pid_of(ws, "svc")? == Some(svc_pid),
        "mutual exclusion left the program untouched",
    )?;

    step("H10. an action can chain `ak restart` via the control plane");
    let out = cli(bin, ws, &["action", "svc", "chained"])?;
    expect(
        out.contains(&format!("old_svc_pid={svc_pid}")),
        "the substitution saw the pre-restart pid",
    )?;
    let new_pid = wait_running(ws, "svc")?;
    expect(
        new_pid != svc_pid,
        "the chained restart replaced the process",
    )?;
    expect(
        out.contains(&new_pid.to_string()),
        &format!("the action observed the new pid ({new_pid}): {out:?}"),
    )?;

    step("H11. actions run for stopped programs; pid substitutes to empty");
    cli(bin, ws, &["stop", "svc"])?;
    wait_state(ws, "svc", "stopped")?;
    let out = cli(bin, ws, &["action", "svc", "stopped_pid"])?;
    expect(
        out.contains("stopped_svc_pid=[]"),
        &format!("the pid of a stopped program is an empty string: {out:?}"),
    )?;

    if cfg!(unix) {
        step("H12. signal: whitelist in/out, case-insensitive, not-running (unix)");
        // USR2 lowercase: accepted, normalized; the trapper ignores USR1/USR2,
        // so its state machine must stay running with the same pid.
        let trap_pid = wait_running(ws, "trapper")?;
        let out = cli(bin, ws, &["signal", "trapper", "usr2"])?;
        expect(out.contains("delivered"), "lowercase usr2 is accepted")?;
        expect(
            pid_of(ws, "trapper")? == Some(trap_pid),
            "the trapped process survived USR2",
        )?;
        expect(
            state_of(ws, "trapper")? == "running",
            "signal did not change the state machine",
        )?;
        let out = cli(bin, ws, &["signal", "trapper", "USR1"])?;
        expect(out.contains("delivered"), "USR1 delivered to the trapper")?;
        expect(
            state_of(ws, "trapper")? == "running",
            "trapper still running after USR1",
        )?;
        // KILL is off-whitelist; the error points at stop.
        let (ok, code, combined) = cli_try(bin, ws, &["signal", "trapper", "KILL"])?;
        expect(!ok && code == 1, "KILL exits 1")?;
        expect(
            combined.contains("whitelist") && combined.contains("stop"),
            "the error explains the whitelist and the stop alternative",
        )?;
        expect(
            pid_of(ws, "trapper")? == Some(trap_pid),
            "the rejected signal did not touch the program",
        )?;
        // TERM really terminates (signalee has restart = "no").
        let out = cli(bin, ws, &["signal", "signalee", "TERM"])?;
        expect(out.contains("delivered"), "TERM delivered")?;
        wait_state(ws, "signalee", "exited")?;
        // No live child anymore: signaling must fail.
        let (ok, code, combined) = cli_try(bin, ws, &["signal", "signalee", "USR1"])?;
        expect(!ok && code == 1, "signaling an exited program exits 1")?;
        expect(
            combined.contains("not running"),
            "the error says there is no child to signal",
        )?;
    } else {
        println!("    (windows: signal scenarios require unix; skipping)");
    }

    step("H13. `stop --app acts` runs in reverse order");
    let out = cli(bin, ws, &["stop", "--app", "acts"])?;
    let (ptrap, psig, psvc) = (
        out.find("  trapper:"),
        out.find("  signalee:"),
        out.find("  svc:"),
    );
    expect(
        matches!((ptrap, psig, psvc), (Some(a), Some(b), Some(c)) if a < b && b < c),
        &format!("fan-out stop lists trapper < signalee < svc (reverse): {out:?}"),
    )?;
    // signalee was TERM-signaled to exit in H12 (restart = no): a
    // terminal state (exited) is as stopped as stopped itself — fan-out stop
    // must simply not leave anything running.
    for name in ["svc", "signalee", "trapper"] {
        wait_state_in(ws, name, &["stopped", "exited"])?;
    }

    step("H14. fan-out start covers user_stopped programs (issued from the shell)");
    // Every acts program is user-stopped at this point (explicit stop above
    // and in H11); the fan-out start must pull all of them up and clear the
    // marker. Issued via the shell to prove the shell verb fans out too.
    let out = cli(bin, ws, &["shell", "-e", "run --app acts"])?;
    expect(
        matches!((out.find("  svc:"), out.find("  signalee:"), out.find("  trapper:")),
            (Some(a), Some(b), Some(c)) if a < b && b < c),
        "fan-out start again lists all three in dependency order",
    )?;
    for name in ["svc", "signalee", "trapper"] {
        wait_running(ws, name)?;
        expect(
            matches!(state_of(ws, name)?.as_str(), "running" | "starting"),
            &format!("user-stopped {name} was started by the fan-out (marker cleared)"),
        )?;
    }

    step("H15. shell: status table, unknown-command tolerance, restart, action exit code");
    // status table: program name is the primary column, app is attribution.
    let out = cli(bin, ws, &["shell", "-e", "status"])?;
    expect(
        ["NAME", "APP", "STATE", "PID", "RESTARTS", "UNHEALTHY"]
            .iter()
            .all(|h| out.contains(h)),
        &format!("shell status prints the aligned table header: {out:?}"),
    )?;
    expect(
        out.contains("svc") && out.contains("acts"),
        "rows name the program with its app as a separate column",
    )?;

    // REPL: an unknown command is reported (with a did-you-mean hint) and the
    // shell keeps reading — piped stdin drives the non-TTY fallback path.
    let svc_pid2 = wait_running(ws, "svc")?;
    let mut repl = Command::new(bin)
        .arg("--config")
        .arg(ws.config_arg())
        .arg("shell")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("spawn shell repl")?;
    {
        use std::io::Write;
        repl.stdin
            .as_mut()
            .expect("stdin piped")
            .write_all(b"sttaus\npid svc\nexit\n")?;
    }
    let repl_out = repl.wait_with_output()?;
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&repl_out.stdout),
        String::from_utf8_lossy(&repl_out.stderr)
    );
    expect(
        repl_out.status.success(),
        "the shell leaves via `exit` with code 0",
    )?;
    expect(
        combined.contains("unknown command"),
        "the typo is reported as an unknown command",
    )?;
    expect(
        combined.contains("status"),
        "the did-you-mean hint suggests `status`",
    )?;
    expect(
        combined.contains(&svc_pid2.to_string()),
        "the shell kept reading after the typo (`pid svc` answered)",
    )?;

    // restart through the shell hits the same control plane (same migration).
    let old_pid = wait_running(ws, "svc")?;
    let out = cli(bin, ws, &["shell", "-e", "restart svc"])?;
    expect(
        out.contains("svc"),
        &format!("shell restart reports the program: {out:?}"),
    )?;
    let new_pid = wait_running(ws, "svc")?;
    expect(
        new_pid != old_pid,
        "shell restart replaced the process like the CLI does",
    )?;

    // shell -e passes the action's own exit code through (like `appkeeper action`).
    let (ok, code, combined) = cli_try(bin, ws, &["shell", "-e", "action svc fail3"])?;
    expect(
        !ok && code == 3,
        &format!("shell action exit-3 → exit 3, got {code}"),
    )?;
    expect(
        combined.contains("exited with code 3"),
        "the shell explains the action's failure",
    )?;
    Ok(())
}

// -- webui lifecycle pass ---------------------------------------------------------

/// `GET /api/health` probe via curl (same mechanism as the daemon wait).
fn http_ok(url: &str) -> bool {
    Command::new("curl")
        .args(["-sf", "-m", "3", url])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// The console is config-driven: `config --delete webui` + `reload` turns it
/// off, `config --set webui.listen=...` + `reload` turns it on or rebinds it
/// (config-driven-webui acceptance scenarios). Restores the setup listen so
/// the browser pass finds the console on `ws.webui_port`.
fn webui_pass(bin: &Path, ws: &Workspace) -> Result<()> {
    let health = |port: u16| http_ok(&format!("http://127.0.0.1:{port}/api/health"));

    step("W1. config --delete webui + reload stops the console (connection refused)");
    expect(
        health(ws.webui_port),
        "the console answers before the test (setup wrote [webui])",
    )?;
    let out = cli(bin, ws, &["config", "--delete", "webui"])?;
    expect(
        out.contains("deleted webui"),
        &format!("delete reports the removed table: {out:?}"),
    )?;
    let out = cli(bin, ws, &["reload"])?;
    expect(
        out.contains("webui: webui console disabled"),
        &format!("reload reports the disabled console: {out:?}"),
    )?;
    expect(
        !health(ws.webui_port),
        "the console port refuses connections after the reload",
    )?;

    step("W2. config --set webui.listen + reload serves on the new address (off→on)");
    let new_port = free_port()?;
    let out = cli(
        bin,
        ws,
        &[
            "config",
            "--set",
            &format!("webui.listen=127.0.0.1:{new_port}"),
        ],
    )?;
    expect(
        out.contains("webui.listen"),
        &format!("set reports the written key: {out:?}"),
    )?;
    let out = cli(bin, ws, &["reload"])?;
    expect(
        out.contains(&format!(
            "webui: webui console serving on http://127.0.0.1:{new_port}"
        )),
        &format!("reload reports the serving console: {out:?}"),
    )?;
    expect(health(new_port), "the console answers on the new port")?;
    expect(!health(ws.webui_port), "the old port stays closed")?;

    step("W3. restoring the listen + reload rebinds back (listen change)");
    let out = cli(
        bin,
        ws,
        &[
            "config",
            "--set",
            &format!("webui.listen=127.0.0.1:{}", ws.webui_port),
        ],
    )?;
    expect(
        out.contains("webui.listen"),
        &format!("set reports the restored key: {out:?}"),
    )?;
    let out = cli(bin, ws, &["reload"])?;
    expect(
        out.contains(&format!("serving on http://127.0.0.1:{}", ws.webui_port)),
        &format!("reload reports the restored address: {out:?}"),
    )?;
    expect(
        health(ws.webui_port),
        "the original console port answers again",
    )?;
    expect(!health(new_port), "the temporary port is released")?;

    step("W4. reload with an occupied port degrades: note in the output, daemon keeps running");
    let blocker = std::net::TcpListener::bind("127.0.0.1:0")?;
    let occupied = blocker.local_addr()?.port();
    let _out = cli(
        bin,
        ws,
        &[
            "config",
            "--set",
            &format!("webui.listen=127.0.0.1:{occupied}"),
        ],
    )?;
    let out = cli(bin, ws, &["reload"])?;
    expect(
        out.contains("webui: webui console unavailable"),
        &format!("reload reports the degraded console: {out:?}"),
    )?;
    expect(
        api(ws, "/v1/status")?.contains("programs"),
        "the daemon keeps serving the control plane",
    )?;

    step("W5. freeing the port + reload recovers the console on the same address");
    drop(blocker);
    std::thread::sleep(Duration::from_millis(150));
    let out = cli(bin, ws, &["reload"])?;
    expect(
        out.contains(&format!(
            "webui: webui console serving on http://127.0.0.1:{occupied}"
        )),
        &format!("reload recovers the console: {out:?}"),
    )?;
    expect(health(occupied), "the console answers after recovery")?;

    step("W6. restore the setup listen for the browser pass");
    let _out = cli(
        bin,
        ws,
        &[
            "config",
            "--set",
            &format!("webui.listen=127.0.0.1:{}", ws.webui_port),
        ],
    )?;
    let _ = cli(bin, ws, &["reload"])?;
    expect(health(ws.webui_port), "the setup port serves again")?;
    expect(!health(occupied), "the temporary port is released")?;

    step(
        "W7. a bare [webui] section serves the built-in default 127.0.0.1:9877 and exits with the daemon",
    );
    // Skip politely when something else owns the default port on this host.
    match std::net::TcpListener::bind("127.0.0.1:9877") {
        Err(_) => println!("    skip: 127.0.0.1:9877 is occupied on this host"),
        Ok(reserve) => {
            drop(reserve);
            let endpoint2 = control_socket_in(&ws.root.join("ws2"));
            let log2 = ws.root.join("logs2");
            let apps2 = ws.root.join("apps2");
            std::fs::create_dir_all(&log2)?;
            std::fs::create_dir_all(&apps2)?;
            // A second daemon with its own control endpoint, log dir and an
            // empty app registry — the bare `[webui]` section is the only
            // switch.
            let cfg2 = ws.root.join("daemon2.toml");
            std::fs::write(
                &cfg2,
                format!(
                    "[daemon]\nsocket = {socket}\nlog_dir = \"{}\"\nlog_level = \"warn\"\napp_dir = \"{}\"\n\n[webui]\n",
                    log2.display().to_string().replace('\\', "/"),
                    apps2.display().to_string().replace('\\', "/"),
                    socket = socket_toml_value(&endpoint2),
                ),
            )?;
            let out = std::fs::File::create(ws.root.join("daemon2.out.log"))?;
            let err = out.try_clone()?;
            let mut child = Command::new(bin)
                .arg("--config")
                .arg(&cfg2)
                .arg("daemon")
                .stdout(Stdio::from(out))
                .stderr(Stdio::from(err))
                .spawn()?;
            let mut up = false;
            for _ in 0..120 {
                if matches!(child.try_wait(), Ok(Some(_))) {
                    break;
                }
                if http_ok("http://127.0.0.1:9877/api/health") {
                    up = true;
                    break;
                }
                std::thread::sleep(Duration::from_millis(150));
            }
            expect(
                up,
                "the console serves on the default listen without an explicit listen",
            )?;
            // The console exits with the daemon (POST /v1/shutdown).
            let _ = curl_socket(&endpoint2, &["-m", "10", "-X", "POST"], "/v1/shutdown").output();
            for _ in 0..80 {
                if matches!(child.try_wait(), Ok(Some(_))) {
                    break;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            std::thread::sleep(Duration::from_millis(300));
            expect(
                !http_ok("http://127.0.0.1:9877/api/health"),
                "the console is gone once the daemon has exited",
            )?;
        }
    }
    Ok(())
}

// -- bench smoke (add-appkeeper-bench) ----------------------------------------------

/// Build the bench binary quietly; returns its path (same profile as the
/// daemon binary we just built).
fn build_bench() -> Result<PathBuf> {
    let root = repo_root();
    let status = Command::new("cargo")
        .args(["build", "--quiet", "-p", "appkeeper-bench"])
        .current_dir(&root)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .context("failed to run cargo build -p appkeeper-bench")?;
    if !status.success() {
        bail!("cargo build -p appkeeper-bench failed");
    }
    Ok(root.join("target/debug/appkeeper-bench"))
}

/// Temp dirs that follow the bench spawn-workspace naming
/// (`xk-bench-<pid>-<nanos>`); anything present after a bench run is residue.
#[cfg(unix)]
fn bench_workspaces() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(entries) = std::fs::read_dir(std::env::temp_dir()) {
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            let rest = match name.strip_prefix("xk-bench-") {
                Some(r) => r,
                None => continue,
            };
            let mut parts = rest.split('-');
            let numeric = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
            if let (Some(a), Some(b)) = (parts.next(), parts.next()) {
                if numeric(a) && numeric(b) && parts.next().is_none() {
                    out.push(e.path());
                }
            }
        }
    }
    out
}

#[cfg(not(unix))]
fn bench_workspaces() -> Vec<PathBuf> {
    Vec::new()
}

/// Bench smoke: a short firehose run of the real `appkeeper-bench` binary
/// (spawning its own isolated daemon on a random port) — verifies the bench
/// wiring, the JSON report contract and the cleanup guarantee.
fn bench_pass(bin: &Path, ws: &Workspace) -> Result<()> {
    step("L. bench smoke: appkeeper-bench firehose cycles its own isolated daemon");
    let bench = build_bench()?;
    let json = ws.root.join("bench-report.json");
    let out = Command::new(&bench)
        .args([
            "--case",
            "firehose",
            "--log-rows",
            "20000",
            "--duration",
            "30",
            "--daemon",
            bin.to_str().unwrap(),
            "--json",
            json.to_str().unwrap(),
        ])
        .output()
        .context("run appkeeper-bench")?;
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    expect(
        out.status.success(),
        &format!("appkeeper-bench exits 0 ({stderr})"),
    )?;
    expect(
        stdout.contains("rows/s") && stdout.contains("integrity"),
        "the human-readable table lands on stdout",
    )?;
    expect(json.is_file(), "the JSON report was written")?;
    let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&json)?)?;
    expect(v["case"] == "firehose", "the report names the case")?;
    expect(
        v["aggregate"]["rows"] == 20000,
        "the row bound is honored exactly (20000 rows)",
    )?;
    expect(
        v["aggregate"]["rows_per_sec"].as_f64().unwrap_or(0.0) > 0.0,
        "rows/s is a positive number",
    )?;
    expect(v["integrity"]["pass"] == true, "integrity passed")?;
    expect(
        v["bin_versions"]["ak"].is_string(),
        "the appkeeper version is recorded in the report",
    )?;
    expect(
        v["wall_time_secs"].as_f64().unwrap_or(0.0) > 0.0,
        "wall_time_secs is a positive number",
    )?;
    expect(
        v["aggregate"]["bytes"].as_u64().unwrap_or(0) > 0
            && v["aggregate"]["bytes_per_sec"].as_f64().unwrap_or(0.0) > 0.0,
        "bytes and bytes/s are reported",
    )?;
    expect(v["rotation_count"] == 0, "firehose rotates nothing")?;
    expect(
        v["programs"][0]["name"] == "appkeeper-bench-firehose",
        "the load program carries the bench prefix",
    )?;
    if cfg!(unix) {
        expect(
            v["daemon_rss"]["peak_kib"].as_u64().unwrap_or(0) > 0,
            "spawn mode samples the daemon RSS",
        )?;
    }
    expect(
        stderr.contains("[bench]"),
        "progress/diagnostics land on stderr",
    )?;
    expect(
        !stdout.contains("[bench]"),
        "stdout stays free of progress noise",
    )?;
    if cfg!(unix) {
        expect(
            pids_with_arg("__generate").is_empty(),
            "no leftover generator processes",
        )?;
    }
    expect(
        bench_workspaces().is_empty(),
        "no leftover bench temp workspaces",
    )?;
    Ok(())
}

// -- tcp control endpoint pass (tcp-control-endpoint) -----------------------------

/// The `[daemon] socket` TCP form: `tcp://127.0.0.1:<port>` with the Bearer
/// token in `auth_token`. The whole pass runs against this one config; the
/// 401 scenarios re-read the SAME endpoint through a config with a wrong
/// token (the daemon-side counterpart of "the fix is in the caller's config").
struct TcpWs {
    /// The bare `host:port` for plain `http://` curls (the config carries the
    /// full `tcp://` URL itself).
    addr: String,
    token: &'static str,
    cfg: PathBuf,
}

/// Plain HTTP over the TCP control endpoint, with an optional Bearer header:
/// the same wire contract `curl_socket` exercises on the IPC form. Returns
/// (status, body).
fn tcp_get(addr: &str, path: &str, token: Option<&str>) -> Result<(u16, String)> {
    let mut cmd = Command::new("curl");
    cmd.arg("-s")
        .arg("-o")
        .arg("-")
        .arg("-w")
        .arg("\n%{http_code}")
        .arg(format!("http://{addr}{path}"));
    if let Some(t) = token {
        cmd.arg("-H").arg(format!("Authorization: Bearer {t}"));
    }
    let out = cmd.output().context("curl the tcp control endpoint")?;
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    let (body, code) = text.rsplit_once('\n').unwrap_or((&text, "0"));
    Ok((code.trim().parse().unwrap_or(0), body.to_string()))
}

/// The CLI against the TCP workspace's config.
fn tcp_cli(bin: &Path, ws: &TcpWs, args: &[&str]) -> Result<String> {
    let (ok, _code, combined) = tcp_cli_try(bin, ws, args);
    if !ok {
        bail!("appkeeper {args:?} failed: {combined}");
    }
    Ok(combined)
}

/// The CLI against the TCP workspace's config, expecting possible FAILURE;
/// returns (success, exit code, stdout+stderr).
fn tcp_cli_try(bin: &Path, ws: &TcpWs, args: &[&str]) -> (bool, i32, String) {
    let out = Command::new(bin)
        .arg("--config")
        .arg(&ws.cfg)
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("run appkeeper {args:?}: {e}"));
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    (
        out.status.success(),
        out.status.code().unwrap_or(-1),
        combined,
    )
}

fn tcp_health_ok(ws: &TcpWs) -> bool {
    matches!(tcp_get(&ws.addr, "/v1/health", None), Ok((200, _)))
}

/// Wait until the program's state is `running` (the built-in startsecs window
/// keeps it in `starting` first) and return its live pid. `status --json` is
/// the machine-readable projection, so the judgment is per-program exact.
fn tcp_wait_running(bin: &Path, ws: &TcpWs, program: &str) -> Result<u32> {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let out = tcp_cli(bin, ws, &["status", "--json"])?;
        let v: serde_json::Value = serde_json::from_str(out.trim())
            .with_context(|| format!("status --json parses: {out:?}"))?;
        let prog = v["programs"]
            .as_array()
            .and_then(|ps| ps.iter().find(|p| p["name"] == program))
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        if prog["state"] == "running" {
            if let Some(pid) = prog["pid"].as_u64() {
                return Ok(pid as u32);
            }
        }
        if Instant::now() > deadline {
            bail!("{program} never reached running (tcp form): {prog}");
        }
        std::thread::sleep(Duration::from_millis(150));
    }
}

fn tcp_wait_dead(ws: &TcpWs) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if !tcp_health_ok(ws) {
            return Ok(());
        }
        if Instant::now() > deadline {
            bail!("the tcp daemon did not shut down");
        }
        std::thread::sleep(Duration::from_millis(150));
    }
}

/// TCP form end-to-end (tcp-control-endpoint D5, tasks 4.4): the daemon and
/// every CLI subcommand speak the SAME protocol over `tcp://` as over the
/// local IPC path, admission rides `auth_token`, and both rejection paths
/// (401 with a wrong token, no-token startup) refuse loudly. The implicit
/// pull-up (`add --apply`, background-daemon spec) is included: the probe
/// must dial the TCP endpoint from a cold start.
fn tcp_control_endpoint_pass(bin: &Path) -> Result<()> {
    step("T. tcp:// control endpoint: same protocol, Bearer admission, full CLI");

    let root = std::env::temp_dir().join(format!("xk-xtask-tcp-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("apps"))?;
    std::fs::create_dir_all(root.join("logs"))?;
    let port = free_port()?;
    let endpoint = format!("tcp://127.0.0.1:{port}");
    let log_dir = root.join("logs").to_string_lossy().replace('\\', "/");
    let app_dir = root.join("apps").to_string_lossy().replace('\\', "/");
    let cfg = root.join("daemon.toml");
    std::fs::write(
        &cfg,
        format!(
            "[daemon]\nsocket = \"{endpoint}\"\nauth_token = \"tcp-e2e-token\"\n\
             log_dir = \"{log_dir}\"\napp_dir = \"{app_dir}\"\nlog_level = \"warn\"\n\
             monitor_interval = 0.5\n\n[app-default]\nrestart = \"always\"\n"
        ),
    )?;
    let ws = TcpWs {
        addr: format!("127.0.0.1:{port}"),
        token: "tcp-e2e-token",
        cfg: cfg.clone(),
    };

    // T0. The combination gate: `tcp://` without a token fails validate AND
    // the daemon refuses to boot (control-plane spec: TCP 无 token 不可启动).
    let no_token_cfg = root.join("daemon-no-token.toml");
    std::fs::write(
        &no_token_cfg,
        format!(
            "[daemon]\nsocket = \"{endpoint}\"\nlog_dir = \"{log_dir}\"\napp_dir = \"{app_dir}\"\n"
        ),
    )?;
    let out = Command::new(bin)
        .arg("--config")
        .arg(&no_token_cfg)
        .args(["validate"])
        .output()
        .context("validate a tcp:// config without auth_token")?;
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    expect(
        !out.status.success(),
        &format!("validate rejects tcp:// without auth_token: {combined:?}"),
    )?;
    expect(
        combined.contains("auth_token") && combined.contains("tcp://"),
        "the refusal names auth_token and the tcp:// form",
    )?;
    let no_token_log = root.join("no-token-daemon.out.log");
    let out_f = std::fs::File::create(&no_token_log)?;
    let err_f = out_f.try_clone()?;
    let mut child = Command::new(bin)
        .arg("--config")
        .arg(&no_token_cfg)
        .arg("daemon")
        .stdout(Stdio::from(out_f))
        .stderr(Stdio::from(err_f))
        .spawn()
        .context("spawn daemon without auth_token")?;
    let mut refused = false;
    for _ in 0..50 {
        match child.try_wait()? {
            Some(st) => {
                refused = !st.success();
                break;
            }
            None => std::thread::sleep(Duration::from_millis(100)),
        }
    }
    if !refused {
        let _ = child.kill();
        let _ = child.wait();
    }
    expect(refused, "the daemon exits non-zero on a tokenless tcp:// config")?;
    expect(
        std::fs::read_to_string(&no_token_log)?.contains("auth_token"),
        "the startup refusal names auth_token",
    )?;

    // T1. Offline, the endpoint answers nothing: exit 3 unreachable, and the
    // implicit pull-up bring the daemon up THROUGH the tcp:// endpoint
    // (background-daemon spec: the readiness probe dials the configured
    // endpoint whatever shape it is).
    let sleep = find_sleep()?;
    let (ok, code, combined) = tcp_cli_try(bin, &ws, &["status"]);
    expect(
        !ok && code == 3 && combined.contains("unreachable"),
        &format!("offline tcp:// status is exit-3 unreachable, got {code}: {combined:?}"),
    )?;
    let out = tcp_cli(
        bin,
        &ws,
        &[
            "add",
            sleep.to_str().unwrap(),
            "--name",
            "tcpdemo",
            "--args",
            "100000",
            "--apply",
        ],
    )?;
    expect(
        out.contains("tcpdemo"),
        &format!("offline add --apply succeeds over tcp://: {out:?}"),
    )?;
    expect(
        root.join("apps/tcpdemo.toml").is_file(),
        "the scaffolded registration landed in app_dir",
    )?;
    expect(
        !root.join("ak.sock").exists() && !root.join("tcp.sock").exists(),
        "a tcp:// daemon creates no socket file",
    )?;

    // T2. Control subcommands over TCP with the correct token: the same
    // verbs, same outputs as the IPC form (control-plane spec: TCP 形态提供
    // 同一套端点).
    let pid = tcp_wait_running(bin, &ws, "tcpdemo")?;
    let out = tcp_cli(bin, &ws, &["status"])?;
    expect(
        out.contains("tcpdemo") && out.contains("running") && !out.contains("starting"),
        &format!("status lists the program over tcp://: {out:?}"),
    )?;
    expect(
        tcp_cli(bin, &ws, &["pid", "tcpdemo"])?.trim() == pid.to_string(),
        "pid prints the live pid over tcp://",
    )?;
    // `log` rides the token'd endpoint too; the sleeper is silent, so the
    // assertion is the exit-0 round trip itself.
    tcp_cli(bin, &ws, &["log", "tcpdemo", "--tail", "5"])?;
    let out = tcp_cli(bin, &ws, &["run", "--", sleep.to_str().unwrap(), "100000", "--name", "tcpadhoc"])?;
    expect(
        out.contains("tcpadhoc") && out.contains("pid"),
        &format!("ad-hoc run registers over tcp://: {out:?}"),
    )?;
    tcp_wait_running(bin, &ws, "tcpadhoc")?;
    // reload + apply ride the token'd endpoint too (a disk edit enters
    // pending, reload previews it, apply lands it).
    std::fs::write(
        root.join("apps/tcpdemo.toml"),
        format!("[app]\nrestart = \"always\"\n\n[program.tcpdemo]\ncommand = {:?}\nargs = [\"200000\"]\nstartsecs = 0.2\n", sleep.display().to_string()),
    )?;
    let out = tcp_cli(bin, &ws, &["reload"])?;
    expect(
        out.contains("tcpdemo") && out.contains("apply"),
        &format!("reload previews the pending change over tcp://: {out:?}"),
    )?;
    let out = tcp_cli(bin, &ws, &["apply"])?;
    expect(
        out.contains("update-and-restart") && out.contains("tcpdemo"),
        &format!("apply lands the change over tcp://: {out:?}"),
    )?;
    let new_pid = tcp_wait_running(bin, &ws, "tcpdemo")?;
    expect(new_pid != pid, "the changed program was restarted (pid changed)")?;
    // stop + start round the gate as well.
    tcp_cli(bin, &ws, &["stop", "tcpadhoc"])?;
    let (ok, _code, combined) = {
        // start-or-hint on a registered program goes through `run <name>`.
        tcp_cli_try(bin, &ws, &["run", "tcpadhoc"])
    };
    expect(ok, &format!("run <name> restarts the stopped program: {combined:?}"))?;
    tcp_wait_running(bin, &ws, "tcpadhoc")?;
    expect(
        tcp_cli(bin, &ws, &["list"])?.contains("tcpdemo"),
        "list works over tcp://",
    )?;

    // T3. Wire contract: /v1/health is credential-free, /v1/status is not
    // (control-plane spec: TCP 形态拒绝匿名请求 / 健康探测免凭据).
    let (code, _) = tcp_get(&ws.addr, "/v1/health", None)?;
    expect(code == 200, "health over tcp:// needs no credential")?;
    let (code, body) = tcp_get(&ws.addr, "/v1/status", None)?;
    expect(code == 401, &format!("anonymous /v1/status is 401, got {code}"))?;
    expect(
        !body.contains("programs") && !body.contains("tcpdemo"),
        "the 401 leaks no program state",
    )?;
    let (code, _) = tcp_get(&ws.addr, "/v1/status", Some("wrong-token"))?;
    expect(code == 401, "a wrong token is 401 too")?;
    let (code, body) = tcp_get(&ws.addr, "/v1/status", Some(ws.token))?;
    expect(code == 200 && body.contains("tcpdemo"), "the right token passes")?;

    // T4. A caller config with the WRONG token: 401 is a business answer —
    // exit 3, the message names auth_token, and it is NOT "unreachable"
    // (control-plane spec: TCP 凭据错误的专属提示).
    let wrong_cfg = root.join("daemon-wrong-token.toml");
    std::fs::write(
        &wrong_cfg,
        format!(
            "[daemon]\nsocket = \"{endpoint}\"\nauth_token = \"not-the-daemons-token\"\n\
             log_dir = \"{log_dir}\"\napp_dir = \"{app_dir}\"\n"
        ),
    )?;
    let out = Command::new(bin)
        .arg("--config")
        .arg(&wrong_cfg)
        .args(["status"])
        .output()
        .context("status with a wrong token")?;
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    expect(
        out.status.code() == Some(3),
        &format!("a rejected credential exits 3, got {:?}", out.status.code()),
    )?;
    expect(
        combined.contains("401") && combined.contains("auth_token"),
        &format!("the error names 401 and the auth_token fix: {combined:?}"),
    )?;
    expect(
        !combined.contains("unreachable"),
        "a 401 is never phrased as daemon unreachable",
    )?;

    // T5. Shutdown through the token'd endpoint; the daemon goes away.
    tcp_cli(bin, &ws, &["shutdown"])?;
    tcp_wait_dead(&ws)?;
    let _ = std::fs::remove_dir_all(&root);
    Ok(())
}

// -- browser pass -----------------------------------------------------------------

/// Drive the embedded console with playwright (node script via npx-resolved
/// module). Kept as a subprocess so xtask carries no node dependency.
fn browser_pass(ws: &Workspace) -> Result<()> {
    step("F. browser: badge appears, app-scoped apply works, result renders");
    let script = browser_script(ws);
    let script_path = ws.root.join("e2e-browser.mjs");
    std::fs::write(&script_path, script)?;

    // Resolve playwright from the npx cache (no fresh install), then run.
    let resolve = r#"
let candidates = [];
const envDir = process.env.HOME + '/.npm/_npx';
try {
  for (const d of require('fs').readdirSync(envDir)) {
    candidates.push(envDir + '/' + d + '/node_modules/playwright');
  }
} catch {}
const local = process.argv[2] + '/webui/node_modules/playwright';
candidates.push(local);
for (const c of candidates) {
  try { require.resolve(c + '/package.json'); console.log(c); process.exit(0); } catch {}
}
console.error('playwright not found'); process.exit(3);
"#;
    let resolve_path = ws.root.join("resolve-playwright.cjs");
    std::fs::write(&resolve_path, resolve)?;
    let pw = Command::new("node")
        .arg(&resolve_path)
        .arg(repo_root())
        .output()
        .context("resolve playwright")?;
    if !pw.status.success() {
        bail!(
            "playwright unavailable ({}): install with `npx playwright install chromium` \
             or rerun with --no-browser",
            String::from_utf8_lossy(&pw.stderr)
        );
    }
    let pw_dir = PathBuf::from(String::from_utf8_lossy(&pw.stdout).trim());

    let status = Command::new("node")
        .env("NODE_PATH", pw_dir.join(".."))
        .arg(&script_path)
        .arg(pw_dir.to_str().unwrap())
        .status()
        .context("run browser e2e script")?;
    if !status.success() {
        bail!("browser pass failed with {status}");
    }
    Ok(())
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask lives in the repo")
        .to_path_buf()
}

/// The browser script asserts the webui-ui spec scenarios. Node-side asserts
/// print `ok:` lines; failures throw and exit non-zero.
fn browser_script(ws: &Workspace) -> String {
    format!(
        r###"// Generated by xtask e2e — drives the embedded console UI.
import {{ createRequire }} from 'node:module';
const require = createRequire(import.meta.url);
const {{ chromium }} = require(process.argv[2] + '/index.js');

const URL = 'http://127.0.0.1:{webui}/';
const ALPHA_CFG = '{alpha}';
const ok = (m) => console.log('    ok: ' + m);

// The overview bar and app view live in shadow roots; pierce with a helper.
const pierce = (el, sel) => el.shadowRoot?.querySelector(sel);

const browser = await chromium.launch({{ headless: true }});
const page = await browser.newPage();
page.setDefaultTimeout(10_000);
try {{
  await page.goto(URL);
  await page.waitForFunction(() => {{
    const app = document.querySelector('ak-app');
    const bar = app?.shadowRoot?.querySelector('ak-overview-bar');
    return !!bar?.shadowRoot?.querySelector('.bar');
  }});
  // No pending initially: the badge must not exist.
  await page.waitForFunction(() => {{
    const app = document.querySelector('ak-app');
    const bar = app?.shadowRoot?.querySelector('ak-overview-bar');
    return bar && !bar.shadowRoot.querySelector('.pending');
  }});
  ok('console loads with no pending badge');

  // Edit alpha's config on disk → badge with count 1.
  const fs = await import('node:fs');
  fs.writeFileSync(ALPHA_CFG, [
    '[app]',
    'restart = "always"',
    '',
    '[program.web]',
    'command = "sleep 100000"',
    'startsecs = 0.2',
    '',
    '[program.worker]',
    'command = "sleep 400000"',
    'startsecs = 0.2',
    '',
  ].join('\n'));

  await page.waitForFunction(() => {{
    const app = document.querySelector('ak-app');
    const bar = app?.shadowRoot?.querySelector('ak-overview-bar');
    const badge = bar?.shadowRoot?.querySelector('.pending .badge');
    return badge && badge.textContent.includes('待应用变更');
  }});
  ok('pending badge appears after the disk edit');

  // Navigate to the alpha app view; the changed row carries the mark.
  await page.goto(URL + '#/app/alpha').catch(() => {{}});
  // SPA uses history routing; drive the tree link instead.
  await page.evaluate(() => {{
    const app = document.querySelector('ak-app');
    const tree = app?.shadowRoot?.querySelector('ak-console-tree');
    const link = [...(tree?.shadowRoot?.querySelectorAll('a') ?? [])].find((a) =>
      a.getAttribute('href')?.includes('/app/alpha'),
    );
    link?.click();
  }});
  await page.waitForFunction(() => {{
    const app = document.querySelector('ak-app');
    const view = app?.shadowRoot?.querySelector('ak-app-overview');
    return !!view?.shadowRoot?.querySelector('.apply-strip button');
  }});
  ok('alpha view shows the apply strip');

  // Confirm dialog: auto-accept.
  page.on('dialog', (d) => d.accept());
  await page.evaluate(() => {{
    const app = document.querySelector('ak-app');
    const view = app?.shadowRoot?.querySelector('ak-app-overview');
    view?.shadowRoot?.querySelector('.apply-strip button')?.click();
  }});

  // Result text renders with the per-program action.
  await page.waitForFunction(() => {{
    const app = document.querySelector('ak-app');
    const view = app?.shadowRoot?.querySelector('ak-app-overview');
    const fb = view?.shadowRoot?.querySelector('.apply-strip .feedback');
    return fb && fb.textContent.includes('update-and-restart');
  }});
  ok('apply result lists the rebuilt program');

  // Badge cleared after the (alpha-scoped) apply.
  await page.waitForFunction(() => {{
    const app = document.querySelector('ak-app');
    const bar = app?.shadowRoot?.querySelector('ak-overview-bar');
    return bar && !bar.shadowRoot.querySelector('.pending');
  }});
  ok('badge clears after the scoped apply');
}} finally {{
  await browser.close();
}}
console.log('BROWSER PASS OK');
"###,
        webui = ws.webui_port,
        // Forward slashes: a raw Windows path would put `\x`-style escapes
        // into the JS string literal (node fs accepts `/` everywhere).
        alpha = ws
            .root
            .join("app-alpha/appkeeper.toml")
            .display()
            .to_string()
            .replace('\\', "/"),
    )
}

// -- entry ----------------------------------------------------------------------

pub(crate) fn run(args: Args) -> Result<()> {
    let started = Instant::now();
    let bin = crate::build_daemon()?;
    let ws = setup_workspace("main")?;
    // The actions scenario app must exist before the daemon boots (bootstrap
    // loads it, restart = "no" keeps everything under our control).
    setup_actions_app(&ws, &bin)?;

    println!(
        "workspace: {}\ndaemon: run (webui 127.0.0.1:{} via [webui]), control endpoint {}",
        ws.root.display(),
        ws.webui_port,
        ws.control_socket.display()
    );
    // Offline scenarios first: the daemon must NOT be running yet.
    offline_pass(&bin, &ws)?;
    actions_offline_pass(&bin, &ws)?;
    config_pass(&bin, &ws)?;
    let mut daemon = spawn_daemon(&bin, &ws)?;
    let result = (|| {
        cli_pass(&bin, &ws)?;
        actions_online_pass(&bin, &ws)?;
        revamp_pass(&bin)?;
        run_daemon_first_pass(&bin)?;
        detach_pass(&bin)?;
        webui_pass(&bin, &ws)?;
        if args.no_browser {
            println!("\n(--no-browser: skipping browser pass)");
        } else {
            browser_pass(&ws)?;
        }
        bench_pass(&bin, &ws)?;
        tcp_control_endpoint_pass(&bin)?;
        Ok(())
    })();

    shutdown_daemon(&ws, &mut daemon);
    let outcome = match result {
        Ok(()) => {
            println!(
                "\nACCEPTANCE E2E PASSED ({:.1}s)",
                started.elapsed().as_secs_f64()
            );
            Ok(())
        }
        Err(e) => {
            eprintln!("\nACCEPTANCE E2E FAILED: {e:#}");
            Err(e)
        }
    };
    if args.keep {
        println!("workspace kept at {}", ws.root.display());
    } else if let Err(e) = std::fs::remove_dir_all(&ws.root) {
        eprintln!("warning: cleanup failed: {e}");
    }
    outcome
}
