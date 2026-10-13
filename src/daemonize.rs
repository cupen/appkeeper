//! Background daemon spawn + readiness polling (background-daemon spec).
//!
//! Two entry points share this module: the explicit `ak daemon --detach`
//! and the implicit pull-up performed by the "take effect now" entries
//! (`add --apply` and the run ad-hoc form) when the daemon is offline.
//!
//! Protocol (design D6): spawn `current_exe() daemon -c <config>` in a
//! detached process group (unix `process_group(0)`, windows
//! `CREATE_NEW_PROCESS_GROUP | DETACHED_PROCESS`), redirect stdout/stderr
//! (append) to `{log_dir}/daemon.out.log`, then poll `/v1/health` every
//! ~200ms for up to ~10s. There is no lock file: a second instance loses the
//! port race and exits on its own (the existing "bind conflict" behavior),
//! and any caller that observes a healthy daemon is satisfied — it never
//! fails because its own child exited.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};

use crate::client::Client;
use crate::config::DaemonConfig;

/// Poll interval for the readiness probe.
pub const HEALTH_POLL_INTERVAL: Duration = Duration::from_millis(200);
/// Total readiness budget before the pull-up is declared failed.
pub const READY_TIMEOUT: Duration = Duration::from_secs(10);
/// Name of the redirected daemon stdout/stderr log inside `log_dir`.
pub const DAEMON_LOG_NAME: &str = "daemon.out.log";

/// Outcome of a pull-up attempt.
#[derive(Debug)]
pub enum SpawnOutcome {
    /// A healthy daemon answered within the budget (it may be our child or a
    /// concurrent instance — the caller only cares that it is serving).
    Ready { already_online: bool },
    /// No healthy daemon within the budget. `log_path` names where the
    /// background daemon's stdout/stderr went (for the error message).
    TimedOut { log_path: PathBuf },
}

/// `{log_dir}/daemon.out.log` resolved against the config directory.
pub fn daemon_log_path(config: &DaemonConfig, config_dir: &Path) -> PathBuf {
    crate::config::resolve_path(&config.daemon.log_dir, config_dir).join(DAEMON_LOG_NAME)
}

/// Ensure a daemon is serving: probe first (online short-circuit), otherwise
/// spawn a detached daemon and poll until it is healthy or the budget runs
/// out. Never binds a lock: a concurrent pull-up just races on the port and
/// the loser self-terminates.
pub fn ensure_daemon(config: &DaemonConfig, config_path: &Path, config_dir: &Path) -> SpawnOutcome {
    // Short-timeout probes: a listener that accepts TCP but never answers
    // HTTP must not eat the readiness budget (see `Client::probe`).
    let probe = Client::probe(config);
    if probe.health().is_ok() {
        return SpawnOutcome::Ready {
            already_online: true,
        };
    }
    let log_path = daemon_log_path(config, config_dir);
    let _ = spawn_detached(config_path, &log_path);
    match wait_ready(&probe, READY_TIMEOUT) {
        true => SpawnOutcome::Ready {
            already_online: false,
        },
        false => SpawnOutcome::TimedOut { log_path },
    }
}

/// Poll `/v1/health` until it answers or `timeout` elapses.
pub fn wait_ready(client: &Client, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if client.health().is_ok() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(HEALTH_POLL_INTERVAL);
    }
}

/// Build the re-exec command for the background daemon: `ak daemon -c
/// <config>`. `daemon` is the only daemon entry (daemon-subcommand spec);
/// pure so the argv contract is unit-testable.
fn daemon_command(exe: &Path, config_path: &Path) -> Command {
    let mut cmd = Command::new(exe);
    cmd.arg("daemon").arg("-c").arg(config_path);
    cmd
}

/// Spawn `ak daemon -c <config>` detached, with stdout/stderr appended to
/// `log_path`. Returns the child pid for diagnostics.
pub fn spawn_detached(config_path: &Path, log_path: &Path) -> Result<u32> {
    let exe = std::env::current_exe().context("cannot locate the appkeeper executable")?;
    if let Some(dir) = log_path.parent() {
        std::fs::create_dir_all(dir)
            .with_context(|| format!("cannot create log dir {}", dir.display()))?;
    }
    let out = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)
        .with_context(|| format!("cannot open {}", log_path.display()))?;
    let err = out
        .try_clone()
        .with_context(|| format!("cannot duplicate {}", log_path.display()))?;

    let mut cmd = daemon_command(&exe, config_path);
    cmd.stdin(Stdio::null())
        .stdout(Stdio::from(out))
        .stderr(Stdio::from(err));
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // Own process group: the caller's terminal Ctrl+C / exit cannot reach it.
        cmd.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // CREATE_NEW_PROCESS_GROUP | DETACHED_PROCESS: no console, own group.
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        cmd.creation_flags(CREATE_NEW_PROCESS_GROUP | DETACHED_PROCESS);
    }
    let child = cmd
        .spawn()
        .with_context(|| format!("cannot spawn the daemon ({})", log_path.display()))?;
    let pid = child.id();
    // Do not wait: the child is detached. Dropping the handle leaves it
    // running (it is not killed on drop).
    drop(child);
    Ok(pid)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg_with(port: u16, log_dir: &Path) -> DaemonConfig {
        let mut c: DaemonConfig = toml::from_str(&format!("[daemon]\nport = {port}\n")).unwrap();
        c.daemon.log_dir = log_dir.to_path_buf();
        c
    }

    fn tmp_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("xk-daemonize-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// The background daemon log path is `{log_dir}/daemon.out.log`, resolved
    /// against the config directory.
    #[test]
    fn daemon_log_path_resolves_against_config_dir() {
        let dir = tmp_dir("logpath");
        let cfg = cfg_with(7310, Path::new("logs"));
        let p = daemon_log_path(&cfg, &dir);
        assert_eq!(p, dir.join("logs").join("daemon.out.log"));
        // An absolute log_dir ignores the config dir.
        let abs = cfg_with(7310, &dir.join("abs"));
        assert_eq!(
            daemon_log_path(&abs, Path::new("/elsewhere")),
            dir.join("abs").join("daemon.out.log")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A reachable daemon short-circuits: `ensure_daemon` reports it was
    /// already online and never spawns anything (the log file is not created).
    #[test]
    fn ensure_daemon_short_circuits_when_online() {
        let dir = tmp_dir("online");
        // A fake health answerer on a free port.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (mut sock, _) = listener.accept().unwrap();
            let mut buf = [0u8; 4096];
            let _ = std::io::Read::read(&mut sock, &mut buf);
            let body = "{}";
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = std::io::Write::write_all(&mut sock, resp.as_bytes());
        });
        let cfg = cfg_with(port, &dir);
        let config_path = dir.join("daemon.toml");
        std::fs::write(&config_path, "").unwrap();
        match ensure_daemon(&cfg, &config_path, &dir) {
            SpawnOutcome::Ready {
                already_online: true,
            } => {}
            other => panic!("online must short-circuit, got {other:?}"),
        }
        assert!(
            !daemon_log_path(&cfg, &dir).exists(),
            "no spawn happened, so no daemon log was created"
        );
        server.join().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// An unsatisfiable config (port never answers, child fails to bind) ends
    /// in `TimedOut` naming the background log path. Uses a tiny timeout so
    /// the test stays fast.
    #[test]
    fn wait_ready_times_out_and_names_log() {
        let dir = tmp_dir("timeout");
        // loopback:1 is never open.
        let cfg = cfg_with(1, &dir);
        let client = Client::from_config(&cfg);
        let t0 = Instant::now();
        assert!(
            !wait_ready(&client, Duration::from_millis(300)),
            "no daemon on :1"
        );
        assert!(t0.elapsed() >= Duration::from_millis(250));
        assert_eq!(
            daemon_log_path(&cfg, &dir),
            dir.join("daemon.out.log"),
            "the timeout names where the daemon's output went"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The re-exec argv targets the `daemon` subcommand (daemon-subcommand
    /// spec): `daemon -c <config>` — the old `run` entry would be intercepted
    /// with exit 2 before clap, silently breaking the pull-up protocol.
    #[test]
    fn reexec_targets_the_daemon_subcommand() {
        let cmd = daemon_command(
            Path::new("/usr/local/bin/ak"),
            Path::new("/etc/appkeeper/daemon.toml"),
        );
        let args: Vec<String> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            args,
            vec!["daemon", "-c", "/etc/appkeeper/daemon.toml"],
            "the background daemon must be re-exec'd via `ak daemon -c <config>`"
        );
        assert_eq!(
            cmd.get_program().to_string_lossy(),
            "/usr/local/bin/ak",
            "the program is the current executable path"
        );
    }

    /// `spawn_detached` creates the log file (append) and reports a pid; the
    /// process is a real appkeeper invocation pointed at the given config.
    /// Guarded to unix (it actually starts a process).
    #[test]
    #[cfg(unix)]
    fn spawn_detached_creates_log_and_returns_pid() {
        let dir = tmp_dir("spawn");
        let log = dir.join("logs").join("daemon.out.log");
        let config_path = dir.join("daemon.toml");
        std::fs::write(&config_path, "[daemon]\nport = 1\n").unwrap();
        let pid = spawn_detached(&config_path, &log).expect("spawn");
        assert!(pid > 0);
        assert!(log.exists(), "log file created");
        // Give it a moment; the daemon will fail to bind :1 and exit, which is
        // fine — we only assert the plumbing.
        std::thread::sleep(Duration::from_millis(200));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
