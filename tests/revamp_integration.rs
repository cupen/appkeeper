//! cli-revamp integration tests: the CLI against a real daemon binary.
//!
//! These span process boundaries (spawned daemons, background pull-up, the
//! persisted user-stop record) and take seconds each, so — like the bench
//! integration suite — they are `#[ignore]`d by default. Run them explicitly:
//!
//! ```sh
//! cargo test --test revamp_integration -- --ignored --test-threads=1
//! ```
//!
//! Coverage targets the cli-revamp BREAKING/high-risk surfaces not pinned by
//! the unit tests: the shutdown-does-not-write-a-record contract, the
//! `unless-stopped` record across repeated daemon restarts, `always` pull-up
//! after a manual stop, the offline `run --` cold start, `daemon --detach`
//! timeout against an occupied port, and `log` dual-stream / multi-program
//! follow aggregation.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// The daemon/CLI binary under test (provided by cargo for integration tests).
fn bin() -> &'static Path {
    Path::new(env!("CARGO_BIN_EXE_ak"))
}

fn free_port() -> u16 {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    l.local_addr().unwrap().port()
}

/// One isolated daemon workspace: temp root with apps/logs, its own config and
/// a random control port.
struct Ws {
    root: PathBuf,
    port: u16,
}

impl Ws {
    fn new(tag: &str) -> Ws {
        let root = std::env::temp_dir().join(format!("xk-revamp-it-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("apps")).unwrap();
        std::fs::create_dir_all(root.join("logs")).unwrap();
        let ws = Ws {
            root,
            port: free_port(),
        };
        ws.write_daemon("");
        ws
    }

    fn cfg(&self) -> PathBuf {
        self.root.join("daemon.toml")
    }

    /// Write the daemon config; `extra` splices raw keys into `[daemon]`.
    fn write_daemon(&self, extra: &str) {
        let log_dir = self.root.join("logs").to_string_lossy().replace('\\', "/");
        let app_dir = self.root.join("apps").to_string_lossy().replace('\\', "/");
        std::fs::write(
            self.cfg(),
            format!(
                "[daemon]\nhost = \"127.0.0.1\"\nport = {}\nlog_dir = \"{log_dir}\"\n\
                 app_dir = \"{app_dir}\"\nlog_level = \"warn\"\nmonitor_interval = 0.3\n{extra}",
                self.port
            ),
        )
        .unwrap();
    }

    /// Append a raw section (e.g. `[app-default]\nrestart = "always"`) after
    /// the `[daemon]` table — the `config --init` out-of-box shape.
    #[allow(dead_code)]
    fn append_raw(&self, raw: &str) {
        let mut text = std::fs::read_to_string(self.cfg()).unwrap();
        text.push('\n');
        text.push_str(raw);
        std::fs::write(self.cfg(), text).unwrap();
    }

    /// Write a real app config directly into `app_dir` (a plain file is a valid
    /// registration record — the daemon scans the directory).
    fn write_app(&self, name: &str, text: &str) {
        std::fs::write(self.root.join("apps").join(format!("{name}.toml")), text).unwrap();
    }

    fn state_file(&self) -> PathBuf {
        self.root.join("logs/state.json")
    }

    /// Run the CLI against this workspace; returns (ok, exit code, stdout+stderr).
    fn cli(&self, args: &[&str]) -> (bool, i32, String) {
        let out = Command::new(bin())
            .arg("--config")
            .arg(self.cfg())
            .args(args)
            .output()
            .expect("spawn the appkeeper CLI");
        let mut combined = String::from_utf8_lossy(&out.stdout).to_string();
        combined.push_str(&String::from_utf8_lossy(&out.stderr));
        (
            out.status.success(),
            out.status.code().unwrap_or(-1),
            combined,
        )
    }

    fn cli_ok(&self, args: &[&str]) -> String {
        let (ok, code, out) = self.cli(args);
        assert!(ok, "{args:?} must succeed (exit {code}): {out}");
        out
    }

    /// Spawn the daemon in the foreground of a child process.
    fn spawn_daemon(&self) -> Daemon {
        let out = std::fs::File::create(self.root.join("daemon.out.log")).unwrap();
        let err = out.try_clone().unwrap();
        let child = Command::new(bin())
            .arg("--config")
            .arg(self.cfg())
            .arg("daemon")
            .stdout(Stdio::from(out))
            .stderr(Stdio::from(err))
            .spawn()
            .expect("spawn the daemon");
        Daemon { child }
    }

    fn health(&self) -> bool {
        let Ok(mut s) = TcpStream::connect(("127.0.0.1", self.port)) else {
            return false;
        };
        let _ = s.set_read_timeout(Some(Duration::from_millis(500)));
        if s.write_all(b"GET /v1/health HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n")
            .is_err()
        {
            return false;
        }
        let mut buf = Vec::new();
        let _ = s.read_to_end(&mut buf);
        String::from_utf8_lossy(&buf).contains("200")
    }

    fn wait_health(&self, want: bool) {
        let deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < deadline {
            if self.health() == want {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        panic!("daemon health never became {want}");
    }

    /// Poll `ak pid <name>`: Some(pid) when running, None when not.
    fn pid_of(&self, name: &str) -> Option<u32> {
        let (ok, _code, out) = self.cli(&["pid", name]);
        if !ok {
            return None;
        }
        out.trim().parse::<u32>().ok()
    }

    /// Raw control-plane POST with a JSON body; returns (status, body). The
    /// response is read to EOF (the endpoints under test answer with a
    /// Content-Length or a chunked stream).
    fn post_json(&self, path: &str, body: &serde_json::Value) -> (u16, String) {
        let body = body.to_string();
        let req = format!(
            "POST {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\n\
             Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let mut s =
            TcpStream::connect(("127.0.0.1", self.port)).expect("connect the control plane");
        let _ = s.set_read_timeout(Some(Duration::from_secs(120)));
        s.write_all(req.as_bytes()).expect("write the request");
        let mut buf = Vec::new();
        let _ = s.read_to_end(&mut buf);
        let text = String::from_utf8_lossy(&buf).to_string();
        let status = text
            .lines()
            .next()
            .and_then(|l| l.split_whitespace().nth(1))
            .and_then(|c| c.parse().ok())
            .unwrap_or(0);
        let payload = text
            .split_once("\r\n\r\n")
            .map(|(_, b)| b.to_string())
            .unwrap_or_default();
        (status, payload)
    }

    fn wait_running(&self, name: &str) {
        let deadline = Instant::now() + Duration::from_secs(15);
        while Instant::now() < deadline {
            if self.pid_of(name).is_some() {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        panic!("program {name} never reached running");
    }

    fn wait_stopped(&self, name: &str) {
        let deadline = Instant::now() + Duration::from_secs(15);
        while Instant::now() < deadline {
            if self.pid_of(name).is_none() {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        panic!("program {name} never reached stopped");
    }

    /// Ask the daemon to exit and wait for the control port to close.
    fn shutdown(&self) {
        let _ = self.cli(&["shutdown"]);
        self.wait_health(false);
    }
}

impl Drop for Ws {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

struct Daemon {
    child: Child,
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

const SLEEP: &str = "sleep 100000";

/// Absolute path to `sleep`, for the ad-hoc form (whose first token must
/// resolve on PATH/canonicalize).
fn sleep_bin() -> String {
    for p in ["/bin/sleep", "/usr/bin/sleep", "/usr/sbin/sleep"] {
        if Path::new(p).is_file() {
            return p.to_string();
        }
    }
    panic!("no absolute `sleep` binary found for the ad-hoc test");
}

// -- restart policies / the persisted user-stop record ----------------------

/// process-management: a normal `shutdown` (which stops every program) MUST
/// NOT write a user-stop record. Proof: an `unless-stopped` program that was
/// never explicitly stopped comes back up after the daemon is restarted.
#[test]
#[ignore = "spawns a real daemon; run with --ignored"]
fn shutdown_does_not_write_user_stop_record() {
    let ws = Ws::new("shutdown-norecord");
    ws.write_app(
        "demo",
        &format!(
            "[program.us]\ncommand = \"{SLEEP}\"\nstartsecs = 0.2\nrestart = \"unless-stopped\"\n"
        ),
    );
    let d = ws.spawn_daemon();
    ws.wait_health(true);
    ws.wait_running("us");
    assert!(
        !ws.state_file().exists(),
        "no explicit stop happened, so no state file exists yet"
    );

    ws.shutdown();
    drop(d);

    // A plain shutdown stopped `us`; if it had recorded a user stop, the next
    // daemon would keep `us` down. It must be pulled back up instead.
    let _d = ws.spawn_daemon();
    ws.wait_health(true);
    ws.wait_running("us");
    let text = std::fs::read_to_string(ws.state_file()).unwrap_or_default();
    assert!(
        !text.contains("\"us\""),
        "shutdown must not record a user stop: {text:?}"
    );
}

/// process-management: the `unless-stopped` record survives *repeated* daemon
/// restarts (not just one), and an explicit start clears it.
#[test]
#[ignore = "spawns a real daemon; run with --ignored"]
fn unless_stopped_record_survives_repeated_restarts() {
    let ws = Ws::new("us-repeat");
    ws.write_app(
        "demo",
        &format!(
            "[program.us]\ncommand = \"{SLEEP}\"\nstartsecs = 0.2\nrestart = \"unless-stopped\"\n"
        ),
    );
    let d = ws.spawn_daemon();
    ws.wait_health(true);
    ws.wait_running("us");
    ws.cli_ok(&["stop", "us"]);
    ws.wait_stopped("us");
    let text = std::fs::read_to_string(ws.state_file()).unwrap_or_default();
    assert!(text.contains("\"us\""), "stop persisted the record: {text:?}");
    ws.shutdown();
    drop(d);

    for round in 1..=2 {
        let d = ws.spawn_daemon();
        ws.wait_health(true);
        std::thread::sleep(Duration::from_millis(800));
        assert!(
            ws.pid_of("us").is_none(),
            "round {round}: unless-stopped stays stopped across a daemon restart"
        );
        ws.shutdown();
        drop(d);
    }

    // Explicit start clears the record.
    let _d = ws.spawn_daemon();
    ws.wait_health(true);
    ws.cli_ok(&["run", "us"]);
    ws.wait_running("us");
    let text = std::fs::read_to_string(ws.state_file()).unwrap_or_default();
    assert!(
        !text.contains("\"us\""),
        "an explicit start cleared the record: {text:?}"
    );
}

/// process-management: `always` ignores the user-stop record — a manual stop
/// only lasts until the daemon restarts, after which the program is pulled up.
#[test]
#[ignore = "spawns a real daemon; run with --ignored"]
fn always_pull_up_after_manual_stop_and_restart() {
    let ws = Ws::new("always-restart");
    ws.write_app(
        "demo",
        &format!("[program.al]\ncommand = \"{SLEEP}\"\nstartsecs = 0.2\nrestart = \"always\"\n"),
    );
    let d = ws.spawn_daemon();
    ws.wait_health(true);
    ws.wait_running("al");
    ws.cli_ok(&["stop", "al"]);
    ws.wait_stopped("al");
    ws.shutdown();
    drop(d);

    let _d = ws.spawn_daemon();
    ws.wait_health(true);
    ws.wait_running("al");
}

// -- run ad-hoc offline (run-daemon-first) ----------------------------------

/// background-daemon / control-plane: the ad-hoc form is daemon-first — when
/// the daemon is offline it exits 3 and MUST NOT pull a daemon up or write
/// anything into app_dir (run-daemon-first).
#[test]
#[ignore = "spawns a real daemon; run with --ignored"]
fn run_adhoc_offline_exits_3_without_pull_up() {
    let ws = Ws::new("adhoc-offline");
    assert!(!ws.health(), "the daemon must start offline");

    let sleep = sleep_bin();
    let (ok, code, out) = ws.cli(&["run", "--", &sleep, "--name", "cold"]);
    assert!(!ok, "offline run -- must fail: {out}");
    assert_eq!(code, 3, "offline run exits 3: {out}");
    assert!(
        out.contains("unreachable") || out.contains("daemon"),
        "the error explains the daemon is unreachable: {out}"
    );
    assert!(
        !ws.root.join("apps/cold.toml").exists(),
        "no registration is written while offline"
    );
    // No daemon was pulled up: the control port stays free.
    assert!(
        !ws.health(),
        "run must NOT start the daemon in the background"
    );
    ws.shutdown();
}

// -- run daemon-first: online register-and-start ----------------------------

/// The `app_dir` entries (file names, sorted).
fn app_dir_entries(ws: &Ws) -> Vec<String> {
    let mut out: Vec<String> = std::fs::read_dir(ws.root.join("apps"))
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    out.sort();
    out
}

/// Assert nothing was left behind for `name` (no record, no placeholder) and
/// no stray `.toml.pending` anywhere in `app_dir` (app-registry spec).
fn assert_no_residue(ws: &Ws, name: &str) {
    assert!(
        !ws.root.join("apps").join(format!("{name}.toml")).exists(),
        "{name}.toml must not exist: {:?}",
        app_dir_entries(ws)
    );
    assert!(
        !ws.root
            .join("apps")
            .join(format!("{name}.toml.pending"))
            .exists(),
        "{name}.toml.pending must not exist: {:?}",
        app_dir_entries(ws)
    );
    assert!(
        app_dir_entries(ws)
            .iter()
            .all(|f| !f.ends_with(".toml.pending")),
        "no placeholder residue at all: {:?}",
        app_dir_entries(ws)
    );
}

/// control-plane / app-registry: a successful ad-hoc `run --` prints
/// `{program, config_path, pid}` and lands the record only after the program
/// is judged started. `run <name>` then reports the same three fields, and
/// `--json` is the machine-readable form.
#[test]
#[ignore = "spawns a real daemon; run with --ignored"]
fn run_daemon_first_returns_program_config_pid() {
    let ws = Ws::new("rdf-success");
    let d = ws.spawn_daemon();
    ws.wait_health(true);
    let sleep = sleep_bin();

    let out = ws.cli_ok(&["run", "--", &sleep, "300", "--name", "web"]);
    assert!(out.contains("web"), "the report names the program: {out:?}");
    assert!(
        out.contains("apps/web.toml"),
        "the report names the config path: {out:?}"
    );
    let pid = ws.pid_of("web").expect("web is running");
    assert!(
        out.contains(&pid.to_string()),
        "the report carries the live pid ({pid}): {out:?}"
    );
    assert!(
        ws.root.join("apps/web.toml").is_file(),
        "the registration landed: {:?}",
        app_dir_entries(&ws)
    );

    // `run <name>` on the stopped program renders the same fields.
    ws.cli_ok(&["stop", "web"]);
    ws.wait_stopped("web");
    let out = ws.cli_ok(&["run", "web"]);
    assert!(
        out.contains("web") && out.contains("apps/web.toml") && out.contains("pid="),
        "run <name> renders name/config/pid: {out:?}"
    );

    // `--json` is one machine-readable object.
    ws.cli_ok(&["stop", "web"]);
    ws.wait_stopped("web");
    let out = ws.cli_ok(&["run", "web", "--json"]);
    let v: serde_json::Value = serde_json::from_str(out.trim()).expect("run --json parses as JSON");
    assert_eq!(v["name"], "web");
    assert!(
        v["config_path"]
            .as_str()
            .unwrap_or("")
            .ends_with("web.toml"),
        "json config_path: {v}"
    );
    assert!(v["pid"].as_u64().is_some(), "json pid: {v}");
    assert_eq!(v["status"], "started");

    ws.shutdown();
    drop(d);
}

/// app-registry: every failure path leaves no `.toml` or `.toml.pending`
/// residue — unresolvable command (exit 2), early exit before `startsecs`,
/// cross-app name collision, and the reserved `all`. A same-name ad-hoc
/// re-submission is not a failure: it regenerates (idempotent or restart).
#[test]
#[ignore = "spawns a real daemon; run with --ignored"]
fn run_daemon_first_failures_leave_no_residue() {
    let ws = Ws::new("rdf-residue");
    let d = ws.spawn_daemon();
    ws.wait_health(true);
    let sleep = sleep_bin();

    // Unresolvable command → exit 2, nothing written.
    let (ok, code, out) = ws.cli(&["run", "--", "definitely-not-a-real-cmd", "--x"]);
    assert!(!ok, "unresolvable command must fail: {out}");
    assert_eq!(code, 2, "command resolution failure exits 2: {out}");
    assert_no_residue(&ws, "definitely-not-a-real-cmd");

    // Exits before startsecs → non-zero, nothing written.
    let (ok, code, out) = ws.cli(&["run", "--", &sleep, "0.05", "--name", "flaky"]);
    assert!(!ok, "early exit must fail: {out}");
    assert_ne!(code, 0, "early exit is non-zero: {out}");
    assert!(
        out.contains("startsecs") || out.contains("startup"),
        "the error names the startup failure: {out:?}"
    );
    assert_no_residue(&ws, "flaky");

    // A cross-app program-name collision is refused (control-plane spec:
    // 名称冲突被拒绝 — 与其他应用冲突). Register an app that owns the
    // program name `clash`, then ad-hoc `--name clash`.
    ws.cli_ok(&["run", "--", &sleep, "300", "--name", "web"]);
    let owner = ws.root.join("owner-app");
    std::fs::create_dir_all(&owner).unwrap();
    std::fs::write(
        owner.join("appkeeper.toml"),
        format!("[program.clash]\ncommand = \"{sleep}\"\nargs = [\"300\"]\nstartsecs = 0.2\n"),
    )
    .unwrap();
    ws.cli_ok(&["add", owner.to_str().unwrap(), "--name", "owner"]);
    ws.cli_ok(&["apply", "owner"]);
    let (ok, code, out) = ws.cli(&["run", "--", &sleep, "301", "--name", "clash"]);
    assert!(!ok && code != 0, "cross-app collision must fail: {out}");
    assert!(
        out.contains("already registered"),
        "the collision is explained: {out:?}"
    );
    assert!(
        out.contains("owner"),
        "the error names the owning app: {out:?}"
    );
    assert_no_residue(&ws, "clash");

    // A same-name ad-hoc re-submission is NOT a collision: it regenerates
    // (app-registry: 同名全量再生). Identical content is idempotent.
    let web_pid = ws.pid_of("web").expect("web is running");
    let (ok, _code, out) = ws.cli(&["run", "--", &sleep, "300", "--name", "web"]);
    assert!(ok, "identical same-name re-run succeeds: {out}");
    assert_eq!(ws.pid_of("web"), Some(web_pid), "idempotent, no restart");
    // Changed content restarts with a new pid and rewrites the record.
    let (ok, _code, out) = ws.cli(&["run", "--", &sleep, "301", "--name", "web"]);
    assert!(ok, "changed same-name re-run succeeds: {out}");
    let web_text = std::fs::read_to_string(ws.root.join("apps/web.toml")).unwrap();
    assert!(web_text.contains("301"), "the record was regenerated: {web_text:?}");
    assert_ne!(ws.pid_of("web"), Some(web_pid), "the process restarted");
    assert!(
        !ws.root.join("apps/web.toml.pending").exists(),
        "regeneration leaves no placeholder"
    );

    // Reserved `all`.
    let (ok, _code, out) = ws.cli(&["run", "--", &sleep, "302", "--name", "all"]);
    assert!(!ok, "reserved `all` must fail: {out}");
    assert!(
        out.contains("reserved"),
        "the reserved word is explained: {out:?}"
    );
    assert_no_residue(&ws, "all");

    ws.shutdown();
    drop(d);
}

/// app-registry (D2): while the startup judgement is pending only the
/// non-`.toml` placeholder exists — `ak list` and the pending projection never
/// see a half-written registration.
#[test]
#[ignore = "spawns a real daemon; run with --ignored"]
fn run_daemon_first_staging_is_invisible_to_scanners() {
    let ws = Ws::new("rdf-atomic");
    let d = ws.spawn_daemon();
    ws.wait_health(true);
    let sleep = sleep_bin();

    // The built-in startsecs is 3.0s: the staging window is comfortably long.
    let out_path = ws.root.join("atomic.out");
    let out = std::fs::File::create(&out_path).unwrap();
    let mut child = Command::new(bin())
        .arg("--config")
        .arg(ws.cfg())
        .args(["run", "--", &sleep, "400", "--name", "atomic"])
        .stdout(Stdio::from(out))
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn the ad-hoc run");

    std::thread::sleep(Duration::from_millis(800));
    assert!(
        !ws.root.join("apps/atomic.toml").exists(),
        "no record while the judgement is pending"
    );
    assert!(
        ws.root.join("apps/atomic.toml.pending").is_file(),
        "the placeholder exists mid-flight: {:?}",
        app_dir_entries(&ws)
    );
    let listed = ws.cli_ok(&["list"]);
    assert!(
        !listed.contains("atomic"),
        "`ak list` excludes the in-flight app: {listed:?}"
    );

    // A reload mid-flight must not disturb the staging.
    ws.cli_ok(&["reload"]);
    let status = child.wait().expect("wait for the ad-hoc run");
    assert!(status.success(), "the ad-hoc run still succeeds ({status})");
    assert!(
        ws.root.join("apps/atomic.toml").is_file()
            && !ws.root.join("apps/atomic.toml.pending").exists(),
        "the placeholder became the record: {:?}",
        app_dir_entries(&ws)
    );
    assert!(ws.pid_of("atomic").is_some(), "atomic is running");

    ws.shutdown();
    drop(d);
}

/// app-registry (D4): a failed rename keeps the process and prints the full
/// config; a re-run detects the residual, refuses non-interactively without
/// restarting, and (via the control-plane commit) re-lands the record with the
/// pid unchanged.
#[test]
#[ignore = "spawns a real daemon; run with --ignored"]
fn run_daemon_first_partial_failure_and_residual() {
    let ws = Ws::new("rdf-residual");
    let d = ws.spawn_daemon();
    ws.wait_health(true);
    let sleep = sleep_bin();

    // Force the rename to fail after startup succeeded: a directory named
    // `<name>.toml` makes the same-directory rename fail with EISDIR.
    std::fs::create_dir_all(ws.root.join("apps/weird.toml")).unwrap();
    let (ok, code, out) = ws.cli(&["run", "--", &sleep, "400", "--name", "weird"]);
    assert!(!ok && code != 0, "the partial failure is non-zero: {out}");
    assert!(
        out.contains("registration was not written"),
        "it explains the process is unregistered: {out:?}"
    );
    assert!(
        out.contains("command") && out.contains("args") && out.contains("work_dir"),
        "it prints the full config: {out:?}"
    );
    let pid = ws.pid_of("weird").expect("the process was kept running");
    assert!(
        ws.root.join("apps/weird.toml.pending").is_file(),
        "the placeholder is kept for diagnosis"
    );

    // A non-interactive re-run refuses and must not restart the process.
    let (ok, code, out) = ws.cli(&["run", "--", &sleep, "400", "--name", "weird"]);
    assert!(!ok && code != 0, "a non-TTY re-run refuses: {out}");
    assert!(
        out.contains("already running") && out.contains("not registered"),
        "it reports the residual: {out:?}"
    );
    assert_eq!(ws.pid_of("weird"), Some(pid), "the refusal did not restart");
    assert!(
        !ws.root.join("apps/weird.toml").is_file(),
        "the refusal wrote no record"
    );

    // Confirm the residual through the control plane: the record lands and the
    // process is reused (pid unchanged).
    std::fs::remove_dir(ws.root.join("apps/weird.toml")).unwrap();
    let (status, body) = ws.post_json("/v1/run/commit", &serde_json::json!({ "name": "weird" }));
    assert_eq!(status, 200, "commit answers 200: {body}");
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["pid"].as_u64(), Some(u64::from(pid)), "the pid is reused");
    assert!(
        ws.root.join("apps/weird.toml").is_file()
            && !ws.root.join("apps/weird.toml.pending").exists(),
        "the record landed: {:?}",
        app_dir_entries(&ws)
    );
    assert_eq!(
        ws.pid_of("weird"),
        Some(pid),
        "the process was not restarted"
    );

    ws.shutdown();
    drop(d);
}

/// configuration / process-management: the built-in defaults changed to
/// `app_dir = "app.d"` and `startsecs = 3.0`; an explicit value still wins.
#[test]
#[ignore = "spawns a real daemon; run with --ignored"]
fn defaults_are_app_d_and_startsecs_three() {
    let ws = Ws::new("rdf-defaults");
    // A fresh config that does NOT pin `app_dir`: `config --get` answers the
    // built-in default.
    let fresh = ws.root.join("fresh.toml");
    std::fs::write(&fresh, "[daemon]\nport = 7310\n").unwrap();
    let out = Command::new(bin())
        .arg("--config")
        .arg(&fresh)
        .args(["config", "--get", "app_dir"])
        .output()
        .expect("run config --get app_dir");
    assert!(out.status.success(), "config --get succeeds");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        "app.d",
        "app_dir default is app.d"
    );

    // The built-in startsecs is 3.0: a program with no explicit startsecs takes
    // ~3s to be judged started. `run <name>` blocks for that window, so the
    // elapsed time is the observable proof.
    ws.write_app(
        "defaults",
        &format!("[program.d3]\ncommand = \"{SLEEP}\"\nrestart = \"no\"\n"),
    );
    let d = ws.spawn_daemon();
    ws.wait_health(true);
    let t0 = Instant::now();
    ws.cli_ok(&["run", "d3"]);
    let elapsed = t0.elapsed();
    assert!(
        elapsed >= Duration::from_millis(2700),
        "the built-in startsecs is ~3s (run blocked {elapsed:?})"
    );
    ws.shutdown();
    drop(d);
}

// -- daemon --detach --------------------------------------------------------

/// background-daemon: two concurrent implicit pull-ups end with exactly one
/// daemon serving, and both callers succeed (the loser self-terminates on the
/// port race; the health-only check makes it a success).
#[test]
#[ignore = "spawns a real daemon; run with --ignored"]
fn concurrent_pull_up_yields_one_daemon() {
    let ws = Ws::new("concurrent-pull");
    assert!(!ws.health(), "the daemon must start offline");
    let cfg = ws.cfg();
    let spawn = || {
        let cfg = cfg.clone();
        std::thread::spawn(move || {
            let out = Command::new(bin())
                .arg("--config")
                .arg(&cfg)
                .args(["daemon", "--detach"])
                .output()
                .expect("spawn daemon --detach");
            (
                out.status.success(),
                String::from_utf8_lossy(&out.stdout).to_string()
                    + &String::from_utf8_lossy(&out.stderr),
            )
        })
    };
    let a = spawn();
    let b = spawn();
    let (ok_a, out_a) = a.join().unwrap();
    let (ok_b, out_b) = b.join().unwrap();
    assert!(ok_a, "first pull-up succeeds: {out_a}");
    assert!(ok_b, "second pull-up succeeds: {out_b}");
    ws.wait_health(true);

    // Exactly one daemon owns the port: a plain bind must fail.
    assert!(
        std::net::TcpListener::bind(("127.0.0.1", ws.port)).is_err(),
        "exactly one daemon survives and holds the control port"
    );
    ws.shutdown();
}

/// background-daemon: when the control port is occupied by a foreign process
/// the background daemon cannot bind, so `daemon --detach` exits non-zero and
/// names the background log — no silent success.
///
/// The holder answers HTTP with 500 (so the health probe fails fast) yet keeps
/// the port bound; the readiness loop then exhausts its budget. (A holder that
/// accepts but never answers is the separate, currently-unbounded case pinned
/// by `detach_readiness_is_bounded`.)
#[test]
#[ignore = "spawns a real daemon; run with --ignored"]
fn daemon_detach_times_out_when_port_is_occupied() {
    let ws = Ws::new("detach-occupied");
    let holder = std::net::TcpListener::bind(("127.0.0.1", ws.port)).unwrap();
    std::thread::spawn(move || {
        for s in holder.incoming().flatten() {
            let mut s = s;
            let _ = s.write_all(
                b"HTTP/1.1 500 Internal Server Error\r\nContent-Length: 2\r\n\
                  Connection: close\r\n\r\n{}",
            );
        }
    });
    let (ok, code, out) = ws.cli(&["daemon", "--detach"]);
    assert!(!ok, "daemon --detach must fail on an occupied port: {out}");
    assert_ne!(code, 0, "non-zero exit, got {code}");
    assert!(
        out.contains("daemon.out.log"),
        "the error names the background log: {out}"
    );
}

/// background-daemon / design D6: the readiness probe is bounded (~10s
/// budget) even when the control port is held by a listener that accepts
/// connections but never answers HTTP — the probe uses its own short-timeout
/// client, so one hung listener cannot eat the whole window.
#[test]
#[ignore = "spawns a real daemon; run with --ignored"]
fn detach_readiness_is_bounded() {
    let ws = Ws::new("detach-bound");
    // A listener that accepts connections but never answers HTTP.
    let holder = std::net::TcpListener::bind(("127.0.0.1", ws.port)).unwrap();
    std::thread::spawn(move || {
        let mut kept = Vec::new();
        for s in holder.incoming().flatten() {
            kept.push(s); // hold the connection open, never respond
        }
    });
    let t0 = Instant::now();
    let (_ok, _code, _out) = ws.cli(&["daemon", "--detach"]);
    let elapsed = t0.elapsed();
    assert!(
        elapsed < Duration::from_secs(30),
        "the readiness probe must be bounded (~10s budget), took {elapsed:?}"
    );
}

// -- log: dual stream + multi-program follow --------------------------------

/// control-plane: `--stream both` surfaces both stdout and stderr lines.
#[cfg(unix)]
#[test]
#[ignore = "spawns a real daemon; run with --ignored"]
fn log_stream_both_shows_out_and_err() {
    let ws = Ws::new("log-both");
    ws.write_app(
        "demo",
        "[program.noisy]\ncommand = \"sh\"\nargs = [\"-c\", \"sleep 0.5; echo OUT-LINE; echo ERR-LINE 1>&2; sleep 100000\"]\nstartsecs = 0.2\nrestart = \"always\"\n",
    );
    let d = ws.spawn_daemon();
    ws.wait_health(true);
    ws.wait_running("noisy");
    std::thread::sleep(Duration::from_millis(600));
    let out = ws.cli_ok(&["log", "noisy", "--stream", "both", "--tail", "10"]);
    assert!(out.contains("OUT-LINE"), "stdout line visible: {out:?}");
    assert!(out.contains("ERR-LINE"), "stderr line visible: {out:?}");
    ws.shutdown();
    drop(d);
}

/// control-plane: multi-program `log -f` interleaves lines with a `[name] `
/// prefix; a single program carries no prefix.
#[cfg(unix)]
#[test]
#[ignore = "spawns a real daemon; run with --ignored"]
fn log_multi_follow_prefixes_each_line() {
    let ws = Ws::new("log-follow");
    ws.write_app(
        "demo",
        "[program.echo-a]\ncommand = \"sh\"\nargs = [\"-c\", \"while true; do echo a-line; sleep 0.2; done\"]\nstartsecs = 0.2\nrestart = \"always\"\n\n\
         [program.echo-b]\ncommand = \"sh\"\nargs = [\"-c\", \"while true; do echo b-line; sleep 0.2; done\"]\nstartsecs = 0.2\nrestart = \"always\"\n",
    );
    let d = ws.spawn_daemon();
    ws.wait_health(true);
    ws.wait_running("echo-a");
    ws.wait_running("echo-b");
    std::thread::sleep(Duration::from_millis(600));

    // Follow runs until interrupted: spawn it, collect output for ~2s, kill.
    let mut child = Command::new(bin())
        .arg("--config")
        .arg(ws.cfg())
        .args(["log", "echo-a", "echo-b", "-f"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn the follow CLI");
    std::thread::sleep(Duration::from_millis(2000));
    let _ = child.kill();
    let out = child.wait_with_output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(text.contains("[echo-a] a-line"), "echo-a prefixed: {text:?}");
    assert!(text.contains("[echo-b] b-line"), "echo-b prefixed: {text:?}");

    // A single program has no prefix.
    let single = ws.cli_ok(&["log", "echo-a", "--tail", "1"]);
    assert!(
        !single.contains("[echo-a]"),
        "single-program log adds no prefix: {single:?}"
    );
    ws.shutdown();
    drop(d);
}

// -- apply scope edge cases -------------------------------------------------

/// apply-workflow: `apply all <program>` is rejected (the `all` keyword takes
/// no program argument) instead of silently applying everything.
#[test]
#[ignore = "spawns a real daemon; run with --ignored"]
fn apply_all_with_program_is_rejected() {
    let ws = Ws::new("apply-all-program");
    ws.write_app(
        "demo",
        &format!("[program.p]\ncommand = \"{SLEEP}\"\nstartsecs = 0.2\nrestart = \"always\"\n"),
    );
    let d = ws.spawn_daemon();
    ws.wait_health(true);
    let (ok, code, out) = ws.cli(&["apply", "all", "p"]);
    assert!(!ok, "apply all <program> must fail: {out}");
    assert_ne!(code, 0);
    assert!(
        out.contains("full-scope") || out.contains("takes no program"),
        "the error explains the reserved keyword: {out:?}"
    );
    ws.shutdown();
    drop(d);
}

/// control-plane: an unknown program via the start form is a 404 → exit 1.
#[test]
#[ignore = "spawns a real daemon; run with --ignored"]
fn run_unknown_program_exits_one() {
    let ws = Ws::new("run-unknown");
    let d = ws.spawn_daemon();
    ws.wait_health(true);
    let (ok, code, out) = ws.cli(&["run", "nosuch"]);
    assert!(!ok && code == 1, "unknown program exits 1, got {code}: {out}");
    assert!(
        out.contains("404") || out.contains("unknown program"),
        "the error carries the 404/unknown-program message: {out:?}"
    );
    ws.shutdown();
    drop(d);
}

// -- add --restart over a legacy [app] table --------------------------------

/// configuration spec: every write path writes only the new `restart` field.
/// Registering with `--restart` over a pre-revamp deployment whose `[app]`
/// table still carries `autostart`/`autorestart` must retire the deprecated
/// keys in that layer — leaving them behind would mix new + old (which the
/// same spec's mixing rule rejects), so `validate` would fail and the daemon
/// would skip the app the user just registered.
#[test]
#[ignore = "spawns a real daemon; run with --ignored"]
fn add_restart_over_legacy_app_table_stays_valid() {
    let ws = Ws::new("legacy-migrate");
    // A deployment directory in the pre-revamp shape.
    let deploy = ws.root.join("deploy");
    std::fs::create_dir_all(&deploy).unwrap();
    std::fs::write(
        deploy.join("appkeeper.toml"),
        "[app]\nautorestart = \"always\"\n\n[program.p]\ncommand = \"sleep 100000\"\nstartsecs = 0.2\n",
    )
    .unwrap();

    let (ok, code, out) = ws.cli(&[
        "add",
        deploy.to_str().unwrap(),
        "--name",
        "legacy",
        "--restart",
        "on-failure",
    ]);
    assert!(ok, "add must succeed (exit {code}): {out}");

    // The written file must not mix the new and deprecated fields in one layer.
    let text = std::fs::read_to_string(deploy.join("appkeeper.toml")).unwrap();
    assert!(
        !(text.contains("restart =")
            && (text.contains("autorestart") || text.contains("autostart"))),
        "add must migrate the legacy fields, not mix them: {text}"
    );
    let (ok, _code, out) = ws.cli(&["validate", deploy.join("appkeeper.toml").to_str().unwrap()]);
    assert!(ok, "the migrated app must still validate: {out}");

    // And the daemon must actually load it.
    let d = ws.spawn_daemon();
    ws.wait_health(true);
    ws.wait_running("p");
    ws.shutdown();
    drop(d);
}
