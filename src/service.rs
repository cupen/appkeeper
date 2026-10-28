//! System service registration: `ak service install|uninstall`.
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

use anyhow::{Result, bail};
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
/// systemd `Restart=` policies accepted by `service install --restart`.
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
fn quote_scm_token(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "\\\""))
}

/// Render the full service binpath: `"<exe>" run --config "<conf>" --service
/// <name>`. Mirrors the Linux unit's ExecStart plus the hidden diversion
/// flag the SCM shell needs to know which service name it is running as.
pub fn render_binpath(exe: &Path, config_path: &Path, service_name: &str) -> String {
    format!(
        "{} daemon --config {} --service {}",
        quote_scm_token(&exe.display().to_string()),
        quote_scm_token(&config_path.display().to_string()),
        service_name
    )
}

/// Normalize a binpath read back from the SCM before snapshot comparison:
/// trim and collapse incidental whitespace runs between tokens. Applied to
/// both sides, so exactly-rendered binpaths still compare equal.
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

/// Render the unit file. `timeout_stop_sec` should already account for the
/// slowest configured graceful stop (see `compute_timeout_stop_sec`).
/// `desc` defaults to `DEFAULT_DESCRIPTION`.
#[cfg_attr(windows, allow(dead_code))]
pub fn render_unit(
    exe: &Path,
    config_path: &Path,
    desc: Option<&str>,
    user: Option<&str>,
    restart: RestartPolicy,
    timeout_stop_sec: u64,
) -> String {
    let mut s = String::new();
    s.push_str("[Unit]\n");
    s.push_str(&format!(
        "Description={}\n",
        desc.unwrap_or(DEFAULT_DESCRIPTION)
    ));
    s.push_str("After=network.target\n");
    s.push('\n');
    s.push_str("[Service]\n");
    s.push_str(&format!(
        "ExecStart={} daemon --config {}\n",
        quote_exec_arg(&exe.display().to_string()),
        quote_exec_arg(&config_path.display().to_string())
    ));
    s.push_str(&format!("Restart={restart}\n"));
    s.push_str("RestartSec=3\n");
    s.push_str("KillSignal=SIGTERM\n");
    s.push_str(&format!("TimeoutStopSec={timeout_stop_sec}\n"));
    // The control socket lives under /run/appkeeper/<uid>/ak.sock. Declaring
    // RuntimeDirectory has systemd create /run/appkeeper (0755) at start and
    // remove it at stop. Deliberately NO ProtectSystem=strict / ReadWritePaths
    // sandboxing here: those would make that directory unwritable and the
    // daemon could not bind. The daemon also creates the directory itself
    // when it is missing, so an older unit without this key still works.
    s.push_str("RuntimeDirectory=appkeeper\n");
    s.push_str("RuntimeDirectoryMode=0755\n");
    if let Some(u) = user {
        s.push_str(&format!("User={u}\n"));
    }
    s.push('\n');
    s.push_str("[Install]\n");
    s.push_str("WantedBy=multi-user.target\n");
    s
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

/// Try to load the daemon config and every registered app and derive
/// `TimeoutStopSec` from the largest `stop_timeout`. Any load failure falls
/// back to a conservative 90s.
#[cfg_attr(windows, allow(dead_code))]
pub fn compute_timeout_stop_sec(config_path: &Path) -> u64 {
    let compute = || -> Result<u64> {
        let (config, _) = crate::config::DaemonConfig::load_or_default(config_path)?;
        let config_dir = config_path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let apps = crate::registry::ListedApp::good(crate::registry::list(&config, config_dir)?);
        let mut max = 0.0f64;
        for listed in apps {
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
    };
    compute().unwrap_or(FALLBACK_TIMEOUT_STOP_SEC)
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
use anyhow::Context as _;

#[cfg(unix)]
fn require_root() -> Result<()> {
    if unsafe { libc::geteuid() } != 0 {
        bail!("service install/uninstall requires root (try sudo)");
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
    let rendered = render_unit(
        &exe,
        &config_abs,
        opts.desc.as_deref(),
        opts.user.as_deref(),
        opts.restart,
        compute_timeout_stop_sec(config_path),
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
    std::fs::remove_file(path).with_context(|| format!("cannot remove {}", path.display()))?;
    println!("unit removed: {}", path.display());
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
                "service install/uninstall requires administrator privileges (run from an elevated shell)"
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

    // -- service mode: the SCM side of `ak daemon --service <name>` ---------

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

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn temp_dir(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!("ak-test-svc-{tag}-{}", std::process::id()))
    }

    #[test]
    fn unit_file_accepts_legal_and_rejects_illegal() {
        assert!(is_valid_unit_file(Path::new("/etc/systemd/system/ak.service")));
        assert!(is_valid_unit_file(Path::new("ak-prod_1.service")));
        assert!(is_valid_unit_file(Path::new("a.service")));
        assert!(is_valid_unit_file(Path::new("my-app_v2.service")));
        for bad in [
            "appkeeper",             // missing .service
            ".service",            // empty stem
            "ends..service",       // stem ends with '.'
            "my app.service",      // space in stem
            "x.service.bak",       // wrong extension
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

    #[test]
    fn renders_unit_without_user() {
        let unit = render_unit(
            Path::new("/usr/local/bin/ak"),
            Path::new("/etc/appkeeper/daemon.toml"),
            None,
            None,
            RestartPolicy::Always,
            90,
        );
        assert_eq!(
            unit,
            "[Unit]\n\
             Description=appkeeper process keeper\n\
             After=network.target\n\
             \n\
             [Service]\n\
             ExecStart=/usr/local/bin/ak daemon --config /etc/appkeeper/daemon.toml\n\
             Restart=always\n\
             RestartSec=3\n\
             KillSignal=SIGTERM\n\
             TimeoutStopSec=90\n\
             RuntimeDirectory=appkeeper\n\
             RuntimeDirectoryMode=0755\n\
             \n\
             [Install]\n\
             WantedBy=multi-user.target\n"
        );
    }

    /// 7.1: the unit creates the control socket's parent directory, and adds
    /// nothing that would make it unwritable (a sandbox directive here would
    /// silently break socket creation for every systemd install).
    #[test]
    fn unit_declares_runtime_directory_and_no_socket_blocking_sandbox() {
        let unit = render_unit(
            Path::new("/usr/local/bin/ak"),
            Path::new("/etc/appkeeper/daemon.toml"),
            None,
            Some("akuser"),
            RestartPolicy::Always,
            90,
        );
        assert!(
            unit.contains("RuntimeDirectory=appkeeper\n"),
            "systemd must create /run/appkeeper: {unit}"
        );
        assert!(unit.contains("RuntimeDirectoryMode=0755\n"), "{unit}");
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

    #[test]
    fn renders_unit_with_user_and_quoted_paths() {
        let unit = render_unit(
            Path::new("/opt/my tools/appkeeper"),
            Path::new("/etc/my conf/appkeeper.toml"),
            None,
            Some("svc-ak"),
            RestartPolicy::Always,
            60,
        );
        assert!(unit.contains(
            "ExecStart=\"/opt/my tools/appkeeper\" daemon --config \"/etc/my conf/appkeeper.toml\"\n"
        ));
        assert!(unit.contains("User=svc-ak\n"));
        assert!(unit.contains("TimeoutStopSec=60\n"));
    }

    #[test]
    fn render_honours_restart_policy() {
        let unit = render_unit(
            Path::new("/usr/local/bin/ak"),
            Path::new("/etc/appkeeper/daemon.toml"),
            None,
            None,
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
            r#""C:\Program Files\appkeeper\ak.exe" daemon --config "C:\ProgramData\appkeeper\daemon.toml" --service ak"#
        );
        // Space-free paths are quoted too (uniform rule, read-back matches).
        let bp = render_binpath(
            Path::new(r"C:\ak\ak.exe"),
            Path::new(r"C:\ak\daemon.toml"),
            "ak",
        );
        assert_eq!(
            bp,
            r#""C:\ak\ak.exe" daemon --config "C:\ak\daemon.toml" --service ak"#
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
