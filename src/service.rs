//! System service registration: `ak daemon install|uninstall`.
//!
//! Linux: writes a systemd unit (default `/etc/systemd/system/ak.service`,
//! override with `--unit-file`), then drives `systemctl` to reload/enable/start
//! it. Windows: registers an SCM service (auto-start, failure-restart after
//! 3s, graceful stop with wait hints) via the `windows-service` crate plus a
//! few direct `windows-sys` calls where the crate cannot express what we need
//! (exactly-quoted binpaths, description read-back).
//!
//! All template rendering, path/name validation, account mapping and write
//! decisions are platform-independent pure functions so they are testable
//! everywhere.

use std::fmt;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, bail};
use clap::ValueEnum;

/// Default `--unit-file` value (Linux).
#[cfg_attr(windows, allow(dead_code))]
pub const DEFAULT_UNIT_FILE: &str = "/etc/systemd/system/ak.service";
/// Default Windows service name.
pub const DEFAULT_SERVICE_NAME: &str = "ak";
/// Default service description (both platforms).
pub const DEFAULT_DESCRIPTION: &str = "appkeeper process keeper";
/// Fallback stop budget when the config cannot be loaded.
#[cfg_attr(windows, allow(dead_code))]
const FALLBACK_TIMEOUT_STOP_SEC: u64 = 90;

// The pure helpers below run on one platform in production but are exercised
// by the unit tests on every platform, hence the windows dead_code allowances.
/// systemd `Restart=` policies accepted by `daemon install --restart`.
/// Variants render as their exact systemd spelling in the unit file; the
/// ValueEnum derive lists the same names as `[possible values:]` in `--help`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum RestartPolicy {
    /// Never restart (systemd's own default)
    No,
    /// Restart on clean exit only
    OnSuccess,
    /// Restart on non-zero exit, signal, timeout or watchdog
    OnFailure,
    /// Restart on signal, timeout or watchdog — not on plain exit codes
    OnAbnormal,
    /// Restart on watchdog timeout only
    OnWatchdog,
    /// Restart on abnormal termination by signal only
    OnAbort,
    /// Restart regardless of exit status (also after clean exit)
    Always,
}

impl fmt::Display for RestartPolicy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            RestartPolicy::No => "no",
            RestartPolicy::OnSuccess => "on-success",
            RestartPolicy::OnFailure => "on-failure",
            RestartPolicy::OnAbnormal => "on-abnormal",
            RestartPolicy::OnWatchdog => "on-watchdog",
            RestartPolicy::OnAbort => "on-abort",
            RestartPolicy::Always => "always",
        };
        f.write_str(name)
    }
}
#[derive(Debug, Clone)]
pub struct ServiceOptions {
    /// Linux: full path of the unit file to write/remove (must end in
    /// `.service`). Windows: rejected — use `name`.
    pub unit_file: Option<PathBuf>,
    /// Windows: service name (default `ak`). Linux: rejected — the unit
    /// name comes from `--unit-file`.
    pub name: Option<String>,
    /// Service description (default `appkeeper process keeper`); SCM
    /// description on Windows, `Description=` in the unit on Linux.
    pub desc: Option<String>,
    /// Linux: `User=` the service runs as. Windows: builtin account only
    /// (`LocalSystem` default / `LocalService` / `NetworkService`).
    pub user: Option<String>,
    /// Overwrite an existing service/unit whose configuration differs.
    pub force: bool,
    /// `systemctl start` right after install.
    pub start: bool,
    /// `Restart=` policy written into the unit (`--restart`, default `always`).
    pub restart: RestartPolicy,
}

// -- platform-independent pure logic ----------------------------------------

/// Platform the service commands run on; the flag checks take it explicitly
/// so both platforms' rejection paths are unit-testable everywhere. Each
/// platform constructs only its own variant outside tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum Platform {
    Unix,
    Windows,
}

impl Platform {
    pub fn current() -> Platform {
        #[cfg(windows)]
        {
            Platform::Windows
        }
        #[cfg(unix)]
        {
            Platform::Unix
        }
    }
}

// -- service-managed detection (harden-daemon-autostart 4.1-4.4) --------------

/// Which service manager (if any) is running this daemon, judged once at
/// startup and injected into the supervisor state (4.3). Pure and
/// platform-agnostic: `platform` picks the criterion, `service_mode` is the
/// Windows `daemon start --service <NAME>` run mode and `lookup_env` is the
/// environment lookup (production passes `std::env::var`; tests pass canned
/// tables). Linux recognizes systemd by the non-empty `INVOCATION_ID` the
/// manager injects on every start; Windows recognizes the SCM by the
/// `--service` run mode alone (service-registration: 服务托管识别).
pub fn managed_source(
    platform: Platform,
    service_mode: bool,
    lookup_env: &dyn Fn(&str) -> Option<String>,
) -> Option<&'static str> {
    match platform {
        Platform::Windows => service_mode.then_some("scm"),
        Platform::Unix => {
            let id = lookup_env("INVOCATION_ID")?;
            (!id.trim().is_empty()).then_some("systemd")
        }
    }
}

/// The systemd unit name carried by one `/proc/self/cgroup` line (4.2):
/// `0::/system.slice/ak.service` (v2 unified) or `7:cpu:/system.slice/…
/// ak.service` (v1) — the unit is the last `:`-separated field's final path
/// segment with its `.service` suffix stripped. Anything else (a scope, the
/// root cgroup, no unit segment) is `None`; the caller falls back to the
/// "current unit" wording (control-plane spec: 服务托管时的停机命令语义).
pub fn unit_name_from_cgroup(line: &str) -> Option<String> {
    // The controller-number prefix is everything up to the last `:` (v1 has
    // several, v2 exactly two). A bare path — no colons — is accepted too.
    let path = line.rsplit_once(':').map(|(_, p)| p).unwrap_or(line);
    let last = path.rsplit('/').find(|s| !s.is_empty())?;
    last.strip_suffix(".service")
        .filter(|s| !s.is_empty())
        .map(String::from)
}

/// The systemd unit this process runs in, from `/proc/self/cgroup` (4.2).
/// `None` when the file is unreadable or the daemon sits in no `.service`
/// cgroup — the shutdown hint then keeps the generic `systemctl stop` shape.
#[cfg(unix)]
pub fn current_systemd_unit() -> Option<String> {
    let text = std::fs::read_to_string("/proc/self/cgroup").ok()?;
    text.lines().find_map(unit_name_from_cgroup)
}

/// Non-unix systems have no cgroup: never service-managed by systemd.
#[cfg(not(unix))]
pub fn current_systemd_unit() -> Option<String> {
    None
}

/// The refusal text for a service-managed daemon (4.4, pure): it names the
/// manager and the stop command the service manager owns — `systemctl stop
/// <unit>` on Linux (unit from the daemon's own cgroup; unknown → the
/// "current unit" wording, still a `systemctl stop` command) and
/// `sc stop <name>` on Windows.
pub fn managed_stop_message(
    source: &str,
    unit: Option<&str>,
    service_name: Option<&str>,
) -> String {
    match source {
        "scm" => format!(
            "daemon is managed by the Windows service manager — stop it with \
             `sc stop {}` (it owns the service lifecycle)",
            service_name.unwrap_or(DEFAULT_SERVICE_NAME)
        ),
        _ => match unit {
            Some(u) => format!(
                "daemon is managed by systemd — stop it with `systemctl stop {u}` \
                 (it owns the service lifecycle)"
            ),
            None => "daemon is managed by systemd — stop it with `systemctl stop <current unit>` \
                     (it owns the service lifecycle)"
                .to_string(),
        },
    }
}

/// The concrete 409 body for `POST /v1/shutdown` on a service-managed daemon
/// (4.4): wires the pure message to the runtime inputs — the daemon's own
/// cgroup unit on Linux, the latched SCM service name on Windows.
pub fn managed_stop_hint(source: &str) -> String {
    managed_stop_message(
        source,
        current_systemd_unit().as_deref(),
        windows_service_name().as_deref(),
    )
}

/// The service name the SCM shell latched (`daemon start --service <name>`);
/// `None` outside the Windows service run mode.
#[cfg(windows)]
pub(crate) fn windows_service_name() -> Option<String> {
    win::latched_service_name()
}

/// Unix never runs under the SCM.
#[cfg(not(windows))]
pub(crate) fn windows_service_name() -> Option<String> {
    None
}

/// Windows service run mode (`daemon start --service`): `true` only inside
/// the SCM dispatcher. Unix is never in service mode.
#[cfg(not(windows))]
pub(crate) fn windows_service_mode() -> bool {
    false
}

/// Reject platform-misused flags and invalid values before any side effect
/// (no file writes, no SCM changes). This is the dispatch gate named by the
/// spec: Linux + `--name`, Windows + `--unit-file`, bad service names and
/// non-builtin `--user` all fail here.
pub fn check_install_flags(platform: Platform, opts: &ServiceOptions) -> Result<()> {
    check_name_flags(platform, opts)?;
    if let Platform::Windows = platform {
        let name = opts.name.as_deref().unwrap_or(DEFAULT_SERVICE_NAME);
        if !is_valid_service_name(name) {
            bail!("invalid service name {name:?} (must be non-empty and contain no '/' or '\\')");
        }
        if let Some(user) = opts.user.as_deref() {
            builtin_account(user)?;
        }
    }
    Ok(())
}

/// Same platform-misuse gate for `uninstall` (no user/desc checks there).
pub fn check_uninstall_flags(platform: Platform, opts: &ServiceOptions) -> Result<()> {
    check_name_flags(platform, opts)?;
    if let Platform::Windows = platform {
        let name = opts.name.as_deref().unwrap_or(DEFAULT_SERVICE_NAME);
        if !is_valid_service_name(name) {
            bail!("invalid service name {name:?} (must be non-empty and contain no '/' or '\\')");
        }
    }
    Ok(())
}

fn check_name_flags(platform: Platform, opts: &ServiceOptions) -> Result<()> {
    match platform {
        Platform::Unix => {
            if opts.name.is_some() {
                bail!(
                    "--name is only supported on Windows (systemd units are named by --unit-file)"
                );
            }
        }
        Platform::Windows => {
            if opts.unit_file.is_some() {
                bail!(
                    "--unit-file is only supported on Linux (systemd); \
                     on Windows the service name is chosen with --name"
                );
            }
        }
    }
    Ok(())
}

/// Windows service names: non-empty, no `/` and no `\` (SCM would misparse
/// both the service key and the binpath).
pub fn is_valid_service_name(name: &str) -> bool {
    !name.is_empty() && !name.contains(['/', '\\'])
}

/// Windows `--user`: builtin passwordless accounts only, case-insensitive,
/// mapped to the `NT AUTHORITY\...` form the SCM stores.
pub fn builtin_account(user: &str) -> Result<&'static str> {
    match user.to_ascii_lowercase().as_str() {
        "localsystem" => Ok("NT AUTHORITY\\SYSTEM"),
        "localservice" => Ok("NT AUTHORITY\\LocalService"),
        "networkservice" => Ok("NT AUTHORITY\\NetworkService"),
        _ => bail!(
            "unsupported --user {user:?}: on Windows only the builtin accounts \
             LocalSystem / LocalService / NetworkService are accepted (no password support)"
        ),
    }
}

/// Quote one token of an SCM command line. Paths containing spaces must be
/// wrapped in double quotes as a whole — backslash-escaping the space does
/// not work for the SCM binary-path parser (CreateService rule).
///
/// Windows-only in production (the binpath is only rendered there); kept
/// under `test` so the pure renderer stays unit-tested on every platform.
#[cfg(any(windows, test))]
fn quote_scm_token(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "\\\""))
}

/// Render the full service binpath: `"<exe>" daemon start --service <name>
/// --config "<conf>"`. Mirrors the Linux unit's ExecStart plus the hidden
/// `--service` run-mode marker the SCM shell needs to know which service
/// name it is running as (background-daemon spec: `--service` lives on
/// `daemon start` and conflicts with `--foreground`).
#[cfg(any(windows, test))]
pub fn render_binpath(exe: &Path, config_path: &Path, service_name: &str) -> String {
    format!(
        "{} daemon start --service {} --config {}",
        quote_scm_token(&exe.display().to_string()),
        service_name,
        quote_scm_token(&config_path.display().to_string())
    )
}

/// Normalize a binpath read back from the SCM before snapshot comparison:
/// trim and collapse incidental whitespace runs between tokens. Applied to
/// both sides, so exactly-rendered binpaths still compare equal.
#[cfg(any(windows, test))]
pub fn normalize_binpath(binpath: &str) -> String {
    binpath.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// systemd unit file paths we accept: file name is a legal unit name
/// (ASCII alphanumerics plus `.` `_` `-`, non-empty, not ending in `.`)
/// with a mandatory `.service` extension.
#[cfg_attr(windows, allow(dead_code))]
pub fn is_valid_unit_file(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    let Some(stem) = name.strip_suffix(".service") else {
        return false;
    };
    is_valid_unit_name(stem)
}

/// systemd unit names we accept: ASCII alphanumerics plus `.` `_` `-`,
/// non-empty, not ending in `.`.
#[cfg_attr(windows, allow(dead_code))]
fn is_valid_unit_name(name: &str) -> bool {
    !name.is_empty()
        && !name.ends_with('.')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// Unit name (without `.service`) derived from a validated unit file path.
#[cfg_attr(windows, allow(dead_code))]
fn unit_name_of(path: &Path) -> String {
    path.file_name()
        .and_then(|n| n.to_str())
        .and_then(|n| n.strip_suffix(".service"))
        .unwrap_or_default()
        .to_string()
}

/// Quote an ExecStart argument when it contains whitespace, systemd-style.
#[cfg_attr(windows, allow(dead_code))]
fn quote_exec_arg(s: &str) -> String {
    if s.is_empty() || s.chars().any(|c| c.is_whitespace()) {
        format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
    } else {
        s.to_string()
    }
}

/// Deterministic PATH for the service form: services do not inherit a login
/// shell environment, so the unit pins the standard system paths instead of
/// relying on the distro/systemd default (spec: 服务形态 PATH 确定).
pub const UNIT_SERVICE_PATH: &str = "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin";

/// Render the unit file. `timeout_stop_sec` should already account for the
/// slowest configured graceful stop (see `compute_timeout_stop_sec`).
/// `desc` defaults to `DEFAULT_DESCRIPTION`. `dir_uid` is the numeric uid of
/// the service run account (see `unit_dir_uid`) rendered literally into the
/// per-uid directory layers.
#[cfg_attr(windows, allow(dead_code))]
pub fn render_unit(
    exe: &Path,
    config_path: &Path,
    desc: Option<&str>,
    user: Option<&str>,
    dir_uid: u32,
    restart: RestartPolicy,
    timeout_stop_sec: u64,
) -> String {
    let mut s = String::new();
    s.push_str("[Unit]\n");
    s.push_str(&format!(
        "Description={}\n",
        desc.unwrap_or(DEFAULT_DESCRIPTION)
    ));
    // Boot ordering: wait for the network to be online before pulling up
    // programs that depend on it (lowers the window where an early-boot
    // start is misjudged as a failure), and cap the restart storm so a
    // repeatedly failing daemon ends up failed instead of flapping forever.
    s.push_str("Wants=network-online.target\n");
    s.push_str("After=network.target network-online.target\n");
    s.push_str("StartLimitIntervalSec=60\n");
    s.push_str("StartLimitBurst=5\n");
    s.push('\n');
    s.push_str("[Service]\n");
    s.push_str(&format!(
        "ExecStart={} daemon start --foreground --config {}\n",
        quote_exec_arg(&exe.display().to_string()),
        quote_exec_arg(&config_path.display().to_string())
    ));
    s.push_str(&format!("Restart={restart}\n"));
    s.push_str("RestartSec=3\n");
    s.push_str("KillSignal=SIGTERM\n");
    // KillMode=mixed: the stop SIGTERM goes to the daemon main process only,
    // which then stops every program gracefully in reverse start order with
    // each program's own stop_timeout. The default `control-group` would
    // race that by SIGTERMing every managed child at once, and the
    // TimeoutStopSec expiry SIGKILLs the whole cgroup as the backstop.
    s.push_str("KillMode=mixed\n");
    s.push_str(&format!("TimeoutStopSec={timeout_stop_sec}\n"));
    s.push_str(&format!("Environment=PATH={UNIT_SERVICE_PATH}\n"));
    // The control socket lives at /run/appkeeper/<uid>/ak.sock. The uid is
    // rendered LITERALLY (resolved from `--user` at install time): the `%U`
    // specifier would be wrong here — in a system unit it always expands to
    // `0` regardless of `User=` (systemd.unit(5): "In case of the system
    // manager this resolves to '0'. Note that this setting is not influenced
    // by the User="), pointing every account at the same root-owned
    // appkeeper/0 leaf. The literal makes systemd create (and clean up on
    // stop) only THIS instance's directory — declaring the shared root would
    // recursively delete every other user's /run/appkeeper/<uid> on stop.
    // systemd creates the parent /run/appkeeper (0755) as needed. Deliberately
    // NO ProtectSystem=strict / ReadWritePaths sandboxing here: those would
    // make the directory unwritable and the daemon could not bind. The daemon
    // also creates the directory itself when it is missing, so an older unit
    // without these keys still works.
    s.push_str(&format!("RuntimeDirectory=appkeeper/{dir_uid}\n"));
    s.push_str("RuntimeDirectoryMode=0700\n");
    // Persistent state (the `unless-stopped` user-stop records) under
    // /var/lib/appkeeper/<uid>: created by systemd per User=, kept across
    // restarts and never removed by the unit or `daemon uninstall` (literal
    // uid for the same reason as above).
    s.push_str(&format!("StateDirectory=appkeeper/{dir_uid}\n"));
    s.push_str("StateDirectoryMode=0750\n");
    if let Some(u) = user {
        s.push_str(&format!("User={u}\n"));
    }
    s.push('\n');
    s.push_str("[Install]\n");
    s.push_str("WantedBy=multi-user.target\n");
    s
}

/// The numeric uid the unit's per-uid directory layers (`appkeeper/<uid>`)
/// are rendered with: the `--user` account's uid, `0` for a root service (no
/// `--user`). The `%U` specifier is NOT usable here — in a system unit it
/// always expands to `0`, uninfluenced by `User=` (systemd.unit(5)) — so the
/// uid is resolved at install time and written literally. An account the
/// passwd database does not know falls back to `0` with a warning: systemd
/// fails the service on the unknown `User=` at start anyway, and the unit
/// stays honest about which directories it declares.
fn unit_dir_uid(user: Option<&str>, lookup: impl Fn(&str) -> Option<u32>) -> (u32, Option<String>) {
    match user {
        None => (0, None),
        Some(name) => match lookup(name) {
            Some(uid) => (uid, None),
            None => (
                0,
                Some(format!(
                    "cannot resolve --user {name:?} to a numeric uid — the unit declares the \
                     root (0) runtime/state directory layers; systemd will refuse to start the \
                     service until the account exists"
                )),
            ),
        },
    }
}

/// The passwd-database uid of an account name (install-time resolution for
/// [`unit_dir_uid`]).
#[cfg(unix)]
fn account_uid(user: &str) -> Option<u32> {
    let Ok(c) = std::ffi::CString::new(user) else {
        return None;
    };
    // SAFETY: `c` is a valid NUL-terminated string; getpwnam only reads it
    // and returns a pointer into libc static storage, from which the uid is
    // copied out before any other libc call.
    let pw = unsafe { libc::getpwnam(c.as_ptr()) };
    if pw.is_null() {
        None
    } else {
        Some(unsafe { (*pw).pw_uid })
    }
}

/// Everything `uninstall` removes: exactly the unit file. The RuntimeDirectory
/// is systemd's own cleanup and the StateDirectory (`/var/lib/appkeeper/<uid>`,
/// holding the persistent user-stop records) MUST survive uninstall — both are
/// deliberately absent from this list (spec: 卸载服务保留状态目录).
#[cfg(unix)]
fn uninstall_removals(unit_file: &Path) -> Vec<PathBuf> {
    vec![unit_file.to_path_buf()]
}

/// Pretty-render a written unit file inside a rounded box for the terminal.
/// `color` enables ANSI styling (section headers cyan, keys bold, borders
/// dim); padding is computed on visible characters so colored output stays
/// aligned. Pure so it is testable on every platform.
#[cfg_attr(windows, allow(dead_code))]
pub fn render_unit_display(path: &str, unit: &str, color: bool) -> String {
    // colorize one unit-file line; the caller pads the raw line first so
    // escape sequences never skew the box width
    fn paint(line: &str, color: bool) -> String {
        if !color {
            return line.to_string();
        }
        let t = line.trim_start();
        if t.starts_with('[') && t.ends_with(']') {
            format!("\x1b[1;36m{line}\x1b[0m")
        } else if t.starts_with('#') || t.starts_with(';') {
            format!("\x1b[90m{line}\x1b[0m")
        } else if let Some(eq) = line.find('=') {
            format!("\x1b[1m{}\x1b[0m{}", &line[..=eq], &line[eq + 1..])
        } else {
            line.to_string()
        }
    }

    let lines: Vec<&str> = unit.lines().collect();
    let title_w = path.chars().count();
    let content_w = lines.iter().map(|l| l.chars().count()).max().unwrap_or(0);
    // title + 2 keeps at least one filler dash on each side of the title
    let w = content_w.max(title_w + 2);
    let dash = "─".repeat(w - title_w - 1);
    let rule = "─".repeat(w + 2);

    let mut out = String::new();
    if color {
        out.push_str(&format!(
            "\x1b[2m╭─\x1b[0m \x1b[1m{path}\x1b[0m \x1b[2m{dash}╮\x1b[0m\n"
        ));
    } else {
        out.push_str(&format!("╭─ {path} {dash}╮\n"));
    }
    for line in &lines {
        let pad = " ".repeat(w - line.chars().count());
        let body = paint(line, color);
        if color {
            out.push_str(&format!("\x1b[2m│\x1b[0m {body}{pad} \x1b[2m│\x1b[0m\n"));
        } else {
            out.push_str(&format!("│ {body}{pad} │\n"));
        }
    }
    if color {
        out.push_str(&format!("\x1b[2m╰{rule}╯\x1b[0m"));
    } else {
        out.push_str(&format!("╰{rule}╯"));
    }
    out
}

/// What to do with an existing unit file before writing.
#[derive(Debug, PartialEq, Eq)]
#[cfg_attr(windows, allow(dead_code))]
pub enum WriteDecision {
    /// No existing unit — write the rendered one.
    Write,
    /// Existing unit is byte-identical — skip the write (still reload+enable).
    SkipIdentical,
    /// Existing unit differs and `--force` was not given.
    RefuseNeedsForce,
}

#[cfg_attr(windows, allow(dead_code))]
pub fn decide_write(existing: Option<&str>, rendered: &str, force: bool) -> WriteDecision {
    match existing {
        None => WriteDecision::Write,
        Some(cur) if cur == rendered => WriteDecision::SkipIdentical,
        Some(_) if force => WriteDecision::Write,
        Some(_) => WriteDecision::RefuseNeedsForce,
    }
}

/// Stop budget from the slowest program: 2× max stop_timeout + 10s margin.
#[cfg_attr(windows, allow(dead_code))]
fn timeout_from_max_stop(max_stop_timeout: f64) -> u64 {
    (2.0 * max_stop_timeout + 10.0).ceil() as u64
}

/// The directory a config path sits in (`.` for a bare file name) — the
/// anchor for relative `app_dir` / `state_dir` values.
#[cfg_attr(windows, allow(dead_code))]
fn config_dir_of(config_path: &Path) -> PathBuf {
    config_path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."))
        .to_path_buf()
}

/// `TimeoutStopSec` from the slowest graceful stop across the given registry
/// apps. Apps that fail to resolve are skipped (the precheck warns about
/// them separately); a wholesale failure is the caller's fallback decision.
#[cfg_attr(windows, allow(dead_code))]
fn timeout_from_listed(
    config: &crate::config::DaemonConfig,
    listed: &[crate::registry::ListedApp],
) -> Result<u64> {
    let mut max = 0.0f64;
    for listed in listed.iter().filter(|a| a.broken.is_none()) {
        let (raw, _) = crate::config::AppRaw::load(&listed.path)?;
        let app = crate::config::resolve_app(
            &listed.name,
            &listed.path,
            &raw,
            config.app_default.as_ref(),
        )?;
        for p in &app.programs {
            max = max.max(p.stop_timeout);
        }
    }
    Ok(timeout_from_max_stop(max))
}

/// Try to load the daemon config and every registered app and derive
/// `TimeoutStopSec` from the largest `stop_timeout`. Any load failure falls
/// back to a conservative 90s — the SERVICE RUNTIME keeps this lenient
/// fallback (a service cannot refuse to run over a bad registry), while
/// `daemon install` refuses instead (see [`InstallScan::load`]). Only the
/// Windows service loop calls it in production; the install path shares the
/// same load through [`InstallScan`] / [`timeout_from_listed`].
#[cfg(any(windows, test))]
pub fn compute_timeout_stop_sec(config_path: &Path) -> u64 {
    let compute = || -> Result<u64> {
        let (config, _) = crate::config::DaemonConfig::load_or_default(config_path)?;
        let config_dir = config_dir_of(config_path);
        let listed = crate::registry::list(&config, &config_dir)?;
        timeout_from_listed(&config, &listed)
    };
    compute().unwrap_or(FALLBACK_TIMEOUT_STOP_SEC)
}

// -- install precheck (service-registration: 安装期预检) -----------------------

/// Everything the install precheck looks at, resolved by the caller before
/// any side effect. Plain data: the check itself does no IO, so all input
/// classes are unit-testable on every platform (harden-daemon-autostart 3.1).
pub struct InstallPrecheck<'a> {
    /// Daemon config load outcome — a missing or broken file refuses the
    /// install (the unit would boot an empty daemon); the message names the
    /// path and the fix.
    pub config: Result<&'a crate::config::DaemonConfig, String>,
    /// The daemon config path the CLI resolved (default or `-c`); the 5.3
    /// migration hint echoes it.
    pub config_path: &'a Path,
    /// harden-daemon-autostart 5.3: the CLI resolved the config through the
    /// legacy `%APPDATA%` default — `Some` carries the machine-level path to
    /// migrate to. Windows-only input (Linux passes `None`).
    pub legacy_config_target: Option<&'a Path>,
    /// Registry scan: every listed app, good and broken (`broken` = reason).
    pub apps: &'a [crate::registry::ListedApp],
    /// Effective log_dir — child-process logs only (the persistent state
    /// moved to `state_dir` and is judged separately).
    pub log_dir: &'a Path,
    /// Effective persistent-state dir; `None` = not derivable in this
    /// context (the daemon would fall back to log_dir at startup).
    pub state_dir: Option<&'a Path>,
    /// This platform's temp root (`/tmp`, `%TEMP%`): a log/state dir under
    /// it warns — volatile across reboots.
    pub temp_root: &'a Path,
    pub platform: Platform,
    /// Windows: the user name the DEFAULT IPC pipe is named after — `Some`
    /// only when the effective socket IS that default per-user pipe (i.e.
    /// the config did not declare `socket`).
    pub default_pipe_user: Option<&'a str>,
    /// Windows: short name of the SCM account the service will run under.
    pub service_account: Option<&'a str>,
}

/// What the precheck decided: at most one refusal (the install must bail
/// with zero side effects) plus advisory warnings (stderr, install goes on).
#[derive(Debug, Default, PartialEq, Eq)]
pub struct PrecheckOutcome {
    pub refusal: Option<String>,
    pub warnings: Vec<String>,
}

/// Lexical "is this path the temp root or under it" (callers pass the real
/// temp root; no canonicalization inside keeps the check pure).
fn is_under_temp(path: &Path, temp_root: &Path) -> bool {
    path.starts_with(temp_root)
}

pub fn install_precheck(p: &InstallPrecheck<'_>) -> PrecheckOutcome {
    let mut out = PrecheckOutcome::default();
    // Refusal: a missing or invalid config. Either way the registered unit
    // would boot an empty daemon or not boot at all, and `--force` has no
    // voice here — it only governs the existing-unit/service decision.
    if let Err(msg) = &p.config {
        out.refusal = Some(msg.clone());
        return out; // without a config the remaining inputs say nothing
    }
    // Broken registry apps: warn per app, never block — the daemon skips
    // them at startup.
    for app in p.apps.iter().filter(|a| a.broken.is_some()) {
        out.warnings.push(format!(
            "registered app {:?} cannot be parsed and will be skipped: {}",
            app.name,
            app.broken.as_deref().unwrap_or_default()
        ));
    }
    // Volatile log_dir: child-process logs die with the reboot.
    if is_under_temp(p.log_dir, p.temp_root) {
        let hint = match p.platform {
            Platform::Unix => "/var/log/appkeeper",
            Platform::Windows => r"C:\ProgramData\appkeeper\logs",
        };
        out.warnings.push(format!(
            "log_dir {} is inside the temporary directory — child-process logs are lost on \
             reboot; point log_dir at a persistent directory (e.g. {hint})",
            p.log_dir.display()
        ));
    }
    // Volatile state dir: the `unless-stopped` user-stop records die with
    // the reboot (a separate warning from the log one, with its own fix).
    if let Some(state_dir) = p.state_dir
        && is_under_temp(state_dir, p.temp_root)
    {
        out.warnings.push(format!(
            "state_dir {} is inside the temporary directory — `unless-stopped` user-stop \
             records are lost on reboot; set a persistent state_dir explicitly, or let the \
             service provide one (systemd StateDirectory)",
            state_dir.display()
        ));
    }
    // Windows: the default pipe name embeds the INSTALLING user's name, but
    // the service runs under its own account — the CLI would dial a pipe the
    // service never creates.
    if p.platform == Platform::Windows
        && let (Some(pipe_user), Some(account)) = (p.default_pipe_user, p.service_account)
        && !pipe_user.eq_ignore_ascii_case(account)
    {
        out.warnings.push(format!(
            "the default control pipe is named after the installing user ({pipe_user}) \
             but the service runs as {account} — CLI clients would dial a pipe the \
             service never creates; declare `socket` explicitly in the daemon config \
             (a fixed pipe path or tcp://host:port)"
        ));
    }
    // 5.3: the config rides the legacy %APPDATA% fallback — point at the
    // machine-level target and the `-c` escape hatch.
    if let Some(target) = p.legacy_config_target {
        out.warnings.push(format!(
            "daemon config {} is the legacy %APPDATA% default — migrate it to the \
             machine-level {}, or point every command at the new path with -c <path>",
            p.config_path.display(),
            target.display()
        ));
    }
    out
}

/// Load the daemon config for install. Unlike the daemon itself (missing
/// file = run with defaults), install refuses a missing file — it would
/// register a service that boots an empty daemon — and a broken one, which
/// previously fell back to a 90s stop budget and installed anyway
/// (harden-daemon-autostart 3.3 changes that to a refusal).
fn install_config(config_path: &Path) -> Result<crate::config::DaemonConfig> {
    if !config_path.exists() {
        bail!(
            "daemon config {} does not exist — create one with `ak config --init` or point at \
             an existing one with -c/--config",
            config_path.display()
        );
    }
    // A parseable-but-invalid config refuses too, with the path and the
    // remedy attached (spec: 错误信息 SHALL 指明配置路径与补救方式).
    crate::config::DaemonConfig::load_or_default(config_path)
        .map(|(c, _)| c)
        .with_context(|| {
            format!(
                "daemon config {} is invalid — repair it (`ak config --edit`) or point at a \
                 valid one with -c/--config",
                config_path.display()
            )
        })
}

/// Everything one install needs from disk, resolved ONCE
/// (harden-daemon-autostart 3.3: the same load feeds the precheck and
/// `TimeoutStopSec`, so the two answers cannot disagree).
struct InstallScan {
    /// Config load outcome — a missing/broken file refuses the install (the
    /// pure precheck turns this into the refusal).
    config: Result<crate::config::DaemonConfig, String>,
    /// The config path this scan loaded (echoed by the 5.3 migration hint).
    config_path: PathBuf,
    /// Directory the config path sits in (relative `app_dir` resolution).
    config_dir: PathBuf,
    /// Registry scan. Empty when the config is broken or the scan failed
    /// wholesale (scan failures degrade to a warning; bad apps warn per app
    /// through the precheck).
    apps: Vec<crate::registry::ListedApp>,
    /// `TimeoutStopSec` from the slowest stop_timeout (conservative 90s
    /// fallback when the registry scan failed or apps do not resolve).
    timeout_stop_sec: u64,
    /// harden-daemon-autostart 5.3: the machine-level config path when the
    /// CLI resolved `config_path` through the legacy `%APPDATA%` fallback
    /// (Windows only; `None` elsewhere).
    legacy_config_target: Option<PathBuf>,
}

impl InstallScan {
    fn load(config_path: &Path) -> InstallScan {
        let config_dir = config_dir_of(config_path);
        let legacy_config_target = crate::config::legacy_config_migration_target(
            &crate::config::ConfigPathEnv::capture(),
            config_path,
        );
        match install_config(config_path) {
            Ok(config) => {
                let (apps, timeout_stop_sec) = match crate::registry::list(&config, &config_dir) {
                    Ok(listed) => {
                        let t = timeout_from_listed(&config, &listed)
                            .unwrap_or(FALLBACK_TIMEOUT_STOP_SEC);
                        (listed, t)
                    }
                    Err(e) => {
                        eprintln!("ak: warning: cannot scan the app registry: {e:#}");
                        (Vec::new(), FALLBACK_TIMEOUT_STOP_SEC)
                    }
                };
                InstallScan {
                    config: Ok(config),
                    config_path: config_path.to_path_buf(),
                    config_dir,
                    apps,
                    timeout_stop_sec,
                    legacy_config_target,
                }
            }
            Err(e) => InstallScan {
                // The whole chain: the refusal must carry the path, the fix
                // AND the underlying validation detail.
                config: Err(format!("{e:#}")),
                config_path: config_path.to_path_buf(),
                config_dir,
                apps: Vec::new(),
                timeout_stop_sec: FALLBACK_TIMEOUT_STOP_SEC,
                legacy_config_target,
            },
        }
    }
}

/// The user name the DEFAULT IPC pipe is named after — `Some` only when the
/// config's effective socket IS that default per-user pipe (i.e. `socket`
/// was never declared, or was declared to exactly the default value).
/// harden-daemon-autostart 5.2: the default name rides
/// `Endpoint::default_name` (the same derivation the daemon itself uses).
#[cfg(windows)]
fn default_pipe_user(config: &crate::config::DaemonConfig) -> Option<String> {
    default_pipe_user_from(
        &config.daemon.socket.to_string_lossy(),
        &crate::transport::Endpoint::default_name(),
    )
}

/// 5.2 pure core (every platform): given the effective socket and the
/// platform's default per-user pipe name (`\\.\pipe\ak-<user>.sock`), extract
/// the user name that pipe is named after. A `tcp://` endpoint, a socket
/// different from the default, or a malformed default name all yield `None`.
#[cfg(any(windows, test))]
fn default_pipe_user_from(socket: &str, default_pipe: &str) -> Option<String> {
    if socket.starts_with("tcp://") {
        return None;
    }
    if socket != default_pipe {
        return None;
    }
    let stem = default_pipe
        .strip_prefix(r"\\.\pipe\ak-")?
        .strip_suffix(".sock")?;
    (!stem.is_empty()).then(|| stem.to_string())
}

/// Unix never carries the pipe mismatch input (the warning is Windows-only).
#[cfg(not(windows))]
fn default_pipe_user(_config: &crate::config::DaemonConfig) -> Option<String> {
    None
}

/// The SCM account the service will run under, as a short comparable name
/// (`LocalSystem` default / `LocalService` / `NetworkService`).
#[cfg(windows)]
fn service_account_short(opts: &ServiceOptions) -> Option<String> {
    Some(
        opts.user
            .clone()
            .unwrap_or_else(|| "LocalSystem".to_string()),
    )
}

/// Unix has no SCM account (the pipe warning is Windows-only).
#[cfg(not(windows))]
fn service_account_short(_opts: &ServiceOptions) -> Option<String> {
    None
}

/// Run the install precheck over the one shared scan
/// (harden-daemon-autostart 3.2): a refusal is `bail!`ed with zero side
/// effects, warnings go to stderr and install continues. `--force` is
/// deliberately NOT an input: it only governs the existing-unit/service
/// decision, never the precheck (3.4). `service_mode` selects the Windows
/// service state-dir default (`%ProgramData%`) — the installed daemon runs
/// as a service, not from the installer's console.
fn run_install_precheck(
    scan: &InstallScan,
    opts: &ServiceOptions,
    service_mode: bool,
) -> Result<()> {
    let temp_root = std::env::temp_dir();
    let (log_dir, state_dir) = match scan.config.as_ref() {
        Ok(config) => {
            let log_dir = crate::config::resolve_path(&config.daemon.log_dir, &scan.config_dir);
            let state_dir = crate::config::resolve_state_dir(&crate::config::StateDirEnv::capture(
                config.daemon.state_dir.as_deref(),
                service_mode,
            ))
            .map(|p| crate::config::resolve_path(&p, &scan.config_dir));
            (log_dir, state_dir)
        }
        // The refusal path needs no directory inputs.
        Err(_) => (PathBuf::new(), None),
    };
    let pipe_user = scan.config.as_ref().ok().and_then(default_pipe_user);
    let account = service_account_short(opts);
    let outcome = install_precheck(&InstallPrecheck {
        config: scan.config.as_ref().map_err(|e| e.clone()),
        config_path: &scan.config_path,
        legacy_config_target: scan.legacy_config_target.as_deref(),
        apps: &scan.apps,
        log_dir: &log_dir,
        state_dir: state_dir.as_deref(),
        temp_root: &temp_root,
        platform: Platform::current(),
        default_pipe_user: pipe_user.as_deref(),
        service_account: account.as_deref(),
    });
    if let Some(refusal) = outcome.refusal {
        bail!("{refusal}");
    }
    for w in &outcome.warnings {
        eprintln!("ak: warning: {w}");
    }
    Ok(())
}

// -- platform entry points ---------------------------------------------------

pub fn install(config_path: &Path, opts: &ServiceOptions) -> Result<()> {
    #[cfg(windows)]
    {
        check_install_flags(Platform::current(), opts)?;
        win_install(config_path, opts)
    }
    #[cfg(unix)]
    {
        check_install_flags(Platform::current(), opts)?;
        unix_install(config_path, opts)
    }
}

pub fn uninstall(_config_path: &Path, opts: &ServiceOptions) -> Result<()> {
    #[cfg(windows)]
    {
        check_uninstall_flags(Platform::current(), opts)?;
        win_uninstall(opts)
    }
    #[cfg(unix)]
    {
        check_uninstall_flags(Platform::current(), opts)?;
        unix_uninstall(opts)
    }
}

// -- unix (systemd) implementation -------------------------------------------

#[cfg(unix)]
fn require_root() -> Result<()> {
    if unsafe { libc::geteuid() } != 0 {
        bail!("daemon install/uninstall requires root (try sudo)");
    }
    Ok(())
}

/// Color the unit box only when stdout is a real terminal and the user has
/// not opted out via `NO_COLOR`.
#[cfg(unix)]
fn use_color() -> bool {
    std::env::var_os("NO_COLOR").is_none() && unsafe { libc::isatty(libc::STDOUT_FILENO) } == 1
}

#[cfg(unix)]
fn unix_install(config_path: &Path, opts: &ServiceOptions) -> Result<()> {
    let unit_file = opts
        .unit_file
        .clone()
        .unwrap_or_else(|| PathBuf::from(DEFAULT_UNIT_FILE));
    if !is_valid_unit_file(&unit_file) {
        bail!(
            "invalid unit file {} (file name must be a legal unit name ending in '.service': \
             letters, digits, '.', '_', '-'; must not end with '.')",
            unit_file.display()
        );
    }
    // Install precheck before ANY side effect (harden-daemon-autostart 3.2) —
    // it runs before require_root so the refusal path is drivable (and
    // unit-testable) without privileges; `--force` cannot bypass it (3.4).
    // One config load + one registry scan feed both the precheck and the
    // TimeoutStopSec below (3.3).
    let scan = InstallScan::load(config_path);
    run_install_precheck(&scan, opts, false)?;
    require_root()?;
    let unit_name = unit_name_of(&unit_file);

    let exe = std::env::current_exe()
        .context("cannot locate the current executable")?
        .canonicalize()
        .context("cannot resolve the current executable path")?;
    let config_abs = std::path::absolute(config_path).with_context(|| {
        format!(
            "cannot resolve daemon config path {}",
            config_path.display()
        )
    })?;
    // The per-uid directory layers are rendered with the LITERAL uid of the
    // service account (`%U` would always expand to 0 in a system unit — see
    // `unit_dir_uid`). An unresolvable account warns and falls back to 0.
    let (dir_uid, uid_warning) = unit_dir_uid(opts.user.as_deref(), account_uid);
    if let Some(w) = uid_warning {
        eprintln!("ak: warning: {w}");
    }
    let rendered = render_unit(
        &exe,
        &config_abs,
        opts.desc.as_deref(),
        opts.user.as_deref(),
        dir_uid,
        opts.restart,
        scan.timeout_stop_sec,
    );

    let path = &unit_file;
    let existing = std::fs::read_to_string(path).ok();
    match decide_write(existing.as_deref(), &rendered, opts.force) {
        WriteDecision::RefuseNeedsForce => bail!(
            "unit {} already exists with different content; use --force to overwrite",
            path.display()
        ),
        WriteDecision::SkipIdentical => {
            println!("unit {} already up to date", path.display());
        }
        WriteDecision::Write => {
            std::fs::write(path, &rendered)
                .with_context(|| format!("cannot write {}", path.display()))?;
            println!("unit written: {}", path.display());
        }
    }

    systemctl(&["daemon-reload"])?;
    systemctl(&["enable", &format!("{unit_name}.service")])?;
    if opts.start {
        systemctl(&["start", &format!("{unit_name}.service")])?;
        println!("service {unit_name} started");
    }
    // only reached on a fully successful install — show what landed on disk
    println!(
        "{}",
        render_unit_display(&path.display().to_string(), &rendered, use_color())
    );
    Ok(())
}

#[cfg(unix)]
fn unix_uninstall(opts: &ServiceOptions) -> Result<()> {
    let unit_file = opts
        .unit_file
        .clone()
        .unwrap_or_else(|| PathBuf::from(DEFAULT_UNIT_FILE));
    if !is_valid_unit_file(&unit_file) {
        bail!(
            "invalid unit file {} (file name must be a legal unit name ending in '.service': \
             letters, digits, '.', '_', '-'; must not end with '.')",
            unit_file.display()
        );
    }
    require_root()?;
    let unit_name = unit_name_of(&unit_file);

    let path = &unit_file;
    if !path.exists() {
        println!("service {unit_name} is not installed");
        return Ok(());
    }

    // stop and disable failures are non-fatal: the goal is a clean removal.
    if let Err(e) = systemctl(&["stop", &format!("{unit_name}.service")]) {
        eprintln!("ak: warning: {e:#}");
    }
    if let Err(e) = systemctl(&["disable", &format!("{unit_name}.service")]) {
        eprintln!("ak: warning: {e:#}");
    }
    for path in uninstall_removals(&unit_file) {
        std::fs::remove_file(&path).with_context(|| format!("cannot remove {}", path.display()))?;
        println!("unit removed: {}", path.display());
    }
    systemctl(&["daemon-reload"])?;
    Ok(())
}

#[cfg(unix)]
fn systemctl(args: &[&str]) -> Result<()> {
    let out = std::process::Command::new("systemctl")
        .args(args)
        .output()
        .with_context(|| format!("failed to run systemctl {}", args.join(" ")))?;
    if out.status.success() {
        Ok(())
    } else {
        bail!(
            "systemctl {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
}

// -- windows (SCM) implementation --------------------------------------------

#[cfg(windows)]
mod win {
    use std::ffi::OsStr;
    use std::io;
    use std::path::Path;
    use std::ptr;
    use std::time::Duration;

    use anyhow::{Context as _, Result, bail};
    use windows_service::service::{
        Service, ServiceAccess, ServiceAction, ServiceActionType, ServiceFailureActions,
        ServiceFailureResetPeriod, ServiceState,
    };
    use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};
    use windows_sys::Win32::Foundation::ERROR_SERVICE_DOES_NOT_EXIST;
    use windows_sys::Win32::Security::{
        AllocateAndInitializeSid, CheckTokenMembership, FreeSid, SID_IDENTIFIER_AUTHORITY,
    };
    use windows_sys::Win32::System::Services::{
        ChangeServiceConfigW, CloseServiceHandle, CreateServiceW, OpenSCManagerW,
        QueryServiceConfig2W, QueryServiceConfigW, SC_HANDLE, SC_MANAGER_ALL_ACCESS,
        SERVICE_AUTO_START, SERVICE_CONFIG_DESCRIPTION, SERVICE_DESCRIPTIONW, SERVICE_ERROR_NORMAL,
        SERVICE_QUERY_CONFIG, SERVICE_WIN32_OWN_PROCESS,
    };
    use windows_sys::Win32::System::SystemServices::SECURITY_BUILTIN_DOMAIN_RID;

    use super::*;

    /// Failure action per the spec: restart 3s after an abnormal exit,
    /// failure counter reset after 24h. SCM only triggers actions on
    /// abnormal exits, so a regular stop never restarts the service.
    const RESTART_DELAY: Duration = Duration::from_millis(3000);
    const RESET_PERIOD: Duration = Duration::from_secs(86400);

    type WinResult<T> = std::result::Result<T, windows_service::Error>;

    /// Connect to the local SCM with raw `windows-sys` (the crate's
    /// `ServiceManager` does not expose its handle, and creation needs an
    /// exactly-quoted binpath the crate's escaper cannot emit).
    struct Scm {
        handle: SC_HANDLE,
    }

    impl Scm {
        fn connect() -> Result<Scm> {
            let handle = unsafe { OpenSCManagerW(ptr::null(), ptr::null(), SC_MANAGER_ALL_ACCESS) };
            if handle.is_null() {
                bail!(
                    "cannot connect to the Service Control Manager: {}",
                    io::Error::last_os_error()
                );
            }
            Ok(Scm { handle })
        }
    }

    impl Drop for Scm {
        fn drop(&mut self) {
            unsafe {
                let _ = CloseServiceHandle(self.handle);
            }
        }
    }

    fn open_service(name: &str, access: ServiceAccess) -> WinResult<Service> {
        // OpenServiceW via the crate path: build a ServiceManager each call.
        let mgr = ServiceManager::local_computer(None::<&OsStr>, ServiceManagerAccess::CONNECT)?;
        mgr.open_service(name, access)
    }

    fn last_error(context: &str) -> anyhow::Error {
        anyhow::anyhow!("{context}: {}", io::Error::last_os_error())
    }

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    unsafe fn wide_to_string(p: *const u16) -> String {
        if p.is_null() {
            return String::new();
        }
        unsafe {
            let mut len = 0usize;
            while *p.add(len) != 0 {
                len += 1;
            }
            String::from_utf16_lossy(std::slice::from_raw_parts(p, len))
        }
    }

    /// Elevated-token check (CheckTokenMembership against the builtin
    /// Administrators group). Runs before any SCM mutation.
    pub fn is_admin() -> bool {
        unsafe {
            let authority = SID_IDENTIFIER_AUTHORITY {
                Value: [0, 0, 0, 0, 0, 5], // SECURITY_NT_AUTHORITY
            };
            let mut sid: *mut core::ffi::c_void = ptr::null_mut();
            let admins_rid: u32 = 0x00000220; // DOMAIN_ALIAS_RID_ADMINS
            if AllocateAndInitializeSid(
                &authority,
                2,
                SECURITY_BUILTIN_DOMAIN_RID as u32,
                admins_rid,
                0,
                0,
                0,
                0,
                0,
                0,
                &mut sid,
            ) == 0
            {
                return false;
            }
            let mut member: i32 = 0;
            let ok = CheckTokenMembership(ptr::null_mut(), sid, &mut member);
            let _ = FreeSid(sid);
            ok != 0 && member != 0
        }
    }

    fn require_admin() -> Result<()> {
        if !is_admin() {
            bail!(
                "daemon install/uninstall requires administrator privileges (run from an elevated shell)"
            );
        }
        Ok(())
    }

    /// The configuration we want SCM to hold.
    #[derive(Debug, PartialEq, Eq)]
    struct Snapshot {
        binpath: String,
        autostart: bool,
        account: String,
        description: String,
        restart_delay_ms: Option<u64>,
        reset_period_sec: Option<u64>,
    }

    impl Snapshot {
        fn target(binpath: &str, account: &str, description: &str) -> Snapshot {
            Snapshot {
                binpath: normalize_binpath(binpath),
                autostart: true,
                account: account.to_string(),
                description: description.to_string(),
                restart_delay_ms: Some(RESTART_DELAY.as_millis() as u64),
                reset_period_sec: Some(RESET_PERIOD.as_secs()),
            }
        }
    }

    /// Read the live service configuration back: binpath / start type /
    /// account via raw `QueryServiceConfigW` (the crate's `ServiceConfig`
    /// lacks nothing here, but we already own the raw handle and this keeps
    /// the exact SCM strings) plus the description via
    /// `QueryServiceConfig2W(SERVICE_CONFIG_DESCRIPTION)` — the crate has no
    /// description getter at all.
    fn read_snapshot(service: &Service) -> Result<Snapshot> {
        let handle = service.raw_handle();

        let mut bytes_needed: u32 = 0;
        let mut buf = vec![0u8; 8192];
        let ok = unsafe {
            QueryServiceConfigW(
                handle,
                buf.as_mut_ptr() as _,
                buf.len() as u32,
                &mut bytes_needed,
            )
        };
        if ok == 0 {
            return Err(last_error("cannot query service configuration"));
        }
        let cfg = unsafe {
            &*(buf.as_ptr() as *const windows_sys::Win32::System::Services::QUERY_SERVICE_CONFIGW)
        };
        let binpath = unsafe { wide_to_string(cfg.lpBinaryPathName) };
        let account = unsafe { wide_to_string(cfg.lpServiceStartName) };
        let autostart = cfg.dwStartType == SERVICE_AUTO_START;

        // Description: two-step sized query.
        let mut desc_buf_size: u32 = 0;
        unsafe {
            QueryServiceConfig2W(
                handle,
                SERVICE_CONFIG_DESCRIPTION,
                ptr::null_mut(),
                0,
                &mut desc_buf_size,
            )
        };
        let description = if desc_buf_size == 0 {
            String::new()
        } else {
            let mut dbuf = vec![0u8; desc_buf_size as usize];
            let ok = unsafe {
                QueryServiceConfig2W(
                    handle,
                    SERVICE_CONFIG_DESCRIPTION,
                    dbuf.as_mut_ptr(),
                    dbuf.len() as u32,
                    &mut desc_buf_size,
                )
            };
            if ok == 0 {
                return Err(last_error("cannot query service description"));
            }
            let desc = unsafe { &*(dbuf.as_ptr() as *const SERVICE_DESCRIPTIONW) };
            unsafe { wide_to_string(desc.lpDescription) }
        };

        // Failure actions via the crate (raw read would need exact struct
        // sizing for the trailing SC_ACTION array).
        let failure = service
            .get_failure_actions()
            .context("cannot query service failure actions")?;
        let reset_period_sec = match failure.reset_period {
            ServiceFailureResetPeriod::Never => None,
            ServiceFailureResetPeriod::After(d) => Some(d.as_secs()),
        };
        let restart_delay_ms = failure.actions.as_ref().and_then(|actions| {
            actions.first().and_then(|a| {
                if a.action_type == ServiceActionType::Restart {
                    Some(a.delay.as_millis() as u64)
                } else {
                    None
                }
            })
        });

        Ok(Snapshot {
            binpath: normalize_binpath(&binpath),
            autostart,
            account,
            description,
            restart_delay_ms,
            reset_period_sec,
        })
    }

    fn target_failure_actions() -> ServiceFailureActions {
        ServiceFailureActions {
            reset_period: ServiceFailureResetPeriod::After(RESET_PERIOD),
            reboot_msg: None,
            command: None,
            actions: Some(vec![ServiceAction {
                action_type: ServiceActionType::Restart,
                delay: RESTART_DELAY,
            }]),
        }
    }

    /// Create a fresh SCM service. The binpath is passed exactly as rendered
    /// (whole-path quoted) — `CreateServiceW` stores the string verbatim,
    /// and the crate's `ServiceInfo` path escaping would mangle spaces.
    fn create_service(scm: &Scm, name: &str, binpath: &str, account: &str) -> Result<()> {
        let name_w = wide(name);
        let display_w = wide(name);
        let binpath_w = wide(binpath);
        let account_w = wide(account);
        let handle = unsafe {
            CreateServiceW(
                scm.handle,
                name_w.as_ptr(),
                display_w.as_ptr(),
                SERVICE_QUERY_CONFIG,
                SERVICE_WIN32_OWN_PROCESS,
                SERVICE_AUTO_START,
                SERVICE_ERROR_NORMAL,
                binpath_w.as_ptr(),
                ptr::null(),
                ptr::null_mut(),
                ptr::null(),
                account_w.as_ptr(),
                ptr::null(),
            )
        };
        if handle.is_null() {
            return Err(last_error("cannot create service"));
        }
        unsafe {
            let _ = CloseServiceHandle(handle);
        }
        Ok(())
    }

    /// Point an existing service at the target configuration. Binpath /
    /// start type / account go through raw `ChangeServiceConfigW` for the
    /// same exact-string reason as `create_service`.
    fn change_service_config(service: &Service, binpath: &str, account: &str) -> Result<()> {
        let binpath_w = wide(binpath);
        let account_w = wide(account);
        let handle = service.raw_handle();
        let ok = unsafe {
            ChangeServiceConfigW(
                handle,
                SERVICE_WIN32_OWN_PROCESS,
                SERVICE_AUTO_START,
                SERVICE_ERROR_NORMAL,
                binpath_w.as_ptr(),
                ptr::null(),
                ptr::null_mut(),
                ptr::null(),
                account_w.as_ptr(),
                ptr::null(),
                ptr::null(),
            )
        };
        if ok == 0 {
            return Err(last_error("cannot change service configuration"));
        }
        Ok(())
    }

    pub fn win_install(config_path: &Path, opts: &ServiceOptions) -> Result<()> {
        require_admin()?;
        let name = opts
            .name
            .clone()
            .unwrap_or_else(|| DEFAULT_SERVICE_NAME.to_string());
        let description = opts
            .desc
            .clone()
            .unwrap_or_else(|| DEFAULT_DESCRIPTION.to_string());
        let account = builtin_account(opts.user.as_deref().unwrap_or("LocalSystem"))?.to_string();

        // Install precheck before any SCM query or mutation
        // (harden-daemon-autostart 3.2); `--force` cannot bypass it (3.4).
        // The installed daemon runs as a SERVICE, so its state-dir default is
        // the service one (%ProgramData%), not the console's.
        let scan = InstallScan::load(config_path);
        run_install_precheck(&scan, opts, true)?;

        let exe = std::env::current_exe()
            .context("cannot locate the current executable")?
            .canonicalize()
            .context("cannot resolve the current executable path")?;
        let config_abs = std::path::absolute(config_path).with_context(|| {
            format!(
                "cannot resolve daemon config path {}",
                config_path.display()
            )
        })?;
        let binpath = render_binpath(&exe, &config_abs, &name);
        let target = Snapshot::target(&binpath, &account, &description);

        let scm = Scm::connect()?;
        let access = ServiceAccess::QUERY_CONFIG
            | ServiceAccess::CHANGE_CONFIG
            | ServiceAccess::QUERY_STATUS
            | ServiceAccess::START;

        match open_service(&name, access) {
            Ok(service) => {
                let snapshot = read_snapshot(&service)
                    .with_context(|| format!("cannot read configuration of service {name}"))?;
                if snapshot == target {
                    println!("service {name} already up to date");
                } else if !opts.force {
                    bail!(
                        "service {name} already exists with a different configuration; \
                         use --force to overwrite it"
                    );
                } else {
                    change_service_config(&service, &binpath, &account)
                        .with_context(|| format!("cannot reconfigure service {name}"))?;
                    service
                        .set_description(&description)
                        .context("cannot set service description")?;
                    service
                        .update_failure_actions(target_failure_actions())
                        .context("cannot set service failure actions")?;
                    if service
                        .query_status()
                        .context("cannot query service status")?
                        .current_state
                        != ServiceState::Stopped
                    {
                        println!(
                            "service {name} is running — configuration changes take effect on next start"
                        );
                    }
                    println!("service {name} configuration updated");
                }
                if opts.start {
                    service
                        .start(&[] as &[&OsStr])
                        .context("cannot start service")?;
                    println!("service {name} started");
                }
            }
            Err(windows_service::Error::Winapi(e))
                if e.raw_os_error() == Some(ERROR_SERVICE_DOES_NOT_EXIST as i32) =>
            {
                create_service(&scm, &name, &binpath, &account)
                    .with_context(|| format!("cannot create service {name}"))?;
                // Description and failure actions via a fresh handle: the
                // create path above closed its handle already.
                let service = open_service(
                    &name,
                    ServiceAccess::QUERY_CONFIG | ServiceAccess::CHANGE_CONFIG,
                )?;
                service
                    .set_description(&description)
                    .context("cannot set service description")?;
                service
                    .update_failure_actions(target_failure_actions())
                    .context("cannot set service failure actions")?;
                println!("service {name} installed");
                if opts.start {
                    let service = open_service(&name, ServiceAccess::START)?;
                    service
                        .start(&[] as &[&OsStr])
                        .context("cannot start service")?;
                    println!("service {name} started");
                }
            }
            Err(e) => return Err(e).context("cannot open service"),
        }

        // What landed in the SCM — mirrors the Linux unit box.
        println!();
        println!("service {name}");
        println!("    binpath     : {binpath}");
        println!("    start       : automatic");
        println!("    account     : {account}");
        println!(
            "    restart     : {}s after abnormal exit (reset after {}s)",
            RESTART_DELAY.as_secs(),
            RESET_PERIOD.as_secs()
        );
        println!("    description : {description}");
        println!();
        println!(
            "note: while running as a service the daemon has no console — \
             stderr (including the env_logger output) is not visible anywhere; \
             watch programs via the web console or `ak log <name>` instead"
        );
        Ok(())
    }

    pub fn win_uninstall(opts: &ServiceOptions) -> Result<()> {
        require_admin()?;
        let name = opts
            .name
            .clone()
            .unwrap_or_else(|| DEFAULT_SERVICE_NAME.to_string());
        let service = match open_service(
            &name,
            ServiceAccess::QUERY_STATUS | ServiceAccess::STOP | ServiceAccess::DELETE,
        ) {
            Ok(s) => s,
            Err(windows_service::Error::Winapi(e))
                if e.raw_os_error() == Some(ERROR_SERVICE_DOES_NOT_EXIST as i32) =>
            {
                println!("service {name} is not installed");
                return Ok(());
            }
            Err(e) => return Err(e).context("cannot open service"),
        };
        if let Err(e) = service.stop() {
            eprintln!("ak: warning: failed to stop service {name}: {e}");
        }
        service
            .delete()
            .with_context(|| format!("cannot delete service {name}"))?;
        println!("service {name} uninstalled");
        Ok(())
    }

    // -- service mode: the SCM side of `ak daemon start --service <name>` ----

    use std::ffi::OsString;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, OnceLock};
    use std::time::Instant;

    use windows_service::define_windows_service;
    use windows_service::service::{
        ServiceControl, ServiceControlAccept, ServiceExitCode, ServiceStatus, ServiceType,
    };
    use windows_service::service_control_handler::{
        self, ServiceControlHandlerResult, ServiceStatusHandle,
    };
    use windows_service::service_dispatcher;

    use crate::supervisor::Supervisor;

    /// Args handed from `run_daemon` to the service main thread. The SCM
    /// service-main signature is fixed, so the values travel through statics.
    struct ServiceRun {
        config_path: PathBuf,
        name: String,
    }

    static SERVICE_RUN: OnceLock<ServiceRun> = OnceLock::new();

    /// Whether this process entered through the SCM dispatcher
    /// (`daemon start --service`): the entry point latches the service name
    /// before the dispatcher starts, so the daemon's state-dir default
    /// (%ProgramData% vs %LOCALAPPDATA%) can key off it.
    pub(crate) fn service_mode_active() -> bool {
        SERVICE_RUN.get().is_some()
    }

    /// The service name the SCM entry latched — the shutdown 409 hint names
    /// it (`sc stop <name>`).
    pub(crate) fn latched_service_name() -> Option<String> {
        SERVICE_RUN.get().map(|r| r.name.clone())
    }
    /// Set once the supervisor exists; the control handler forwards
    /// Stop/Shutdown to it. A request that arrives earlier latches
    /// `STOP_REQUESTED` and is re-issued after startup.
    static SUPERVISOR: OnceLock<Arc<Supervisor>> = OnceLock::new();
    static STOP_REQUESTED: AtomicBool = AtomicBool::new(false);

    define_windows_service!(ffi_service_main, service_main);

    fn service_main(_arguments: Vec<OsString>) {
        if let Err(err) = run_service() {
            log::error!("service mode failed: {err:#}");
        }
    }

    /// Entry called from `run_daemon` when `--service <name>` is present.
    /// Blocks the calling thread in the SCM dispatcher for the lifetime of
    /// the service.
    pub fn run_windows_service(config_path: &Path, service_name: &str) -> Result<()> {
        let _ = SERVICE_RUN.set(ServiceRun {
            config_path: config_path.to_path_buf(),
            name: service_name.to_string(),
        });
        service_dispatcher::start(service_name, ffi_service_main)
            .context("cannot connect to the Service Control Manager (is the service installed?)")
    }

    fn shutdown_latched(sup: &Supervisor) -> bool {
        sup.state.lock().unwrap().shutdown.load(Ordering::SeqCst)
    }

    fn report_status(
        handle: &ServiceStatusHandle,
        state: ServiceState,
        accepted: ServiceControlAccept,
        checkpoint: u32,
        wait_hint: Duration,
    ) -> Result<()> {
        handle
            .set_service_status(ServiceStatus {
                service_type: ServiceType::OWN_PROCESS,
                current_state: state,
                controls_accepted: accepted,
                exit_code: ServiceExitCode::Win32(0),
                checkpoint,
                wait_hint,
                process_id: Some(std::process::id()),
            })
            .context("cannot report service status to the SCM")
    }

    fn run_service() -> Result<()> {
        let run = SERVICE_RUN
            .get()
            .context("service arguments not initialized")?;
        let stop_budget = Duration::from_secs(compute_timeout_stop_sec(&run.config_path));

        let handle = service_control_handler::register(&run.name, |control| match control {
            ServiceControl::Stop | ServiceControl::Shutdown => {
                STOP_REQUESTED.store(true, Ordering::SeqCst);
                if let Some(sup) = SUPERVISOR.get() {
                    sup.request_shutdown();
                }
                ServiceControlHandlerResult::NoError
            }
            ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
            _ => ServiceControlHandlerResult::NotImplemented,
        })
        .context("cannot register the service control handler")?;

        // START_PENDING: no controls accepted yet; the wait hint covers
        // config load, registry scan and bootstrap.
        report_status(
            &handle,
            ServiceState::StartPending,
            ServiceControlAccept::empty(),
            1,
            stop_budget,
        )?;

        let sup = crate::run_daemon_core(&run.config_path)?;
        let _ = SUPERVISOR.set(sup.clone());
        if STOP_REQUESTED.load(Ordering::SeqCst) {
            sup.request_shutdown();
        }
        log::info!(
            "ak {} running as service {}",
            env!("CARGO_PKG_VERSION"),
            run.name
        );

        let accepted = ServiceControlAccept::STOP | ServiceControlAccept::SHUTDOWN;
        report_status(&handle, ServiceState::Running, accepted, 0, Duration::ZERO)?;

        // Supervision loop on its own thread; this thread only reports
        // status. `done` fires once every program has stopped.
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let sup2 = sup.clone();
        std::thread::Builder::new()
            .name("supervisor".into())
            .spawn(move || {
                sup2.run();
                let _ = done_tx.send(());
            })
            .context("failed to spawn supervisor thread")?;

        // Idle until the stop request reaches the supervisor.
        while !shutdown_latched(&sup) {
            if STOP_REQUESTED.load(Ordering::SeqCst) {
                sup.request_shutdown();
            }
            std::thread::sleep(Duration::from_millis(100));
        }

        // Graceful stop in progress: report STOP_PENDING with the remaining
        // budget, bumping the checkpoint so the SCM sees live progress.
        let deadline = Instant::now() + stop_budget;
        let mut checkpoint = 1u32;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            report_status(
                &handle,
                ServiceState::StopPending,
                ServiceControlAccept::empty(),
                checkpoint,
                remaining.max(Duration::from_secs(1)),
            )?;
            match done_rx.recv_timeout(Duration::from_millis(500)) {
                Ok(()) => break,
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                    checkpoint = checkpoint.wrapping_add(1);
                }
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }

        report_status(
            &handle,
            ServiceState::Stopped,
            ServiceControlAccept::empty(),
            0,
            Duration::ZERO,
        )?;
        log::info!("service {} stopped", run.name);
        Ok(())
    }
}

#[cfg(windows)]
pub use win::run_windows_service;
#[cfg(windows)]
use win::{win_install, win_uninstall};

/// Windows service run mode (`daemon start --service`): the supervisor's
/// state-dir default keys off it (see `config::resolve_state_dir`).
#[cfg(windows)]
pub(crate) fn windows_service_mode() -> bool {
    win::service_mode_active()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn temp_dir(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!("ak-test-svc-{tag}-{}", std::process::id()))
    }

    #[test]
    fn unit_file_accepts_legal_and_rejects_illegal() {
        assert!(is_valid_unit_file(Path::new(
            "/etc/systemd/system/ak.service"
        )));
        assert!(is_valid_unit_file(Path::new("ak-prod_1.service")));
        assert!(is_valid_unit_file(Path::new("a.service")));
        assert!(is_valid_unit_file(Path::new("my-app_v2.service")));
        for bad in [
            "appkeeper",            // missing .service
            ".service",             // empty stem
            "ends..service",        // stem ends with '.'
            "my app.service",       // space in stem
            "x.service.bak",        // wrong extension
            "/etc/systemd/system/", // directory, no file name
        ] {
            assert!(
                !is_valid_unit_file(Path::new(bad)),
                "expected {bad:?} to be rejected"
            );
        }
    }

    #[test]
    fn unit_name_derived_from_file_stem() {
        assert_eq!(
            unit_name_of(Path::new("/etc/systemd/system/ak.service")),
            "ak"
        );
        assert_eq!(unit_name_of(Path::new("my-app.service")), "my-app");
    }

    /// Test twin of unix_install's render step: resolve the directory uid
    /// with an injectable lookup (like `account_uid`), collect the warning,
    /// then render.
    fn render_with_lookup(
        user: Option<&str>,
        lookup: impl Fn(&str) -> Option<u32>,
        warnings: &mut Vec<String>,
    ) -> String {
        let (dir_uid, warn) = unit_dir_uid(user, lookup);
        if let Some(w) = warn {
            warnings.push(w);
        }
        render_unit(
            Path::new("/usr/local/bin/ak"),
            Path::new("/etc/appkeeper/daemon.toml"),
            None,
            user,
            dir_uid,
            RestartPolicy::Always,
            90,
        )
    }

    #[test]
    fn renders_unit_without_user() {
        let mut warnings = Vec::new();
        let unit = render_with_lookup(None, |_| Some(4242), &mut warnings);
        assert!(warnings.is_empty());
        assert_eq!(
            unit,
            "[Unit]\n\
             Description=appkeeper process keeper\n\
             Wants=network-online.target\n\
             After=network.target network-online.target\n\
             StartLimitIntervalSec=60\n\
             StartLimitBurst=5\n\
             \n\
             [Service]\n\
             ExecStart=/usr/local/bin/ak daemon start --foreground --config /etc/appkeeper/daemon.toml\n\
             Restart=always\n\
             RestartSec=3\n\
             KillSignal=SIGTERM\n\
             KillMode=mixed\n\
             TimeoutStopSec=90\n\
             Environment=PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin\n\
             RuntimeDirectory=appkeeper/0\n\
             RuntimeDirectoryMode=0700\n\
             StateDirectory=appkeeper/0\n\
             StateDirectoryMode=0750\n\
             \n\
             [Install]\n\
             WantedBy=multi-user.target\n"
        );
    }

    /// harden-daemon-autostart 1.1/1.4: the unit hardens the stop path
    /// (`KillMode=mixed`) and boot ordering (`Environment=PATH=...`,
    /// network-online, explicit start limits) with every key in its correct
    /// section (`After`/`Wants`/`StartLimit*` in `[Unit]`, the rest in
    /// `[Service]`).
    #[test]
    fn unit_hardening_keys_and_sections() {
        let mut warnings = Vec::new();
        let unit = render_with_lookup(None, |_| Some(4242), &mut warnings);
        assert!(warnings.is_empty());
        assert!(unit.contains("KillMode=mixed\n"), "{unit}");
        assert!(
            unit.contains(
                "Environment=PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin\n"
            ),
            "the service PATH must be deterministic: {unit}"
        );
        // Section placement: keys are asserted per section slice so a key
        // drifting into the wrong section fails here.
        let (unit_head, rest) = unit.split_once("[Service]").unwrap();
        let (service_head, install_tail) = rest.split_once("[Install]").unwrap();
        assert!(
            unit_head.contains("Description=")
                && unit_head.contains("Wants=network-online.target")
                && unit_head.contains("After=network.target network-online.target")
                && unit_head.contains("StartLimitIntervalSec=60")
                && unit_head.contains("StartLimitBurst=5"),
            "[Unit] must carry description, network-online and start limits: {unit_head}"
        );
        assert!(
            service_head.contains("ExecStart=")
                && service_head.contains("KillMode=mixed")
                && service_head.contains("TimeoutStopSec=90")
                && service_head.contains("Environment=PATH=")
                && service_head.contains("RuntimeDirectory=")
                && service_head.contains("StateDirectory="),
            "[Service] must carry exec/stop/environment/directory keys: {service_head}"
        );
        assert!(
            install_tail.contains("WantedBy=multi-user.target"),
            "{install_tail}"
        );
    }

    /// harden-daemon-autostart 1.2/1.3: the runtime directory is per-uid
    /// (`appkeeper/<uid>`, 0700) and a persistent state directory
    /// (`appkeeper/<uid>`, 0750) is declared, with the service account's
    /// LITERAL uid — `%U` would expand to 0 in a system unit regardless of
    /// `User=` (systemd.unit(5)) and point every account at the same
    /// root-owned leaf; the socket parent stays creatable (no sandbox
    /// directive that would break the bind).
    #[test]
    fn unit_declares_runtime_directory_and_no_socket_blocking_sandbox() {
        let mut warnings = Vec::new();
        let unit = render_with_lookup(Some("akuser"), |_| Some(1000), &mut warnings);
        assert!(warnings.is_empty());
        assert!(
            unit.contains("RuntimeDirectory=appkeeper/1000\n"),
            "systemd must create the per-uid runtime dir with the resolved uid: {unit}"
        );
        assert!(unit.contains("RuntimeDirectoryMode=0700\n"), "{unit}");
        assert!(
            unit.contains("StateDirectory=appkeeper/1000\n"),
            "systemd must create the per-uid state dir with the resolved uid: {unit}"
        );
        assert!(unit.contains("StateDirectoryMode=0750\n"), "{unit}");
        assert!(
            !unit.contains("%U"),
            "the %U specifier resolves to 0 in a system unit regardless of User= — the uid \
             must be literal: {unit}"
        );
        for blocking in [
            "ProtectSystem=strict",
            "ProtectSystem=true",
            "ReadOnlyPaths=",
            "PrivateTmp=yes",
            "ProtectHome=yes",
        ] {
            assert!(
                !unit.contains(blocking),
                "{blocking} would block /run/appkeeper and the socket bind: {unit}"
            );
        }
        // A non-root unit still gets the directory; the daemon creates the
        // per-uid subdirectory under it.
        assert!(unit.contains("User=akuser\n"), "{unit}");
    }

    /// harden-daemon-autostart 1.5: regressions for D1 (KillMode=control-group
    /// racing the daemon's reverse-order graceful stop) and D2 (the shared
    /// root `appkeeper` as a RuntimeDirectory/StateDirectory — a stop would
    /// recursively delete every other user's directories).
    #[test]
    fn unit_avoids_control_group_killmode_and_shared_root_directories() {
        let mut warnings = Vec::new();
        let unit = render_with_lookup(None, |_| Some(4242), &mut warnings);
        assert!(warnings.is_empty());
        assert!(
            !unit.contains("KillMode=control-group"),
            "control-group would SIGTERM every managed child in parallel with the daemon: {unit}"
        );
        for line in unit.lines() {
            assert!(
                line != "RuntimeDirectory=appkeeper" && line != "StateDirectory=appkeeper",
                "the shared root must not be declared as a unit directory (it would be \
                 recursively removed on stop, hitting other users' instances): {line}"
            );
        }
    }

    /// The directory uid resolution: no `--user` → root's 0; a resolvable
    /// account → its LITERAL uid; an unresolvable account → 0 plus a warning
    /// naming it. `%U` is never an option here — a system unit expands it to
    /// 0 regardless of `User=` (systemd.unit(5)).
    #[test]
    fn unit_dir_uid_resolves_user_and_warns_on_unknown() {
        // No --user: the service runs as root — uid 0, no warning.
        assert_eq!(unit_dir_uid(None, |_| Some(4242)), (0, None));
        // A resolvable account: its literal uid lands in the unit.
        assert_eq!(unit_dir_uid(Some("akuser"), |_| Some(1000)), (1000, None));
        // Unresolvable account: fallback 0 with a warning that names it.
        let (uid, warn) = unit_dir_uid(Some("ghost"), |_| None);
        assert_eq!(uid, 0);
        let warn = warn.expect("a fallback warning");
        assert!(warn.contains("ghost") && warn.contains("0"), "{warn}");
    }

    /// The production lookup (`getpwnam`): resolves a real account, refuses
    /// unknown ones and names a C-string-incompatible one without panicking.
    #[cfg(unix)]
    #[test]
    fn account_uid_resolves_and_rejects_unusable_names() {
        // root exists on every unix host.
        assert_eq!(account_uid("root"), Some(0));
        assert_eq!(account_uid("no-such-ak-account-xyz"), None);
        // A NUL byte cannot enter the C string — unresolvable, not a panic.
        assert_eq!(account_uid("a\0b"), None);
    }

    /// harden-daemon-autostart 1.3: uninstall removes exactly the unit file —
    /// never the StateDirectory with the persistent user-stop records.
    #[test]
    #[cfg(unix)]
    fn uninstall_removes_only_the_unit_file() {
        let removals = uninstall_removals(Path::new("/etc/systemd/system/ak.service"));
        assert_eq!(
            removals,
            vec![PathBuf::from("/etc/systemd/system/ak.service")]
        );
        let joined = removals
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join(" ");
        assert!(
            !joined.contains("/var/lib/appkeeper") && !joined.contains("/run/appkeeper"),
            "uninstall must not touch the runtime or state directories: {joined}"
        );
    }

    #[test]
    fn renders_unit_with_user_and_quoted_paths() {
        let unit = render_unit(
            Path::new("/opt/my tools/appkeeper"),
            Path::new("/etc/my conf/appkeeper.toml"),
            None,
            Some("svc-ak"),
            1002,
            RestartPolicy::Always,
            60,
        );
        assert!(unit.contains(
            "ExecStart=\"/opt/my tools/appkeeper\" daemon start --foreground --config \"/etc/my conf/appkeeper.toml\"\n"
        ));
        assert!(unit.contains("User=svc-ak\n"));
        assert!(unit.contains("TimeoutStopSec=60\n"));
        // The renderer passes the uid through literally.
        assert!(unit.contains("RuntimeDirectory=appkeeper/1002\n"), "{unit}");
        assert!(unit.contains("StateDirectory=appkeeper/1002\n"), "{unit}");
    }

    #[test]
    fn render_honours_restart_policy() {
        let unit = render_unit(
            Path::new("/usr/local/bin/ak"),
            Path::new("/etc/appkeeper/daemon.toml"),
            None,
            None,
            0,
            RestartPolicy::OnFailure,
            90,
        );
        assert!(unit.contains("Restart=on-failure\n"));
        assert!(!unit.contains("Restart=always\n"));
    }

    #[test]
    fn restart_policy_displays_systemd_names() {
        let pairs = [
            (RestartPolicy::No, "no"),
            (RestartPolicy::OnSuccess, "on-success"),
            (RestartPolicy::OnFailure, "on-failure"),
            (RestartPolicy::OnAbnormal, "on-abnormal"),
            (RestartPolicy::OnWatchdog, "on-watchdog"),
            (RestartPolicy::OnAbort, "on-abort"),
            (RestartPolicy::Always, "always"),
        ];
        for (policy, name) in pairs {
            assert_eq!(policy.to_string(), name);
            // the CLI value spelling must match what lands in the unit
            assert_eq!(
                policy.to_possible_value().unwrap().get_name(),
                name,
                "ValueEnum name drifted from Display for {name}"
            );
        }
    }

    #[test]
    fn unit_display_plain_box_frames_and_aligns() {
        let out = render_unit_display("/x.service", "[Unit]\nDescription=demo\n", false);
        assert_eq!(
            out,
            "╭─ /x.service ─────╮\n\
             │ [Unit]           │\n\
             │ Description=demo │\n\
             ╰──────────────────╯"
        );
    }

    #[test]
    fn unit_display_color_highlights_and_stays_aligned() {
        fn strip_ansi(s: &str) -> String {
            let mut out = String::new();
            let mut it = s.chars();
            while let Some(c) = it.next() {
                if c == '\x1b' {
                    for e in it.by_ref() {
                        if e == 'm' {
                            break;
                        }
                    }
                } else {
                    out.push(c);
                }
            }
            out
        }
        let unit = render_unit(
            Path::new("/usr/local/bin/ak"),
            Path::new("/etc/appkeeper/daemon.toml"),
            None,
            None,
            0,
            RestartPolicy::Always,
            90,
        );
        let out = render_unit_display("/etc/systemd/system/ak.service", &unit, true);
        assert!(out.contains("\x1b[1;36m[Unit]\x1b[0m"));
        assert!(out.contains("\x1b[1mExecStart=\x1b[0m"));
        assert!(out.starts_with("\x1b[2m╭─"));
        assert!(out.contains("\x1b[2m╰"));
        // escapes must not skew the box: every line has the same visible width
        let widths: std::collections::HashSet<usize> =
            out.lines().map(|l| strip_ansi(l).chars().count()).collect();
        assert_eq!(widths.len(), 1, "uneven box lines: {widths:?}");
    }

    #[test]
    fn write_decision_idempotent_refuse_and_force() {
        let rendered = "unit\n";
        assert_eq!(decide_write(None, rendered, false), WriteDecision::Write);
        // identical content is idempotent regardless of --force
        assert_eq!(
            decide_write(Some(rendered), rendered, false),
            WriteDecision::SkipIdentical
        );
        assert_eq!(
            decide_write(Some(rendered), rendered, true),
            WriteDecision::SkipIdentical
        );
        assert_eq!(
            decide_write(Some("other\n"), rendered, false),
            WriteDecision::RefuseNeedsForce
        );
        assert_eq!(
            decide_write(Some("other\n"), rendered, true),
            WriteDecision::Write
        );
    }

    #[test]
    fn timeout_scales_with_max_stop_timeout() {
        assert_eq!(timeout_from_max_stop(0.0), 10);
        assert_eq!(timeout_from_max_stop(25.0), 60);
        assert_eq!(timeout_from_max_stop(12.25), 35); // ceil(34.5)
    }

    #[test]
    fn timeout_falls_back_when_config_unloadable() {
        let dir = temp_dir("fallback");
        std::fs::create_dir_all(&dir).unwrap();
        let config = dir.join("appkeeper.toml");
        std::fs::write(&config, "not [valid toml").unwrap();
        assert_eq!(compute_timeout_stop_sec(&config), FALLBACK_TIMEOUT_STOP_SEC);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn timeout_computed_from_registered_apps() {
        let dir = temp_dir("apps");
        let apps = dir.join("apps");
        std::fs::create_dir_all(&apps).unwrap();
        let config = dir.join("appkeeper.toml");
        std::fs::write(&config, "[daemon]\napp_dir = \"apps\"\n").unwrap();
        std::fs::write(
            apps.join("demo.toml"),
            "[program.api]\ncommand = \"python -m http.server 8000\"\nstop_timeout = 25\n",
        )
        .unwrap();
        assert_eq!(compute_timeout_stop_sec(&config), 60);
        std::fs::remove_dir_all(&dir).ok();
    }

    // -- install precheck (harden-daemon-autostart 2.5 / 3.1-3.4) ------------

    /// A minimal valid daemon config whose log_dir sits outside `temp`.
    fn precheck_config(dir: &Path) -> crate::config::DaemonConfig {
        let text = format!(
            "[daemon]\nlog_dir = {:?}\n",
            dir.join("logs").to_string_lossy()
        );
        toml::from_str(&text).unwrap()
    }

    #[allow(clippy::too_many_arguments)] // test shim: one flat positional per precheck input
    fn precheck(
        config: Result<&crate::config::DaemonConfig, String>,
        apps: &[crate::registry::ListedApp],
        log_dir: &Path,
        state_dir: Option<&Path>,
        temp_root: &Path,
        platform: Platform,
        pipe_user: Option<&str>,
        account: Option<&str>,
    ) -> PrecheckOutcome {
        install_precheck(&InstallPrecheck {
            config,
            config_path: Path::new(r"C:\Users\alice\AppData\appkeeper\daemon.toml"),
            legacy_config_target: None,
            apps,
            log_dir,
            state_dir,
            temp_root,
            platform,
            default_pipe_user: pipe_user,
            service_account: account,
        })
    }

    /// 3.1 inputs 1+2: a missing config and a broken config refuse the
    /// install (previously both installed with a 90s fallback budget); the
    /// message names the config path and the fix. `--force` is structurally
    /// unable to bypass it (3.4): the precheck input carries no force flag.
    #[test]
    fn precheck_refuses_missing_and_broken_config() {
        let dir = temp_dir("precheck-refuse");
        let temp_root = dir.join("tmp");
        std::fs::create_dir_all(&temp_root).unwrap();
        // Both messages in the shape `install_config` produces: they must
        // name the config path AND a remedy (spec: 错误信息 SHALL 指明配置
        // 路径与补救方式).
        let missing = "daemon config /etc/appkeeper/daemon.toml does not exist — create one \
                       with `ak config --init` or point at an existing one with -c/--config";
        let broken = "daemon config /etc/appkeeper/daemon.toml is invalid — repair it \
                      (`ak config --edit`) or point at a valid one with -c/--config: \
                      daemon.socket: a `tcp://` control endpoint requires a non-empty \
                      auth_token";
        for refusal_msg in [missing.to_string(), broken.to_string()] {
            let outcome = precheck(
                Err(refusal_msg.clone()),
                &[],
                Path::new(""),
                None,
                &temp_root,
                Platform::Unix,
                None,
                None,
            );
            assert!(outcome.refusal.is_some(), "{refusal_msg}");
            assert!(outcome.warnings.is_empty());
            let r = outcome.refusal.as_deref().unwrap();
            assert!(
                r.contains("/etc/appkeeper/daemon.toml"),
                "the refusal names the config path: {r}"
            );
            assert!(r.contains("-c/--config"), "the refusal names a remedy: {r}");
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    /// 3.1 input 3: unparseable registry apps warn per app (name + reason)
    /// but never block the install — the daemon skips them at startup.
    #[test]
    fn precheck_warns_on_broken_apps_without_blocking() {
        let dir = temp_dir("precheck-apps");
        let temp_root = dir.join("tmp");
        std::fs::create_dir_all(&temp_root).unwrap();
        let config = precheck_config(&dir);
        let apps = vec![
            crate::registry::ListedApp {
                name: "broken".into(),
                path: PathBuf::from("/x/broken.toml"),
                description: None,
                broken: Some("cannot parse: missing command".into()),
                origin: crate::registry::Origin::Compose,
            },
            crate::registry::ListedApp {
                name: "good".into(),
                path: PathBuf::from("/x/good.toml"),
                description: None,
                broken: None,
                origin: crate::registry::Origin::Compose,
            },
        ];
        let outcome = precheck(
            Ok(&config),
            &apps,
            &dir.join("logs"),
            None,
            &temp_root,
            Platform::Unix,
            None,
            None,
        );
        assert!(
            outcome.refusal.is_none(),
            "bad apps must not block: {outcome:?}"
        );
        assert_eq!(outcome.warnings.len(), 1, "{outcome:?}");
        let w = &outcome.warnings[0];
        assert!(
            w.contains("broken") && w.contains("cannot parse: missing command"),
            "the warning names the app and the reason: {w}"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// 3.1 inputs 4+5 (2.5): a log_dir or state_dir under the temp root each
    /// warn — with SEPARATE messages (logs vs user-stop records) — and
    /// persistent locations warn about nothing.
    #[test]
    fn precheck_warns_temp_log_dir_and_temp_state_dir_separately() {
        let dir = temp_dir("precheck-temp");
        let temp_root = dir.join("tmp");
        std::fs::create_dir_all(&temp_root).unwrap();
        let config = precheck_config(&dir);
        // Both under /tmp: two distinct warnings.
        let outcome = precheck(
            Ok(&config),
            &[],
            &temp_root.join("appkeeper/logs"),
            Some(&temp_root.join("appkeeper/state")),
            &temp_root,
            Platform::Unix,
            None,
            None,
        );
        assert!(outcome.refusal.is_none(), "{outcome:?}");
        assert_eq!(outcome.warnings.len(), 2, "{outcome:?}");
        let logs_w = outcome
            .warnings
            .iter()
            .find(|w| w.contains("log_dir"))
            .expect("a log_dir warning");
        assert!(
            logs_w.contains("logs are lost on reboot") && logs_w.contains("persistent"),
            "the log warning only talks about logs: {logs_w}"
        );
        let state_w = outcome
            .warnings
            .iter()
            .find(|w| w.contains("state_dir"))
            .expect("a state_dir warning");
        assert!(
            state_w.contains("user-stop") && state_w.contains("state_dir"),
            "the state warning names the records and the fix: {state_w}"
        );
        // Persistent locations: no warnings at all.
        let outcome = precheck(
            Ok(&config),
            &[],
            &dir.join("logs"),
            Some(&dir.join("state")),
            &temp_root,
            Platform::Unix,
            None,
            None,
        );
        assert!(outcome.refusal.is_none());
        assert!(outcome.warnings.is_empty(), "{outcome:?}");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// 3.1 input 6 (5.2's pure half): the Windows default-pipe mismatch warns
    /// with the explicit-socket fix — and only on Windows; an explicit (or
    /// non-default) socket and a matching name stay silent.
    #[test]
    fn precheck_windows_pipe_mismatch() {
        let dir = temp_dir("precheck-pipe");
        let temp_root = dir.join("tmp");
        std::fs::create_dir_all(&temp_root).unwrap();
        let config = precheck_config(&dir);
        let outcome = precheck(
            Ok(&config),
            &[],
            &dir.join("logs"),
            None,
            &temp_root,
            Platform::Windows,
            Some("alice"),
            Some("LocalSystem"),
        );
        assert!(outcome.refusal.is_none());
        assert_eq!(outcome.warnings.len(), 1, "{outcome:?}");
        let w = &outcome.warnings[0];
        assert!(
            w.contains("alice") && w.contains("LocalSystem") && w.contains("socket"),
            "the warning names both sides and the fix: {w}"
        );
        // The installer literally being the service account: no mismatch.
        let outcome = precheck(
            Ok(&config),
            &[],
            &dir.join("logs"),
            None,
            &temp_root,
            Platform::Windows,
            Some("localsystem"),
            Some("LocalSystem"),
        );
        assert!(outcome.warnings.is_empty(), "{outcome:?}");
        // Linux never gets the pipe warning, even with the same inputs.
        let outcome = precheck(
            Ok(&config),
            &[],
            &dir.join("logs"),
            None,
            &temp_root,
            Platform::Unix,
            Some("alice"),
            Some("LocalSystem"),
        );
        assert!(outcome.warnings.is_empty(), "{outcome:?}");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// 5.2 pure half on the derivation side: the pipe-user extraction rides
    /// `Endpoint::default_name`'s shape — the default pipe yields its user
    /// stem, while a tcp:// endpoint, a different socket or a malformed
    /// default all yield `None` (no input for the mismatch warning).
    #[test]
    fn default_pipe_user_extraction_follows_the_default_pipe() {
        let default = r"\\.\pipe\ak-alice.sock";
        assert_eq!(
            default_pipe_user_from(default, default).as_deref(),
            Some("alice")
        );
        // An explicitly declared (non-default) socket disqualifies.
        assert_eq!(
            default_pipe_user_from(r"\\.\pipe\ak-ak.sock", default),
            None
        );
        // The tcp:// form never carries a pipe user.
        assert_eq!(
            default_pipe_user_from("tcp://127.0.0.1:7310", default),
            None
        );
        // A malformed default name is not parseable either.
        assert_eq!(default_pipe_user_from(default, r"\\.\pipe\weird"), None);
        assert_eq!(default_pipe_user_from(default, r"\\.\pipe\ak-.sock"), None);
    }

    /// 5.3: when the CLI resolved the config through the legacy %APPDATA%
    /// fallback, install warns with the migration hint — it names the legacy
    /// path in effect, the machine-level target and the `-c` escape hatch.
    #[test]
    fn precheck_warns_on_legacy_config_fallback() {
        let dir = temp_dir("precheck-legacy");
        let temp_root = dir.join("tmp");
        std::fs::create_dir_all(&temp_root).unwrap();
        let config = precheck_config(&dir);
        let legacy = r"C:\Users\alice\AppData\appkeeper\daemon.toml";
        let target = r"C:\ProgramData\appkeeper\daemon.toml";
        let outcome = install_precheck(&InstallPrecheck {
            config: Ok(&config),
            config_path: Path::new(legacy),
            legacy_config_target: Some(Path::new(target)),
            apps: &[],
            log_dir: &dir.join("logs"),
            state_dir: None,
            temp_root: &temp_root,
            platform: Platform::Windows,
            default_pipe_user: None,
            service_account: None,
        });
        assert!(outcome.refusal.is_none(), "{outcome:?}");
        assert_eq!(outcome.warnings.len(), 1, "{outcome:?}");
        let w = &outcome.warnings[0];
        assert!(
            w.contains(legacy) && w.contains(target) && w.contains("-c"),
            "the hint names the legacy path, the target and -c usage: {w}"
        );
        // No fallback → no hint (Linux always passes None).
        let outcome = precheck(
            Ok(&config),
            &[],
            &dir.join("logs"),
            None,
            &temp_root,
            Platform::Unix,
            None,
            None,
        );
        assert!(outcome.warnings.is_empty(), "{outcome:?}");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// 4.1: the managed-source judgement — systemd via a non-empty
    /// `INVOCATION_ID` on Linux, SCM via the `--service` run mode on Windows,
    /// and manual starts (neither marker) stay unmanaged.
    #[test]
    fn managed_source_judges_the_run_mode_and_environment() {
        fn env_of<'a>(table: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
            move |key: &str| {
                table
                    .iter()
                    .find(|(k, _)| *k == key)
                    .map(|(_, v)| v.to_string())
            }
        }
        // Linux: systemd injects INVOCATION_ID on every start.
        assert_eq!(
            managed_source(
                Platform::Unix,
                false,
                &env_of(&[("INVOCATION_ID", "0e1a2b3c")])
            ),
            Some("systemd")
        );
        // An empty value is not an invocation (manual start in a doctored env).
        assert_eq!(
            managed_source(Platform::Unix, false, &env_of(&[("INVOCATION_ID", "  ")])),
            None
        );
        // Manual start: no marker at all.
        assert_eq!(managed_source(Platform::Unix, false, &env_of(&[])), None);
        // Windows: the `--service` run mode is the criterion — the env lookup
        // is never consulted there.
        assert_eq!(
            managed_source(Platform::Windows, true, &env_of(&[])),
            Some("scm")
        );
        assert_eq!(
            managed_source(
                Platform::Windows,
                true,
                &env_of(&[("INVOCATION_ID", "0e1a2b3c")])
            ),
            Some("scm")
        );
        assert_eq!(managed_source(Platform::Windows, false, &env_of(&[])), None);
    }

    /// 4.2: `/proc/self/cgroup` lines → unit names — v2 unified, v1
    /// controller-prefixed, user-manager nesting, and the non-unit
    /// degenerate shapes (root cgroup, a scope) yield `None`.
    #[test]
    fn unit_name_from_cgroup_takes_the_last_service_segment() {
        // v2 unified: `0::<path>`.
        assert_eq!(
            unit_name_from_cgroup("0::/system.slice/ak.service").as_deref(),
            Some("ak")
        );
        // v1: `<hierarchy>:<controllers>:<path>`.
        assert_eq!(
            unit_name_from_cgroup("11:cpu,cpuacct:/system.slice/ak.service").as_deref(),
            Some("ak")
        );
        // User manager: the service unit is still the last segment.
        assert_eq!(
            unit_name_from_cgroup(
                "0::/user.slice/user-1000.slice/user@1000.service/app.slice/ak.service"
            )
            .as_deref(),
            Some("ak")
        );
        // A bare path (no controller prefix) is accepted too.
        assert_eq!(
            unit_name_from_cgroup("/system.slice/ak.service").as_deref(),
            Some("ak")
        );
        // No unit segment: the root cgroup and a scope are not services.
        assert_eq!(unit_name_from_cgroup("0::/"), None);
        assert_eq!(unit_name_from_cgroup("0::/init.scope"), None);
        assert_eq!(unit_name_from_cgroup(""), None);
    }

    /// 4.4 pure half: the refusal text names the manager and the stop command
    /// — the detected unit, the "current unit" wording when unknown, and the
    /// SCM service name on Windows.
    #[test]
    fn managed_stop_message_names_the_service_manager_command() {
        assert!(
            managed_stop_message("systemd", Some("ak"), None).contains("systemctl stop ak"),
            "the systemd hint names the unit"
        );
        let unknown = managed_stop_message("systemd", None, None);
        assert!(
            unknown.contains("systemctl stop") && unknown.contains("current unit"),
            "the unknown-unit fallback keeps the systemctl stop shape: {unknown}"
        );
        assert!(
            managed_stop_message("scm", None, Some("ak")).contains("sc stop ak"),
            "the SCM hint names the service"
        );
        assert!(
            managed_stop_message("scm", Some("ak"), None).contains("sc stop ak"),
            "the SCM hint does not consume the cgroup unit"
        );
    }

    /// 3.2/3.4: the wired gate refuses a missing config with zero side
    /// effects — the unit file that WOULD be written does not exist after the
    /// refusal, and `--force` does not change the outcome.
    #[test]
    fn install_gate_refusal_writes_nothing_even_with_force() {
        let dir = temp_dir("precheck-gate");
        std::fs::create_dir_all(&dir).unwrap();
        let unit_file = dir.join("ak.service");
        // Case 1: no config file at all.
        let missing = dir.join("missing.toml");
        // Case 2: a parseable but INVALID config (tcp endpoint without the
        // required auth_token) — the same refusal, with path and remedy.
        let broken = dir.join("broken.toml");
        std::fs::write(&broken, "[daemon]\nsocket = \"tcp://127.0.0.1:7310\"\n").unwrap();
        for config_path in [&missing, &broken] {
            for force in [false, true] {
                let forced = ServiceOptions { force, ..opts() };
                let scan = InstallScan::load(config_path);
                let err = scan
                    .config
                    .as_ref()
                    .expect_err("the bad config must refuse");
                assert!(
                    err.contains("config") && err.contains("-c/--config"),
                    "the refusal names the config and the fix: {err}"
                );
                let err =
                    run_install_precheck(&scan, &forced, false).expect_err("the refusal must bail");
                assert!(
                    err.to_string().contains("-c/--config"),
                    "the refusal names the fix: {err:#}"
                );
                assert!(
                    !unit_file.exists(),
                    "the refusal path must not write the unit file"
                );
            }
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    /// 3.3: one load feeds the precheck and `TimeoutStopSec` — the scan's
    /// timeout equals the lenient runtime helper's answer on a valid config,
    /// while a BROKEN config refuses the install (the runtime helper keeps
    /// its 90s fallback because a running service cannot refuse to run).
    #[test]
    fn scan_timeout_matches_runtime_and_refuses_broken_config() {
        let dir = temp_dir("precheck-scan");
        let apps = dir.join("apps");
        std::fs::create_dir_all(&apps).unwrap();
        let config = dir.join("appkeeper.toml");
        std::fs::write(&config, "[daemon]\napp_dir = \"apps\"\n").unwrap();
        std::fs::write(
            apps.join("demo.toml"),
            "[program.api]\ncommand = \"python -m http.server 8000\"\nstop_timeout = 25\n",
        )
        .unwrap();
        let scan = InstallScan::load(&config);
        let ok_cfg = scan.config.as_ref().expect("valid config");
        assert_eq!(scan.timeout_stop_sec, 60);
        assert_eq!(
            scan.timeout_stop_sec,
            compute_timeout_stop_sec(&config),
            "the two loads must not disagree"
        );
        // The effective state dir rides the same scan inputs.
        let state_dir = crate::config::resolve_state_dir(&crate::config::StateDirEnv::capture(
            ok_cfg.daemon.state_dir.as_deref(),
            false,
        ));
        assert!(
            state_dir.is_some(),
            "the platform default is derivable here"
        );
        std::fs::remove_dir_all(&dir).ok();

        // Broken config: the install scan carries the refusal (with the path
        // and the remedy attached)…
        std::fs::create_dir_all(&dir).unwrap();
        let broken = dir.join("broken.toml");
        std::fs::write(&broken, "not [valid toml").unwrap();
        let scan = InstallScan::load(&broken);
        let err = scan
            .config
            .as_ref()
            .expect_err("a broken config must refuse");
        assert!(
            err.contains("broken.toml") && err.contains("-c/--config"),
            "the refusal names the path and the remedy: {err}"
        );
        assert!(run_install_precheck(&scan, &opts(), false).is_err());
        // …while the service runtime keeps its conservative fallback.
        assert_eq!(compute_timeout_stop_sec(&broken), FALLBACK_TIMEOUT_STOP_SEC);
        std::fs::remove_dir_all(&dir).ok();
    }

    // -- Windows SCM pure logic (exercised on every platform) -----------------

    fn opts() -> ServiceOptions {
        ServiceOptions {
            unit_file: None,
            name: None,
            desc: None,
            user: None,
            force: false,
            start: false,
            restart: RestartPolicy::Always,
        }
    }

    #[test]
    fn service_name_must_be_non_empty_without_slashes() {
        assert!(is_valid_service_name("ak"));
        assert!(is_valid_service_name("ak-xtask-e2e-1234"));
        assert!(!is_valid_service_name(""));
        assert!(!is_valid_service_name("a/b"));
        assert!(!is_valid_service_name("a\\b"));
    }

    #[test]
    fn builtin_account_mapping_is_case_insensitive() {
        assert_eq!(
            builtin_account("LocalSystem").unwrap(),
            "NT AUTHORITY\\SYSTEM"
        );
        assert_eq!(
            builtin_account("localsystem").unwrap(),
            "NT AUTHORITY\\SYSTEM"
        );
        assert_eq!(
            builtin_account("LOCALSERVICE").unwrap(),
            "NT AUTHORITY\\LocalService"
        );
        assert_eq!(
            builtin_account("NetworkService").unwrap(),
            "NT AUTHORITY\\NetworkService"
        );
        for bad in ["Administrator", "SYSTEM", "Local Service", ""] {
            assert!(
                builtin_account(bad).is_err(),
                "expected {bad:?} to be rejected"
            );
        }
    }

    #[test]
    fn binpath_renders_whole_quoted_exe_and_config() {
        // Paths with spaces must be wrapped in quotes as a whole — the SCM
        // binary-path parser does not understand backslash-escaped spaces.
        let bp = render_binpath(
            Path::new(r"C:\Program Files\appkeeper\ak.exe"),
            Path::new(r"C:\ProgramData\appkeeper\daemon.toml"),
            "ak",
        );
        assert_eq!(
            bp,
            r#""C:\Program Files\appkeeper\ak.exe" daemon start --service ak --config "C:\ProgramData\appkeeper\daemon.toml""#
        );
        // Space-free paths are quoted too (uniform rule, read-back matches).
        let bp = render_binpath(
            Path::new(r"C:\ak\ak.exe"),
            Path::new(r"C:\ak\daemon.toml"),
            "ak",
        );
        assert_eq!(
            bp,
            r#""C:\ak\ak.exe" daemon start --service ak --config "C:\ak\daemon.toml""#
        );
    }

    #[test]
    fn binpath_normalization_collapses_whitespace_only() {
        assert_eq!(normalize_binpath("  a   b  c "), "a b c");
        assert_eq!(normalize_binpath("a b"), "a b");
        // Quoting is significant and must survive normalization.
        assert_eq!(normalize_binpath("\"x\" run"), "\"x\" run");
    }

    #[test]
    fn install_flags_reject_platform_misuse() {
        // Linux names units by --unit-file, never --name.
        let unix_with_name = ServiceOptions {
            name: Some("ak".into()),
            ..opts()
        };
        assert!(check_install_flags(Platform::Unix, &unix_with_name).is_err());
        assert!(check_uninstall_flags(Platform::Unix, &unix_with_name).is_err());
        // Windows names the service with --name, never --unit-file.
        let win_with_unit = ServiceOptions {
            unit_file: Some(PathBuf::from("/etc/systemd/system/ak.service")),
            ..opts()
        };
        assert!(check_install_flags(Platform::Windows, &win_with_unit).is_err());
        assert!(check_uninstall_flags(Platform::Windows, &win_with_unit).is_err());
        // Properly-scoped combos pass.
        let unix_plain = ServiceOptions {
            unit_file: Some(PathBuf::from("/etc/systemd/system/ak.service")),
            ..opts()
        };
        assert!(check_install_flags(Platform::Unix, &unix_plain).is_ok());
        assert!(check_uninstall_flags(Platform::Unix, &unix_plain).is_ok());
        let win_plain = ServiceOptions {
            name: Some("ak".into()),
            ..opts()
        };
        assert!(check_install_flags(Platform::Windows, &win_plain).is_ok());
        assert!(check_uninstall_flags(Platform::Windows, &win_plain).is_ok());
    }

    #[test]
    fn install_flags_validate_windows_name_and_user() {
        let base = opts();
        assert!(check_install_flags(Platform::Windows, &base).is_ok());
        let bad_name = ServiceOptions {
            name: Some("a/b".into()),
            ..base.clone()
        };
        assert!(check_install_flags(Platform::Windows, &bad_name).is_err());
        assert!(check_uninstall_flags(Platform::Windows, &bad_name).is_err());
        let bad_user = ServiceOptions {
            user: Some("Administrator".into()),
            ..base.clone()
        };
        assert!(check_install_flags(Platform::Windows, &bad_user).is_err());
        let good_user = ServiceOptions {
            user: Some("networkservice".into()),
            ..base.clone()
        };
        assert!(check_install_flags(Platform::Windows, &good_user).is_ok());
        // systemd User= is free-form: no builtin-account restriction there.
        let unix_user = ServiceOptions {
            user: Some("anybody".into()),
            ..base.clone()
        };
        assert!(check_install_flags(Platform::Unix, &unix_user).is_ok());
    }
}
