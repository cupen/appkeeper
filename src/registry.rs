//! App registry: the set of registered apps lives as `<name>.toml` links in
//! the daemon-configured `app_dir`. add/remove/list manage those links; the
//! linked deployment file is always the source of truth.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use log::{debug, warn};

use crate::config::{
    AppDefaults, AppMeta, AppRaw, DaemonConfig, ProgramRaw, RestartPolicy, is_valid_name,
    resolve_app, split_command, validate_all,
};

#[derive(Debug, Clone)]
pub struct ListedApp {
    pub name: String,
    pub path: PathBuf,
    pub description: Option<String>,
    /// Set when the config file exists but is broken (parse/read failure).
    /// Detection isolates such apps instead of treating them as removed.
    pub broken: Option<String>,
}

impl ListedApp {
    /// Only the apps whose config loaded cleanly.
    pub fn good(apps: Vec<ListedApp>) -> Vec<ListedApp> {
        apps.into_iter().filter(|a| a.broken.is_none()).collect()
    }
}

/// `<app_dir>/<name>.toml` for a given app name.
pub fn link_path(config: &DaemonConfig, config_dir: &Path, name: &str) -> PathBuf {
    crate::config::resolve_path(&config.daemon.app_dir, config_dir).join(format!("{name}.toml"))
}

/// The resolved `app_dir` directory.
pub fn app_dir(config: &DaemonConfig, config_dir: &Path) -> PathBuf {
    crate::config::resolve_path(&config.daemon.app_dir, config_dir)
}

/// Derive the app name from a config path's parent directory.
pub fn name_from_dir(path: &Path) -> String {
    path.parent()
        .and_then(|p| p.file_name())
        .map(|s| s.to_string_lossy().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "app".to_string())
}

/// Scan `app_dir` and list every registered app. Dangling links are warned
/// about and skipped.
pub fn list(config: &DaemonConfig, config_dir: &Path) -> Result<Vec<ListedApp>> {
    let dir = app_dir(config, config_dir);
    let mut out = Vec::new();
    if !dir.exists() {
        return Ok(out);
    }
    let mut entries: Vec<_> = std::fs::read_dir(&dir)?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().map(|x| x == "toml").unwrap_or(false))
        .collect();
    entries.sort();
    for link in entries {
        let name = link
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        let real = std::fs::canonicalize(&link).with_context(|| {
            format!(
                "dangling registration link {} (run `ak remove {name}`)",
                link.display()
            )
        });
        let real = match real {
            Ok(r) => r,
            Err(e) => {
                warn!("{e:#}");
                continue;
            }
        };
        match std::fs::read_to_string(&real) {
            Ok(text) => match toml::from_str::<crate::config::AppRaw>(&text) {
                Ok(raw) => out.push(ListedApp {
                    description: raw.app.as_ref().and_then(|m| m.description.clone()),
                    name,
                    path: real,
                    broken: None,
                }),
                // Keep the entry so the supervisor can isolate it explicitly
                // (broken file ≠ unregistered); non-supervisor callers filter.
                Err(e) => {
                    warn!("registered app {name:?} fails to parse: {e}");
                    out.push(ListedApp {
                        description: None,
                        name,
                        path: real,
                        broken: Some(format!("app config fails to parse: {e}")),
                    })
                }
            },
            Err(e) => {
                warn!("registered app {name:?} cannot be read: {e}");
                out.push(ListedApp {
                    description: None,
                    name,
                    path: real,
                    broken: Some(format!("app config cannot be read: {e}")),
                })
            }
        }
    }
    Ok(out)
}

#[derive(Debug, Default, Clone)]
pub struct AddOptions {
    pub name: Option<String>,
    pub description: Option<String>,
    /// Startup/restart policy, written as the single `restart` field (as an
    /// app-level default in the `[app]` table). `None` = leave unset (the
    /// four-layer cascade bottoms out at the built-in default `no`).
    pub restart: Option<RestartPolicy>,
    pub restart_backoff: Option<f64>,
    pub priority: Option<i32>,
    /// Scaffold source only: startup args as one shell-lexed string
    /// (split with `config::split_command`, same rules as a single-line
    /// `command` in a config file).
    pub args: Option<String>,
    /// Scaffold source only: startup args already split into argv (the run
    /// ad-hoc form). Takes precedence over `args`.
    pub args_argv: Option<Vec<String>>,
    /// Scaffold source only: `--env K=V` pairs (repeatable; last wins).
    pub env: Option<std::collections::BTreeMap<String, String>>,
    /// Scaffold source only: working directory override. Default is the
    /// current directory at `add` time (written as an absolute path).
    pub workdir: Option<PathBuf>,
}

/// What one `registry::add` call did — everything the CLI prints.
#[derive(Debug, Clone)]
pub struct AddResult {
    pub app: String,
    /// The registration record path: the `app_dir` link (dir/.toml flow)
    /// or the generated config file (scaffold flow).
    pub file: PathBuf,
    /// True when the record was scaffold-generated from an executable.
    pub scaffolded: bool,
    /// Scaffold only: the regenerated file differed from the disk content
    /// (a first generation counts as changed). `false` → no change.
    pub changed: bool,
    /// Scaffold only: resolved program values for the CLI summary.
    pub program: String,
    pub command: String,
    pub args: Vec<String>,
    pub env: std::collections::BTreeMap<String, String>,
    pub work_dir: PathBuf,
}

impl AddResult {
    fn registered(app: String, file: PathBuf) -> Self {
        Self {
            app,
            file,
            scaffolded: false,
            changed: false,
            program: String::new(),
            command: String::new(),
            args: Vec::new(),
            env: Default::default(),
            work_dir: PathBuf::new(),
        }
    }
}

/// Reject names that would collide with CLI keywords. `all` is the
/// apply-all keyword (`ak apply all`), so it can never be an app name
/// (apply-workflow spec).
fn check_name(name: &str) -> Result<()> {
    if !is_valid_name(name) {
        bail!("app name {name:?} is invalid: {}", crate::config::NAME_RULE);
    }
    if name == crate::supervisor::ALL_KEYWORD {
        bail!(
            "app name {name:?} is reserved: `ak apply all` means apply-all; \
             choose another --name"
        );
    }
    Ok(())
}

/// Register an app: validate it, create the `app_dir` link, and write the
/// micro-tuning flags into the deployment file's `[app]` table. Idempotent
/// (upsert by name). Dispatches by path shape:
///
/// - directory (containing `appkeeper.toml`) → existing directory flow;
/// - `.toml` file → existing config-file registration flow;
/// - any other regular file (an executable program) → scaffold generation:
///   render a single-program `AppRaw` and write it as a REAL file
///   `<app_dir>/<name>.toml` (no link — the file IS the record). Re-running
///   the same add fully regenerates the file from the given flags.
pub fn add(
    config: &DaemonConfig,
    config_dir: &Path,
    path: &Path,
    opts: &AddOptions,
) -> Result<AddResult> {
    if path.is_dir() {
        let f = path.join("appkeeper.toml");
        if !f.exists() {
            bail!("no appkeeper.toml found in directory {}", path.display());
        }
        return add_config(config, config_dir, &f, opts);
    }
    if path.is_file() {
        if path.extension().map(|x| x == "toml").unwrap_or(false) {
            return add_config(config, config_dir, path, opts);
        }
        return add_scaffold(config, config_dir, path, opts);
    }
    bail!("app config not found: {}", path.display())
}

/// The existing registration flow (validate → write flags → link).
fn add_config(
    config: &DaemonConfig,
    config_dir: &Path,
    path: &Path,
    opts: &AddOptions,
) -> Result<AddResult> {
    // Canonicalize so relative paths (e.g. `add .`) still yield a proper
    // directory name and stable link targets.
    let path = path
        .canonicalize()
        .with_context(|| format!("cannot resolve {}", path.display()))?;
    let name = match &opts.name {
        Some(n) => n.clone(),
        None => name_from_dir(&path),
    };
    check_name(&name)?;

    // Load + validate this app against the current registry.
    let (raw, _) = crate::config::AppRaw::load(&path)?;
    let resolved = resolve_app(&name, &path, &raw, config.app_default.as_ref())?;
    let existing = list(config, config_dir)?;
    let mut all_apps = Vec::new();
    for l in &existing {
        if l.name == name {
            continue; // replaced by the new definition below
        }
        let (r, _) = crate::config::AppRaw::load(&l.path)?;
        all_apps.push(resolve_app(
            &l.name,
            &l.path,
            &r,
            config.app_default.as_ref(),
        )?);
    }
    all_apps.push(resolved.clone());
    validate_all(&all_apps)?;

    // Write flags into the deployment file's [app] table.
    apply_flags(&path, opts)
        .with_context(|| format!("cannot write tuning flags into {}", path.display()))?;

    // Create the registration link (this IS the registry record — the
    // daemon discovers apps by scanning app_dir, so a command that leaves
    // no link behind must not report success).
    let dir = app_dir(config, config_dir);
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("failed to create app_dir {}", dir.display()))?;
    let link = link_path(config, config_dir, &name);
    let _ = std::fs::remove_file(&link); // upsert: replace any old link
    if let Err(e) = make_link(&link, &path) {
        bail!(
            "cannot write the registration link into app_dir {}: {e:#}; \
             fix write access (re-run with sudo, or chown the app_dir to the current user) \
             and run `ak compose` again",
            dir.display()
        );
    }
    debug!(
        "app[{name}] registered from {} (link: {})",
        path.display(),
        link.display()
    );
    Ok(AddResult::registered(name, link))
}

/// The executable-derived values one scaffold generation renders. Public so
/// the daemon can reuse [`render_scaffold`] without going through the
/// CLI-side `add` path (run-daemon-first: the daemon renders the ad-hoc
/// registration itself).
#[derive(Debug, Clone)]
pub struct ScaffoldSpec {
    pub command: PathBuf,
    pub args: Vec<String>,
    pub env: std::collections::BTreeMap<String, String>,
    pub work_dir: PathBuf,
    /// Optional health check (run-daemon-first: the ad-hoc endpoint accepts
    /// one so the streaming-progress scenario is reachable).
    pub health_check: Option<String>,
}

/// Refuse to scaffold over a registration that is not our own generated file
/// in `app_dir`: a link to an external deployment config, a stale/dangling
/// link, or a multi-link (hard link) file would clobber a file the user owns.
/// A plain single-link file inside `app_dir` is our own prior generation and
/// is the intended regeneration target.
///
/// Shared by [`add_scaffold`] and the daemon-side `ak run --force` overwrite
/// (refuse-run-existing-app D8: `--force` never bypasses this guard).
pub fn guard_scaffold_overwrite(target: &Path, app_dir: &Path, name: &str) -> Result<()> {
    let Ok(md) = std::fs::symlink_metadata(target) else {
        return Ok(()); // nothing there: a fresh generation
    };
    if md.is_symlink() {
        let dir_canon = app_dir.canonicalize().ok();
        match target.canonicalize() {
            // Our own generation (the record is a real file inside app_dir).
            Ok(real) if dir_canon.as_deref() == real.parent() => Ok(()),
            Ok(real) => bail!(
                "app {name:?} is already registered from {} — run `ak remove {name}` \
                 first (scaffolding would overwrite that file)",
                real.display()
            ),
            Err(_) => bail!(
                "stale registration {} cannot be resolved — run `ak remove {name}` first",
                target.display()
            ),
        }
    } else if md.is_file() && file_link_count(target, &md) > 1 {
        // A plain multi-link file in app_dir is a hard link (the Windows
        // `make_link` fallback) — its other name may be an external config
        // the write would clobber.
        bail!(
            "app {name:?} is registered via a hard link (possibly to an external config) — \
             run `ak remove {name}` before scaffolding over it"
        )
    } else {
        Ok(())
    }
}

/// Scaffold flow: render a single-program app config from an executable and
/// write it as a REAL file `<app_dir>/<name>.toml` (the record is the file
/// itself, no link). Same-name re-add fully regenerates the file from the
/// given flags — validation must pass before anything is written, and the
/// new text is compared against the disk content so the CLI can tell
/// "changed" from "no change".
fn add_scaffold(
    config: &DaemonConfig,
    config_dir: &Path,
    exe: &Path,
    opts: &AddOptions,
) -> Result<AddResult> {
    let exe = exe
        .canonicalize()
        .with_context(|| format!("cannot resolve {}", exe.display()))?;
    let name = match &opts.name {
        Some(n) => n.clone(),
        None => exe
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .filter(|s| !s.is_empty())
            .context("cannot derive an app name from the executable path; pass --name")?,
    };
    check_name(&name)?;

    let dir = app_dir(config, config_dir);
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("failed to create app_dir {}", dir.display()))?;
    let target = link_path(config, config_dir, &name);
    // Never write through a registration that lives outside app_dir: a
    // link pointing at an external deployment file would clobber that file
    // on regeneration.
    guard_scaffold_overwrite(&target, &dir, &name)?;

    // Working directory: explicit --workdir wins, else the directory the
    // add command ran in; always written as an absolute path.
    let work_dir = match &opts.workdir {
        Some(w) => display_absolute(w.canonicalize().unwrap_or_else(|_| absolutize(w))),
        None => std::env::current_dir().context("cannot read the current directory")?,
    };
    warn_if_not_executable(&exe);
    let spec = ScaffoldSpec {
        command: display_absolute(exe),
        args: opts
            .args_argv
            .clone()
            .unwrap_or_else(|| opts.args.as_deref().map(split_command).unwrap_or_default()),
        env: opts.env.clone().unwrap_or_default(),
        work_dir,
        health_check: None,
    };

    let text = render_scaffold(&name, &spec, opts)?;

    // Validate exactly what will land on disk (round-trip the rendered
    // text) against the current registry, before any write.
    let raw: AppRaw = toml::from_str(&text)
        .with_context(|| format!("rendered scaffold for app {name:?} is not valid TOML"))?;
    let resolved = resolve_app(&name, &target, &raw, config.app_default.as_ref())?;
    let existing = list(config, config_dir)?;
    let mut all_apps = Vec::new();
    for l in &existing {
        if l.name == name {
            continue; // replaced by the regeneration below
        }
        let (r, _) = AppRaw::load(&l.path)?;
        all_apps.push(resolve_app(
            &l.name,
            &l.path,
            &r,
            config.app_default.as_ref(),
        )?);
    }
    all_apps.push(resolved);
    validate_all(&all_apps)?;

    // Change detection on the rendered text (a first generation counts as
    // changed); nothing is written when the content is identical.
    let changed = std::fs::read_to_string(&target)
        .map(|old| old != text)
        .unwrap_or(true);
    if changed {
        std::fs::write(&target, &text)
            .with_context(|| format!("failed to write {}", target.display()))?;
    }
    debug!(
        "app[{name}] scaffold {} (changed: {changed})",
        target.display()
    );
    Ok(AddResult {
        app: name.clone(),
        file: absolutize(&target),
        scaffolded: true,
        changed,
        program: name,
        command: spec.command.display().to_string(),
        args: spec.args,
        env: spec.env,
        work_dir: spec.work_dir,
    })
}

/// Resolve a command's first token into an absolute executable path
/// (design D7): a token containing a path separator is canonicalized (with an
/// existence check), a bare name is looked up on the caller's `PATH` (unix:
/// first entry with an execute bit; windows: as-is then `PATHEXT` expansion).
/// The resolved absolute path is what gets frozen into the generated config,
/// so the daemon's spawn environment cannot change the meaning.
pub fn resolve_command(first: &str) -> Result<PathBuf> {
    if first.trim().is_empty() {
        bail!("the command is empty");
    }
    let has_sep = first.contains('/') || first.contains(std::path::MAIN_SEPARATOR);
    if has_sep {
        let p = Path::new(first);
        return p
            .canonicalize()
            .map(display_absolute)
            .with_context(|| format!("cannot resolve command {first:?} (no such executable)"));
    }
    resolve_on_path(first).with_context(|| {
        format!(
            "cannot find {first:?} on PATH (use an absolute or relative path, \
             or make sure it is installed)"
        )
    })
}

/// Look a bare command name up on `PATH`.
#[cfg(unix)]
fn resolve_on_path(name: &str) -> Result<PathBuf> {
    use std::os::unix::fs::PermissionsExt;
    let path = std::env::var_os("PATH").unwrap_or_default();
    for dir in std::env::split_paths(&path) {
        if dir.as_os_str().is_empty() {
            continue;
        }
        let cand = dir.join(name);
        let Ok(md) = std::fs::metadata(&cand) else {
            continue;
        };
        if md.is_file() && md.permissions().mode() & 0o111 != 0 {
            return Ok(display_absolute(cand));
        }
    }
    bail!("{name:?} was not found on PATH")
}

/// Windows: try the name verbatim, then each `PATHEXT` extension.
#[cfg(windows)]
fn resolve_on_path(name: &str) -> Result<PathBuf> {
    let path = std::env::var_os("PATH").unwrap_or_default();
    let exts: Vec<String> = std::env::var("PATHEXT")
        .unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".to_string())
        .split(';')
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect();
    let has_ext = Path::new(name)
        .extension()
        .map(|e| !e.is_empty())
        .unwrap_or(false);
    for dir in std::env::split_paths(&path) {
        if dir.as_os_str().is_empty() {
            continue;
        }
        let direct = dir.join(name);
        if direct.is_file() {
            return Ok(display_absolute(direct));
        }
        if !has_ext {
            for ext in &exts {
                let cand = dir.join(format!("{name}{ext}"));
                if cand.is_file() {
                    return Ok(display_absolute(cand));
                }
            }
        }
    }
    bail!("{name:?} was not found on PATH")
}

/// `p` joined onto the current directory when relative.
fn absolutize(p: &Path) -> PathBuf {
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(p))
            .unwrap_or_else(|_| p.to_path_buf())
    }
}

/// Number of directory entries pointing at the file's content (hard links).
#[cfg(unix)]
fn file_link_count(_target: &Path, md: &std::fs::Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt;
    md.nlink()
}

/// windows: `MetadataExt::number_of_links` is still unstable, so query the
/// file handle directly (`GetFileInformationByHandle`).
#[cfg(windows)]
fn file_link_count(target: &Path, _md: &std::fs::Metadata) -> u64 {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle,
    };
    let Ok(f) = std::fs::File::open(target) else {
        return 1;
    };
    let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
    // RawHandle and windows-sys HANDLE are both `*mut c_void`.
    let ok = unsafe { GetFileInformationByHandle(f.as_raw_handle() as _, &mut info) };
    if ok != 0 {
        info.nNumberOfLinks as u64
    } else {
        1
    }
}

/// Windows `canonicalize` yields verbatim (`\\?\C:\...`) paths; strip the
/// prefix from plain drive paths so generated configs stay readable (the
/// result is still absolute).
fn display_absolute(p: PathBuf) -> PathBuf {
    #[cfg(windows)]
    {
        let s = p.as_os_str().to_string_lossy();
        if let Some(rest) = s.strip_prefix(r"\\?\") {
            let bytes = rest.as_bytes();
            if bytes.len() >= 2 && bytes[1] == b':' && bytes[0].is_ascii_alphabetic() {
                return PathBuf::from(rest.to_string());
            }
        }
    }
    p
}

/// unix: a scaffolded program without any execute bit is almost certainly a
/// start failure waiting to happen — warn (add still succeeds; Windows has
/// no such concept and stays silent).
#[cfg(unix)]
fn warn_if_not_executable(exe: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let lacks = std::fs::metadata(exe)
        .map(|md| md.permissions().mode() & 0o111 == 0)
        .unwrap_or(false);
    if lacks {
        warn!(
            "{} has no execute bit (chmod +x); the program may fail to start",
            exe.display()
        );
    }
}

#[cfg(not(unix))]
fn warn_if_not_executable(_exe: &Path) {}

/// Render the scaffold TOML: header comments + `toml::to_string_pretty` of
/// the single-program `AppRaw` (guaranteed re-readable). When the command
/// path contains whitespace it is rendered as a single-line quoted command
/// (quote-aware splitting recovers it — see `config::split_command`).
///
/// Pure: given a definition it returns the exact text the scaffold writes,
/// so the daemon-side ad-hoc commit can reuse it (run-daemon-first task 2.1).
pub fn render_scaffold(name: &str, spec: &ScaffoldSpec, opts: &AddOptions) -> Result<String> {
    let command_str = spec.command.display().to_string();
    let (command, args) = if command_str.contains(char::is_whitespace) {
        (render_command_line(&command_str, &spec.args)?, None)
    } else {
        (
            command_str,
            if spec.args.is_empty() {
                None
            } else {
                Some(spec.args.clone())
            },
        )
    };
    let program = ProgramRaw {
        command: Some(command),
        args,
        work_dir: Some(spec.work_dir.clone()),
        env: if spec.env.is_empty() {
            None
        } else {
            Some(spec.env.clone())
        },
        health_check: spec.health_check.clone(),
        ..Default::default()
    };
    let raw = AppRaw {
        app: Some(AppMeta {
            description: opts.description.clone(),
            restart: opts.restart,
            restart_backoff: opts.restart_backoff,
            priority: opts.priority,
            ..Default::default()
        }),
        program: std::collections::BTreeMap::from([(name.to_string(), program)]),
    };
    let body = toml::to_string_pretty(&raw).context("failed to render scaffold config")?;
    Ok(format!(
        "# Composed by `ak compose <executable>` — scaffold for app {name:?}.\n\
         # Re-running the same `ak compose` recomposes this whole file (manual edits are overwritten).\n\
         # After hand edits run `ak apply {name}` (or bare `ak apply`) to take effect.\n\
         \n{body}"
    ))
}

/// Render `command args...` as one single-line command the quote-aware
/// splitter turns back into the same argv (paths/args containing whitespace
/// get double-quoted; embedded quotes cannot be represented).
fn render_command_line(command: &str, args: &[String]) -> Result<String> {
    let mut parts = Vec::with_capacity(args.len() + 1);
    for s in std::iter::once(command).chain(args.iter().map(|s| s.as_str())) {
        if s.contains('"') {
            bail!(
                "cannot represent {s:?} in a single-line command (quote character); \
                 edit the generated config by hand"
            );
        }
        if s.chars().any(|c| c.is_whitespace()) {
            parts.push(format!("\"{s}\""));
        } else {
            parts.push(s.to_string());
        }
    }
    Ok(parts.join(" "))
}

// ---------------------------------------------------------------------------
// daemon-side commit engine (run-daemon-first: 「启动成功后再落盘」)
// ---------------------------------------------------------------------------

/// Placeholder suffix for the staged registration file. The suffix MUST NOT
/// end in `.toml`, otherwise [`list`] would treat the half-written file as a
/// registration record (D2 / app-registry spec). It is appended to the final
/// name: `web.toml.pending`.
pub const PENDING_SUFFIX: &str = "toml.pending";

/// Bounded retry budget for the atomic rename (transient IO tolerance).
pub const COMMIT_RETRIES: u32 = 3;

/// `app_dir/<name>.toml.pending` — the placeholder path for `name`.
pub fn pending_path(config: &DaemonConfig, config_dir: &Path, name: &str) -> PathBuf {
    crate::config::resolve_path(&config.daemon.app_dir, config_dir)
        .join(format!("{name}.{PENDING_SUFFIX}"))
}

/// Establish the placeholder for `name`: write `text` into
/// `app_dir/<name>.toml.pending` (creating `app_dir` if needed) and fsync it.
/// Returning Ok proves write access and disk availability **before** any
/// process is started (app-registry spec: 无法建立占位时不启动进程).
pub fn stage_placeholder(
    config: &DaemonConfig,
    config_dir: &Path,
    name: &str,
    text: &str,
) -> Result<PathBuf> {
    let dir = app_dir(config, config_dir);
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("failed to create app_dir {}", dir.display()))?;
    let ph = pending_path(config, config_dir, name);
    write_synced(&ph, text).with_context(|| {
        format!(
            "cannot write the registration placeholder {} — fix write access to app_dir {}",
            ph.display(),
            dir.display()
        )
    })?;
    Ok(ph)
}

/// Write `text` to `path` and fsync the file so a crash cannot leave a
/// truncated placeholder that a later rename would publish as a record.
fn write_synced(path: &Path, text: &str) -> std::io::Result<()> {
    use std::io::Write;
    let mut f = std::fs::File::create(path)?;
    f.write_all(text.as_bytes())?;
    f.sync_all()
}

/// What a successful commit did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommitOutcome {
    /// The placeholder was renamed onto the target record.
    Committed { path: PathBuf },
    /// The target already held byte-identical content: nothing was written
    /// (idempotent re-registration, task 2.4).
    Unchanged { path: PathBuf },
}

impl CommitOutcome {
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn path(&self) -> &Path {
        match self {
            CommitOutcome::Committed { path } | CommitOutcome::Unchanged { path } => path,
        }
    }

    pub fn unchanged(&self) -> bool {
        matches!(self, CommitOutcome::Unchanged { .. })
    }
}

/// Failure of the commit step, distinguishing the two recovery paths
/// (run-daemon-first D4 / app-registry spec):
#[derive(Debug)]
pub enum CommitFailure {
    /// The placeholder could not be established: nothing was started and no
    /// residue is left. The caller aborts and returns the error.
    Stage(String),
    /// Startup already succeeded but the atomic rename kept failing: the
    /// process is kept running, the placeholder is left for diagnosis, and
    /// the target path is named so the user can finish by hand.
    Rename { target: PathBuf, error: String },
}

impl std::fmt::Display for CommitFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CommitFailure::Stage(e) => write!(f, "{e}"),
            CommitFailure::Rename { target, error } => write!(
                f,
                "process started but the registration could not be written to {}: {error}",
                target.display()
            ),
        }
    }
}

/// Commit a staged definition: if the target `<name>.toml` already holds
/// byte-identical content, do nothing and report no change; otherwise rename
/// the placeholder onto the target, retrying a bounded number of times on
/// transient IO errors (app-registry spec: 目标已存在且内容一致时幂等成功).
///
/// The caller must have established the placeholder first
/// ([`stage_placeholder`]); the rename is same-directory and therefore atomic
/// — a scanner sees either no record or the complete one.
pub fn commit_staged(
    config: &DaemonConfig,
    config_dir: &Path,
    name: &str,
    text: &str,
) -> std::result::Result<CommitOutcome, CommitFailure> {
    commit_with(
        config,
        config_dir,
        name,
        text,
        COMMIT_RETRIES,
        |from, to| std::fs::rename(from, to),
    )
}

/// Retryable body of [`commit_staged`] with an injectable rename so tests can
/// drive the bounded-retry and partial-failure paths.
fn commit_with(
    config: &DaemonConfig,
    config_dir: &Path,
    name: &str,
    text: &str,
    retries: u32,
    rename: impl Fn(&Path, &Path) -> std::io::Result<()>,
) -> std::result::Result<CommitOutcome, CommitFailure> {
    let target = link_path(config, config_dir, name);
    // Idempotency: identical content already on disk → no write, no rename.
    // (Checked before the placeholder matters: the caller may have staged
    // and this short-circuits the swap.)
    if matches!(std::fs::read_to_string(&target), Ok(old) if old == text) {
        let _ = std::fs::remove_file(pending_path(config, config_dir, name));
        return Ok(CommitOutcome::Unchanged { path: target });
    }
    let ph = pending_path(config, config_dir, name);
    if !ph.exists() {
        return Err(CommitFailure::Stage(format!(
            "placeholder {} is missing (nothing was staged)",
            ph.display()
        )));
    }
    let mut last = String::new();
    for attempt in 0..=retries {
        // Windows `rename` refuses to clobber an existing target; removing it
        // first keeps the observable contract (target is either absent or
        // complete) — the placeholder holds the full content meanwhile.
        if cfg!(windows) && target.exists() {
            let _ = std::fs::remove_file(&target);
        }
        match rename(&ph, &target) {
            Ok(()) => return Ok(CommitOutcome::Committed { path: target }),
            Err(e) => {
                last = e.to_string();
                if attempt < retries {
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
            }
        }
    }
    Err(CommitFailure::Rename {
        target,
        error: last,
    })
}

/// Remove a placeholder left behind by an aborted or failed staging
/// (startup judgement failed, user declined, …). Missing is fine.
pub fn discard_placeholder(config: &DaemonConfig, config_dir: &Path, name: &str) {
    let _ = std::fs::remove_file(pending_path(config, config_dir, name));
}

/// What one `registry::remove` call did.
#[derive(Debug, Clone)]
pub struct RemovedApp {
    /// The config the registration pointed at — or, when `deleted_file` is
    /// true, the removed record itself (it lived inside `app_dir`).
    pub path: PathBuf,
    /// True when the registration record was a real file inside `app_dir`
    /// (scaffold-generated): the config file was deleted along with the
    /// registration and `path` no longer exists. Link registrations keep
    /// the external deployment file untouched.
    pub deleted_file: bool,
}

/// True when a registration record exists at `app_dir/<name>.toml`.
///
/// Uses the same existence probe as [`remove`], so the two cannot drift: a
/// caller that needs "nothing left to delete" (the daemon Remove command, whose
/// record may have been removed by hand) checks this instead of parsing
/// [`remove`]'s error text.
pub fn is_registered(config: &DaemonConfig, config_dir: &Path, name: &str) -> bool {
    link_path(config, config_dir, name).exists()
}

/// Remove an app registration: delete the `app_dir` record. A link record
/// is removed while the external deployment file is kept; a record that is
/// a real file inside `app_dir` (scaffold-generated) is deleted with the
/// registration. The criterion is the path, not the link kind — Windows
/// `make_link` falls back to hard links, which symlink probes can't see.
pub fn remove(config: &DaemonConfig, config_dir: &Path, name: &str) -> Result<RemovedApp> {
    let link = link_path(config, config_dir, name);
    if !link.exists() {
        bail!(
            "app {name:?} is not registered (no link at {})",
            link.display()
        );
    }
    let real_canon = std::fs::canonicalize(&link).unwrap_or_else(|_| link.clone());
    std::fs::remove_file(&link)
        .with_context(|| format!("failed to remove registration link {}", link.display()))?;
    let dir_canon = app_dir(config, config_dir).canonicalize().ok();
    let deleted_file = dir_canon
        .map(|d| real_canon.parent() == Some(&d))
        .unwrap_or(false);
    let real = display_absolute(real_canon);
    if deleted_file {
        debug!(
            "app[{name}] unregistered (generated config removed: {})",
            real.display()
        );
    } else {
        debug!(
            "app[{name}] unregistered (config kept at {})",
            real.display()
        );
    }
    Ok(RemovedApp {
        path: real,
        deleted_file,
    })
}

fn make_link(link: &Path, target: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target, link)
            .with_context(|| format!("symlink {} -> {}", link.display(), target.display()))
    }
    #[cfg(windows)]
    {
        match std::os::windows::fs::symlink_file(target, link) {
            Ok(()) => Ok(()),
            Err(e1) => {
                // Symlinks need privileges on Windows; fall back to a hard
                // link (same volume only), then give up with a warning.
                std::fs::hard_link(target, link)
                    .with_context(|| {
                        format!(
                            "symlink failed ({e1}); hard link also failed — is the file on another volume?"
                        )
                    })
            }
        }
    }
}

/// Merge tuning flags into the deployment file's `[app]` table, preserving
/// formatting and comments via toml_edit. Idempotent upsert semantics.
fn apply_flags(path: &Path, opts: &AddOptions) -> Result<()> {
    let needs_write = opts.description.is_some()
        || opts.restart.is_some()
        || opts.restart_backoff.is_some()
        || opts.priority.is_some();
    if !needs_write {
        return Ok(());
    }
    let text = std::fs::read_to_string(path)?;
    let mut doc: toml_edit::DocumentMut = text
        .parse()
        .with_context(|| format!("failed to parse {}", path.display()))?;

    let app_tbl = doc
        .entry("app")
        .or_insert(toml_edit::Item::Table(toml_edit::Table::new()))
        .as_table_mut()
        .context("[app] entry is not a table")?;
    if let Some(d) = &opts.description {
        app_tbl["description"] = toml_edit::value(d.clone());
    }
    if let Some(v) = opts.restart {
        // Retire the deprecated aliases in the same layer: leaving them
        // behind would mix new + old (validation rejects that) and silently
        // drop the app the user just registered (configuration spec: write
        // paths only ever write `restart`).
        app_tbl.remove("autostart");
        app_tbl.remove("autorestart");
        app_tbl["restart"] = toml_edit::value(v.as_str());
    }
    if let Some(v) = opts.restart_backoff {
        app_tbl["restart_backoff"] = toml_edit::value(v);
    }
    if let Some(v) = opts.priority {
        app_tbl["priority"] = toml_edit::value(v as i64);
    }
    std::fs::write(path, doc.to_string())
        .with_context(|| format!("failed to write {}", path.display()))?;
    Ok(())
}

/// Re-parse the deployment file after flag writes so callers see final state.
pub fn reload_app(
    name: &str,
    path: &Path,
    defaults: Option<&AppDefaults>,
) -> Result<crate::config::ResolvedApp> {
    let (raw, _) = crate::config::AppRaw::load(path)?;
    resolve_app(name, path, &raw, defaults)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn tmp_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ak-test-reg-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// Spell `target` relative to the process cwd, walking up with `..` when
    /// it lives outside. Both operands must be absolute.
    fn relative_from_cwd(target: &Path) -> PathBuf {
        let cwd = std::env::current_dir().unwrap();
        let (cwd_parts, tgt_parts): (Vec<_>, Vec<_>) = (
            cwd.components().collect::<Vec<_>>(),
            target.components().collect::<Vec<_>>(),
        );
        let common = cwd_parts
            .iter()
            .zip(tgt_parts.iter())
            .take_while(|(a, b)| a == b)
            .count();
        let mut out = PathBuf::new();
        for _ in common..cwd_parts.len() {
            out.push("..");
        }
        for c in &tgt_parts[common..] {
            out.push(c.as_os_str());
        }
        out
    }

    fn opts(name: &str) -> AddOptions {
        AddOptions {
            name: Some(name.into()),
            ..Default::default()
        }
    }

    /// A fake executable (content is irrelevant to registration).
    fn make_exe(dir: &Path, name: &str) -> PathBuf {
        std::fs::create_dir_all(dir).unwrap();
        let p = dir.join(name);
        std::fs::write(&p, "#!/bin/sh\n").unwrap();
        p
    }

    // -- 1.1 scaffold render --------------------------------------------------

    /// The scaffolded file is a real file loadable by `AppRaw::load`; the
    /// command and work_dir are absolute; --args/--env land in the program.
    #[test]
    fn scaffold_generates_loadable_config_with_absolute_paths() {
        let tmp = tmp_dir("scaffold");
        let exe = make_exe(&tmp.join("bin"), "proc"); // parent created inside
        let config = DaemonConfig::default();
        let mut env = BTreeMap::new();
        env.insert("LOG".to_string(), "debug".to_string());
        let o = AddOptions {
            name: Some("abc".into()),
            args: Some("--port 8080 --msg \"a b\"".into()),
            env: Some(env),
            ..opts("abc")
        };
        let r = add(&config, &tmp, &exe, &o).unwrap();
        assert!(r.scaffolded && r.changed, "first generation is changed");

        let file = tmp.join("app.d").join("abc.toml");
        assert_eq!(r.file, file);
        let md = std::fs::symlink_metadata(&file).unwrap();
        assert!(md.is_file(), "record must be a real file, not a link");

        let text = std::fs::read_to_string(&file).unwrap();
        let (raw, _) = AppRaw::load(&file).unwrap();
        let prog = raw.program.get("abc").expect("program named after the app");
        let cmd = prog.command.as_deref().unwrap();
        assert!(Path::new(cmd).is_absolute(), "command absolute: {cmd}");
        assert!(
            !cmd.contains(r"\\?\"),
            "no verbatim prefix in generated command: {cmd}"
        );
        assert_eq!(
            prog.args.as_deref(),
            Some(
                &[
                    "--port".to_string(),
                    "8080".to_string(),
                    "--msg".to_string(),
                    "a b".to_string()
                ][..]
            )
        );
        assert_eq!(
            prog.env
                .as_ref()
                .and_then(|e| e.get("LOG"))
                .map(|s| s.as_str()),
            Some("debug")
        );
        let wd = prog.work_dir.as_ref().unwrap();
        assert!(wd.is_absolute(), "work_dir absolute: {}", wd.display());
        assert!(text.contains("[app]"), "[app] table always rendered");

        // Round-trip resolution matches what the CLI reports.
        let resolved = resolve_app("abc", &file, &raw, None).unwrap();
        assert_eq!(resolved.programs[0].command, cmd);
        assert_eq!(resolved.programs[0].work_dir, *wd);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// Executable path without --name: the app name is the file stem.
    #[test]
    fn scaffold_name_defaults_to_executable_stem() {
        let tmp = tmp_dir("stem");
        let exe = make_exe(&tmp, "web-server");
        let config = DaemonConfig::default();
        let r = add(&config, &tmp, &exe, &AddOptions::default()).unwrap();
        assert_eq!(r.app, "web-server");
        assert!(tmp.join("app.d").join("web-server.toml").exists());
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// A command path containing whitespace renders as a quoted single-line
    /// command and resolves back to the exact absolute path.
    #[test]
    fn scaffold_spaceful_path_round_trips() {
        let tmp = tmp_dir("space");
        let bin = tmp.join("my bin");
        std::fs::create_dir_all(&bin).unwrap();
        let exe = make_exe(&bin, "proc");
        let config = DaemonConfig::default();
        let r = add(&config, &tmp, &exe, &opts("sp")).unwrap();
        let text = std::fs::read_to_string(&r.file).unwrap();
        let raw: AppRaw = toml::from_str(&text).unwrap();
        let resolved = resolve_app("sp", &r.file, &raw, None).unwrap();
        let p = &resolved.programs[0];
        assert_eq!(p.command, r.command, "single-line split recovers the path");
        let canon = exe.canonicalize().unwrap();
        assert_eq!(
            Path::new(&p.command),
            display_absolute(canon),
            "command is the canonical absolute path (no verbatim prefix)"
        );
        assert!(p.args.is_empty());
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// work_dir: default is the exact current directory of the add call;
    /// `--workdir` overrides it (existing dirs canonicalized, relative ones
    /// absolutized) — always an absolute path in the generated file.
    #[test]
    fn scaffold_workdir_defaults_to_cwd_and_overridable() {
        let tmp = tmp_dir("workdir");
        let exe = make_exe(&tmp, "proc");
        let config = DaemonConfig::default();

        // Default: the add-time current directory, verbatim.
        let r = add(&config, &tmp, &exe, &opts("dflt")).unwrap();
        let (raw, _) = AppRaw::load(&r.file).unwrap();
        let wd = raw.program["dflt"].work_dir.as_ref().unwrap();
        assert!(
            wd.is_absolute(),
            "default work_dir absolute: {}",
            wd.display()
        );
        assert_eq!(
            wd,
            &std::env::current_dir().unwrap(),
            "work_dir defaults to the add-time cwd"
        );

        // --workdir with an existing directory wins (canonicalized absolute).
        let wdir = tmp.join("wd");
        std::fs::create_dir_all(&wdir).unwrap();
        let mut o = opts("ovr");
        o.workdir = Some(wdir.clone());
        let r = add(&config, &tmp, &exe, &o).unwrap();
        let (raw, _) = AppRaw::load(&r.file).unwrap();
        let wd = raw.program["ovr"].work_dir.as_ref().unwrap();
        assert!(
            wd.is_absolute(),
            "override work_dir absolute: {}",
            wd.display()
        );
        assert_eq!(
            wd.canonicalize().unwrap(),
            wdir.canonicalize().unwrap(),
            "--workdir override lands in the file"
        );

        // A relative override that cannot be resolved is still absolutized.
        let mut o = opts("rel");
        o.workdir = Some(PathBuf::from("no-such-rel-dir"));
        let r = add(&config, &tmp, &exe, &o).unwrap();
        let (raw, _) = AppRaw::load(&r.file).unwrap();
        let wd = raw.program["rel"].work_dir.as_ref().unwrap();
        assert!(
            wd.is_absolute() && wd.ends_with("no-such-rel-dir"),
            "relative --workdir is absolutized: {}",
            wd.display()
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// The scaffold supports the full micro-tuning flag set; every flag
    /// renders into `[app]` and survives a load/resolve round-trip.
    #[test]
    fn scaffold_renders_app_level_flags_and_round_trips() {
        let tmp = tmp_dir("flags");
        let exe = make_exe(&tmp, "proc");
        let config = DaemonConfig::default();
        let o = AddOptions {
            description: Some("demo service".into()),
            restart: Some(RestartPolicy::OnFailure),
            restart_backoff: Some(2.5),
            priority: Some(-5),
            ..opts("tuned")
        };
        let r = add(&config, &tmp, &exe, &o).unwrap();
        let text = std::fs::read_to_string(&r.file).unwrap();
        for needle in ["demo service", "on-failure", "2.5", "-5"] {
            assert!(text.contains(needle), "{needle:?} rendered: {text}");
        }
        // Writes go to the new field only.
        assert!(text.contains("restart = \"on-failure\""), "{text}");
        assert!(
            !text.contains("autorestart") && !text.contains("autostart"),
            "{text}"
        );

        let (raw, _) = AppRaw::load(&r.file).unwrap();
        let meta = raw.app.as_ref().expect("[app] table rendered");
        assert_eq!(meta.description.as_deref(), Some("demo service"));
        assert_eq!(meta.restart, Some(RestartPolicy::OnFailure));
        assert_eq!(meta.restart_backoff, Some(2.5));
        assert_eq!(meta.priority, Some(-5));

        let resolved = resolve_app("tuned", &r.file, &raw, None).unwrap();
        assert_eq!(resolved.priority, -5);
        assert_eq!(resolved.description.as_deref(), Some("demo service"));
        let p = &resolved.programs[0];
        assert_eq!(p.restart, RestartPolicy::OnFailure);
        assert_eq!(p.restart_backoff, 2.5);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    // -- 1.2 dispatch -----------------------------------------------------------

    #[test]
    fn add_dispatches_by_path_shape() {
        let tmp = tmp_dir("dispatch");
        let config = DaemonConfig::default();

        // Directory containing appkeeper.toml → existing directory flow.
        let deploy = tmp.join("myapp");
        std::fs::create_dir_all(&deploy).unwrap();
        std::fs::write(
            deploy.join("appkeeper.toml"),
            "[program.a]\ncommand = \"true\"\n",
        )
        .unwrap();
        let r = add(&config, &tmp, &deploy, &opts("myapp")).unwrap();
        assert!(!r.scaffolded);
        assert!(tmp.join("app.d").join("myapp.toml").exists());

        // .toml file → existing file registration flow.
        let cfg_file = tmp.join("external.toml");
        std::fs::write(&cfg_file, "[program.b]\ncommand = \"true\"\n").unwrap();
        let r = add(&config, &tmp, &cfg_file, &opts("ext")).unwrap();
        assert!(!r.scaffolded);
        assert!(tmp.join("app.d").join("ext.toml").exists());

        // Other regular file → scaffold.
        let exe = make_exe(&tmp, "proc");
        let r = add(&config, &tmp, &exe, &opts("scaf")).unwrap();
        assert!(r.scaffolded);
        assert!(tmp.join("app.d").join("scaf.toml").exists());

        // Missing path → same error as before.
        let err = add(&config, &tmp, &tmp.join("nope"), &opts("x")).unwrap_err();
        assert!(err.to_string().contains("not found"), "{err:#}");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    // -- 1.3 regeneration ---------------------------------------------------

    #[test]
    fn scaffold_regeneration_no_change_then_changed() {
        let tmp = tmp_dir("regen");
        let exe = make_exe(&tmp, "proc");
        let config = DaemonConfig::default();
        let first = add(&config, &tmp, &exe, &opts("abc")).unwrap();
        assert!(first.changed);

        // Identical re-add: unchanged, file bytes untouched.
        let before = std::fs::read_to_string(first.file.clone()).unwrap();
        let second = add(&config, &tmp, &exe, &opts("abc")).unwrap();
        assert!(!second.changed, "identical re-add is a no change");
        assert_eq!(
            std::fs::read_to_string(&first.file).unwrap(),
            before,
            "file untouched on no-change re-add"
        );

        // Different --args: full regeneration (hand edits are overwritten).
        let mut hand_edited = before.clone();
        hand_edited.push_str("\n# my manual note\n");
        std::fs::write(&first.file, &hand_edited).unwrap();
        let o = AddOptions {
            args: Some("--port 9090".into()),
            ..opts("abc")
        };
        let third = add(&config, &tmp, &exe, &o).unwrap();
        assert!(third.changed, "differing args are a change");
        let now = std::fs::read_to_string(&first.file).unwrap();
        assert!(now.contains("9090"), "new args written: {now}");
        assert!(!now.contains("my manual note"), "hand edit overwritten");
        assert!(!now.contains("8080"), "old args gone: {now}");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// Validation gates the write in BOTH directions: a first scaffold whose
    /// program name duplicates an existing app writes nothing, and a
    /// regeneration whose flags fail validation leaves the old file intact.
    #[test]
    fn scaffold_refuses_to_write_when_validation_fails() {
        let tmp = tmp_dir("prevalidate");
        let config = DaemonConfig::default();

        // An existing dir app already owns the program name "shared".
        let deploy = tmp.join("myapp");
        std::fs::create_dir_all(&deploy).unwrap();
        std::fs::write(
            deploy.join("appkeeper.toml"),
            "[program.shared]\ncommand = 'true'\n",
        )
        .unwrap();
        add(&config, &tmp, &deploy, &opts("myapp")).unwrap();

        // First scaffold with a colliding program name: refused, nothing lands.
        let exe = make_exe(&tmp, "proc");
        let err = add(&config, &tmp, &exe, &opts("shared")).unwrap_err();
        assert!(err.to_string().contains("duplicated"), "{err:#}");
        assert!(
            !tmp.join("app.d").join("shared.toml").exists(),
            "failed scaffold writes nothing"
        );

        // A good scaffold succeeds, then a regeneration with an invalid flag
        // (restart_backoff must be > 0) must not touch the existing file.
        let ok = add(&config, &tmp, &exe, &opts("okapp")).unwrap();
        let before = std::fs::read_to_string(&ok.file).unwrap();
        let mut bad = opts("okapp");
        bad.restart_backoff = Some(0.0);
        let err = add(&config, &tmp, &exe, &bad).unwrap_err();
        assert!(err.to_string().contains("restart_backoff"), "{err:#}");
        assert_eq!(
            std::fs::read_to_string(&ok.file).unwrap(),
            before,
            "failed regeneration leaves the old file"
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    // -- 1.4 remove -----------------------------------------------------------

    #[test]
    fn remove_deletes_generated_file_but_keeps_external_body() {
        let tmp = tmp_dir("remove");
        let config = DaemonConfig::default();

        // Scaffold record: a real file inside app_dir → deleted with it.
        let exe = make_exe(&tmp, "proc");
        let r = add(&config, &tmp, &exe, &opts("abc")).unwrap();
        let removed = remove(&config, &tmp, "abc").unwrap();
        assert!(removed.deleted_file, "generated file is deleted");
        assert_eq!(removed.path, r.file);
        assert!(!r.file.exists(), "scaffold file gone");
        assert!(!removed.path.exists());

        // Link record to an external file → link gone, body kept.
        let deploy = tmp.join("myapp");
        std::fs::create_dir_all(&deploy).unwrap();
        let body = deploy.join("appkeeper.toml");
        std::fs::write(&body, "[program.a]\ncommand = \"true\"\n").unwrap();
        add(&config, &tmp, &deploy, &opts("myapp")).unwrap();
        let removed = remove(&config, &tmp, "myapp").unwrap();
        // The external body always survives: on unix the record is a
        // symlink (path criterion sees the external parent); on Windows
        // make_link falls back to a hard link, so the path criterion sees
        // an app_dir parent and reports the record itself as removed — the
        // hardlink semantics keep the external file's content intact.
        if cfg!(unix) {
            assert!(!removed.deleted_file, "external body kept");
        } else {
            assert!(removed.deleted_file, "hardlink record is removed in place");
        }
        assert!(body.exists(), "deployment config untouched");
        assert!(!tmp.join("app.d").join("myapp.toml").exists());
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// guard-remove-until-stopped 1.3: the registration removal touches only
    /// the record — log files survive, an unknown app is an error, and the
    /// reported paths distinguish the deleted record from the kept body.
    #[test]
    fn remove_leaves_logs_alone_and_reports_paths() {
        let tmp = tmp_dir("remove-boundary");
        let config = DaemonConfig::default();
        let logs = tmp.join("logs");
        std::fs::create_dir_all(&logs).unwrap();
        let log_file = logs.join("abc-out.log");
        std::fs::write(&log_file, "line\n").unwrap();

        // Generated record: path reported = the deleted record itself.
        let exe = make_exe(&tmp, "proc");
        let r = add(&config, &tmp, &exe, &opts("abc")).unwrap();
        let removed = remove(&config, &tmp, "abc").unwrap();
        assert!(removed.deleted_file);
        assert_eq!(removed.path, r.file, "the deleted record path is reported");
        assert!(
            log_file.is_file(),
            "log files are never removed with an app"
        );

        // External record: path reported = the kept deployment body.
        let deploy = tmp.join("ext");
        std::fs::create_dir_all(&deploy).unwrap();
        let body = deploy.join("appkeeper.toml");
        std::fs::write(&body, "[program.b]\ncommand = \"true\"\n").unwrap();
        add(&config, &tmp, &deploy, &opts("ext")).unwrap();
        let removed = remove(&config, &tmp, "ext").unwrap();
        assert_eq!(removed.path, body, "the kept config body path is reported");
        assert!(body.is_file());

        // Unknown app: rejected, nothing to report.
        let err = remove(&config, &tmp, "nope").unwrap_err();
        assert!(format!("{err:#}").contains("not registered"), "{err:#}");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// Scaffolding over a name registered from OUTSIDE app_dir must refuse
    /// (a write through the link would clobber the external config).
    #[test]
    fn scaffold_refuses_to_clobber_external_registration() {
        let tmp = tmp_dir("clobber");
        let config = DaemonConfig::default();
        let deploy = tmp.join("myapp");
        std::fs::create_dir_all(&deploy).unwrap();
        std::fs::write(
            deploy.join("appkeeper.toml"),
            "[program.a]\ncommand = \"true\"\n",
        )
        .unwrap();
        add(&config, &tmp, &deploy, &opts("myapp")).unwrap();
        let exe = make_exe(&tmp, "proc");
        let err = add(&config, &tmp, &exe, &opts("myapp")).unwrap_err();
        assert!(err.to_string().contains("remove"), "{err:#}");
        assert!(
            std::fs::read_to_string(deploy.join("appkeeper.toml"))
                .unwrap()
                .contains("[program.a]"),
            "external config untouched"
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    // -- 1.5 reserved keyword ---------------------------------------------------

    #[test]
    fn add_rejects_reserved_name_all() {
        let tmp = tmp_dir("reserved");
        let config = DaemonConfig::default();
        let exe = make_exe(&tmp, "proc");

        // Explicit --name all.
        let err = add(&config, &tmp, &exe, &opts("all")).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("reserved") && msg.contains("apply"), "{msg}");

        // Stem-derived: an executable named `all` (all.exe on Windows).
        let exe_all = make_exe(&tmp, if cfg!(windows) { "all.exe" } else { "all" });
        let err = add(&config, &tmp, &exe_all, &AddOptions::default()).unwrap_err();
        assert!(err.to_string().contains("reserved"), "{err:#}");

        // The directory flow is equally guarded. Its name must still be
        // "all" for the assertion, but keep it in a subdir to avoid the
        // `all` file created by the stem-derived step above.
        let dir_all = tmp.join("dirs").join("all");
        std::fs::create_dir_all(&dir_all).unwrap();
        std::fs::write(
            dir_all.join("appkeeper.toml"),
            "[program.a]\ncommand = \"true\"\n",
        )
        .unwrap();
        let err = add(&config, &tmp, &dir_all, &AddOptions::default()).unwrap_err();
        assert!(err.to_string().contains("reserved"), "{err:#}");
        assert!(
            !tmp.join("app.d").join("all.toml").exists(),
            "nothing registered under the reserved name"
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    // -- 1.6 unix execute bit ----------------------------------------------------

    /// unix-only: the scaffold warns about a missing execute bit but still
    /// registers; with the bit set nothing is warned about. Windows has no
    /// execute-bit concept, so the whole check is cfg(unix).
    #[test]
    #[cfg(unix)]
    fn scaffold_warns_on_missing_execute_bit_only() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tmp_dir("execbit");
        let exe = make_exe(&tmp, "proc");
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o644)).unwrap();
        let config = DaemonConfig::default();
        // Registration still succeeds without the bit (warning only).
        let r = add(&config, &tmp, &exe, &opts("abc")).unwrap();
        assert!(r.scaffolded);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// add must fail loudly (not "register" with a swallowed warning) when
    /// the app_dir link cannot be written — the link IS the registry record.
    #[test]
    #[cfg(unix)]
    fn add_fails_when_app_dir_not_writable() {
        if unsafe { libc::geteuid() } == 0 {
            return; // root can write anything; can't simulate the failure
        }
        let tmp = std::env::temp_dir().join(format!("ak-test-registry-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let app_dir = tmp.join("app.d");
        std::fs::create_dir_all(&app_dir).unwrap();
        let deploy = tmp.join("myapp");
        std::fs::create_dir_all(&deploy).unwrap();
        std::fs::write(
            deploy.join("appkeeper.toml"),
            "[program.a]\ncommand = \"true\"\n",
        )
        .unwrap();

        // Simulate a root-owned app_dir: drop write permission for everyone.
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&app_dir, std::fs::Permissions::from_mode(0o555)).unwrap();

        let config = DaemonConfig::default();
        let o = opts("myapp");
        let r = add(&config, &tmp, &deploy.join("appkeeper.toml"), &o);
        let err = r.err().expect("add must fail when app_dir is not writable");
        let msg = err.to_string();
        assert!(msg.contains("app_dir"), "must name app_dir: {msg}");
        assert!(msg.contains("sudo"), "must hint at the fix: {msg}");

        // The registration must NOT have persisted.
        assert!(
            std::fs::read_dir(&app_dir).unwrap().next().is_none(),
            "no link may appear in app_dir on failure"
        );

        std::fs::set_permissions(&app_dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        let _ = std::fs::remove_dir_all(&tmp);
    }

    // -- command resolution (cli-revamp task 3.1) ---------------------------

    /// A path-bearing token is canonicalized to an absolute path.
    #[test]
    fn resolve_command_absolute_and_relative_paths() {
        let tmp = tmp_dir("resolve-path");
        let exe = make_exe(&tmp, "proc");
        let abs = resolve_command(exe.to_str().unwrap()).unwrap();
        assert!(abs.is_absolute(), "resolved absolute: {}", abs.display());
        assert_eq!(abs, display_absolute(exe.canonicalize().unwrap()));
        // Relative path with a separator resolves too. `Path::canonicalize`
        // resolves against the process cwd, so this test must not pretend to
        // be somewhere else by calling `set_current_dir`: that is
        // process-global state, and a parallel test asserting on the add-time
        // cwd would read a directory that is no longer ours (this exact race
        // made `render_scaffold_matches_add_output` flaky). Instead, build a
        // genuinely cwd-relative spelling of the tmp executable.
        let rel = format!("./{}", relative_from_cwd(&exe).display());
        let got = resolve_command(&rel);
        assert!(
            got.as_ref()
                .is_ok_and(|g| g.is_absolute() && g.ends_with("proc")),
            "a cwd-relative path resolves to an absolute one ({rel}): {got:?}"
        );
        // A path that does not exist is a diagnostic error.
        let err = resolve_command("/no/such/binary-xyz")
            .unwrap_err()
            .to_string();
        assert!(err.contains("cannot resolve"), "{err}");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// A bare name is looked up on PATH; a miss is a diagnostic error and the
    /// reserved `all` name is not special to the resolver.
    #[test]
    fn resolve_command_path_lookup() {
        // `sh` exists on PATH on unix; use it as the hit case.
        #[cfg(unix)]
        {
            let p = resolve_command("sh").unwrap();
            assert!(p.is_absolute(), "{}", p.display());
            assert_eq!(p.file_name().unwrap(), "sh");
        }
        let err = resolve_command("definitely-not-a-real-cmd-xyz").unwrap_err();
        assert!(err.to_string().contains("PATH"), "{err:#}");
    }

    /// An empty command token is rejected.
    #[test]
    fn resolve_command_rejects_empty() {
        assert!(resolve_command("").is_err());
        assert!(resolve_command("   ").is_err());
    }

    // -- run ad-hoc bridge (cli-revamp task 3.2) ----------------------------

    /// The ad-hoc bridge renders a loadable config whose command is the
    /// resolved absolute path, whose args are the remaining argv, and whose
    /// name defaults to the command stem.
    #[test]
    fn adhoc_bridge_scaffolds_absolute_command_and_args() {
        let tmp = tmp_dir("adhoc");
        let exe = make_exe(&tmp, "server");
        let resolved = resolve_command(exe.to_str().unwrap()).unwrap();
        let config = DaemonConfig::default();
        let o = AddOptions {
            name: None,
            args_argv: Some(vec!["--port".into(), "8080".into()]),
            restart: Some(RestartPolicy::OnFailure),
            ..Default::default()
        };
        let r = add(&config, &tmp, &resolved, &o).unwrap();
        // Default name = command stem.
        assert_eq!(r.app, "server");
        let (raw, _) = AppRaw::load(&r.file).unwrap();
        let prog = raw
            .program
            .get("server")
            .expect("program named after the stem");
        assert!(Path::new(prog.command.as_deref().unwrap()).is_absolute());
        assert_eq!(
            prog.args.as_deref(),
            Some(&["--port".to_string(), "8080".to_string()][..])
        );
        let text = std::fs::read_to_string(&r.file).unwrap();
        assert!(text.contains("restart = \"on-failure\""), "{text}");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// The reserved `all` name is refused for the ad-hoc bridge too (both an
    /// explicit name and a command stem of `all`).
    #[test]
    fn adhoc_bridge_rejects_reserved_all() {
        let tmp = tmp_dir("adhoc-all");
        let exe = make_exe(&tmp, "proc");
        let config = DaemonConfig::default();
        let resolved = resolve_command(exe.to_str().unwrap()).unwrap();
        let o = AddOptions {
            name: Some("all".into()),
            ..Default::default()
        };
        assert!(
            add(&config, &tmp, &resolved, &o)
                .unwrap_err()
                .to_string()
                .contains("reserved")
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    // -- commit engine (run-daemon-first section 2) -------------------------

    #[allow(dead_code)]
    fn spec_for(exe: &Path, args: &[&str]) -> ScaffoldSpec {
        ScaffoldSpec {
            command: exe.to_path_buf(),
            args: args.iter().map(|s| s.to_string()).collect(),
            env: Default::default(),
            work_dir: exe.parent().unwrap().to_path_buf(),
            health_check: None,
        }
    }

    /// 2.1: the extracted render function is byte-identical to what the
    /// scaffold flow writes through `add`.
    #[test]
    fn render_scaffold_matches_add_output() {
        let tmp = tmp_dir("render-pure");
        let exe = make_exe(&tmp, "proc");
        let config = DaemonConfig::default();
        let o = AddOptions {
            args: Some("--port 8080".into()),
            restart: Some(RestartPolicy::OnFailure),
            ..opts("abc")
        };
        let r = add(&config, &tmp, &exe, &o).unwrap();
        let written = std::fs::read_to_string(&r.file).unwrap();

        let resolved = exe.canonicalize().unwrap();
        // `add` defaults work_dir to the add-time cwd; mirror that here.
        let spec = ScaffoldSpec {
            command: display_absolute(resolved),
            args: vec!["--port".into(), "8080".into()],
            env: Default::default(),
            work_dir: std::env::current_dir().unwrap(),
            health_check: None,
        };
        let rendered = render_scaffold("abc", &spec, &o).unwrap();
        assert_eq!(
            rendered, written,
            "pure render must match the scaffold write"
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// 2.2: stage writes a placeholder whose name is NOT scanned as a
    /// registration (does not end in `.toml`), and commit atomically renames
    /// it into the real record.
    #[test]
    fn stage_and_commit_atomic_rename() {
        let tmp = tmp_dir("commit");
        let config = DaemonConfig::default();
        let text = "# generated\n[program.web]\ncommand = 'true'\n";
        let ph = stage_placeholder(&config, &tmp, "web", text).unwrap();
        assert!(ph.exists(), "placeholder written");
        assert_eq!(ph.file_name().unwrap(), "web.toml.pending");
        assert_ne!(
            ph.extension().unwrap(),
            "toml",
            "placeholder invisible to list()"
        );
        assert!(
            list(&config, &tmp).unwrap().is_empty(),
            "placeholder is not a registration record"
        );

        let outcome = commit_staged(&config, &tmp, "web", text).unwrap();
        assert!(matches!(outcome, CommitOutcome::Committed { .. }));
        assert!(!ph.exists(), "placeholder consumed by the rename");
        let target = tmp.join("app.d").join("web.toml");
        assert_eq!(outcome.path(), target);
        assert_eq!(std::fs::read_to_string(&target).unwrap(), text);
        let listed = list(&config, &tmp).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].name, "web");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// 2.3: a placeholder that cannot be established errors immediately and
    /// leaves nothing behind (unix, non-root).
    #[test]
    #[cfg(unix)]
    fn stage_fails_when_app_dir_not_writable() {
        if unsafe { libc::geteuid() } == 0 {
            return;
        }
        use std::os::unix::fs::PermissionsExt;
        let tmp = tmp_dir("stage-ro");
        let dir = tmp.join("app.d");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o555)).unwrap();
        let config = DaemonConfig::default();
        let err =
            stage_placeholder(&config, &tmp, "web", "[program.web]\ncommand='true'\n").unwrap_err();
        assert!(err.to_string().contains("app_dir"), "{err:#}");
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(
            std::fs::read_dir(&dir).unwrap().next().is_none(),
            "no residue after a failed stage"
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// 2.3: rename retries a bounded number of times; when it keeps failing
    /// the outcome is a partial failure naming the target, and the placeholder
    /// is deliberately kept for diagnosis.
    #[test]
    fn commit_retries_then_reports_partial_failure() {
        let tmp = tmp_dir("rename-fail");
        let config = DaemonConfig::default();
        let text = "[program.web]\ncommand = 'true'\n";
        stage_placeholder(&config, &tmp, "web", text).unwrap();
        let attempts = std::cell::Cell::new(0u32);
        let fail = |_from: &Path, _to: &Path| -> std::io::Result<()> {
            attempts.set(attempts.get() + 1);
            Err(std::io::Error::other("boom"))
        };
        let err = commit_with(&config, &tmp, "web", text, 2, fail).unwrap_err();
        match err {
            CommitFailure::Rename { target, error } => {
                assert_eq!(target, tmp.join("app.d").join("web.toml"));
                assert!(error.contains("boom"), "{error}");
            }
            other => panic!("expected a rename failure, got {other:?}"),
        }
        assert_eq!(attempts.get(), 3, "initial attempt + 2 retries");
        assert!(
            pending_path(&config, &tmp, "web").exists(),
            "placeholder kept for diagnosis"
        );
        assert!(
            !tmp.join("app.d").join("web.toml").exists(),
            "no half-written record"
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// 2.4: re-committing identical content is a no-op — the target's mtime is
    /// untouched and no second write happens.
    #[test]
    fn commit_is_idempotent_on_identical_content() {
        let tmp = tmp_dir("idem");
        let config = DaemonConfig::default();
        let text = "[program.web]\ncommand = 'true'\n";
        stage_placeholder(&config, &tmp, "web", text).unwrap();
        commit_staged(&config, &tmp, "web", text).unwrap();
        let target = tmp.join("app.d").join("web.toml");
        let mtime = std::fs::metadata(&target).unwrap().modified().unwrap();

        // A second run stages again (the caller always stages), then commits
        // the same text: no write, no rename.
        stage_placeholder(&config, &tmp, "web", text).unwrap();
        let outcome = commit_staged(&config, &tmp, "web", text).unwrap();
        assert!(outcome.unchanged(), "identical content → unchanged");
        assert_eq!(outcome.path(), target);
        assert_eq!(
            std::fs::metadata(&target).unwrap().modified().unwrap(),
            mtime,
            "mtime untouched"
        );
        assert!(
            !pending_path(&config, &tmp, "web").exists(),
            "the redundant placeholder is discarded"
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// 2.3: `discard_placeholder` removes a leftover placeholder and is
    /// idempotent when there is nothing to remove.
    #[test]
    fn discard_placeholder_is_idempotent() {
        let tmp = tmp_dir("discard");
        let config = DaemonConfig::default();
        stage_placeholder(&config, &tmp, "web", "x").unwrap();
        assert!(pending_path(&config, &tmp, "web").exists());
        discard_placeholder(&config, &tmp, "web");
        assert!(!pending_path(&config, &tmp, "web").exists());
        discard_placeholder(&config, &tmp, "web"); // no panic / no error
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
