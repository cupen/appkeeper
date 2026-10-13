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

// -- run ad-hoc offline cold start ------------------------------------------

/// background-daemon: the ad-hoc form pulls the daemon up when it is offline,
/// registers the command line, and takes effect immediately (exit 0).
#[test]
#[ignore = "spawns a real daemon; run with --ignored"]
fn run_adhoc_offline_cold_start() {
    let ws = Ws::new("adhoc-cold");
    // The out-of-box shape: `config --init` writes [app-default] restart = "always",
    // which is what makes a cold-start command actually run (the built-in
    // cascade default is `no`).
    ws.append_raw("[app-default]\nrestart = \"always\"\n");
    assert!(!ws.health(), "the daemon must start offline");

    let sleep = sleep_bin();
    let (ok, code, out) = ws.cli(&["run", "--", &sleep, "--name", "cold"]);
    assert!(ok, "offline run -- must succeed (exit {code}): {out}");
    assert!(
        !out.contains("daemon is offline") && !out.contains("daemon unreachable"),
        "no more 'daemon offline' error: {out}"
    );
    assert!(
        ws.root.join("apps/cold.toml").is_file(),
        "the ad-hoc registration was written"
    );
    ws.wait_health(true);
    ws.wait_running("cold");
    ws.shutdown();
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
