//! The supervisor: shared state, the command-driven main loop, ordered
//! startup, per-app reload and shutdown orchestration.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use log::{error, info, warn};
use serde::{Deserialize, Serialize};

use crate::config::{self, DaemonConfig, ResolvedApp, ResolvedProgram};
use crate::health;
use crate::program::{ManagedProgram, ProgramState};
use crate::registry;

pub type Reply = std::sync::mpsc::Sender<Result<String>>;

/// Reply channel for the structured ad-hoc results (run-daemon-first D3/D5).
pub type AdhocUpReply =
    std::sync::mpsc::Sender<std::result::Result<AdhocUpResult, AdhocUpError>>;
pub type AdhocCommitReply = std::sync::mpsc::Sender<Result<AdhocCommitResult>>;
pub type AdhocAbortReply = std::sync::mpsc::Sender<Result<String>>;

/// A temporary app definition handed to the daemon by `ak run`'s ad-hoc
/// forms. The CLI resolves the command to an absolute path first; the daemon
/// re-validates and renders the config itself (run-daemon-first D3).
#[derive(Debug, Clone)]
pub struct AdhocSpec {
    /// Explicit `--name`; `None` means "derive from the command stem".
    pub name: Option<String>,
    /// Command first token, already resolved to an absolute path by the CLI.
    pub command: PathBuf,
    pub args: Vec<String>,
    pub env: std::collections::BTreeMap<String, String>,
    pub work_dir: Option<PathBuf>,
    pub description: Option<String>,
    pub restart: Option<crate::config::RestartPolicy>,
    pub restart_backoff: Option<f64>,
    /// Optional health check (`http(s)://` | `tcp://host:port` | an exec
    /// command line) so the streaming-progress scenario is reachable.
    pub health_check: Option<String>,
    /// `--restart=no`: no startup judgement, register only (D8).
    pub register_only: bool,
}

/// A staged (started but not yet persisted) ad-hoc app (run-daemon-first D1).
#[derive(Debug, Clone)]
pub struct StagedApp {
    pub app: String,
    pub program: String,
    /// Rendered config text, ready to be written to the target record.
    pub text: String,
    /// The target registration file (`app_dir/<name>.toml`).
    pub target: PathBuf,
    /// The placeholder written at stage time (`app_dir/<name>.toml.pending`).
    /// Kept for diagnosis on partial failure and used by tests to assert the
    /// staged path.
    #[allow(dead_code)]
    pub placeholder: PathBuf,
    /// Resolved definition, merged into detection so the program survives
    /// periodic rescans while staged.
    pub resolved: ResolvedApp,
    /// True once the record was committed (rename done).
    pub committed: bool,
}

/// Why an `AdhocUp` was refused without starting anything.
#[derive(Debug)]
pub enum AdhocUpError {
    /// The name (or program name) collides / is invalid / is the reserved
    /// `all` keyword.
    Validation(String),
    /// The placeholder could not be established: nothing started (D2).
    Stage(String),
}

impl std::fmt::Display for AdhocUpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AdhocUpError::Validation(m) | AdhocUpError::Stage(m) => f.write_str(m),
        }
    }
}

impl std::error::Error for AdhocUpError {}

impl AdhocUpError {
    /// The HTTP status a validation failure maps to: a name collision or an
    /// invalid name is a 400; everything else is a 409 (state conflict).
    pub fn http_status(&self) -> u16 {
        match self {
            AdhocUpError::Validation(_) => 400,
            AdhocUpError::Stage(_) => 500,
        }
    }

    /// The startup phase that failed, for the structured error reply.
    pub fn phase(&self) -> &'static str {
        match self {
            AdhocUpError::Validation(_) => "validation",
            AdhocUpError::Stage(_) => "stage",
        }
    }
}

/// Successful `AdhocUp`: the program is spawned and staged, not yet persisted.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AdhocUpResult {
    pub app: String,
    pub program: String,
    /// The registration file the commit will produce.
    pub config_path: String,
    pub pid: Option<u32>,
    /// Upper bound (seconds) the caller should wait before giving up (D5).
    pub estimate_secs: f64,
    /// True when the program declares a health check (the endpoint streams an
    /// extra waiting frame; D5).
    pub has_health: bool,
    /// `--restart=no`: no startup judgement; the endpoint commits directly.
    pub register_only: bool,
    /// True when the definition was already staged/started by an earlier run
    /// whose commit failed: the CLI must ask before re-committing (D4).
    pub residual: bool,
    /// True when a same-name registration already held byte-identical content:
    /// an idempotent upsert that neither wrote the record nor restarted the
    /// process. `pid` is the existing one (control-plane spec: 同名重复提交走再生).
    pub unchanged: bool,
    /// True when the record is ALREADY on disk (a regeneration of a committed
    /// same-name app): the caller waits for the startup judgement but MUST NOT
    /// run the commit step again (there is no placeholder / staged entry).
    pub committed: bool,
}

/// Successful `AdhocCommit`: the record is on disk.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AdhocCommitResult {
    pub app: String,
    pub program: String,
    pub config_path: String,
    pub pid: Option<u32>,
    /// True when the target already held identical content (idempotent).
    pub unchanged: bool,
    /// Partial failure: startup succeeded but the rename kept failing; the
    /// process is kept running and the full config is returned so the user can
    /// finish by hand (D4 / app-registry spec).
    pub partial: bool,
    /// Human-readable partial-failure explanation (empty when `partial` false).
    pub partial_reason: String,
    /// Complete configuration info for manual landing on partial failure.
    pub config_text: String,
}

#[derive(Debug)]
pub enum Command {
    Start {
        name: String,
        reply: Option<Reply>,
    },
    Stop {
        name: String,
        reply: Option<Reply>,
    },
    Restart {
        name: String,
        reply: Option<Reply>,
    },
    /// App-level fan-out: start/stop/restart every program of one app,
    /// expanded server-side in the established ordering (actions spec).
    AppFanout {
        kind: FanoutKind,
        app: String,
        reply: Option<Reply>,
    },
    Reload {
        reply: Option<Reply>,
    },
    Apply {
        scope: ApplyScope,
        restart: bool,
        reply: Option<Reply>,
    },
    Shutdown {
        reply: Option<Reply>,
        /// Signalled by the HTTP layer once the response bytes are on the
        /// wire. The supervisor waits (bounded) on it before tearing the
        /// process down, otherwise the client can see an EOF instead of the
        /// 200 (exit 3).
        flushed: Option<std::sync::mpsc::Receiver<()>>,
    },
    /// Sent by the health checker when a program needs a restart.
    HealthRestart {
        name: String,
    },
    /// run-daemon-first D3: validate an ad-hoc definition, stage it and spawn
    /// its program. Fast and synchronous — the caller (the HTTP connection
    /// thread) does the actual waiting, never this loop.
    AdhocUp {
        spec: AdhocSpec,
        reply: AdhocUpReply,
    },
    /// run-daemon-first D3: rename the placeholder onto the real record,
    /// clear the staged entry and update the app records.
    AdhocCommit {
        name: String,
        reply: AdhocCommitReply,
    },
    /// run-daemon-first D3: stop and drop a staged program, delete the
    /// placeholder and clear the staged entry.
    AdhocAbort {
        name: String,
        reply: AdhocAbortReply,
    },
}

/// Which per-program action an app fan-out runs (actions spec: 动作层级矩阵).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FanoutKind {
    Start,
    Stop,
    Restart,
}

impl FanoutKind {
    pub fn as_str(self) -> &'static str {
        match self {
            FanoutKind::Start => "start",
            FanoutKind::Stop => "stop",
            FanoutKind::Restart => "restart",
        }
    }
}

/// One per-program line of a fan-out result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FanoutStep {
    pub program: String,
    /// Human-readable per-program outcome ("started", "already running", …).
    pub result: String,
}

/// Structured result of one app fan-out. The supervisor replies with this
/// serialized; the CLI renders it via [`render_fanout`], the API returns it
/// alongside the rendered text.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FanoutResult {
    pub app: String,
    pub action: String,
    pub programs: Vec<FanoutStep>,
}

/// Render a fan-out result for the CLI/shell: one line per program, in the
/// order the programs were processed (start order; stop is reversed).
pub fn render_fanout(r: &FanoutResult) -> String {
    let mut out = vec![format!(
        "app[{}] {}: {} program(s)",
        r.app,
        r.action,
        r.programs.len()
    )];
    for s in &r.programs {
        out.push(format!("  {}: {}", s.program, s.result));
    }
    out.join("\n")
}

/// Why a `signal` request failed; carries the HTTP status for the API.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SignalError {
    UnknownProgram,
    NotRunning,
    Delivery(String),
}

impl SignalError {
    pub fn http_status(&self) -> u16 {
        match self {
            SignalError::UnknownProgram => 404,
            SignalError::NotRunning => 409,
            SignalError::Delivery(_) => 400,
        }
    }
}

impl std::fmt::Display for SignalError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SignalError::UnknownProgram => write!(f, "unknown program"),
            SignalError::NotRunning => {
                write!(f, "program is not running (no child process to signal)")
            }
            SignalError::Delivery(msg) => write!(f, "{msg}"),
        }
    }
}

/// Deliver a whitelist signal to a program's current child process. The
/// state machine is untouched (actions spec: signal 成功投递 MUST NOT 改变
/// 程序状态机); a program without a live child is an error.
pub fn signal_program(
    sup: &Supervisor,
    name: &str,
    sig: crate::platform::Signal,
) -> std::result::Result<String, SignalError> {
    let (pid, state) = {
        let st = sup.state.lock().unwrap();
        let p = st.programs.get(name).ok_or(SignalError::UnknownProgram)?;
        (
            p.pid().ok_or(SignalError::NotRunning)?,
            p.state().to_string(),
        )
    };
    crate::platform::send_signal(pid, sig).map_err(SignalError::Delivery)?;
    Ok(format!(
        "signal {} delivered to program {name} (pid {pid}, state {state})",
        sig.name()
    ))
}

/// Snapshot the runtime values `${...}` substitution can resolve to
/// (one state lock, then the worker thread substitutes + spawns without
/// touching supervisor state).
pub fn run_context(st: &SupervisorState) -> crate::action::RunContext {
    let mut ctx = crate::action::RunContext {
        daemon_pid: std::process::id(),
        daemon_socket: st.config.daemon.socket.to_string_lossy().into_owned(),
        daemon_log_dir: config::resolve_path(&st.config.daemon.log_dir, &st.config_dir),
        daemon_app_dir: config::resolve_path(&st.config.daemon.app_dir, &st.config_dir),
        ..Default::default()
    };
    for a in &st.apps {
        ctx.apps.insert(a.name.clone(), a.path.clone());
    }
    for p in st.programs.values() {
        ctx.programs.insert(
            p.def.name.clone(),
            crate::action::ProgramVars {
                pid: p.pid(),
                state: p.state().to_string(),
                app: p.def.app.clone(),
                work_dir: p.def.work_dir.clone(),
                log_dir: p.log_dir.clone(),
            },
        );
    }
    ctx
}

/// Range an `apply` acts on: everything, one app, or one program.
#[derive(Debug, Clone)]
pub enum ApplyScope {
    All,
    App(String),
    Program(String, String),
}

impl ApplyScope {
    pub fn matches(&self, app: &str, program: &str) -> bool {
        match self {
            ApplyScope::All => true,
            ApplyScope::App(a) => a == app,
            ApplyScope::Program(a, p) => a == app && p == program,
        }
    }
}

/// The literal `all` in an apply scope is the whole-registry keyword
/// (`apply all` == bare `apply`), not an app name. `registry::add` rejects
/// registering an app named `all` so the two can never collide
/// (apply-workflow spec).
pub const ALL_KEYWORD: &str = "all";

/// Validate apply-scope request parts against the registered apps/programs
/// and resolve them into an [`ApplyScope`]. Single source shared by
/// `/v1/apply` (server.rs) and `/api/apply` (web.rs): the `all` keyword and
/// the unknown app/program 404s must never drift apart.
///
/// `Err` carries the user-facing message ("unknown app/program ...").
pub fn resolve_apply_scope(
    sup: &Supervisor,
    app: Option<&str>,
    program: Option<&str>,
) -> Result<ApplyScope, String> {
    if app == Some(ALL_KEYWORD) {
        // `all` is the reserved full-scope keyword and never addresses a
        // real app (registry::add rejects it), so `apply all <program>`
        // has no coherent meaning — reject instead of silently ignoring
        // the program part.
        if program.is_some() {
            return Err(format!(
                "{ALL_KEYWORD:?} is the full-scope keyword and takes no program argument"
            ));
        }
        return Ok(ApplyScope::All);
    }
    let st = sup.state.lock().unwrap();
    if let Some(a) = app {
        // Loaded apps validate directly; a freshly registered app may only
        // be pending (detected on disk, not applied yet) — it is still a
        // valid target (apply-workflow: a new app's programs start when
        // they enter the apply scope).
        let known = st.apps.iter().any(|r| r.name == a)
            || registry::list(&st.config, &st.config_dir)
                .map(|listed| listed.iter().any(|l| l.name == a && l.broken.is_none()))
                .unwrap_or(false);
        if !known {
            return Err(format!("unknown app {a:?}"));
        }
        if let Some(p) = program {
            let known = st.programs.get(p).map(|x| x.def.app == a).unwrap_or(false);
            if !known {
                return Err(format!("unknown program {p:?}"));
            }
        }
    }
    Ok(match (app, program) {
        (Some(a), Some(p)) => ApplyScope::Program(a.to_string(), p.to_string()),
        (Some(a), None) => ApplyScope::App(a.to_string()),
        _ => ApplyScope::All,
    })
}

#[derive(Clone)]
pub struct AppRecord {
    pub name: String,
    #[allow(dead_code)]
    pub path: PathBuf,
    pub priority: i32,
}

/// The persisted "user stopped" record file name inside `log_dir`.
const USER_STOPPED_FILE: &str = "state.json";

/// Load the persisted user-stop set. Missing file → empty; unreadable or
/// corrupt → warn and treat as empty (worst case: one extra pull-up).
fn load_user_stopped(log_dir: &Path) -> HashSet<String> {
    let path = log_dir.join(USER_STOPPED_FILE);
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return HashSet::new(),
        Err(e) => {
            warn!(
                "cannot read {} ({}); treating the user-stopped set as empty",
                path.display(),
                e
            );
            return HashSet::new();
        }
    };
    #[derive(Deserialize)]
    struct State {
        #[serde(default)]
        user_stopped: Vec<String>,
    }
    match serde_json::from_str::<State>(&text) {
        Ok(s) => s.user_stopped.into_iter().collect(),
        Err(e) => {
            warn!(
                "{} is not valid state JSON ({}); treating the user-stopped set as empty",
                path.display(),
                e
            );
            HashSet::new()
        }
    }
}

/// Persist the user-stop set atomically (temp file + rename). A write failure
/// is logged, never fatal — the in-memory set still governs this session.
fn save_user_stopped(log_dir: &Path, set: &HashSet<String>) {
    #[derive(Serialize)]
    struct State<'a> {
        user_stopped: Vec<&'a str>,
    }
    let mut names: Vec<&str> = set.iter().map(|s| s.as_str()).collect();
    names.sort();
    let body = match serde_json::to_string_pretty(&State {
        user_stopped: names,
    }) {
        Ok(b) => b,
        Err(e) => {
            warn!("cannot serialize the user-stopped set: {e}");
            return;
        }
    };
    let path = log_dir.join(USER_STOPPED_FILE);
    let tmp = log_dir.join(format!("{USER_STOPPED_FILE}.tmp"));
    if let Err(e) = std::fs::write(&tmp, body) {
        warn!("cannot write {}: {e}", tmp.display());
        return;
    }
    if let Err(e) = std::fs::rename(&tmp, &path) {
        warn!("cannot replace {}: {e}", path.display());
        let _ = std::fs::remove_file(&tmp);
    }
}

/// One program's entry in the pending-change projection: the on-disk config
/// differs from what the daemon is currently running (or the program is not
/// yet known to the daemon at all).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PendingProgram {
    pub app: String,
    pub program: String,
    /// Running now (`true`) vs not running (`false`): a changed program that
    /// is up will restart on apply; a stopped one only gets redefined.
    pub running: bool,
}

/// The detected-but-not-yet-applied configuration diff, as shown by
/// `GET /v1/pending`, the `reload` preview and the WS snapshot.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct PendingDoc {
    pub programs: Vec<PendingProgram>,
    pub apps_added: Vec<String>,
    pub apps_removed: Vec<String>,
    /// Daemon fields that cannot hot-apply (host/port/auth...).
    pub daemon_hints: Vec<String>,
    /// Per-app load failures isolated by detection (bad files keep the old
    /// definition; the text explains what is wrong).
    pub errors: Vec<String>,
}

impl PendingDoc {
    pub fn is_empty(&self) -> bool {
        self.programs.is_empty()
            && self.apps_added.is_empty()
            && self.apps_removed.is_empty()
            && self.daemon_hints.is_empty()
            && self.errors.is_empty()
    }
}

/// What an apply did to one program (one entry per in-scope program).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProgramAction {
    pub app: String,
    pub program: String,
    /// Disk config differs from the running definition (new = changed).
    pub changed: bool,
    /// update-and-restart | update-only | restart | start | keep-stopped |
    /// none | remove
    pub action: String,
    /// "ok" or a short failure explanation.
    pub result: String,
}

/// The structured result of one `apply`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ApplyResult {
    pub programs: Vec<ProgramAction>,
    pub apps_added: Vec<String>,
    pub apps_removed: Vec<String>,
    pub daemon_hints: Vec<String>,
    /// Apps whose (re)load failed during this apply; their pending entry is
    /// kept so a later apply can retry.
    pub errors: Vec<String>,
    /// One human-readable line ("no changes" when nothing was pending).
    pub summary: String,
}

/// A full detection pass: what differs between disk and the running
/// daemon, without touching any process.
struct Detection {
    config: DaemonConfig,
    /// Apps that loaded cleanly (their definitions may still differ from
    /// what is running — that is what `pending` is about).
    resolved: Vec<ResolvedApp>,
    records: Vec<AppRecord>,
    /// Apps whose files failed to load; they keep their old definition.
    failed: Vec<String>,
}

/// Everything the API threads read and the main loop mutates.
pub struct SupervisorState {
    pub programs: HashMap<String, ManagedProgram>,
    pub apps: Vec<AppRecord>,
    pub config: DaemonConfig,
    pub config_dir: PathBuf,
    pub shutdown: AtomicBool,
    /// Latest detection outcome (disk config vs running definitions).
    /// Written by the supervision loop, read by the API threads.
    pub pending: PendingDoc,
    /// mtime+size snapshot per config file, to skip re-parsing unchanged
    /// files on every detect pass.
    pub file_stamps: HashMap<PathBuf, (std::time::SystemTime, u64)>,
    /// Programs the user explicitly stopped (persisted across daemon restarts
    /// in `{log_dir}/state.json`). Consulted by `unless-stopped` to decide
    /// whether to pull a program up at daemon start / on apply. `always` and
    /// `on-failure` ignore it; shutdown never writes it.
    pub user_stopped: HashSet<String>,
    /// run-daemon-first D1: ad-hoc apps that are started but not yet persisted
    /// (keyed by app name). Detection merges these so their programs survive
    /// periodic rescans until the commit lands or the abort drops them.
    pub staged: HashMap<String, StagedApp>,
}

pub struct Supervisor {
    pub state: Arc<Mutex<SupervisorState>>,
    queue: Mutex<VecDeque<Command>>,
    cv: Condvar,
    pub health_tasks: health::TaskMap,
    /// Latest metrics from the sideband sampler thread; the only writer is
    /// [`crate::metrics::spawn_sampler`], the supervision loop never touches it.
    pub metrics: Arc<crate::metrics::MetricsTable>,
    /// (program, action) mutual exclusion for custom actions. Only the
    /// control-plane worker threads touch it — never the supervision loop.
    pub actions: Arc<crate::action::ActionRuns>,
    /// Web-console intent handle (config-driven-webui): reload publishes a
    /// new intent here, the resident manager thread converges the runtime.
    pub webui: Arc<WebuiControl>,
    /// The console's access-control state (`webui.auth_token`), published by
    /// whichever console instance is currently bound so the credential
    /// endpoints answer consistently with the guard middleware.
    pub console_auth: Mutex<std::sync::Arc<crate::web::ConsoleAuth>>,
    config_path: Mutex<PathBuf>,
    started: std::time::Instant,
    interval: Duration,
}

impl Supervisor {
    pub fn new(config: DaemonConfig, config_dir: &Path) -> Result<Arc<Self>> {
        let log_dir = config::resolve_path(&config.daemon.log_dir, config_dir);
        std::fs::create_dir_all(&log_dir)
            .with_context(|| format!("failed to create log dir: {}", log_dir.display()))?;
        let interval = Duration::from_secs_f64(config.daemon.monitor_interval.max(0.05));
        // Load the persisted "user stopped" records (unless-stopped support):
        // a missing file is an empty set, a corrupt one is warned about and
        // treated as empty (worst case: one extra pull-up).
        let user_stopped = load_user_stopped(&log_dir);
        Ok(Arc::new(Supervisor {
            state: Arc::new(Mutex::new(SupervisorState {
                programs: HashMap::new(),
                apps: Vec::new(),
                config,
                config_dir: config_dir.to_path_buf(),
                shutdown: AtomicBool::new(false),
                pending: PendingDoc::default(),
                file_stamps: HashMap::new(),
                user_stopped,
                staged: HashMap::new(),
            })),
            queue: Mutex::new(VecDeque::new()),
            cv: Condvar::new(),
            health_tasks: Arc::new(Mutex::new(HashMap::new())),
            metrics: Arc::new(crate::metrics::MetricsTable::new()),
            actions: crate::action::ActionRuns::new(),
            webui: WebuiControl::new(),
            console_auth: Mutex::new(std::sync::Arc::new(
                crate::web::ConsoleAuth::disabled(),
            )),
            config_path: Mutex::new(config_dir.join("appkeeper.toml")),
            started: std::time::Instant::now(),
            interval,
        }))
    }

    /// Remember where the daemon config lives so `reload` re-reads it.
    pub fn set_config_path(&self, p: &Path) {
        *self.config_path.lock().unwrap() = p.to_path_buf();
    }

    /// Where the daemon config lives (config-source display for the console).
    pub fn config_path(&self) -> PathBuf {
        self.config_path.lock().unwrap().clone()
    }

    /// How long the daemon has been running (console overview).
    pub fn uptime_secs(&self) -> f64 {
        self.started.elapsed().as_secs_f64()
    }

    pub fn enqueue(&self, cmd: Command) {
        self.queue.lock().unwrap().push_back(cmd);
        self.cv.notify_all();
    }

    pub fn request_shutdown(&self) {
        self.state
            .lock()
            .unwrap()
            .shutdown
            .store(true, Ordering::SeqCst);
        self.cv.notify_all();
    }

    fn pop_command(&self) -> Option<Command> {
        self.queue.lock().unwrap().pop_front()
    }

    // -- bootstrap ----------------------------------------------------------

    /// Load every registered app and start eligible programs.
    pub fn bootstrap(&self) -> Result<()> {
        let (config, config_dir) = {
            let st = self.state.lock().unwrap();
            (st.config.clone(), st.config_dir.clone())
        };
        let listed = registry::list(&config, &config_dir)?;
        let mut records = Vec::new();
        let mut resolved: Vec<ResolvedApp> = Vec::new();
        for l in &listed {
            if let Some(reason) = &l.broken {
                error!(
                    "app[{}] failed to load at startup (skipped): {}",
                    l.name, reason
                );
                continue;
            }
            match registry::reload_app(&l.name, &l.path, config.app_default.as_ref()) {
                Ok(a) => {
                    records.push(AppRecord {
                        name: a.name.clone(),
                        path: a.path.clone(),
                        priority: a.priority,
                    });
                    resolved.push(a);
                }
                Err(e) => error!("app[{}] failed to load at startup (skipped): {e:#}", l.name),
            }
        }
        // Uniqueness of program names across apps is enforced at add-time, but
        // files may have drifted since; report instead of refusing to boot.
        if let Err(e) = config::validate_all(&resolved) {
            error!("cross-app validation problem at startup: {e}");
        }
        {
            let mut st = self.state.lock().unwrap();
            let log_dir = config::resolve_path(&st.config.daemon.log_dir, &st.config_dir);
            let ring_cap = st.config.daemon.log_buffer_lines;
            for app in &resolved {
                for p in app.programs.clone() {
                    st.programs.insert(
                        p.name.clone(),
                        ManagedProgram::new(p, log_dir.clone(), ring_cap),
                    );
                }
            }
            st.apps = records;
        }
        self.sync_health_tasks();
        {
            let st = self.state.lock().unwrap();
            info!(
                "bootstrap: {} app(s), {} program(s)",
                st.apps.len(),
                st.programs.len()
            );
        }
        self.start_eligible();
        Ok(())
    }

    fn sync_health_tasks(&self) {
        let st = self.state.lock().unwrap();
        let mut tasks = self.health_tasks.lock().unwrap();
        tasks.clear();
        for p in st.programs.values() {
            if let Some(h) = &p.def.health {
                tasks.insert(p.def.name.clone(), health::HealthTask { check: h.clone() });
            }
        }
    }

    // -- ordered startup ----------------------------------------------------

    /// Priority-ordered startup with dependency waiting/giving-up. Programs
    /// whose dependencies are not yet running stay `stopped` (with a wait
    /// reason); permanently unavailable dependencies make them `fatal`.
    ///
    /// Pull-up eligibility follows each program's `restart` policy: `no` is
    /// never pulled up; `on-failure`/`always` are; `unless-stopped` is pulled
    /// up only when there is no "user stopped" record for it (process-management
    /// spec: 守护启动与 apply 引入新程序时的拉起语义).
    pub fn start_eligible(&self) {
        enum Action {
            Spawn,
            Fatalize(String),
        }
        loop {
            let snapshot: Vec<(String, Vec<String>)> = {
                let st = self.state.lock().unwrap();
                let mut eligible: Vec<&AppRecord> = st.apps.iter().collect();
                eligible.sort_by_key(|a| (a.priority, a.name.clone()));
                eligible
                    .iter()
                    .flat_map(|a| {
                        let mut names: Vec<&String> = st
                            .programs
                            .values()
                            .filter(|p| p.def.app == a.name)
                            .map(|p| &p.def.name)
                            .collect();
                        names.sort();
                        names.into_iter().cloned().collect::<Vec<_>>()
                    })
                    .map(|n| {
                        let deps = st
                            .programs
                            .get(&n)
                            .map(|p| p.def.depends_on.clone())
                            .unwrap_or_default();
                        (n, deps)
                    })
                    .collect()
            };
            // Pass 1: decide (immutable).
            let mut actions: Vec<(String, Action)> = Vec::new();
            {
                let st = self.state.lock().unwrap();
                for (name, deps) in &snapshot {
                    let Some(p) = st.programs.get(name) else {
                        continue;
                    };
                    if p.state() != ProgramState::Stopped || p.user_stopped() {
                        continue;
                    }
                    // `no` programs are never pulled up; `unless-stopped`
                    // honors the persisted "user stopped" record.
                    if !p.def.restart.pulls_up(st.user_stopped.contains(name)) {
                        continue;
                    }
                    let mut ready = true;
                    let mut dead = Vec::new();
                    for d in deps {
                        match st.programs.get(d) {
                            Some(dep) => match dep.state() {
                                ProgramState::Running => {}
                                ProgramState::Fatal => dead.push(format!(
                                    "{d} (fatal: {})",
                                    dep.fatal_reason().unwrap_or("unknown")
                                )),
                                ProgramState::Exited => dead.push(d.clone()),
                                _ => ready = false,
                            },
                            None => dead.push(format!("{d} (not registered)")),
                        }
                    }
                    if !dead.is_empty() {
                        actions.push((name.clone(), Action::Fatalize(dead.join(", "))));
                    } else if ready {
                        actions.push((name.clone(), Action::Spawn));
                    }
                }
            }
            // Pass 2: apply (mutable).
            let mut progress = false;
            {
                let mut st = self.state.lock().unwrap();
                for (name, action) in actions {
                    match action {
                        Action::Spawn => {
                            if let Some(p) = st.programs.get_mut(&name) {
                                if p.state() == ProgramState::Stopped {
                                    p.spawn();
                                    progress = true;
                                }
                            }
                        }
                        Action::Fatalize(reason) => {
                            if let Some(p) = st.programs.get_mut(&name) {
                                p.mark_dependency_fatal(reason);
                            }
                        }
                    }
                }
            }
            if !progress {
                break;
            }
        }
    }

    // -- command execution --------------------------------------------------

    fn execute(&self, cmd: Command) {
        match cmd {
            Command::Start { name, reply } => answer(reply, self.cmd_start(&name)),
            Command::Stop { name, reply } => answer(reply, self.cmd_stop(&name)),
            Command::Restart { name, reply } => {
                let r = self.cmd_stop(&name).and_then(|_| self.cmd_start(&name));
                answer(reply, r);
            }
            Command::AppFanout { kind, app, reply } => {
                answer(reply, self.cmd_app_fanout(kind, &app))
            }
            Command::HealthRestart { name } => {
                warn!("health checker requesting restart of program[{name}]");
                let _ = self.cmd_stop(&name);
                let _ = self.cmd_start(&name);
            }
            Command::AdhocUp { spec, reply } => {
                let _ = reply.send(self.cmd_adhoc_up(&spec));
            }
            Command::AdhocCommit { name, reply } => {
                let _ = reply.send(self.cmd_adhoc_commit(&name));
            }
            Command::AdhocAbort { name, reply } => {
                let _ = reply.send(self.cmd_adhoc_abort(&name));
            }
            Command::Reload { reply } => answer(reply, self.cmd_reload()),
            Command::Apply {
                scope,
                restart,
                reply,
            } => answer(reply, self.cmd_apply(&scope, restart)),
            Command::Shutdown { reply, flushed } => {
                answer(reply, Ok("shutting down".to_string()));
                // Wait (bounded) for the HTTP layer to write the response
                // before shutting down; without this the client races the
                // process exit and sees "Unexpected EOF" (exit 3).
                if let Some(rx) = flushed {
                    let _ = rx.recv_timeout(std::time::Duration::from_secs(5));
                }
                self.request_shutdown();
            }
        }
    }

    fn cmd_start(&self, name: &str) -> Result<String> {
        let mut st = self.state.lock().unwrap();
        let state_now = {
            let p = st
                .programs
                .get_mut(name)
                .ok_or_else(|| anyhow::anyhow!("unknown program {name:?}"))?;
            if matches!(p.state(), ProgramState::Running | ProgramState::Starting) {
                bail!("program {name:?} is already {}", p.state());
            }
            p.clear_fatal();
            p.spawn();
            p.state()
        };
        // An explicit start clears the persisted "user stopped" record
        // (process-management spec: 对该程序的显式启动 SHALL 清除记录).
        let changed = st.user_stopped.remove(name);
        let log_dir = config::resolve_path(&st.config.daemon.log_dir, &st.config_dir);
        if changed {
            save_user_stopped(&log_dir, &st.user_stopped);
        }
        Ok(format!("program {name} is {state_now}"))
    }

    fn cmd_stop(&self, name: &str) -> Result<String> {
        let mut st = self.state.lock().unwrap();
        let already = {
            let p = st
                .programs
                .get_mut(name)
                .ok_or_else(|| anyhow::anyhow!("unknown program {name:?}"))?;
            if matches!(
                p.state(),
                ProgramState::Stopped | ProgramState::Exited | ProgramState::Fatal
            ) {
                Some(p.state())
            } else {
                p.stop();
                None
            }
        };
        if let Some(s) = already {
            return Ok(format!("program {name} was already {s}"));
        }
        // An explicit stop writes the persisted "user stopped" record, which
        // `unless-stopped` consults at daemon start / apply (process-management
        // spec). `always` ignores it; shutdown never writes it.
        let log_dir = config::resolve_path(&st.config.daemon.log_dir, &st.config_dir);
        if st.user_stopped.insert(name.to_string()) {
            save_user_stopped(&log_dir, &st.user_stopped);
        }
        Ok(format!("program {name} is stopped"))
    }

    // -- app-level fan-out --------------------------------------------------

    /// Programs of one app in fan-out order: dependency topological order
    /// among the app's programs with a name tie-break — the same effective
    /// ordering the daemon startup path uses (app priority is uniform within
    /// one app, names sort). Cross-app dependencies do not constrain the
    /// order. Stop is the reverse of this (actions spec: 排序复用
    /// process-management 规则).
    fn app_program_order(
        st: &SupervisorState,
        app: &str,
    ) -> std::result::Result<Vec<String>, String> {
        if !st.apps.iter().any(|a| a.name == app) {
            return Err(format!("unknown app {app:?}"));
        }
        let mut names: Vec<String> = st
            .programs
            .values()
            .filter(|p| p.def.app == app)
            .map(|p| p.def.name.clone())
            .collect();
        if names.is_empty() {
            return Err(format!("app {app:?} has no programs"));
        }
        names.sort();
        let members: HashSet<&str> = names.iter().map(|s| s.as_str()).collect();
        // Dependency edges restricted to the app (deduplicated so a repeated
        // entry in depends_on cannot skew the in-degree).
        let mut edges: HashSet<(&str, &str)> = HashSet::new();
        for n in &names {
            let deps = st
                .programs
                .get(n.as_str())
                .map(|p| p.def.depends_on.as_slice())
                .unwrap_or(&[]);
            for d in deps {
                if members.contains(d.as_str()) {
                    edges.insert((d.as_str(), n.as_str()));
                }
            }
        }
        let mut indegree: HashMap<&str, usize> = names.iter().map(|n| (n.as_str(), 0)).collect();
        let mut dependents: HashMap<&str, Vec<&str>> = HashMap::new();
        for (dep, dependent) in &edges {
            *indegree.entry(dependent).or_insert(0) += 1;
            dependents.entry(dep).or_default().push(dependent);
        }
        // Kahn's algorithm; the sorted frontier is the name tie-break.
        let mut ready: std::collections::BTreeSet<&str> = names
            .iter()
            .map(|n| n.as_str())
            .filter(|n| indegree[n] == 0)
            .collect();
        let mut out = Vec::with_capacity(names.len());
        while let Some(n) = ready.pop_first() {
            for dep in dependents.remove(n).unwrap_or_default() {
                let e = indegree.get_mut(dep).unwrap();
                *e -= 1;
                if *e == 0 {
                    ready.insert(dep);
                }
            }
            out.push(n.to_string());
        }
        if out.len() < names.len() {
            // Unreachable: validate_all rejects dependency cycles at load.
            // Keep the fan-out total anyway instead of silently dropping.
            let done: HashSet<String> = out.iter().cloned().collect();
            for n in &names {
                if !done.contains(n) {
                    out.push(n.clone());
                }
            }
        }
        Ok(out)
    }

    /// Expand one app fan-out: sequential per-program runs of the existing
    /// start/stop/restart logic (spawn clears `user_stopped`; stop is the
    /// reverse of the start order). Errors are recorded per program — the
    /// fan-out never aborts halfway.
    fn cmd_app_fanout(&self, kind: FanoutKind, app: &str) -> Result<String> {
        let order = {
            let st = self.state.lock().unwrap();
            Self::app_program_order(&st, app).map_err(|e| anyhow::anyhow!("{e}"))?
        };
        let seq: Vec<String> = match kind {
            FanoutKind::Start | FanoutKind::Restart => order,
            FanoutKind::Stop => order.into_iter().rev().collect(),
        };
        let mut steps = Vec::with_capacity(seq.len());
        for name in &seq {
            let result = match kind {
                FanoutKind::Start => {
                    let busy = {
                        let st = self.state.lock().unwrap();
                        st.programs
                            .get(name)
                            .map(|p| {
                                matches!(p.state(), ProgramState::Running | ProgramState::Starting)
                            })
                            .unwrap_or(true)
                    };
                    if busy {
                        Ok(format!("program {name} is already running"))
                    } else {
                        self.cmd_start(name)
                    }
                }
                FanoutKind::Stop => self.cmd_stop(name),
                // Restart bounces each program in start order: dependencies
                // come up first, every program is replaced in place.
                FanoutKind::Restart => self.cmd_stop(name).and_then(|_| self.cmd_start(name)),
            };
            steps.push(FanoutStep {
                program: name.clone(),
                result: match result {
                    Ok(text) => text,
                    Err(e) => format!("error: {e:#}"),
                },
            });
        }
        let fr = FanoutResult {
            app: app.to_string(),
            action: kind.as_str().to_string(),
            programs: steps,
        };
        serde_json::to_string(&fr).context("failed to serialize fan-out result")
    }

    // -- reload / apply -----------------------------------------------------

    /// Rescan the registry and daemon config, compute the pending diff and
    /// publish it. `rescan` forces a full pass (reload/apply commands); the
    /// periodic tick passes `false` so unchanged files (mtime+size match)
    /// skip re-parsing. Errors mean the daemon config itself is invalid —
    /// the previously published pending state is then left untouched.
    fn detect(&self, rescan: bool) -> Result<PendingDoc> {
        let config_path = self.config_path.lock().unwrap().clone();
        let (new_config, _) = DaemonConfig::load_or_default(&config_path)
            .map_err(|e| anyhow::anyhow!("detection failed, daemon config invalid: {e:#}"))?;
        let config_dir = self.state.lock().unwrap().config_dir.clone();
        let defaults = new_config.app_default.clone();

        let listed = registry::list(&new_config, &config_dir)?;
        let stamps_now = self.state.lock().unwrap().file_stamps.clone();
        let cached_defs: Mutex<HashMap<PathBuf, ResolvedApp>> = Mutex::new(HashMap::new());
        let mut errors: Vec<String> = Vec::new();
        let mut new_records: Vec<AppRecord> = Vec::new();
        let mut resolved: Vec<ResolvedApp> = Vec::new();

        for l in &listed {
            if let Some(reason) = &l.broken {
                errors.push(format!("app[{}] kept old config: {reason}", l.name));
                let st = self.state.lock().unwrap();
                if let Some(old) = st.apps.iter().find(|a| a.name == l.name) {
                    new_records.push(old.clone());
                }
                continue;
            }
            // Fast path: file unchanged since the last detection — reuse
            // the disk-side definitions we parsed last time (NOT the running
            // ones: a changed file stays "changed" until applied).
            if !rescan
                && file_stamp(&l.path).map(|s| stamps_now.get(&l.path) == Some(&s)) == Some(true)
            {
                let cached = cached_defs.lock().unwrap().get(&l.path).cloned();
                if let Some(a) = cached {
                    new_records.push(AppRecord {
                        name: a.name.clone(),
                        path: a.path.clone(),
                        priority: a.priority,
                    });
                    resolved.push(a);
                    continue;
                }
                // No cached parse yet (first pass after boot): fall through
                // to a full parse below.
            }
            match registry::reload_app(&l.name, &l.path, defaults.as_ref()) {
                Ok(a) => {
                    cached_defs
                        .lock()
                        .unwrap()
                        .insert(l.path.clone(), a.clone());
                    new_records.push(AppRecord {
                        name: a.name.clone(),
                        path: a.path.clone(),
                        priority: a.priority,
                    });
                    resolved.push(a);
                }
                Err(e) => {
                    errors.push(format!("app[{}] kept old config: {e:#}", l.name));
                    let st = self.state.lock().unwrap();
                    if let Some(old) = st.apps.iter().find(|a| a.name == l.name) {
                        new_records.push(old.clone());
                    }
                }
            }
        }
        if let Err(e) = config::validate_all(&resolved) {
            // Cross-app conflict: refuse to publish any pending state; the
            // running config stays authoritative until the conflict is fixed.
            bail!("detection failed, cross-app validation failed: {e}");
        }
        // run-daemon-first D1: merge staged (started-but-unpersisted) apps so
        // their programs are treated as known and are neither dropped by
        // `keep` nor shown as pending. They are visible in `programs` for the
        // HTTP thread's startup wait.
        self.merge_staged(&mut resolved, &mut new_records);

        let mut doc = PendingDoc {
            errors,
            ..PendingDoc::default()
        };
        {
            let st = self.state.lock().unwrap();
            // Apps no longer registered on disk.
            let keep: HashSet<String> = new_records.iter().map(|a| a.name.clone()).collect();
            for a in &st.apps {
                if !keep.contains(&a.name) {
                    doc.apps_removed.push(a.name.clone());
                }
            }
            // Per-app program diff: hash mismatch, new program, or dropped
            // program (file no longer defines it) — all count as changed.
            for app in &resolved {
                // A staged app is started-but-unpersisted: it is deliberately
                // absent from `st.apps` and MUST NOT show up as "added"
                // (run-daemon-first D1).
                if !st.apps.iter().any(|a| a.name == app.name) && !st.staged.contains_key(&app.name)
                {
                    doc.apps_added.push(app.name.clone());
                }
                let new_names: HashSet<&str> =
                    app.programs.iter().map(|p| p.name.as_str()).collect();
                for def in &app.programs {
                    let changed = match st.programs.get(&def.name) {
                        Some(p) => p.def.hash != def.hash,
                        None => true,
                    };
                    if changed {
                        let running = st
                            .programs
                            .get(&def.name)
                            .map(|p| p.is_running())
                            .unwrap_or(false);
                        doc.programs.push(PendingProgram {
                            app: app.name.clone(),
                            program: def.name.clone(),
                            running,
                        });
                    }
                }
                for p in st.programs.values() {
                    if p.def.app == app.name && !new_names.contains(p.def.name.as_str()) {
                        doc.programs.push(PendingProgram {
                            app: app.name.clone(),
                            program: p.def.name.clone(),
                            running: p.is_running(),
                        });
                    }
                }
            }
            // Daemon fields that cannot hot-apply: the control endpoint is
            // bound once at startup and the listening socket is not rebindable
            // mid-flight.
            if st.config.daemon.socket != new_config.daemon.socket {
                doc.daemon_hints
                    .push("daemon socket changed: restart appkeeper to apply".into());
            }
        }
        // Refresh file stamps (all files we parsed).
        let mut stamps = HashMap::new();
        for l in &listed {
            if let Some(s) = file_stamp(&l.path) {
                stamps.insert(l.path.clone(), s);
            }
        }
        {
            let mut st = self.state.lock().unwrap();
            st.file_stamps = stamps;
            st.pending = doc.clone();
        }
        Ok(doc)
    }

    /// run-daemon-first D1: fold the staged apps into a detection result so
    /// their programs are treated as known. Called with the freshly resolved
    /// disk set before the pending diff is computed — a staged app therefore
    /// does not appear in `apps_added`/`programs` and is not dropped by the
    /// `keep` set. Only apps that are NOT yet on disk are merged (a committed
    /// staged app is already covered by its record).
    fn merge_staged(&self, resolved: &mut Vec<ResolvedApp>, records: &mut Vec<AppRecord>) {
        let staged: Vec<StagedApp> = {
            let st = self.state.lock().unwrap();
            st.staged.values().cloned().collect()
        };
        for s in staged {
            if records.iter().any(|r| r.name == s.app) {
                continue; // already visible on disk (committed)
            }
            records.push(AppRecord {
                name: s.resolved.name.clone(),
                path: s.resolved.path.clone(),
                priority: s.resolved.priority,
            });
            resolved.push(s.resolved.clone());
        }
    }

    // -- ad-hoc registration (run-daemon-first D1/D3) ------------------------

    /// Resolve the app name from the spec (explicit `--name` or the command's
    /// file stem) and validate it: identifier charset, not the reserved `all`
    /// keyword, and no collision with a program name owned by a DIFFERENT app.
    ///
    /// A same-name app's own repeated submission is NOT a collision: per the
    /// control-plane spec it is a full regeneration / upsert (see
    /// [`Self::cmd_adhoc_up`]), so the program belonging to the same app name
    /// is allowed through.
    fn resolve_adhoc_name(&self, spec: &AdhocSpec) -> std::result::Result<String, AdhocUpError> {
        let name = match &spec.name {
            Some(n) => n.clone(),
            None => spec
                .command
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .filter(|s| !s.is_empty())
                .ok_or_else(|| {
                    AdhocUpError::Validation(
                        "cannot derive an app name from the command path; pass --name".into(),
                    )
                })?,
        };
        if !config::is_valid_name(&name) {
            return Err(AdhocUpError::Validation(format!(
                "app name {name:?} is invalid: {}",
                config::NAME_RULE
            )));
        }
        if name == ALL_KEYWORD {
            return Err(AdhocUpError::Validation(format!(
                "app name {name:?} is reserved: `ak apply all` means apply-all; choose another --name"
            )));
        }
        let st = self.state.lock().unwrap();
        // Only a program owned by ANOTHER app is a genuine collision: a
        // same-name app's own program is the upsert/regeneration target
        // (control-plane spec: 同名应用自身的重复提交不属冲突).
        if let Some(p) = st.programs.get(&name)
            && p.def.app != name
        {
            return Err(AdhocUpError::Validation(format!(
                "program name {name:?} is already registered by app {:?}; choose another --name",
                p.def.app
            )));
        }
        Ok(name)
    }

    /// Render + validate the ad-hoc definition without touching disk or
    /// processes; returns the app name, program name and rendered text.
    fn render_adhoc(
        &self,
        spec: &AdhocSpec,
        name: &str,
    ) -> std::result::Result<(PathBuf, String), AdhocUpError> {
        let (config, config_dir) = {
            let st = self.state.lock().unwrap();
            (st.config.clone(), st.config_dir.clone())
        };
        let work_dir = match &spec.work_dir {
            Some(w) => w.clone(),
            None => std::env::current_dir().map_err(|e| {
                AdhocUpError::Validation(format!("cannot read the current directory: {e}"))
            })?,
        };
        let opts = registry::AddOptions {
            name: Some(name.to_string()),
            description: spec.description.clone(),
            restart: spec.restart,
            restart_backoff: spec.restart_backoff,
            priority: None,
            args: None,
            args_argv: Some(spec.args.clone()),
            env: if spec.env.is_empty() {
                None
            } else {
                Some(spec.env.clone())
            },
            workdir: Some(work_dir.clone()),
        };
        let scaf = registry::ScaffoldSpec {
            command: spec.command.clone(),
            args: spec.args.clone(),
            env: spec.env.clone(),
            work_dir,
            health_check: spec.health_check.clone(),
        };        let text = registry::render_scaffold(name, &scaf, &opts)
            .map_err(|e| AdhocUpError::Validation(format!("{e:#}")))?;
        // Validate the rendered config against the current registry (round-trip).
        let raw: crate::config::AppRaw = toml::from_str(&text).map_err(|e| {
            AdhocUpError::Validation(format!("rendered config is not valid TOML: {e}"))
        })?;
        let target = registry::link_path(&config, &config_dir, name);
        let resolved = config::resolve_app(name, &target, &raw, config.app_default.as_ref())
            .map_err(|e| AdhocUpError::Validation(format!("{e:#}")))?;
        let existing = registry::list(&config, &config_dir)
            .map_err(|e| AdhocUpError::Validation(format!("{e:#}")))?;
        let mut all = Vec::new();
        for l in &existing {
            if l.name == name {
                continue;
            }
            let (r, _) = crate::config::AppRaw::load(&l.path)
                .map_err(|e| AdhocUpError::Validation(format!("{e:#}")))?;
            all.push(
                config::resolve_app(&l.name, &l.path, &r, config.app_default.as_ref())
                    .map_err(|e| AdhocUpError::Validation(format!("{e:#}")))?,
            );
        }
        all.push(resolved);
        config::validate_all(&all).map_err(|e| AdhocUpError::Validation(format!("{e:#}")))?;
        Ok((target, text))
    }

    /// `AdhocUp`: validate → stage the placeholder → insert the staged record
    /// and `ManagedProgram` → spawn → reply. Never waits: the caller polls.
    ///
    /// A same-name app that is ALREADY committed (its record is on disk) is an
    /// upsert/regeneration (control-plane spec: 同名重复提交走再生): identical
    /// content is an idempotent success returning the existing pid without
    /// restarting; changed content stops the old process, writes the new
    /// record and starts the new definition. A same-name app that is staged
    /// but not yet committed is the residual (failed-commit) case (D4).
    fn cmd_adhoc_up(&self, spec: &AdhocSpec) -> std::result::Result<AdhocUpResult, AdhocUpError> {
        let name = self.resolve_adhoc_name(spec)?;
        // Residual check (D4): a same-name staged app that already started but
        // whose commit failed. Report it instead of restarting.
        {
            let st = self.state.lock().unwrap();
            if let Some(s) = st.staged.get(&name) {
                let pid = st.programs.get(&s.program).and_then(|p| p.pid());
                let has_health = st
                    .programs
                    .get(&s.program)
                    .map(|p| p.def.health.is_some())
                    .unwrap_or(false);
                return Ok(AdhocUpResult {
                    app: s.app.clone(),
                    program: s.program.clone(),
                    config_path: s.target.display().to_string(),
                    pid,
                    estimate_secs: 0.0,
                    has_health,
                    register_only: false,
                    residual: true,
                    unchanged: false,
                    committed: false,
                });
            }
        }
        // Render + validate the NEW definition before touching anything: a
        // failure here must not disturb the existing record or process.
        let (target, text) = self.render_adhoc(spec, &name)?;
        // Committed same-name app: upsert / full regeneration (no placeholder,
        // no startup judgement — the record is already on disk).
        if let Some(up) = self.adhoc_upsert(spec, &name, &target, &text)? {
            return Ok(up);
        }
        let (config, config_dir) = {
            let st = self.state.lock().unwrap();
            (st.config.clone(), st.config_dir.clone())
        };
        // Write the placeholder BEFORE spawning: a failure here must not start
        // any process (app-registry spec).
        let placeholder = registry::stage_placeholder(&config, &config_dir, &name, &text)
            .map_err(|e| AdhocUpError::Stage(format!("{e:#}")))?;
        // Register-only (`--restart=no`, D8): no startup judgement, no spawn.
        let raw: crate::config::AppRaw = match toml::from_str(&text) {
            Ok(r) => r,
            Err(e) => {
                registry::discard_placeholder(&config, &config_dir, &name);
                return Err(AdhocUpError::Validation(format!("{e}")));
            }
        };
        let resolved = match config::resolve_app(&name, &target, &raw, config.app_default.as_ref())
        {
            Ok(r) => r,
            Err(e) => {
                registry::discard_placeholder(&config, &config_dir, &name);
                return Err(AdhocUpError::Validation(format!("{e:#}")));
            }
        };
        let estimate = self.startup_estimate_secs(&resolved);
        let program = resolved.programs[0].name.clone();
        let staged = StagedApp {
            app: name.clone(),
            program: program.clone(),
            text,
            target: target.clone(),
            placeholder: placeholder.clone(),
            resolved: resolved.clone(),
            committed: false,
        };
        let pid = {
            let mut st = self.state.lock().unwrap();
            let log_dir = config::resolve_path(&st.config.daemon.log_dir, &st.config_dir);
            let ring_cap = st.config.daemon.log_buffer_lines;
            let mut mp = ManagedProgram::new(resolved.programs[0].clone(), log_dir, ring_cap);
            if !spec.register_only {
                mp.spawn();
            }
            let pid = mp.pid();
            st.programs.insert(program.clone(), mp);
            st.staged.insert(name.clone(), staged);
            pid
        };
        // A newly staged program may need its health task registered.
        self.sync_health_tasks();
        Ok(AdhocUpResult {
            app: name,
            program,
            config_path: target.display().to_string(),
            pid,
            estimate_secs: estimate,
            has_health: resolved.programs[0].health.is_some(),
            register_only: spec.register_only,
            residual: false,
            unchanged: false,
            committed: false,
        })
    }

    /// Upsert/regenerate an already-committed same-name app (control-plane
    /// spec: 同名重复提交走再生). Returns `None` when `name` is not a
    /// committed registration (the caller then stages a fresh one).
    ///
    /// - identical content → idempotent: no write, no restart, existing pid;
    /// - changed content → full regeneration: stop the old process, rewrite
    ///   the record and start the new definition (`--restart=no` writes only).
    fn adhoc_upsert(
        &self,
        spec: &AdhocSpec,
        name: &str,
        target: &Path,
        text: &str,
    ) -> std::result::Result<Option<AdhocUpResult>, AdhocUpError> {
        // The app must be an already-committed registration (its record is a
        // real generated file and the app is in `st.apps`). Anything else —
        // not yet registered, or a link/hard-link record to an external file —
        // is NOT an upsert target: the caller stages a fresh one (or the guard
        // below refuses to clobber an external file).
        if !target.is_file() {
            return Ok(None);
        }
        let (config, config_dir) = {
            let st = self.state.lock().unwrap();
            (st.config.clone(), st.config_dir.clone())
        };
        {
            let st = self.state.lock().unwrap();
            if !st.apps.iter().any(|a| a.name == name) {
                return Ok(None);
            }
        }
        let dir = registry::app_dir(&config, &config_dir);
        if let Err(e) = registry::guard_scaffold_overwrite(target, &dir, name) {
            return Err(AdhocUpError::Validation(format!("{e:#}")));
        }
        let old_text = match std::fs::read_to_string(target) {
            Ok(t) => t,
            Err(e) => {
                return Err(AdhocUpError::Validation(format!(
                    "cannot read the existing registration {}: {e}",
                    target.display()
                )));
            }
        };
        // Idempotent: identical content → reuse the running process.
        if old_text == text {
            let st = self.state.lock().unwrap();
            let pid = st.programs.get(name).and_then(|p| p.pid());
            return Ok(Some(AdhocUpResult {
                app: name.to_string(),
                program: name.to_string(),
                config_path: target.display().to_string(),
                pid,
                estimate_secs: 0.0,
                has_health: st
                    .programs
                    .get(name)
                    .map(|p| p.def.health.is_some())
                    .unwrap_or(false),
                register_only: spec.register_only,
                residual: false,
                unchanged: true,
                committed: true,
            }));
        }
        // Changed: full regeneration. Write the record atomically (placeholder
        // + rename), then rebuild the program; respawn per the new definition
        // unless the caller asked for register-only.
        let _placeholder = registry::stage_placeholder(&config, &config_dir, name, text)
            .map_err(|e| AdhocUpError::Stage(format!("{e:#}")))?;
        let raw: crate::config::AppRaw = match toml::from_str(text) {
            Ok(r) => r,
            Err(e) => {
                registry::discard_placeholder(&config, &config_dir, name);
                return Err(AdhocUpError::Validation(format!("{e}")));
            }
        };
        let resolved = match config::resolve_app(name, target, &raw, config.app_default.as_ref()) {
            Ok(r) => r,
            Err(e) => {
                registry::discard_placeholder(&config, &config_dir, name);
                return Err(AdhocUpError::Validation(format!("{e:#}")));
            }
        };
        // Land the record (rename the placeholder); keep the process untouched
        // until the new file is in place.
        match registry::commit_staged(&config, &config_dir, name, text) {
            Ok(_) => {}
            Err(e) => {
                registry::discard_placeholder(&config, &config_dir, name);
                return Err(AdhocUpError::Stage(format!("{e:#}")));
            }
        }
        let estimate = self.startup_estimate_secs(&resolved);
        let program = resolved.programs[0].name.clone();
        let pid = {
            let mut st = self.state.lock().unwrap();
            let log_dir = config::resolve_path(&st.config.daemon.log_dir, &st.config_dir);
            let ring_cap = st.config.daemon.log_buffer_lines;
            // Stop and drop the old process, then install the new definition.
            if let Some(mut old) = st.programs.remove(&program) {
                old.stop();
            }
            let mut mp = ManagedProgram::new(resolved.programs[0].clone(), log_dir, ring_cap);
            if !spec.register_only {
                mp.spawn();
            }
            let pid = mp.pid();
            st.programs.insert(program.clone(), mp);
            if !st.apps.iter().any(|a| a.name == name) {
                st.apps.push(AppRecord {
                    name: resolved.name.clone(),
                    path: resolved.path.clone(),
                    priority: resolved.priority,
                });
            }
            pid
        };
        self.sync_health_tasks();
        Ok(Some(AdhocUpResult {
            app: name.to_string(),
            program,
            config_path: target.display().to_string(),
            pid,
            estimate_secs: estimate,
            has_health: resolved.programs[0].health.is_some(),
            register_only: spec.register_only,
            residual: false,
            unchanged: false,
            committed: true,
        }))
    }

    /// Upper bound (seconds) a caller should wait for the startup judgement
    /// (D5): `startsecs` alone without a health check; otherwise
    /// `startsecs + health_start_period + health_interval × health_retries`.
    fn startup_estimate_secs(&self, resolved: &ResolvedApp) -> f64 {
        resolved
            .programs
            .iter()
            .map(|p| match &p.health {
                None => p.startsecs,
                Some(h) => p.startsecs + h.start_period + h.interval * f64::from(h.retries),
            })
            .fold(0.0_f64, f64::max)
    }

    /// `AdhocCommit`: rename the placeholder onto the record, drop the staged
    /// entry and add the app to `apps`. On a rename failure the process is
    /// kept running and a partial-failure result is returned (D4).
    fn cmd_adhoc_commit(&self, name: &str) -> Result<AdhocCommitResult> {
        let (config, config_dir, staged) = {
            let st = self.state.lock().unwrap();
            let Some(s) = st.staged.get(name) else {
                bail!("program {name:?} is not staged (nothing to commit)");
            };
            (st.config.clone(), st.config_dir.clone(), s.clone())
        };
        let pid = {
            let st = self.state.lock().unwrap();
            st.programs.get(&staged.program).and_then(|p| p.pid())
        };
        let outcome = registry::commit_staged(&config, &config_dir, name, &staged.text);
        let (partial, partial_reason, unchanged) = match outcome {
            Ok(o) => {
                // Persisted: clear the staged entry and register the app record.
                let mut st = self.state.lock().unwrap();
                st.staged.remove(name);
                if !st.apps.iter().any(|a| a.name == staged.app) {
                    st.apps.push(AppRecord {
                        name: staged.resolved.name.clone(),
                        path: staged.resolved.path.clone(),
                        priority: staged.resolved.priority,
                    });
                }
                (false, String::new(), o.unchanged())
            }
            Err(e) => {
                // Keep the process and the staged entry; mark it committed so a
                // later retry does not re-start it. Report partial failure.
                let mut st = self.state.lock().unwrap();
                if let Some(s) = st.staged.get_mut(name) {
                    s.committed = false;
                }
                (true, e.to_string(), false)
            }
        };
        Ok(AdhocCommitResult {
            app: staged.app.clone(),
            program: staged.program.clone(),
            config_path: staged.target.display().to_string(),
            pid,
            unchanged,
            partial,
            partial_reason,
            config_text: staged.text.clone(),
        })
    }

    /// `AdhocAbort`: stop and drop the program, delete the placeholder, clear
    /// the staged entry. Used when the startup judgement fails.
    fn cmd_adhoc_abort(&self, name: &str) -> Result<String> {
        let (config, config_dir, staged) = {
            let st = self.state.lock().unwrap();
            let Some(s) = st.staged.get(name) else {
                bail!("program {name:?} is not staged (nothing to abort)");
            };
            (st.config.clone(), st.config_dir.clone(), s.clone())
        };
        {
            let mut st = self.state.lock().unwrap();
            if let Some(mut p) = st.programs.remove(&staged.program) {
                p.stop();
            }
            st.staged.remove(name);
        }
        registry::discard_placeholder(&config, &config_dir, name);
        self.sync_health_tasks();
        Ok(format!("program {} aborted (not registered)", staged.program))
    }

    /// `reload` now only detects: rescan and publish the pending preview.
    /// The `[webui]` section never enters pending — it is hot-applied here:
    /// publish the new intent to the console manager and wait (bounded) for
    /// convergence. A bind failure degrades to a note (Ok), while an invalid
    /// `[webui]` section fails the whole reload earlier via `detect`
    /// (load-time validation), leaving the running console untouched.
    fn cmd_reload(&self) -> Result<String> {
        let doc = self.detect(true)?;
        let mut out = render_pending(&doc);
        let config_path = self.config_path.lock().unwrap().clone();
        let (new_config, _) = DaemonConfig::load_or_default(&config_path)
            .map_err(|e| anyhow::anyhow!("reload aborted, daemon config invalid: {e:#}"))?;
        let generation = self
            .webui
            .set_desired(WebuiIntent::from_config(&new_config));
        // Degraded convergence is logged by the webui manager; the note is
        // appended to the reply below for the CLI caller.
        let (_, note) = self.webui.wait_applied(generation, WEBUI_CONVERGE_TIMEOUT);
        out.push_str(&format!("\nwebui: {note}"));
        Ok(out)
    }

    /// `apply`: detect fresh, then act on the in-scope part of the diff.
    fn cmd_apply(&self, scope: &ApplyScope, restart: bool) -> Result<String> {
        let det = self.detect_details()?;
        let result = self.apply_detection(&det, scope, restart)?;
        let s = render_apply(&result);
        Ok(s)
    }

    /// Like [`Self::detect`] but returns the loaded definitions so they can
    /// be applied (same isolation rules: broken apps keep their old config
    /// and stay in `failed`).
    fn detect_details(&self) -> Result<Detection> {
        let config_path = self.config_path.lock().unwrap().clone();
        let (new_config, _) = DaemonConfig::load_or_default(&config_path)
            .map_err(|e| anyhow::anyhow!("apply aborted, daemon config invalid: {e:#}"))?;
        let config_dir = self.state.lock().unwrap().config_dir.clone();
        let defaults = new_config.app_default.clone();
        let listed = registry::list(&new_config, &config_dir)?;
        let mut failed: Vec<String> = Vec::new();
        let mut new_records: Vec<AppRecord> = Vec::new();
        let mut resolved: Vec<ResolvedApp> = Vec::new();
        for l in &listed {
            if let Some(reason) = &l.broken {
                failed.push(format!("app[{}] kept old config: {reason}", l.name));
                let st = self.state.lock().unwrap();
                if let Some(old) = st.apps.iter().find(|a| a.name == l.name) {
                    new_records.push(old.clone());
                }
                continue;
            }
            match registry::reload_app(&l.name, &l.path, defaults.as_ref()) {
                Ok(a) => {
                    new_records.push(AppRecord {
                        name: a.name.clone(),
                        path: a.path.clone(),
                        priority: a.priority,
                    });
                    resolved.push(a);
                }
                Err(e) => {
                    failed.push(format!("app[{}] kept old config: {e:#}", l.name));
                    let st = self.state.lock().unwrap();
                    if let Some(old) = st.apps.iter().find(|a| a.name == l.name) {
                        new_records.push(old.clone());
                    }
                }
            }
        }
        if let Err(e) = config::validate_all(&resolved) {
            bail!("apply aborted, running config unchanged: cross-app validation failed: {e}");
        }
        // Staged apps are known (D1): a scoped/full apply must not treat their
        // programs as dropped while the ad-hoc commit is still in flight.
        self.merge_staged(&mut resolved, &mut new_records);
        Ok(Detection {
            config: new_config,
            resolved,
            records: new_records,
            failed,
        })
    }

    /// Mutate the running state per the detection and scope. Reuses the
    /// reload rules: changed programs are stopped, redefined and — when
    /// they were alive (running/starting) or waiting out a crash backoff —
    /// respawned; programs the user stopped stay stopped.
    fn apply_detection(
        &self,
        det: &Detection,
        scope: &ApplyScope,
        restart: bool,
    ) -> Result<ApplyResult> {
        let new_config = &det.config;
        let resolved = &det.resolved;
        let new_records = &det.records;
        let mut result = ApplyResult {
            errors: det.failed.clone(),
            ..ApplyResult::default()
        };
        // Which programs existed before (for app add/remove bookkeeping).
        let prior_apps: Vec<String> = {
            let st = self.state.lock().unwrap();
            st.apps.iter().map(|a| a.name.clone()).collect()
        };

        {
            let mut st = self.state.lock().unwrap();
            let log_dir = config::resolve_path(&st.config.daemon.log_dir, &st.config_dir);
            let ring_cap = st.config.daemon.log_buffer_lines;

            // Apps no longer registered: stop and drop all their programs.
            let keep: HashSet<String> = new_records.iter().map(|a| a.name.clone()).collect();
            let dropped: Vec<String> = st
                .programs
                .values()
                .filter(|p| !keep.contains(&p.def.app))
                .map(|p| p.def.name.clone())
                .collect();
            for name in dropped {
                let in_scope = st
                    .programs
                    .get(&name)
                    .map(|p| scope.matches(&p.def.app, &p.def.name))
                    .unwrap_or(false);
                if !in_scope {
                    continue;
                }
                if let Some(mut p) = st.programs.remove(&name) {
                    info!("program[{name}] stopped (app unregistered)");
                    p.stop();
                    result.programs.push(ProgramAction {
                        app: p.def.app.clone(),
                        program: name.clone(),
                        changed: true,
                        action: "remove".into(),
                        result: "ok".into(),
                    });
                }
            }

            // Per-app program diff.
            for app in resolved {
                let new_defs: &Vec<ResolvedProgram> = &app.programs;
                let new_names: HashSet<&str> = new_defs.iter().map(|p| p.name.as_str()).collect();
                let old_names: Vec<String> = st
                    .programs
                    .values()
                    .filter(|p| p.def.app == app.name)
                    .map(|p| p.def.name.clone())
                    .collect();
                for old in old_names {
                    if !new_names.contains(old.as_str()) {
                        let in_scope = st
                            .programs
                            .get(&old)
                            .map(|p| scope.matches(&p.def.app, &p.def.name))
                            .unwrap_or(false);
                        if !in_scope {
                            continue;
                        }
                        if let Some(mut p) = st.programs.remove(&old) {
                            info!("program[{old}] removed by apply");
                            p.stop();
                            result.programs.push(ProgramAction {
                                app: p.def.app.clone(),
                                program: old.clone(),
                                changed: true,
                                action: "remove".into(),
                                result: "ok".into(),
                            });
                        }
                    }
                }
                for def in new_defs {
                    if !scope.matches(&app.name, &def.name) {
                        continue;
                    }
                    match st.programs.get(&def.name) {
                        Some(p) if p.def.hash == def.hash => {
                            // Unchanged definition. `--restart` restarts it
                            // unless the user stopped it; a program that
                            // crashed mid-retry (backoff) is pulled up.
                            if restart {
                                let s = p.state();
                                let user_stopped = p.user_stopped();
                                if user_stopped || s == ProgramState::Stopped {
                                    result.programs.push(ProgramAction {
                                        app: app.name.clone(),
                                        program: def.name.clone(),
                                        changed: false,
                                        action: "keep-stopped".into(),
                                        result: "ok".into(),
                                    });
                                } else {
                                    let p = st.programs.get_mut(&def.name).unwrap();
                                    if s == ProgramState::Backoff {
                                        p.stop();
                                    }
                                    p.spawn();
                                    result.programs.push(ProgramAction {
                                        app: app.name.clone(),
                                        program: def.name.clone(),
                                        changed: false,
                                        action: "restart".into(),
                                        result: "ok".into(),
                                    });
                                }
                            }
                            // Unchanged and no --restart: nothing happened,
                            // no entry (keeps "no changes" true).
                        }
                        Some(_) => {
                            // Changed definition: stop, rebuild; respawn only
                            // if it was alive (running/starting) or waiting
                            // out a crash backoff — never when the user
                            // stopped it.
                            let mut old = st.programs.remove(&def.name).unwrap();
                            let was_alive =
                                old.is_running() || matches!(old.state(), ProgramState::Backoff);
                            let keep_stopped = old.user_stopped() && !was_alive;
                            old.stop();
                            let mut np =
                                ManagedProgram::new(def.clone(), log_dir.clone(), ring_cap);
                            if was_alive {
                                np.spawn();
                            } else if keep_stopped {
                                // The user stopped this program; a redefinition
                                // must not resurrect it (start_eligible would
                                // otherwise autostart it again).
                                let _ = np.stop();
                            }
                            st.programs.insert(def.name.clone(), np);
                            result.programs.push(ProgramAction {
                                app: app.name.clone(),
                                program: def.name.clone(),
                                changed: true,
                                action: if was_alive {
                                    "update-and-restart".into()
                                } else {
                                    "update-only".into()
                                },
                                result: "ok".into(),
                            });
                        }
                        None => {
                            // New program: pull it up per its `restart` policy
                            // (no → only registered; unless-stopped → unless a
                            // user-stop record exists). `--restart` starts it
                            // regardless.
                            let mut np =
                                ManagedProgram::new(def.clone(), log_dir.clone(), ring_cap);
                            let started = if restart
                                || def.restart.pulls_up(st.user_stopped.contains(&def.name))
                            {
                                np.spawn();
                                true
                            } else {
                                false
                            };
                            st.programs.insert(def.name.clone(), np);
                            result.programs.push(ProgramAction {
                                app: app.name.clone(),
                                program: def.name.clone(),
                                changed: true,
                                action: if started { "start" } else { "none" }.into(),
                                result: "ok".into(),
                            });
                        }
                    }
                }
            }

            // Daemon fields that cannot hot-apply: the control endpoint is
            // bound once at startup and the listening socket is not rebindable
            // mid-flight.
            if st.config.daemon.socket != new_config.daemon.socket {
                result
                    .daemon_hints
                    .push("daemon socket changed: restart appkeeper to apply".into());
            }
            st.config = new_config.clone();
            // Persisted app records exclude staged (started-but-unpersisted)
            // apps: they were merged into the detection only so the `keep` set
            // does not drop their programs. Recording them here would leave a
            // phantom app after an abort (run-daemon-first D1).
            st.apps = new_records
                .iter()
                .filter(|a| !st.staged.contains_key(&a.name))
                .cloned()
                .collect();
        }

        // App add/remove bookkeeping for the result.
        let now_apps: HashSet<String> = new_records.iter().map(|a| a.name.clone()).collect();
        result.apps_removed = prior_apps
            .iter()
            .filter(|a| !now_apps.contains(*a))
            .cloned()
            .collect();
        result.apps_added = new_records
            .iter()
            .map(|a| a.name.clone())
            .filter(|a| !prior_apps.contains(a))
            .collect();

        // Summary line.
        let changed = result.programs.iter().filter(|p| p.changed).count();
        if result.programs.is_empty()
            && result.apps_added.is_empty()
            && result.apps_removed.is_empty()
        {
            result.summary = "no changes".into();
        } else {
            let restarted = result
                .programs
                .iter()
                .filter(|p| p.action.contains("restart") || p.action == "start")
                .count();
            result.summary =
                format!("applied: {changed} program change(s), {restarted} restart/start(s)");
        }
        self.sync_health_tasks();
        self.start_eligible();
        // Re-detect so the published pending reflects the post-apply truth
        // (a scoped apply may leave other apps still pending).
        let _ = self.detect(true);
        for e in &result.errors {
            error!("apply: {e}");
        }
        Ok(result)
    }

    // -- shutdown -----------------------------------------------------------

    /// Stop everything, highest priority first (reverse of startup order).
    pub fn shutdown_all(&self) {
        let order: Vec<String> = {
            let st = self.state.lock().unwrap();
            let mut apps = st.apps.clone();
            // Staged (started-but-unpersisted) apps are not in `st.apps` yet;
            // their programs must be stopped too, or the child would be
            // orphaned when the daemon exits (run-daemon-first D1).
            for s in st.staged.values() {
                if !apps.iter().any(|a| a.name == s.app) {
                    apps.push(AppRecord {
                        name: s.app.clone(),
                        path: s.target.clone(),
                        priority: s.resolved.priority,
                    });
                }
            }
            apps.sort_by_key(|a| std::cmp::Reverse((a.priority, a.name.clone())));
            apps.iter()
                .flat_map(|a| {
                    let mut names: Vec<String> = st
                        .programs
                        .values()
                        .filter(|p| p.def.app == a.name)
                        .map(|p| p.def.name.clone())
                        .collect();
                    names.sort();
                    names
                })
                .collect()
        };
        info!(
            "shutdown: stopping {} program(s) in reverse order",
            order.len()
        );
        let mut st = self.state.lock().unwrap();
        for name in order {
            if let Some(p) = st.programs.get_mut(&name) {
                p.stop();
            }
        }
    }

    // -- main loop ----------------------------------------------------------

    /// Consume commands, tick programs, until shutdown. Runs on the spawned
    /// supervisor thread.
    pub fn run(self: &Arc<Self>) {
        info!(
            "supervisor loop started (interval {:.1}s)",
            self.interval.as_secs_f64()
        );
        while !self.state.lock().unwrap().shutdown.load(Ordering::SeqCst) {
            while let Some(cmd) = self.pop_command() {
                self.execute(cmd);
            }
            if self.state.lock().unwrap().shutdown.load(Ordering::SeqCst) {
                break;
            }
            // Periodic detection: disk edits become pending without any
            // manual `reload`. The mtime+size fast path keeps this cheap.
            if let Err(e) = self.detect(false) {
                warn!("periodic detection skipped: {e:#}");
            }
            {
                let mut st = self.state.lock().unwrap();
                let now = Instant::now();
                for p in st.programs.values_mut() {
                    p.tick(now);
                }
            }
            // Re-evaluate dependencies each pass: a dependency that reached
            // running (or became permanently unavailable) unblocks (or
            // fatalizes) waiting programs.
            self.start_eligible();
            // Sleep in slices so commands and shutdown wake us promptly.
            let deadline = Instant::now() + self.interval;
            while Instant::now() < deadline {
                if self.state.lock().unwrap().shutdown.load(Ordering::SeqCst) {
                    break;
                }
                let guard = self.queue.lock().unwrap();
                let remaining = deadline.saturating_duration_since(Instant::now());
                let slice = remaining.min(Duration::from_millis(100));
                let (guard, _) = self.cv.wait_timeout(guard, slice).unwrap();
                let pending = !guard.is_empty();
                drop(guard);
                if pending {
                    break;
                }
            }
        }
        info!("shutdown requested, stopping all programs...");
        self.shutdown_all();
        info!("all programs stopped, appkeeper exits");
    }
}

// -- Web console lifecycle (config-driven-webui) ----------------------------

/// How long `reload` waits for the webui manager to converge on a new intent.
const WEBUI_CONVERGE_TIMEOUT: Duration = Duration::from_secs(5);
/// How long the manager waits for a running console to stop before detaching
/// it (the detached thread ends on its own; its port may linger briefly).
const WEBUI_STOP_WAIT: Duration = Duration::from_secs(3);
/// Manager poll slice: reaps dead instances while idle between intents.
const WEBUI_MANAGER_POLL: Duration = Duration::from_millis(500);
/// How long `start_webui` waits for the bind report before assuming success.
const WEBUI_BIND_WAIT: Duration = Duration::from_secs(5);

/// The desired web-console state, derived from the `[webui]` config section:
/// present = enabled at `listen`, absent = disabled (design D1: presence IS
/// the switch, there is no boolean).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct WebuiIntent {
    pub enabled: bool,
    pub listen: String,
    /// The console's own access-control token; empty = the console applies no
    /// application-layer credential and its isolation is whatever
    /// `webui.listen` grants (webui-api capability).
    pub auth_token: String,
}

impl WebuiIntent {
    pub fn disabled() -> Self {
        Self::default()
    }

    pub fn from_config(config: &DaemonConfig) -> Self {
        match config.webui_listen() {
            Some(listen) => Self {
                enabled: true,
                listen: listen.to_string(),
                auth_token: config.webui_auth_token().unwrap_or("").to_string(),
            },
            None => Self::disabled(),
        }
    }

    /// Does an instance bound to `listen` with this token already satisfy the
    /// intent? Rotating the token forces a rebind — the router holds it.
    fn serves(&self, listen: &str, auth_token: &str) -> bool {
        self.enabled && self.listen == listen && self.auth_token == auth_token
    }
}

/// Shared intent mailbox between the reload path (writer) and the resident
/// manager thread (reader/converger). Plain Mutex + Condvar on purpose: the
/// manager must be usable before any tokio runtime exists and must never be
/// tied to one (design D3). A generation counter makes the handshake
/// race-free: `set_desired` bumps it and `wait_applied` waits until the
/// manager reports exactly that generation.
#[derive(Default)]
struct WebuiShared {
    /// `None` = disabled intent at generation 0.
    desired: Option<(WebuiIntent, u64)>,
    /// Latest `(generation, ok, note)` reported by the manager.
    applied: Option<(u64, bool, String)>,
}

/// Handle for publishing the console intent and waiting for convergence.
/// Lives on `Supervisor` so the API threads (reload) and main.rs (startup)
/// share one instance with the manager thread.
pub struct WebuiControl {
    shared: Mutex<WebuiShared>,
    cv: Condvar,
}

impl WebuiControl {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            shared: Mutex::new(WebuiShared::default()),
            cv: Condvar::new(),
        })
    }

    /// Publish a new intent and return its generation (reload/startup path).
    pub fn set_desired(&self, intent: WebuiIntent) -> u64 {
        let mut sh = self.shared.lock().unwrap();
        let generation = sh.desired.as_ref().map(|(_, g)| *g).unwrap_or(0) + 1;
        sh.desired = Some((intent, generation));
        sh.applied = None;
        self.cv.notify_all();
        generation
    }

    /// Current intent + generation (manager thread).
    fn desired(&self) -> (WebuiIntent, u64) {
        let sh = self.shared.lock().unwrap();
        sh.desired.clone().unwrap_or((WebuiIntent::disabled(), 0))
    }

    /// Bounded wait until the manager reports `generation` applied. On timeout the
    /// console may still converge later; the caller surfaces the note.
    pub fn wait_applied(&self, generation: u64, timeout: Duration) -> (bool, String) {
        let mut sh = self.shared.lock().unwrap();
        let deadline = Instant::now() + timeout;
        loop {
            if let Some((g, ok, note)) = &sh.applied {
                if *g >= generation {
                    return (*ok, note.clone());
                }
            }
            let now = Instant::now();
            if now >= deadline {
                return (
                    false,
                    "webui change did not converge in time (the manager retries on the next reload)"
                        .to_string(),
                );
            }
            let (guard, _) = self
                .cv
                .wait_timeout(sh, deadline.saturating_duration_since(now))
                .unwrap();
            sh = guard;
        }
    }

    /// Manager thread: record the outcome for generation `generation`.
    fn mark_applied(&self, generation: u64, ok: bool, note: String) {
        let mut sh = self.shared.lock().unwrap();
        sh.applied = Some((generation, ok, note));
        self.cv.notify_all();
    }

    /// Manager thread: bounded wait for a newer intent. Wakes early on
    /// `set_desired` (it notifies the condvar); daemon shutdown is noticed
    /// by the loop's own poll at the latest.
    fn wait_for_change(&self, seen: u64, timeout: Duration) {
        let guard = self.shared.lock().unwrap();
        let current = guard.desired.as_ref().map(|(_, g)| *g).unwrap_or(0);
        if current > seen {
            return;
        }
        let _ = self.cv.wait_timeout(guard, timeout).unwrap();
    }
}

/// One live console instance owned by the manager thread.
struct RunningWebui {
    listen: String,
    /// The token the running router enforces; part of the identity, so a
    /// rotated token rebinds instead of leaving the old one in place.
    auth_token: String,
    stop: Arc<crate::web::WebuiLifecycle>,
    handle: Option<std::thread::JoinHandle<Result<()>>>,
}

impl RunningWebui {
    /// If the serve thread already exited (unexpected crash), take and join
    /// it. Returns true when an instance was reaped.
    fn reap_if_finished(&mut self) -> bool {
        let done = self
            .handle
            .as_ref()
            .map(|h| h.is_finished())
            .unwrap_or(true);
        if done {
            if let Some(h) = self.handle.take() {
                if let Err(e) = h.join() {
                    error!("webui console thread panicked: {e:?}");
                }
            }
            return true;
        }
        false
    }

    /// Ask the instance to stop and wait (bounded) for its thread. Returns
    /// false when it did not stop in time — the thread is detached and its
    /// port may stay bound a little longer; a same-port restart then fails
    /// the bind, which the manager reports non-fatally (design D5).
    fn stop_and_wait(&mut self) -> bool {
        self.stop.stop();
        let deadline = Instant::now() + WEBUI_STOP_WAIT;
        while Instant::now() < deadline {
            if self
                .handle
                .as_ref()
                .map(|h| h.is_finished())
                .unwrap_or(true)
            {
                if let Some(h) = self.handle.take() {
                    let _ = h.join();
                }
                return true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        warn!(
            "webui console on {} did not stop within {:?}; detaching it",
            self.listen, WEBUI_STOP_WAIT
        );
        self.handle = None;
        false
    }
}

/// Spawn one console instance and report `(instance, ok, note)`. Bind
/// failures are non-fatal (design D5): the daemon keeps running without the
/// console and the note explains how to retry.
fn start_webui(
    sup: &Arc<Supervisor>,
    listen: &str,
    auth_token: &str,
) -> (Option<RunningWebui>, bool, String) {
    let stop = crate::web::WebuiLifecycle::new();
    let (tx, rx) = std::sync::mpsc::channel();
    let sup2 = Arc::clone(sup);
    let stop2 = Arc::clone(&stop);
    let addr = listen.to_string();
    let token = auth_token.to_string();
    let handle = std::thread::Builder::new()
        .name("webui".into())
        .spawn(move || crate::web::serve(sup2, &addr, &token, stop2, Some(tx)))
        .expect("spawn webui thread");
    match rx.recv_timeout(WEBUI_BIND_WAIT) {
        Ok(Ok(bound)) => (
            Some(RunningWebui {
                listen: bound.clone(),
                auth_token: auth_token.to_string(),
                stop,
                handle: Some(handle),
            }),
            true,
            format!("webui console serving on http://{bound}"),
        ),
        Ok(Err(e)) => {
            let _ = handle.join();
            (
                None,
                false,
                format!(
                    "webui console unavailable: {e:#} (daemon continues without it; fix the config and reload to retry)"
                ),
            )
        }
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => (
            None,
            false,
            "webui console exited during startup".to_string(),
        ),
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
            // Slow bind (rare): assume it is coming; the manager reaps the
            // instance if the thread dies.
            (
                Some(RunningWebui {
                    listen: listen.to_string(),
                    auth_token: auth_token.to_string(),
                    stop,
                    handle: Some(handle),
                }),
                true,
                format!("webui console binding on {listen}"),
            )
        }
    }
}

/// Resident manager: converges the running console to the latest intent.
/// Runs on its own thread for the daemon's lifetime; on daemon shutdown it
/// stops the console first so the port is released promptly.
pub fn webui_manager_loop(sup: Arc<Supervisor>) {
    let mut running: Option<RunningWebui> = None;
    let mut converged_gen: u64 = 0;
    loop {
        if sup.state.lock().unwrap().shutdown.load(Ordering::SeqCst) {
            if let Some(r) = running.as_mut() {
                r.stop_and_wait();
            }
            return;
        }
        // Reap crashed instances. No auto-restart (design D5): the console
        // comes back on the next intent change (i.e. an explicit reload).
        let reaped = running
            .as_mut()
            .map(|r| (r.reap_if_finished(), r.listen.clone()))
            .filter(|(done, _)| *done)
            .map(|(_, listen)| listen);
        if let Some(listen) = reaped {
            error!(
                "webui console on {listen} exited unexpectedly; daemon continues without it (reload to retry)"
            );
            running = None;
        }
        let (desired, generation) = sup.webui.desired();
        if generation > converged_gen {
            let matches = running
                .as_ref()
                .map(|r| desired.serves(&r.listen, &r.auth_token))
                .unwrap_or(false);
            if matches {
                sup.webui.mark_applied(
                    generation,
                    true,
                    format!("webui console serving on http://{}", desired.listen),
                );
            } else {
                if let Some(old) = running.as_mut() {
                    old.stop_and_wait();
                }
                running = None;
                if desired.enabled {
                    info!("webui console starting on {}", desired.listen);
                    let (inst, ok, note) =
                        start_webui(&sup, &desired.listen, &desired.auth_token);
                    running = inst;
                    // Log degraded starts (bind failure, early exit) right
                    // here so both `run` startup and `reload` record it —
                    // the note alone is only visible to reload's reply.
                    if !ok {
                        error!("{note}");
                    }
                    sup.webui.mark_applied(generation, ok, note);
                } else {
                    info!("webui console disabled");
                    sup.webui
                        .mark_applied(generation, true, "webui console disabled".to_string());
                }
            }
            converged_gen = generation;
        }
        sup.webui.wait_for_change(generation, WEBUI_MANAGER_POLL);
    }
}

/// mtime+size of a config file, or None when it cannot be stat'ed.
fn file_stamp(path: &Path) -> Option<(std::time::SystemTime, u64)> {
    let m = std::fs::metadata(path).ok()?;
    Some((m.modified().ok()?, m.len()))
}

fn answer(reply: Option<Reply>, r: Result<String>) {
    if let Some(tx) = reply {
        let _ = tx.send(r);
    }
}

// -- startup judgement (run-daemon-first D3/D5) ------------------------------

/// Poll interval for the startup judgement. Runs on the HTTP connection
/// thread (never the supervision loop), so a sleep here blocks nothing.
pub const STARTUP_POLL: Duration = Duration::from_millis(50);

/// Outcome of waiting for a staged program to be judged started.
#[derive(Debug, Clone, PartialEq)]
pub enum StartupOutcome {
    /// `Running` and (if a health check exists) its first probe passed.
    Started { pid: Option<u32> },
    /// A terminal failure: the program died before `startsecs`, entered
    /// `fatal`, or was stopped. `phase` names the failed stage.
    Failed { phase: &'static str, reason: String },
    /// The estimate elapsed without a verdict.
    TimedOut { waited_secs: f64 },
}

/// Wait for the startup judgement of `program` (run-daemon-first D3): poll
/// its state until `Running && health_confirmed`, a terminal failure, or the
/// `estimate` budget elapses. Called by the HTTP connection thread — it never
/// touches the command queue and never blocks the supervision loop.
pub fn wait_startup(sup: &Supervisor, program: &str, estimate: Duration) -> StartupOutcome {
    wait_startup_with(sup, program, estimate, STARTUP_POLL)
}

/// Body of [`wait_startup`] with an injectable poll interval (tests).
fn wait_startup_with(
    sup: &Supervisor,
    program: &str,
    estimate: Duration,
    poll: Duration,
) -> StartupOutcome {
    let start = Instant::now();
    let deadline = start + estimate;
    loop {
        {
            let st = sup.state.lock().unwrap();
            match st.programs.get(program) {
                None => {
                    return StartupOutcome::Failed {
                        phase: "startup",
                        reason: format!("program {program:?} disappeared before it started"),
                    };
                }
                Some(p) => match p.state() {
                    ProgramState::Running if p.health_confirmed() => {
                        return StartupOutcome::Started { pid: p.pid() };
                    }
                    ProgramState::Running | ProgramState::Starting | ProgramState::Stopping => {}
                    ProgramState::Fatal => {
                        return StartupOutcome::Failed {
                            phase: "startup",
                            reason: p
                                .fatal_reason()
                                .map(String::from)
                                .unwrap_or_else(|| "program entered the fatal state".into()),
                        };
                    }
                    ProgramState::Exited => {
                        return StartupOutcome::Failed {
                            phase: "startup",
                            reason: p
                                .last_exit()
                                .map(|e| format!("program exited before startsecs: {e}"))
                                .unwrap_or_else(|| "program exited before startsecs".into()),
                        };
                    }
                    ProgramState::Backoff => {
                        // The start failed and a retry is scheduled: the first
                        // failed start IS the failure (process-management spec:
                        // 稳定运行前退出计入启动失败).
                        return StartupOutcome::Failed {
                            phase: "startup",
                            reason: p
                                .last_exit()
                                .map(|e| format!("program exited before startsecs: {e}"))
                                .unwrap_or_else(|| "program exited before startsecs".into()),
                        };
                    }
                    ProgramState::Stopped => {
                        return StartupOutcome::Failed {
                            phase: "startup",
                            reason: "program was stopped before it started".into(),
                        };
                    }
                },
            }
        }
        let now = Instant::now();
        if now >= deadline {
            return StartupOutcome::TimedOut {
                waited_secs: now.duration_since(start).as_secs_f64(),
            };
        }
        std::thread::sleep(poll.min(deadline.saturating_duration_since(now)));
    }
}

// -- human-readable rendering ---------------------------------------------

/// Render a pending preview: what would change if `apply` ran now.
pub fn render_pending(doc: &PendingDoc) -> String {
    if doc.is_empty() {
        return "no pending changes".into();
    }
    let mut out = Vec::new();
    if !doc.programs.is_empty() {
        out.push(format!("pending changes ({}):", doc.programs.len()));
        for p in &doc.programs {
            let state = if p.running { "running" } else { "stopped" };
            out.push(format!(
                "  {}.{} [{state}] config changed",
                p.app, p.program
            ));
        }
    }
    for a in &doc.apps_added {
        out.push(format!("  app[{a}] registered (new)"));
    }
    for a in &doc.apps_removed {
        out.push(format!("  app[{a}] unregistered"));
    }
    for h in &doc.daemon_hints {
        out.push(format!("  {h}"));
    }
    for e in &doc.errors {
        out.push(format!("  {e}"));
    }
    out.push("run `ak apply` to apply".into());
    out.join("\n")
}

/// Render an apply result as three plain groups: applied changes, restarts,
/// untouched — one line per program so what happened is obvious at a glance.
pub fn render_apply(r: &ApplyResult) -> String {
    let mut out = Vec::new();
    let applied: Vec<&ProgramAction> = r
        .programs
        .iter()
        .filter(|p| p.changed && p.action != "remove")
        .collect();
    let restarted: Vec<&ProgramAction> = r
        .programs
        .iter()
        .filter(|p| !p.changed && p.action.contains("restart"))
        .collect();
    let untouched: Vec<&ProgramAction> = r
        .programs
        .iter()
        .filter(|p| p.action == "none" || p.action == "keep-stopped")
        .collect();
    let removed: Vec<&ProgramAction> = r.programs.iter().filter(|p| p.action == "remove").collect();

    if r.programs.is_empty() && r.apps_added.is_empty() && r.apps_removed.is_empty() {
        out.push("no changes".into());
    }
    if !applied.is_empty() {
        out.push("changed:".into());
        for p in applied {
            out.push(format!("  {}.{} -> {}", p.app, p.program, p.action));
        }
    }
    if !restarted.is_empty() {
        out.push("restarted (--restart):".into());
        for p in restarted {
            out.push(format!("  {}.{}", p.app, p.program));
        }
    }
    if !removed.is_empty() {
        out.push("removed:".into());
        for p in removed {
            out.push(format!("  {}.{}", p.app, p.program));
        }
    }
    if !untouched.is_empty() {
        out.push("untouched:".into());
        for p in untouched {
            out.push(format!("  {}.{} ({})", p.app, p.program, p.action));
        }
    }
    for a in &r.apps_added {
        out.push(format!("app[{a}] added"));
    }
    for a in &r.apps_removed {
        out.push(format!("app[{a}] removed"));
    }
    for h in &r.daemon_hints {
        out.push(h.clone());
    }
    for e in &r.errors {
        out.push(e.clone());
    }
    out.push(r.summary.clone());
    out.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Write an app config, register it (link), build a supervisor with it
    /// loaded — the harness for detect/apply tests.
    fn unique_tag() -> u64 {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(0);
        SEQ.fetch_add(1, Ordering::Relaxed) ^ ((std::process::id() as u64) << 32)
    }

    /// Register `target` as `app_dir/<name>.toml` for tests, mirroring
    /// `registry::make_link` (symlink; hard-link fallback where symlinks
    /// need privileges, e.g. unprivileged Windows).
    fn link_for_test(target: &Path, link: &Path) {
        #[cfg(unix)]
        std::os::unix::fs::symlink(target, link).unwrap();
        #[cfg(windows)]
        {
            if std::os::windows::fs::symlink_file(target, link).is_err() {
                std::fs::hard_link(target, link).unwrap();
            }
        }
    }

    fn setup(app_name: &str, toml_text: &str) -> (Arc<Supervisor>, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("ak-test-apply-{}-{}", unique_tag(), app_name));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("app.d")).unwrap();
        std::fs::create_dir_all(dir.join("logs")).unwrap();
        let cfg_path = dir.join("daemon.toml");
        // Pin log_dir per test: the built-in default is a shared
        // /tmp/appkeeper/logs, where the persisted user-stop record
        // (state.json) would leak across concurrently running tests.
        std::fs::write(
            &cfg_path,
            format!(
                "[daemon]\nlog_dir = {:?}\n",
                dir.join("logs").to_string_lossy()
            ),
        )
        .unwrap();
        let app_file = dir.join("appkeeper.toml");
        std::fs::write(&app_file, toml_text).unwrap();
        link_for_test(
            &app_file,
            &dir.join("app.d").join(format!("{app_name}.toml")),
        );

        let (config, _) = DaemonConfig::load_or_default(&cfg_path).unwrap();
        let sup = Supervisor::new(config, &dir).unwrap();
        sup.set_config_path(&cfg_path);
        sup.bootstrap().unwrap();
        (sup, app_file)
    }

    /// Poll until `name` reaches `want`, ticking the program like the
    /// supervision loop would (tests have no run loop).
    fn wait_state(sup: &Supervisor, name: &str, want: ProgramState) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            let mut st = sup.state.lock().unwrap();
            if let Some(p) = st.programs.get_mut(name) {
                p.tick(Instant::now());
                if p.state() == want {
                    return;
                }
            }
            drop(st);
            std::thread::sleep(Duration::from_millis(20));
        }
        let st = sup.state.lock().unwrap();
        let got = st
            .programs
            .get(name)
            .map(|p| p.state())
            .unwrap_or(ProgramState::Fatal);
        panic!("program {name} never reached {want:?}, got {got:?}");
    }

    /// configuration: reload only detects — a modified file shows up in the
    /// pending preview while the program's state and pid are untouched.
    #[test]
    fn reload_detects_without_touching_programs() {
        let (sup, app_file) = setup(
            "demo",
            "[app]\nautostart = true\n[program.p]\ncommand = 'sleep 30'\nstartsecs = 0.0\n",
        );
        let mgr = spawn_manager(&sup);
        wait_state(&sup, "p", ProgramState::Running);
        let pid_before = sup.state.lock().unwrap().programs["p"].pid();

        std::fs::write(
            &app_file,
            "[app]\nautostart = true\n[program.p]\ncommand = 'sleep 60'\nstartsecs = 0.0\n",
        )
        .unwrap();
        let out = sup.cmd_reload().unwrap();
        assert!(
            out.contains("p"),
            "preview lists the changed program: {out}"
        );
        assert!(
            out.contains("webui: webui console disabled"),
            "reload always reports the webui outcome: {out}"
        );

        {
            let st = sup.state.lock().unwrap();
            assert_eq!(st.programs["p"].pid(), pid_before, "no restart on reload");
            let cmdline = format!(
                "{} {}",
                st.programs["p"].def.command,
                st.programs["p"].def.args.join(" ")
            );
            assert_eq!(cmdline, "sleep 30", "old definition still running");
            assert_eq!(st.pending.programs.len(), 1);
            assert!(st.pending.programs[0].running);
        }
        end_manager(&sup, mgr);
        let _ = sup.shutdown_all();
    }

    /// apply-workflow: apply with nothing pending is a no-op.
    #[test]
    fn apply_with_no_changes_is_idempotent() {
        let (sup, _) = setup(
            "demo",
            "[app]\nautostart = false\n[program.p]\ncommand = 'true'\nstartsecs = 0.0\n",
        );
        let out = sup.cmd_apply(&ApplyScope::All, false).unwrap();
        assert!(out.contains("no changes"), "{out}");
        {
            let st = sup.state.lock().unwrap();
            assert!(st.pending.is_empty());
        }
        let _ = sup.shutdown_all();
    }

    /// apply-workflow: a changed running program stops, rebuilds, restarts.
    #[test]
    fn apply_updates_and_restarts_running_program() {
        let (sup, app_file) = setup(
            "demo",
            "[app]\nautostart = true\n[program.p]\ncommand = 'sleep 30'\nstartsecs = 0.0\n",
        );
        wait_state(&sup, "p", ProgramState::Running);
        std::fs::write(
            &app_file,
            "[app]\nautostart = true\n[program.p]\ncommand = 'sleep 60'\nstartsecs = 0.0\n",
        )
        .unwrap();

        let out = sup.cmd_apply(&ApplyScope::All, false).unwrap();
        assert!(out.contains("update-and-restart"), "{out}");
        wait_state(&sup, "p", ProgramState::Running);
        {
            let st = sup.state.lock().unwrap();
            let cmdline = format!(
                "{} {}",
                st.programs["p"].def.command,
                st.programs["p"].def.args.join(" ")
            );
            assert_eq!(cmdline, "sleep 60");
            assert!(st.pending.is_empty(), "pending cleared after full apply");
        }
        let _ = sup.shutdown_all();
    }

    /// apply-workflow: a changed but user-stopped program is redefined and
    /// stays stopped.
    #[test]
    fn apply_redefines_but_keeps_user_stopped() {
        let (sup, app_file) = setup(
            "demo",
            "[app]\nautostart = true\n[program.p]\ncommand = 'sleep 30'\nstartsecs = 0.0\n",
        );
        wait_state(&sup, "p", ProgramState::Running);
        {
            let mut st = sup.state.lock().unwrap();
            st.programs.get_mut("p").unwrap().stop();
        }
        wait_state(&sup, "p", ProgramState::Stopped);
        std::fs::write(
            &app_file,
            "[app]\nautostart = true\n[program.p]\ncommand = 'sleep 60'\nstartsecs = 0.0\n",
        )
        .unwrap();

        let out = sup.cmd_apply(&ApplyScope::All, false).unwrap();
        assert!(out.contains("update-only"), "{out}");
        {
            let st = sup.state.lock().unwrap();
            let cmdline = format!(
                "{} {}",
                st.programs["p"].def.command,
                st.programs["p"].def.args.join(" ")
            );
            assert_eq!(cmdline, "sleep 60", "definition replaced");
            assert_eq!(st.programs["p"].state(), ProgramState::Stopped);
            assert!(st.programs["p"].pid().is_none());
        }
        let _ = sup.shutdown_all();
    }

    /// apply-workflow: --restart restarts an unchanged running program but
    /// never pulls up one the user stopped.
    #[test]
    fn apply_restart_flag_respects_user_stops() {
        let (sup, _) = setup(
            "demo",
            "[app]\nautostart = true\n[program.up]\ncommand = 'sleep 30'\nstartsecs = 0.0\n\
             [program.down]\ncommand = 'sleep 30'\nstartsecs = 0.0\n",
        );
        wait_state(&sup, "up", ProgramState::Running);
        wait_state(&sup, "down", ProgramState::Running);
        let pid_up_before = sup.state.lock().unwrap().programs["up"].pid();
        {
            let mut st = sup.state.lock().unwrap();
            st.programs.get_mut("down").unwrap().stop();
        }
        wait_state(&sup, "down", ProgramState::Stopped);

        let out = sup.cmd_apply(&ApplyScope::All, true).unwrap();
        assert!(out.contains("restarted"), "{out}");
        wait_state(&sup, "up", ProgramState::Running);

        {
            let st = sup.state.lock().unwrap();
            assert_ne!(st.programs["up"].pid(), pid_up_before, "up was restarted");
            assert_eq!(
                st.programs["down"].state(),
                ProgramState::Stopped,
                "user-stopped stays down"
            );
        }
        assert!(
            out.contains("keep-stopped"),
            "result distinguishes the skipped one: {out}"
        );
        let _ = sup.shutdown_all();
    }

    /// apply-workflow: --restart pulls up a program waiting in backoff (it
    /// crashed, the user never stopped it).
    #[test]
    fn apply_restart_flag_pulls_up_backoff_program() {
        let (sup, _) = setup(
            "demo",
            "[app]\nautostart = false\n[program.crashy]\ncommand = 'false'\nstartsecs = 0.0\nautorestart = 'always'\nrestart_backoff = 100.0\n",
        );
        // Start manually, let it die, land in backoff.
        {
            let mut st = sup.state.lock().unwrap();
            st.programs.get_mut("crashy").unwrap().spawn();
        }
        for _ in 0..100 {
            let st = sup.state.lock().unwrap();
            if st.programs["crashy"].state() == ProgramState::Backoff {
                break;
            }
            drop(st);
            std::thread::sleep(Duration::from_millis(20));
        }
        wait_state(&sup, "crashy", ProgramState::Backoff);

        let out = sup.cmd_apply(&ApplyScope::All, true).unwrap();
        // It restarts into starting/backoff again immediately.
        {
            let st = sup.state.lock().unwrap();
            let s = st.programs["crashy"].state();
            assert!(
                matches!(s, ProgramState::Starting | ProgramState::Backoff),
                "pulled up out of backoff, got {s:?}"
            );
        }
        assert!(out.contains("crashy"), "{out}");
        let _ = sup.shutdown_all();
    }

    /// apply-workflow: a scoped apply leaves other apps' pending intact.
    #[test]
    fn scoped_apply_does_not_touch_other_apps() {
        let dir = std::env::temp_dir().join(format!("ak-test-scope-{}", unique_tag()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("app.d")).unwrap();
        std::fs::create_dir_all(dir.join("logs")).unwrap();
        let cfg_path = dir.join("daemon.toml");
        // Pin log_dir per test: the shared default would let the persisted
        // user-stop record (state.json) leak across concurrent tests.
        std::fs::write(
            &cfg_path,
            format!(
                "[daemon]\nlog_dir = {:?}\n",
                dir.join("logs").to_string_lossy()
            ),
        )
        .unwrap();
        let a_file = dir.join("a.toml");
        let b_file = dir.join("b.toml");
        std::fs::write(
            &a_file,
            "[app]\nautostart = true\n[program.a]\ncommand = 'sleep 30'\nstartsecs = 0.0\n",
        )
        .unwrap();
        std::fs::write(
            &b_file,
            "[app]\nautostart = true\n[program.b]\ncommand = 'sleep 30'\nstartsecs = 0.0\n",
        )
        .unwrap();
        link_for_test(&a_file, &dir.join("app.d").join("a.toml"));
        link_for_test(&b_file, &dir.join("app.d").join("b.toml"));

        let (config, _) = DaemonConfig::load_or_default(&cfg_path).unwrap();
        let sup = Supervisor::new(config, &dir).unwrap();
        sup.set_config_path(&cfg_path);
        sup.bootstrap().unwrap();
        wait_state(&sup, "a", ProgramState::Running);
        wait_state(&sup, "b", ProgramState::Running);
        let b_pid = sup.state.lock().unwrap().programs["b"].pid();

        // Change both on disk, apply only app a.
        std::fs::write(
            &a_file,
            "[app]\nautostart = true\n[program.a]\ncommand = 'sleep 60'\nstartsecs = 0.0\n",
        )
        .unwrap();
        std::fs::write(
            &b_file,
            "[app]\nautostart = true\n[program.b]\ncommand = 'sleep 90'\nstartsecs = 0.0\n",
        )
        .unwrap();
        let out = sup.cmd_apply(&ApplyScope::App("a".into()), false).unwrap();
        assert!(out.contains("update-and-restart"), "{out}");
        wait_state(&sup, "a", ProgramState::Running);

        {
            let st = sup.state.lock().unwrap();
            let cmd = |n: &str| {
                let p = &st.programs[n];
                format!("{} {}", p.def.command, p.def.args.join(" "))
            };
            assert_eq!(cmd("a"), "sleep 60");
            assert_eq!(cmd("b"), "sleep 30", "b untouched");
            assert_eq!(st.programs["b"].pid(), b_pid, "b never restarted");
            assert_eq!(
                st.pending.programs.len(),
                1,
                "b still pending: {:?}",
                st.pending.programs
            );
        }
        let _ = sup.shutdown_all();
    }

    /// configuration: a broken app file is isolated — no pending for it, the
    /// rest still detected.
    #[test]
    fn detect_isolates_broken_app_file() {
        let (sup, app_file) = setup(
            "demo",
            "[app]\nautostart = true\n[program.p]\ncommand = 'sleep 30'\nstartsecs = 0.0\n",
        );
        wait_state(&sup, "p", ProgramState::Running);
        std::fs::write(&app_file, "not [ valid toml").unwrap();
        let doc = sup.detect(true).unwrap();
        assert!(!doc.errors.is_empty(), "load failure reported: {doc:?}");
        assert!(
            doc.programs.is_empty(),
            "broken app contributes no pending: {doc:?}"
        );
        {
            let st = sup.state.lock().unwrap();
            let cmdline = format!(
                "{} {}",
                st.programs["p"].def.command,
                st.programs["p"].def.args.join(" ")
            );
            assert_eq!(cmdline, "sleep 30", "old definition kept");
        }
        let _ = sup.shutdown_all();
    }

    /// apply-workflow: a newly registered autostart program starts on apply.
    #[test]
    fn apply_starts_newly_registered_app() {
        let (sup, app_file) = setup(
            "demo",
            "[app]\nautostart = true\n[program.p]\ncommand = 'sleep 30'\nstartsecs = 0.0\n",
        );
        let dir = app_file.parent().unwrap().to_path_buf();
        // Add a second program to the same app file (a "new" definition).
        std::fs::write(&app_file, "[app]\nautostart = true\n[program.p]\ncommand = 'sleep 30'\nstartsecs = 0.0\n[program.q]\ncommand = 'sleep 30'\nstartsecs = 0.0\n").unwrap();
        let out = sup.cmd_apply(&ApplyScope::All, false).unwrap();
        assert!(out.contains("q"), "{out}");
        wait_state(&sup, "q", ProgramState::Running);
        let _ = dir;
        let _ = sup.shutdown_all();
    }

    /// apply-workflow: removing a program from the file stops and drops it.
    #[test]
    fn apply_removes_dropped_program() {
        let (sup, app_file) = setup(
            "demo",
            "[app]\nautostart = true\n[program.p]\ncommand = 'sleep 30'\nstartsecs = 0.0\n[program.q]\ncommand = 'sleep 30'\nstartsecs = 0.0\n",
        );
        wait_state(&sup, "q", ProgramState::Running);
        std::fs::write(
            &app_file,
            "[app]\nautostart = true\n[program.p]\ncommand = 'sleep 30'\nstartsecs = 0.0\n",
        )
        .unwrap();
        let out = sup.cmd_apply(&ApplyScope::All, false).unwrap();
        assert!(out.contains("removed"), "{out}");
        {
            let st = sup.state.lock().unwrap();
            assert!(!st.programs.contains_key("q"));
        }
        let _ = sup.shutdown_all();
    }

    /// apply-workflow: cross-app validation failure aborts with no pending.
    #[test]
    fn detect_refuses_cross_app_conflict() {
        let dir = std::env::temp_dir().join(format!("ak-test-conflict-{}", unique_tag()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("app.d")).unwrap();
        std::fs::create_dir_all(dir.join("logs")).unwrap();
        let cfg_path = dir.join("daemon.toml");
        // Pin log_dir per test: the shared default would let the persisted
        // user-stop record (state.json) leak across concurrent tests.
        std::fs::write(
            &cfg_path,
            format!(
                "[daemon]\nlog_dir = {:?}\n",
                dir.join("logs").to_string_lossy()
            ),
        )
        .unwrap();
        let a_file = dir.join("a.toml");
        let b_file = dir.join("b.toml");
        std::fs::write(&a_file, "[program.dup]\ncommand = 'true'\n").unwrap();
        std::fs::write(&b_file, "[program.dup]\ncommand = 'true'\n").unwrap();
        link_for_test(&a_file, &dir.join("app.d").join("a.toml"));
        link_for_test(&b_file, &dir.join("app.d").join("b.toml"));

        let (config, _) = DaemonConfig::load_or_default(&cfg_path).unwrap();
        let sup = Supervisor::new(config, &dir).unwrap();
        sup.set_config_path(&cfg_path);
        sup.bootstrap().unwrap(); // bootstrap reports, does not fail
        assert!(sup.detect(true).is_err(), "conflict refused");
        {
            let st = sup.state.lock().unwrap();
            assert!(st.pending.is_empty(), "no pending published on conflict");
        }
        let _ = sup.shutdown_all();
    }

    /// apply-workflow: periodic detect picks up a disk edit without any
    /// command; the mtime fast path skips re-parsing unchanged files.
    #[test]
    fn periodic_detect_publishes_pending() {
        let (sup, app_file) = setup(
            "demo",
            "[app]\nautostart = true\n[program.p]\ncommand = 'sleep 30'\nstartsecs = 0.0\n",
        );
        wait_state(&sup, "p", ProgramState::Running);
        std::fs::write(
            &app_file,
            "[app]\nautostart = true\n[program.p]\ncommand = 'sleep 60'\nstartsecs = 0.0\n",
        )
        .unwrap();
        sup.detect(false).unwrap();
        {
            let st = sup.state.lock().unwrap();
            assert_eq!(st.pending.programs.len(), 1);
            assert_eq!(st.pending.programs[0].program, "p");
        }
        // Fast path: a second pass with no further edits is stable.
        sup.detect(false).unwrap();
        {
            let st = sup.state.lock().unwrap();
            assert_eq!(
                st.pending.programs.len(),
                1,
                "unchanged file re-detected from cache"
            );
        }
        let _ = sup.shutdown_all();
    }

    /// apply-workflow: render distinguishes changed / restarted / untouched.
    #[test]
    fn render_apply_groups_by_action() {
        let r = ApplyResult {
            programs: vec![
                ProgramAction {
                    app: "a".into(),
                    program: "x".into(),
                    changed: true,
                    action: "update-and-restart".into(),
                    result: "ok".into(),
                },
                ProgramAction {
                    app: "a".into(),
                    program: "y".into(),
                    changed: false,
                    action: "restart".into(),
                    result: "ok".into(),
                },
                ProgramAction {
                    app: "b".into(),
                    program: "z".into(),
                    changed: false,
                    action: "keep-stopped".into(),
                    result: "ok".into(),
                },
            ],
            apps_added: vec!["c".into()],
            apps_removed: vec![],
            daemon_hints: vec![],
            errors: vec![],
            summary: "applied".into(),
        };
        let out = render_apply(&r);
        assert!(out.contains("changed:") && out.contains("a.x -> update-and-restart"));
        assert!(out.contains("restarted (--restart):") && out.contains("a.y"));
        assert!(out.contains("untouched:") && out.contains("b.z (keep-stopped)"));
        assert!(out.contains("app[c] added"));
    }

    #[test]
    fn apply_scope_matching() {
        assert!(ApplyScope::All.matches("a", "x"));
        assert!(ApplyScope::App("a".into()).matches("a", "x"));
        assert!(!ApplyScope::App("a".into()).matches("b", "x"));
        assert!(ApplyScope::Program("a".into(), "x".into()).matches("a", "x"));
        assert!(!ApplyScope::Program("a".into(), "x".into()).matches("a", "y"));
    }

    /// apply-workflow: the literal `all` resolves to the whole-registry
    /// scope in every request shape; real app/program names still scope.
    #[test]
    fn apply_scope_all_keyword_and_validation() {
        let (sup, _app_file) = setup(
            "demo",
            "[app]\nautostart = false\n[program.p]\ncommand = 'sleep 30'\nstartsecs = 0.0\n",
        );
        // `all` is the reserved keyword; a program part has no meaning.
        assert!(
            resolve_apply_scope(&sup, Some("all"), Some("p"))
                .err()
                .is_some_and(|e| e.contains("full-scope")),
            "apply all <program> must be rejected"
        );
        assert!(matches!(
            resolve_apply_scope(&sup, None, None).unwrap(),
            ApplyScope::All
        ));
        // Real names scope down.
        assert!(
            matches!(
                resolve_apply_scope(&sup, Some("demo"), None).unwrap(),
                ApplyScope::App(a) if a == "demo"
            ),
            "app scope"
        );
        assert!(
            matches!(
                resolve_apply_scope(&sup, Some("demo"), Some("p")).unwrap(),
                ApplyScope::Program(a, p) if a == "demo" && p == "p"
            ),
            "program scope"
        );
        // Validation still rejects unknown names.
        assert!(resolve_apply_scope(&sup, Some("ghost"), None).is_err());
        assert!(resolve_apply_scope(&sup, Some("demo"), Some("ghost")).is_err());
        let _ = sup.shutdown_all();
    }

    /// A freshly registered app that is only pending (on disk, not applied
    /// into the daemon state yet) is still a valid app-scope target — the
    /// add → `apply <app>` flow (app-registry: --apply 在线一步生效).
    #[test]
    fn apply_scope_accepts_pending_new_app() {
        let (sup, app_file) = setup(
            "demo",
            "[app]\nautostart = false\n[program.p]\ncommand = 'sleep 30'\nstartsecs = 0.0\n",
        );
        // Register a second app on disk without any reload/apply.
        let dir = app_file.parent().unwrap();
        let other = dir.join("other.toml");
        std::fs::write(
            &other,
            "[app]\nautostart = false\n[program.q]\ncommand = 'sleep 30'\n",
        )
        .unwrap();
        link_for_test(&other, &dir.join("app.d").join("other.toml"));
        {
            let st = sup.state.lock().unwrap();
            assert!(
                !st.apps.iter().any(|a| a.name == "other"),
                "other is not loaded into state yet"
            );
        }
        assert!(
            matches!(
                resolve_apply_scope(&sup, Some("other"), None).unwrap(),
                ApplyScope::App(a) if a == "other"
            ),
            "pending app is a valid apply target"
        );
        // Broken pending apps are not valid targets either.
        std::fs::write(&other, "not [ valid toml").unwrap();
        assert!(resolve_apply_scope(&sup, Some("other"), None).is_err());
        let _ = sup.shutdown_all();
    }

    // -- app fan-out --------------------------------------------------------

    fn fanout(sup: &Supervisor, kind: FanoutKind, app: &str) -> FanoutResult {
        let raw = sup
            .cmd_app_fanout(kind, app)
            .unwrap_or_else(|e| panic!("fanout {kind:?} {app} failed: {e:#}"));
        serde_json::from_str(&raw).expect("fan-out result is JSON")
    }

    fn names_of(fr: &FanoutResult) -> Vec<&str> {
        fr.programs.iter().map(|s| s.program.as_str()).collect()
    }

    /// actions: the fan-out start expands in dependency order, starts every
    /// program with the existing per-program logic (clearing user_stopped),
    /// and reports one line per program.
    #[test]
    fn app_fanout_start_orders_and_covers_user_stopped() {
        let (sup, _) = setup(
            "demo",
            "[app]\nautostart = false\n\
             [program.a]\ncommand = 'sleep 30'\nstartsecs = 0.0\n\
             [program.b]\ncommand = 'sleep 30'\nstartsecs = 0.0\ndepends_on = ['a']\n\
             [program.c]\ncommand = 'sleep 30'\nstartsecs = 0.0\ndepends_on = ['b']\n",
        );
        let fr = fanout(&sup, FanoutKind::Start, "demo");
        assert_eq!(names_of(&fr), ["a", "b", "c"], "dependency order");
        for n in ["a", "b", "c"] {
            wait_state(&sup, n, ProgramState::Running);
        }
        assert!(fr.programs.iter().all(|s| !s.result.contains("error")));

        // The user stopped b; a fan-out start pulls it up again and clears
        // the marker (spawn clears user_stopped).
        {
            let mut st = sup.state.lock().unwrap();
            st.programs.get_mut("b").unwrap().stop();
        }
        wait_state(&sup, "b", ProgramState::Stopped);
        assert!(sup.state.lock().unwrap().programs["b"].user_stopped());

        let fr = fanout(&sup, FanoutKind::Start, "demo");
        assert_eq!(names_of(&fr), ["a", "b", "c"]);
        wait_state(&sup, "b", ProgramState::Running);
        {
            let st = sup.state.lock().unwrap();
            assert!(
                !st.programs["b"].user_stopped(),
                "fan-out start clears user_stopped"
            );
        }
        // a/c were already running: recorded, not an error.
        let b_res = &fr.programs[0].result;
        assert!(b_res.contains("already running"), "a: {b_res}");
        assert!(
            !fr.programs[1].result.contains("error"),
            "b: {}",
            fr.programs[1].result
        );
        let _ = sup.shutdown_all();
    }

    /// actions: fan-out stop runs in the reverse of the start order;
    /// restart bounces every program (stop+start) in start order.
    #[test]
    fn app_fanout_stop_is_reverse_and_restart_bounces() {
        let (sup, _) = setup(
            "demo",
            "[app]\nautostart = false\n\
             [program.a]\ncommand = 'sleep 30'\nstartsecs = 0.0\n\
             [program.b]\ncommand = 'sleep 30'\nstartsecs = 0.0\ndepends_on = ['a']\n\
             [program.c]\ncommand = 'sleep 30'\nstartsecs = 0.0\ndepends_on = ['b']\n",
        );
        fanout(&sup, FanoutKind::Start, "demo");
        for n in ["a", "b", "c"] {
            wait_state(&sup, n, ProgramState::Running);
        }
        let pids: Vec<Option<u32>> = {
            let st = sup.state.lock().unwrap();
            ["a", "b", "c"]
                .iter()
                .map(|n| st.programs[*n].pid())
                .collect()
        };

        let fr = fanout(&sup, FanoutKind::Stop, "demo");
        assert_eq!(names_of(&fr), ["c", "b", "a"], "stop is the reverse order");
        for n in ["a", "b", "c"] {
            wait_state(&sup, n, ProgramState::Stopped);
        }

        let fr = fanout(&sup, FanoutKind::Restart, "demo");
        assert_eq!(
            names_of(&fr),
            ["a", "b", "c"],
            "restart walks the start order"
        );
        for (i, n) in ["a", "b", "c"].iter().enumerate() {
            wait_state(&sup, n, ProgramState::Running);
            let st = sup.state.lock().unwrap();
            assert_ne!(
                st.programs[*n].pid(),
                pids[i],
                "{n} was replaced by the restart"
            );
        }
        let _ = sup.shutdown_all();
    }

    /// actions: unknown apps and (defensively) apps without programs are
    /// error paths, never silent no-ops.
    #[test]
    fn app_fanout_unknown_and_empty_apps() {
        let (sup, _) = setup(
            "demo",
            "[app]\nautostart = false\n[program.p]\ncommand = 'sleep 30'\nstartsecs = 0.0\n",
        );
        {
            let mut st = sup.state.lock().unwrap();
            st.apps.push(AppRecord {
                name: "hollow".into(),
                path: PathBuf::new(),
                priority: 0,
            });
        }
        let e = sup
            .cmd_app_fanout(FanoutKind::Start, "ghost")
            .err()
            .expect("unknown app must fail")
            .to_string();
        assert!(e.contains("unknown app"), "{e}");
        let e = sup
            .cmd_app_fanout(FanoutKind::Stop, "hollow")
            .err()
            .expect("app without programs must fail")
            .to_string();
        assert!(e.contains("no programs"), "{e}");
        let _ = sup.shutdown_all();
    }

    /// actions: signal reaches the child without touching the state machine;
    /// a stopped program has nothing to signal (SignalError branches).
    #[test]
    #[cfg(unix)]
    fn signal_delivery_and_error_branches() {
        let (sup, _) = setup(
            "demo",
            "[app]\nautostart = false\n[program.p]\ncommand = 'sleep 30'\nstartsecs = 0.0\nautorestart = 'never'\n",
        );
        sup.cmd_start("p").unwrap();
        wait_state(&sup, "p", ProgramState::Running);
        let pid = sup.state.lock().unwrap().programs["p"].pid();

        let out = signal_program(&sup, "p", crate::platform::Signal::Usr1).unwrap();
        assert!(out.contains("delivered"), "{out}");
        // Immediately after delivery the state machine is untouched — the
        // program dies on its own and the next tick observes the exit.
        assert_eq!(
            sup.state.lock().unwrap().programs["p"].state(),
            ProgramState::Running,
            "signal must not change the state machine"
        );
        wait_state(&sup, "p", ProgramState::Exited);

        // Not running: nothing to deliver.
        let e = signal_program(&sup, "p", crate::platform::Signal::Term)
            .err()
            .unwrap();
        assert_eq!(e, SignalError::NotRunning);
        assert_eq!(e.http_status(), 409);
        // Unknown program.
        let e = signal_program(&sup, "ghost", crate::platform::Signal::Term)
            .err()
            .unwrap();
        assert_eq!(e, SignalError::UnknownProgram);
        assert_eq!(e.http_status(), 404);
        let _ = pid;
        let _ = sup.shutdown_all();
    }

    // -- web console lifecycle (config-driven-webui) ------------------------

    /// Spawn the resident manager thread like run_daemon does. Tests end it
    /// with [`end_manager`].
    fn spawn_manager(sup: &Arc<Supervisor>) -> std::thread::JoinHandle<()> {
        let sup2 = Arc::clone(sup);
        std::thread::Builder::new()
            .name("webui-manager-test".into())
            .spawn(move || webui_manager_loop(sup2))
            .unwrap()
    }

    fn free_port() -> u16 {
        std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port()
    }

    /// Flip the daemon shutdown flag so the manager stops any console and
    /// returns, then join it.
    fn end_manager(sup: &Supervisor, mgr: std::thread::JoinHandle<()>) {
        sup.state
            .lock()
            .unwrap()
            .shutdown
            .store(true, Ordering::SeqCst);
        mgr.join().unwrap();
    }

    fn wait_applied_ok(sup: &Supervisor, generation: u64) -> String {
        let (ok, note) = sup.webui.wait_applied(generation, Duration::from_secs(10));
        assert!(ok, "manager converged: {note}");
        note
    }

    /// manager: an enabled intent starts the console, a disabled one stops
    /// it and closes the port (reload 动态开关的服务端机制).
    #[test]
    fn webui_manager_follows_intent_on_and_off() {
        let (sup, _) = setup(
            "demo",
            "[app]\nautostart = false\n[program.p]\ncommand = 'true'\nstartsecs = 0.0\n",
        );
        let listen = format!("127.0.0.1:{}", free_port());
        let mgr = spawn_manager(&sup);

        let generation = sup.webui.set_desired(WebuiIntent {
            enabled: true,
            listen: listen.clone(),
            ..Default::default()
        });
        let note = wait_applied_ok(&sup, generation);
        assert!(
            note.contains("serving on http://"),
            "note names the serving URL: {note}"
        );
        assert!(
            std::net::TcpStream::connect(&listen).is_ok(),
            "console reachable once enabled"
        );

        let generation = sup.webui.set_desired(WebuiIntent::disabled());
        let note = wait_applied_ok(&sup, generation);
        assert_eq!(note, "webui console disabled");
        std::thread::sleep(Duration::from_millis(150));
        assert!(
            std::net::TcpStream::connect(&listen).is_err(),
            "console port closed after disable"
        );
        end_manager(&sup, mgr);
        let _ = sup.shutdown_all();
    }

    /// manager: a listen change stops the old instance and binds the new
    /// address (reload 换址重绑).
    #[test]
    fn webui_manager_rebinds_on_listen_change() {
        let (sup, _) = setup(
            "demo",
            "[app]\nautostart = false\n[program.p]\ncommand = 'true'\nstartsecs = 0.0\n",
        );
        let old = format!("127.0.0.1:{}", free_port());
        let new = format!("127.0.0.1:{}", free_port());
        let mgr = spawn_manager(&sup);

        let generation = sup.webui.set_desired(WebuiIntent {
            enabled: true,
            listen: old.clone(),
            ..Default::default()
        });
        wait_applied_ok(&sup, generation);
        assert!(std::net::TcpStream::connect(&old).is_ok());

        let generation = sup.webui.set_desired(WebuiIntent {
            enabled: true,
            listen: new.clone(),
            ..Default::default()
        });
        wait_applied_ok(&sup, generation);
        std::thread::sleep(Duration::from_millis(150));
        assert!(
            std::net::TcpStream::connect(&old).is_err(),
            "old port released after rebind"
        );
        assert!(
            std::net::TcpStream::connect(&new).is_ok(),
            "new port serving after rebind"
        );
        end_manager(&sup, mgr);
        let _ = sup.shutdown_all();
    }

    /// manager: binding a taken port degrades non-fatally (D5) — the daemon
    /// stays alive, the failure is reported, and the next intent change
    /// (once the port is free again) recovers.
    #[test]
    fn webui_bind_failure_degrades_then_recovers() {
        let (sup, _) = setup(
            "demo",
            "[app]\nautostart = false\n[program.p]\ncommand = 'true'\nstartsecs = 0.0\n",
        );
        let blocker = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let taken = format!("{}", blocker.local_addr().unwrap());
        let mgr = spawn_manager(&sup);

        let generation = sup.webui.set_desired(WebuiIntent {
              enabled: true,
              listen: taken.clone(),
              ..Default::default()
        });
        let (ok, note) = sup.webui.wait_applied(generation, Duration::from_secs(10));
        assert!(!ok, "occupied port must not report success: {note}");
        assert!(
            note.contains("unavailable") && note.contains("reload to retry"),
            "note explains the degradation and the retry path: {note}"
        );
        // The daemon itself is unaffected.
        assert!(!sup.state.lock().unwrap().shutdown.load(Ordering::SeqCst));

        // Free the port and re-publish the same intent: the manager retries.
        drop(blocker);
        std::thread::sleep(Duration::from_millis(100));
        let generation = sup.webui.set_desired(WebuiIntent {
              enabled: true,
              listen: taken.clone(),
              ..Default::default()
        });
        wait_applied_ok(&sup, generation);
        assert!(
            std::net::TcpStream::connect(&taken).is_ok(),
            "console serving after the port freed up"
        );
        end_manager(&sup, mgr);
        let _ = sup.shutdown_all();
    }

    /// The intent mapping is the single source for both startup (`run_daemon`)
    /// and reload (`cmd_reload`): section absent = disabled, bare section =
    /// enabled at the built-in default listen, explicit listen passes through
    /// (webui-api: 启动时按配置伺服; configuration: `listen` 缺省 127.0.0.1:9877).
    #[test]
    fn webui_intent_from_config_maps_presence_and_default() {
        let off: DaemonConfig = toml::from_str("[daemon]\nsocket = \"/tmp/ak.sock\"\n").unwrap();
        assert_eq!(WebuiIntent::from_config(&off), WebuiIntent::disabled());
        // A bare `[webui]` table enables the console at the default address.
        let bare: DaemonConfig = toml::from_str("[webui]\n").unwrap();
        assert_eq!(
            WebuiIntent::from_config(&bare),
            WebuiIntent {
                  enabled: true,
                  listen: crate::config::DEFAULT_WEBUI_LISTEN.to_string(),
                  ..Default::default()
            }
        );
        // An explicit listen passes through verbatim.
        let explicit: DaemonConfig = toml::from_str("[webui]\nlisten = '0.0.0.0:8080'\n").unwrap();
        assert_eq!(
            WebuiIntent::from_config(&explicit),
            WebuiIntent {
                  enabled: true,
                  listen: "0.0.0.0:8080".to_string(),
                  ..Default::default()
            }
        );
    }

    /// reload: an unchanged `[webui]` section re-publishes the same intent and
    /// must NOT bounce the console (a live TCP connection survives), while an
    /// invalid section fails the whole reload with the serving console
    /// untouched (webui-api: reload 时 [webui] 段非法 → 保持既有伺服状态不变),
    /// and a later valid section converges again.
    #[test]
    #[cfg(unix)]
    fn reload_invalid_webui_keeps_serving_and_recovers() {
        use std::io::Read as _;
        let (sup, _app_file) = setup(
            "demo",
            "[app]\nautostart = false\n[program.p]\ncommand = 'true'\nstartsecs = 0.0\n",
        );
        let cfg_path = sup.config_path();
        let mgr = spawn_manager(&sup);
        let port_a = free_port();
        let addr_a = format!("127.0.0.1:{port_a}");

        // Serving on A.
        std::fs::write(
            &cfg_path,
            format!("[webui]\nlisten = '127.0.0.1:{port_a}'\n"),
        )
        .unwrap();
        let out = sup.cmd_reload().unwrap();
        assert!(
            out.contains(&format!("webui: webui console serving on http://{addr_a}")),
            "reload reports the converged console: {out}"
        );
        // A live connection the reload must not disturb.
        let mut held = std::net::TcpStream::connect(&addr_a).unwrap();
        held.set_read_timeout(Some(Duration::from_millis(300)))
            .unwrap();

        // Unchanged section: the intent is republished, the console is not
        // rebound (the held connection never sees an EOF).
        let out = sup.cmd_reload().unwrap();
        assert!(
            out.contains(&format!("webui: webui console serving on http://{addr_a}")),
            "an unchanged section still reports serving: {out}"
        );
        std::thread::sleep(Duration::from_millis(150));
        let mut buf = [0u8; 16];
        match held.read(&mut buf) {
            Ok(0) => panic!("the console was rebound on an unchanged reload"),
            Ok(_) => {} // unsolicited data would be harmless
            Err(e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut => {}
            Err(e) => panic!("the held connection broke on an unchanged reload: {e}"),
        }

        // Invalid section: the whole reload fails, A keeps serving.
        std::fs::write(&cfg_path, "[webui]\nlisten = 'no-host-or-port'\n").unwrap();
        let err = sup
            .cmd_reload()
            .err()
            .expect("invalid [webui] fails reload");
        assert!(
            err.to_string().contains("webui.listen"),
            "the error names the offending key: {err:#}"
        );
        assert!(
            std::net::TcpStream::connect(&addr_a).is_ok(),
            "the serving console is untouched by the failed reload"
        );

        // Recovery: a valid section converges again.
        std::fs::write(
            &cfg_path,
            format!("[webui]\nlisten = '127.0.0.1:{port_a}'\n"),
        )
        .unwrap();
        let out = sup.cmd_reload().unwrap();
        assert!(
            out.contains("webui: webui console serving on http://"),
            "reload recovers the console: {out}"
        );
        end_manager(&sup, mgr);
        let _ = sup.shutdown_all();
    }

    /// reload: the `[webui]` section is hot-applied (off→on→rebind→off),
    /// an invalid section fails the whole reload leaving the console
    /// untouched, and daemon hints still render (回归：[daemon] 语义不变).
    #[test]
    #[cfg(unix)]
    fn reload_hot_applies_webui_and_keeps_daemon_hints() {
        let (sup, _app_file) = setup(
            "demo",
            "[app]\nautostart = false\n[program.p]\ncommand = 'true'\nstartsecs = 0.0\n",
        );
        let cfg_path = sup.config_path();
        let mgr = spawn_manager(&sup);

        // off → on.
        let port_a = free_port();
        std::fs::write(
            &cfg_path,
            format!("[webui]\nlisten = '127.0.0.1:{port_a}'\n"),
        )
        .unwrap();
        let out = sup.cmd_reload().unwrap();
        assert!(
            out.contains(&format!(
                "webui: webui console serving on http://127.0.0.1:{port_a}"
            )),
            "reload reports the converged console: {out}"
        );
        assert!(std::net::TcpStream::connect(&format!("127.0.0.1:{port_a}")).is_ok());

        // Rebind: listen change.
        let port_b = free_port();
        std::fs::write(
            &cfg_path,
            format!("[webui]\nlisten = '127.0.0.1:{port_b}'\n"),
        )
        .unwrap();
        let out = sup.cmd_reload().unwrap();
        assert!(
            out.contains(&format!("serving on http://127.0.0.1:{port_b}")),
            "reload reports the new address: {out}"
        );
        std::thread::sleep(Duration::from_millis(150));
        assert!(std::net::TcpStream::connect(&format!("127.0.0.1:{port_a}")).is_err());
        assert!(std::net::TcpStream::connect(&format!("127.0.0.1:{port_b}")).is_ok());

        // on → off.
        std::fs::write(&cfg_path, "").unwrap();
        let out = sup.cmd_reload().unwrap();
        assert!(
            out.contains("webui: webui console disabled"),
            "reload reports the disabled console: {out}"
        );
        assert!(std::net::TcpStream::connect(&format!("127.0.0.1:{port_b}")).is_err());

        // Invalid listen: the whole reload fails, console stays off.
        std::fs::write(&cfg_path, "[webui]\nlisten = 'no-host-or-port'\n").unwrap();
        let err = sup
            .cmd_reload()
            .err()
            .expect("invalid [webui] fails reload");
        assert!(
            err.to_string().contains("webui.listen"),
            "the error names the offending key: {err:#}"
        );

        // Regression: [daemon] changes still produce restart hints, and webui
        // diffs never appear in pending.
        let port_c = free_port();
        let sock_c = cfg_path.with_extension("sock");
        std::fs::write(
            &cfg_path,
            format!(
                "[daemon]\nsocket = {:?}\n[webui]\nlisten = '127.0.0.1:{port_c}'\n",
                sock_c.to_string_lossy()
            ),
        )
        .unwrap();
        let out = sup.cmd_reload().unwrap();
        assert!(
            out.contains("daemon socket changed: restart appkeeper to apply"),
            "daemon hint still renders: {out}"
        );
        assert!(out.contains("webui:"), "webui note appended: {out}");
        {
            let st = sup.state.lock().unwrap();
            assert!(
                st.pending
                    .daemon_hints
                    .iter()
                    .any(|h| h.contains("daemon socket changed")),
                "hint recorded in pending"
            );
            assert!(
                st.pending.programs.is_empty() && st.pending.apps_added.is_empty(),
                "webui section must not create pending entries"
            );
        }
        end_manager(&sup, mgr);
        let _ = sup.shutdown_all();
    }

    // -- restart policy / user-stop record (tasks 1.2 / 1.3 / 1.4) ----------

    /// process-management: daemon start pulls up per `restart` policy — `no`
    /// stays stopped, `always`/`on-failure` run, `unless-stopped` runs only
    /// without a persisted record.
    #[test]
    fn bootstrap_pull_up_follows_restart_policy() {
        let (sup, _) = setup(
            "demo",
            "[program.none]\ncommand = 'sleep 30'\nstartsecs = 0.0\nrestart = 'no'\n\
             [program.always]\ncommand = 'sleep 30'\nstartsecs = 0.0\nrestart = 'always'\n\
             [program.fail]\ncommand = 'sleep 30'\nstartsecs = 0.0\nrestart = 'on-failure'\n\
             [program.unless]\ncommand = 'sleep 30'\nstartsecs = 0.0\nrestart = 'unless-stopped'\n",
        );
        wait_state(&sup, "always", ProgramState::Running);
        wait_state(&sup, "fail", ProgramState::Running);
        wait_state(&sup, "unless", ProgramState::Running);
        {
            let st = sup.state.lock().unwrap();
            assert_eq!(st.programs["none"].state(), ProgramState::Stopped);
        }
        let _ = sup.shutdown_all();
    }

    /// process-management: `unless-stopped` consults the persisted record;
    /// an explicit stop writes it, an explicit start clears it, and shutdown
    /// does not write it (always ignores it).
    #[test]
    fn user_stop_record_write_clear_and_shutdown() {
        let (sup, _) = setup(
            "demo",
            "[program.unless]\ncommand = 'sleep 30'\nstartsecs = 0.0\nrestart = 'unless-stopped'\n\
             [program.always]\ncommand = 'sleep 30'\nstartsecs = 0.0\nrestart = 'always'\n",
        );
        wait_state(&sup, "unless", ProgramState::Running);
        wait_state(&sup, "always", ProgramState::Running);

        // Explicit stop writes the record; explicit start clears it.
        sup.cmd_stop("unless").unwrap();
        assert!(sup.state.lock().unwrap().user_stopped.contains("unless"));
        // The record is on disk.
        let log_dir = {
            let st = sup.state.lock().unwrap();
            config::resolve_path(&st.config.daemon.log_dir, &st.config_dir)
        };
        let on_disk = std::fs::read_to_string(log_dir.join("state.json")).unwrap();
        assert!(on_disk.contains("unless"), "record persisted: {on_disk}");
        sup.cmd_start("unless").unwrap();
        assert!(!sup.state.lock().unwrap().user_stopped.contains("unless"));

        // Shutdown stops everything but writes no record.
        sup.cmd_stop("always").unwrap();
        assert!(sup.state.lock().unwrap().user_stopped.contains("always"));
        sup.shutdown_all();
        assert!(
            sup.state.lock().unwrap().user_stopped.contains("always"),
            "shutdown does not touch the set (the stop above did)"
        );
    }

    /// process-management: the record survives a daemon restart — a new
    /// supervisor built over the same log_dir reads it back and holds an
    /// `unless-stopped` program down, while `always` is pulled up regardless.
    #[test]
    fn user_stop_record_persists_across_daemon_restart() {
        let dir = std::env::temp_dir().join(format!("ak-test-persist-{}", unique_tag()));
        let _ = std::fs::remove_dir_all(&dir);
        let logs = dir.join("logs");
        std::fs::create_dir_all(dir.join("app.d")).unwrap();
        let cfg_path = dir.join("daemon.toml");
        std::fs::write(
            &cfg_path,
            format!("[daemon]\nlog_dir = {:?}\n", logs.to_string_lossy()),
        )
        .unwrap();
        let app_file = dir.join("appkeeper.toml");
        std::fs::write(
            &app_file,
            "[program.unless]\ncommand = 'sleep 30'\nstartsecs = 0.0\nrestart = 'unless-stopped'\n\
             [program.always]\ncommand = 'sleep 30'\nstartsecs = 0.0\nrestart = 'always'\n",
        )
        .unwrap();
        link_for_test(&app_file, &dir.join("app.d").join("demo.toml"));

        let (config, _) = DaemonConfig::load_or_default(&cfg_path).unwrap();
        let sup = Supervisor::new(config, &dir).unwrap();
        sup.set_config_path(&cfg_path);
        sup.bootstrap().unwrap();
        wait_state(&sup, "unless", ProgramState::Running);
        wait_state(&sup, "always", ProgramState::Running);
        sup.cmd_stop("unless").unwrap();
        sup.cmd_stop("always").unwrap();
        let _ = sup.shutdown_all();

        // A fresh daemon over the same workspace.
        let (config, _) = DaemonConfig::load_or_default(&cfg_path).unwrap();
        let sup2 = Supervisor::new(config, &dir).unwrap();
        sup2.set_config_path(&cfg_path);
        sup2.bootstrap().unwrap();
        wait_state(&sup2, "always", ProgramState::Running);
        {
            let st = sup2.state.lock().unwrap();
            assert_eq!(
                st.programs["unless"].state(),
                ProgramState::Stopped,
                "unless-stopped stays down across a daemon restart"
            );
        }
        let _ = sup2.shutdown_all();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A corrupt state.json is tolerated: the daemon starts with an empty
    /// user-stop set (worst case one extra pull-up) instead of failing.
    #[test]
    fn corrupt_state_file_is_tolerated() {
        let dir = std::env::temp_dir().join(format!("ak-test-state-{}", unique_tag()));
        let _ = std::fs::remove_dir_all(&dir);
        let logs = dir.join("logs");
        std::fs::create_dir_all(&logs).unwrap();
        std::fs::write(logs.join("state.json"), "{ not json").unwrap();
        let cfg_path = dir.join("daemon.toml");
        std::fs::write(
            &cfg_path,
            format!("[daemon]\nlog_dir = {:?}\n", logs.to_string_lossy()),
        )
        .unwrap();
        let (config, _) = DaemonConfig::load_or_default(&cfg_path).unwrap();
        let sup = Supervisor::new(config, &dir).expect("a corrupt state file must not abort");
        assert!(sup.state.lock().unwrap().user_stopped.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// apply-workflow: apply pulls a new program up per its `restart` policy —
    /// `no` only registers (action "none"), `always` starts (action "start").
    #[test]
    fn apply_new_program_follows_restart_policy() {
        let (sup, app_file) = setup(
            "demo",
            "[program.seed]\ncommand = 'sleep 30'\nstartsecs = 0.0\nrestart = 'no'\n",
        );
        std::fs::write(
            &app_file,
            "[program.seed]\ncommand = 'sleep 30'\nstartsecs = 0.0\nrestart = 'no'\n\
             [program.down]\ncommand = 'sleep 30'\nstartsecs = 0.0\nrestart = 'no'\n\
             [program.up]\ncommand = 'sleep 30'\nstartsecs = 0.0\nrestart = 'always'\n",
        )
        .unwrap();
        let out = sup.cmd_apply(&ApplyScope::All, false).unwrap();
        assert!(
            out.contains("down -> none"),
            "no-policy program not started: {out}"
        );
        wait_state(&sup, "up", ProgramState::Running);
        {
            let st = sup.state.lock().unwrap();
            assert_eq!(st.programs["down"].state(), ProgramState::Stopped);
        }
        let _ = sup.shutdown_all();
    }

    /// task 1.4: changing only the `restart` field is detected as a pending
    /// change (the field participates in the definition hash).
    #[test]
    fn detect_treats_restart_change_as_pending() {
        let (sup, app_file) = setup(
            "demo",
            "[program.p]\ncommand = 'sleep 30'\nstartsecs = 0.0\nrestart = 'no'\n",
        );
        let doc = sup.detect(true).unwrap();
        assert!(doc.programs.is_empty(), "initial state is clean: {doc:?}");
        std::fs::write(
            &app_file,
            "[program.p]\ncommand = 'sleep 30'\nstartsecs = 0.0\nrestart = 'always'\n",
        )
        .unwrap();
        let doc = sup.detect(true).unwrap();
        assert_eq!(doc.programs.len(), 1, "restart change is pending: {doc:?}");
        assert_eq!(doc.programs[0].program, "p");
        let _ = sup.shutdown_all();
    }

    // -- ad-hoc registration (run-daemon-first sections 3/4) ----------------

    /// A bare supervisor (no registered apps) with a private app_dir/log_dir
    /// and a running supervision loop, for ad-hoc tests.
    fn adhoc_harness(tag: &str) -> (Arc<Supervisor>, PathBuf, std::thread::JoinHandle<()>) {
        let dir = std::env::temp_dir().join(format!("ak-test-adhoc-{}-{}", unique_tag(), tag));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("logs")).unwrap();
        let cfg_path = dir.join("daemon.toml");
        std::fs::write(
            &cfg_path,
            format!(
                "[daemon]\nlog_dir = {:?}\nmonitor_interval = 0.05\n",
                dir.join("logs").to_string_lossy()
            ),
        )
        .unwrap();
        let (config, _) = DaemonConfig::load_or_default(&cfg_path).unwrap();
        let sup = Supervisor::new(config, &dir).unwrap();
        sup.set_config_path(&cfg_path);
        sup.bootstrap().unwrap();
        let sup2 = Arc::clone(&sup);
        let handle = std::thread::Builder::new()
            .name("adhoc-supervisor".into())
            .spawn(move || sup2.run())
            .unwrap();
        (sup, dir, handle)
    }

    fn stop_adhoc(sup: &Supervisor, handle: std::thread::JoinHandle<()>) {
        sup.request_shutdown();
        handle.join().unwrap();
    }

    fn spec_for(command: &str, args: &[&str]) -> AdhocSpec {
        AdhocSpec {
            name: None,
            command: PathBuf::from(command),
            args: args.iter().map(|s| s.to_string()).collect(),
            env: Default::default(),
            work_dir: None,
            description: None,
            restart: None,
            restart_backoff: None,
            health_check: None,
            register_only: false,
        }
    }

    /// The blocking `sleep` binary used by ad-hoc specs.
    fn sleep_bin() -> PathBuf {
        #[cfg(unix)]
        {
            PathBuf::from("/bin/sleep")
        }
        #[cfg(windows)]
        {
            PathBuf::from("C:\\Windows\\System32\\ping.exe")
        }
    }

    fn adhoc_up(sup: &Supervisor, spec: AdhocSpec) -> std::result::Result<AdhocUpResult, AdhocUpError> {
        sup.cmd_adhoc_up(&spec)
    }

    fn adhoc_commit(sup: &Supervisor, name: &str) -> Result<AdhocCommitResult> {
        sup.cmd_adhoc_commit(name)
    }

    /// 3.1: a staged entry carries the rendered text, target and placeholder
    /// paths and its resolved definition.
    #[test]
    fn staged_app_records_definition_and_paths() {
        let (sup, dir, h) = adhoc_harness("stage");
        let spec = AdhocSpec {
            name: Some("web".into()),
            command: sleep_bin(),
            args: vec!["30".into()],
            ..spec_for("/bin/sleep", &[])
        };
        let up = adhoc_up(&sup, spec).unwrap();
        assert_eq!(up.app, "web");
        assert_eq!(up.program, "web");
        assert!(up.pid.is_some(), "spawned");
        {
            let st = sup.state.lock().unwrap();
            let s = st.staged.get("web").expect("staged entry");
            assert_eq!(s.app, "web");
            assert_eq!(s.program, "web");
            assert!(!s.committed);
            assert!(s.text.contains("[program.web]"), "{}", s.text);
            assert_eq!(s.target, dir.join("app.d").join("web.toml"));
            assert_eq!(s.placeholder, dir.join("app.d").join("web.toml.pending"));
            assert!(s.placeholder.exists(), "placeholder staged");
            assert_eq!(s.resolved.programs[0].name, "web");
        }
        stop_adhoc(&sup, h);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 3.2: a staged program survives repeated detection passes and never
    /// shows up as pending.
    #[test]
    fn staged_program_survives_periodic_detection() {
        let (sup, dir, h) = adhoc_harness("survive");
        let spec = AdhocSpec {
            name: Some("web".into()),
            ..spec_for(sleep_bin().to_str().unwrap(), &["30"])
        };
        adhoc_up(&sup, spec).unwrap();
        for _ in 0..4 {
            let doc = sup.detect(true).unwrap();
            assert!(doc.programs.is_empty(), "staged must not be pending: {doc:?}");
            assert!(doc.apps_added.is_empty(), "staged must not be added: {doc:?}");
            let st = sup.state.lock().unwrap();
            assert!(
                st.programs.contains_key("web"),
                "staged program survives detection"
            );
        }
        stop_adhoc(&sup, h);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 3.3: validation failures do not spawn anything; a valid spec spawns and
    /// leaves the program in `Starting`.
    #[test]
    fn adhoc_up_validates_then_spawns() {
        let (sup, dir, h) = adhoc_harness("up");
        // Reserved name `all`.
        let err = adhoc_up(
            &sup,
            AdhocSpec {
                name: Some("all".into()),
                ..spec_for(sleep_bin().to_str().unwrap(), &["30"])
            },
        )
        .unwrap_err();
        assert!(err.to_string().contains("reserved"), "{err}");
        assert_eq!(err.http_status(), 400);
        assert_eq!(err.phase(), "validation");
        assert!(sup.state.lock().unwrap().programs.is_empty(), "nothing spawned");

        // Invalid charset.
        let err = adhoc_up(
            &sup,
            AdhocSpec {
                name: Some("bad name".into()),
                ..spec_for(sleep_bin().to_str().unwrap(), &["30"])
            },
        )
        .unwrap_err();
        assert!(matches!(err, AdhocUpError::Validation(_)));

        // A valid spec spawns; with the default 3s startsecs it is `Starting`.
        let up = adhoc_up(
            &sup,
            AdhocSpec {
                name: Some("web".into()),
                ..spec_for(sleep_bin().to_str().unwrap(), &["30"])
            },
        )
        .unwrap();
        assert!(up.pid.is_some());
        assert_eq!(
            sup.state.lock().unwrap().programs["web"].state(),
            ProgramState::Starting
        );
        stop_adhoc(&sup, h);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 3.3/4.3: a program name owned by ANOTHER app is refused without
    /// spawning (control-plane spec: 名称冲突被拒绝 — 与其他应用冲突).
    #[test]
    fn adhoc_up_refuses_program_name_collision() {
        let (sup, dir, h) = adhoc_harness("collide");
        // A pre-existing app owns the program name `web`.
        let app_file = dir.join("existing.toml");
        std::fs::write(&app_file, "[program.web]\ncommand = 'sleep 30'\n").unwrap();
        std::fs::create_dir_all(dir.join("app.d")).unwrap();
        link_for_test(&app_file, &dir.join("app.d").join("existing.toml"));
        sup.bootstrap().unwrap();
        let err = adhoc_up(
            &sup,
            AdhocSpec {
                name: Some("web".into()),
                ..spec_for(sleep_bin().to_str().unwrap(), &["30"])
            },
        )
        .unwrap_err();
        assert!(err.to_string().contains("already registered"), "{err}");
        assert!(
            err.to_string().contains("existing"),
            "names the owning app: {err}"
        );
        assert!(!sup.state.lock().unwrap().staged.contains_key("web"));
        stop_adhoc(&sup, h);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// control-plane: a same-name app's repeated submission is NOT a
    /// collision — identical content is an idempotent upsert returning the
    /// existing pid without restarting (同名重复提交走再生, 无变化幂等).
    #[test]
    fn adhoc_upsert_identical_is_idempotent() {
        let (sup, dir, h) = adhoc_harness("upsert-idem");
        let spec = || AdhocSpec {
            name: Some("web".into()),
            ..spec_for(sleep_bin().to_str().unwrap(), &["30"])
        };
        let first = adhoc_up(&sup, spec()).unwrap();
        assert!(!first.unchanged && !first.residual);
        let pid = first.pid.expect("spawned");
        // Commit so the record is on disk (a committed same-name app).
        adhoc_commit(&sup, "web").unwrap();
        assert!(dir.join("app.d").join("web.toml").exists());

        let again = adhoc_up(&sup, spec()).unwrap();
        assert!(
            again.unchanged,
            "identical content is idempotent: {again:?}"
        );
        assert!(!again.residual, "not the residual path");
        assert_eq!(again.pid, Some(pid), "no restart, same pid");
        assert_eq!(again.app, "web");
        // No placeholder was staged and the record is unchanged.
        assert!(!dir.join("app.d").join("web.toml.pending").exists());
        {
            let st = sup.state.lock().unwrap();
            assert!(
                !st.staged.contains_key("web"),
                "no staged entry for a committed app"
            );
            assert_eq!(st.programs["web"].pid(), Some(pid));
        }
        stop_adhoc(&sup, h);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// control-plane: a same-name app resubmitted with DIFFERENT content is
    /// fully regenerated — the record is rewritten and the process restarted
    /// with a new pid (同名重复提交走再生, 有变化重启).
    #[test]
    fn adhoc_upsert_changed_regenerates() {
        let (sup, dir, h) = adhoc_harness("upsert-regen");
        let first = adhoc_up(
            &sup,
            AdhocSpec {
                name: Some("web".into()),
                ..spec_for(sleep_bin().to_str().unwrap(), &["30"])
            },
        )
        .unwrap();
        let pid1 = first.pid.expect("spawned");
        adhoc_commit(&sup, "web").unwrap();
        let record = dir.join("app.d").join("web.toml");
        let text1 = std::fs::read_to_string(&record).unwrap();
        assert!(text1.contains("30"), "{text1}");

        let again = adhoc_up(
            &sup,
            AdhocSpec {
                name: Some("web".into()),
                ..spec_for(sleep_bin().to_str().unwrap(), &["60"])
            },
        )
        .unwrap();
        assert!(!again.unchanged && !again.residual, "{again:?}");
        let pid2 = again.pid.expect("respawned");
        assert_ne!(pid2, pid1, "changed content restarts the process");
        // The record was rewritten in place; no placeholder residue.
        let text2 = std::fs::read_to_string(&record).unwrap();
        assert!(text2.contains("60") && !text2.contains("\"30\""), "{text2}");
        assert!(!dir.join("app.d").join("web.toml.pending").exists());
        {
            let st = sup.state.lock().unwrap();
            assert_eq!(st.programs["web"].pid(), Some(pid2));
            assert!(
                !st.staged.contains_key("web"),
                "regeneration stages nothing"
            );
        }
        stop_adhoc(&sup, h);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// control-plane: a validation failure of the NEW definition must not
    /// disturb the existing record or the running process.
    #[test]
    fn adhoc_upsert_invalid_definition_keeps_existing() {
        let (sup, dir, h) = adhoc_harness("upsert-invalid");
        let first = adhoc_up(
            &sup,
            AdhocSpec {
                name: Some("web".into()),
                ..spec_for(sleep_bin().to_str().unwrap(), &["30"])
            },
        )
        .unwrap();
        let pid = first.pid.expect("spawned");
        adhoc_commit(&sup, "web").unwrap();
        let record = dir.join("app.d").join("web.toml");
        let before = std::fs::read_to_string(&record).unwrap();

        // A definition whose rendered config fails validation (negative
        // restart_backoff) is refused during `render_adhoc`, before any write
        // or stop.
        let err = adhoc_up(
            &sup,
            AdhocSpec {
                name: Some("web".into()),
                restart_backoff: Some(-1.0),
                ..spec_for(sleep_bin().to_str().unwrap(), &["30"])
            },
        )
        .unwrap_err();
        assert!(
            err.to_string().contains("restart_backoff"),
            "the validation error names the field: {err}"
        );
        assert_eq!(
            std::fs::read_to_string(&record).unwrap(),
            before,
            "record untouched"
        );
        assert!(!dir.join("app.d").join("web.toml.pending").exists());
        assert_eq!(
            sup.state.lock().unwrap().programs["web"].pid(),
            Some(pid),
            "the running process is untouched"
        );
        stop_adhoc(&sup, h);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 3.4: commit renames the placeholder onto the record, clears the staged
    /// entry and registers the app.
    #[test]
    fn adhoc_commit_persists_and_clears_staged() {
        let (sup, dir, h) = adhoc_harness("commit");
        adhoc_up(
            &sup,
            AdhocSpec {
                name: Some("web".into()),
                ..spec_for(sleep_bin().to_str().unwrap(), &["30"])
            },
        )
        .unwrap();
        let out = adhoc_commit(&sup, "web").unwrap();
        assert!(!out.partial && !out.unchanged);
        assert_eq!(out.app, "web");
        assert_eq!(out.config_path, dir.join("app.d").join("web.toml").display().to_string());
        assert!(out.pid.is_some());
        assert!(!dir.join("app.d").join("web.toml.pending").exists());
        {
            let st = sup.state.lock().unwrap();
            assert!(!st.staged.contains_key("web"), "staged cleared");
            assert!(st.apps.iter().any(|a| a.name == "web"), "app recorded");
        }
        let (config, _) = DaemonConfig::load_or_default(&dir.join("daemon.toml")).unwrap();
        let listed = registry::list(&config, &dir).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].name, "web");
        stop_adhoc(&sup, h);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 3.5: abort stops the program, removes it and deletes every file.
    #[test]
    fn adhoc_abort_removes_process_and_files() {
        let (sup, dir, h) = adhoc_harness("abort");
        adhoc_up(
            &sup,
            AdhocSpec {
                name: Some("web".into()),
                ..spec_for(sleep_bin().to_str().unwrap(), &["30"])
            },
        )
        .unwrap();
        sup.cmd_adhoc_abort("web").unwrap();
        {
            let st = sup.state.lock().unwrap();
            assert!(!st.programs.contains_key("web"), "program removed");
            assert!(!st.staged.contains_key("web"), "staged cleared");
        }
        let app_dir = dir.join("app.d");
        if app_dir.exists() {
            assert!(
                std::fs::read_dir(&app_dir).unwrap().next().is_none(),
                "no residue after abort"
            );
        }
        stop_adhoc(&sup, h);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 3.6: `--restart=no` registers without spawning (pid = null).
    #[test]
    fn adhoc_register_only_skips_spawn() {
        let (sup, dir, h) = adhoc_harness("regonly");
        let up = adhoc_up(
            &sup,
            AdhocSpec {
                name: Some("web".into()),
                restart: Some(crate::config::RestartPolicy::No),
                register_only: true,
                ..spec_for(sleep_bin().to_str().unwrap(), &["30"])
            },
        )
        .unwrap();
        assert_eq!(up.pid, None, "register-only leaves pid null");
        {
            let st = sup.state.lock().unwrap();
            assert_eq!(
                st.programs["web"].state(),
                ProgramState::Stopped,
                "no spawn"
            );
        }
        let out = adhoc_commit(&sup, "web").unwrap();
        assert_eq!(out.pid, None);
        assert!(dir.join("app.d").join("web.toml").exists(), "record written");
        stop_adhoc(&sup, h);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 3.7: a residual (staged, uncommitted) same-name app reports residual
    /// instead of restarting; a fresh name reports residual = false.
    #[test]
    fn adhoc_up_reports_residual_state() {
        let (sup, dir, h) = adhoc_harness("residual");
        let first = adhoc_up(
            &sup,
            AdhocSpec {
                name: Some("web".into()),
                ..spec_for(sleep_bin().to_str().unwrap(), &["30"])
            },
        )
        .unwrap();
        assert!(!first.residual);
        let pid = first.pid;
        // Second identical run: the staged entry is still there → residual.
        let second = adhoc_up(
            &sup,
            AdhocSpec {
                name: Some("web".into()),
                ..spec_for(sleep_bin().to_str().unwrap(), &["30"])
            },
        )
        .unwrap();
        assert!(second.residual, "same-name staged app is residual");
        assert_eq!(second.pid, pid, "no restart, same pid");
        stop_adhoc(&sup, h);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 4.3: the estimate is startsecs without a health check and
    /// startsecs + start_period + interval × retries with one.
    #[test]
    fn startup_estimate_formula() {
        let (sup, dir, h) = adhoc_harness("estimate");
        let no_health = crate::config::resolve_app(
            "demo",
            Path::new("x.toml"),
            &toml::from_str("[program.p]\ncommand='x'\nstartsecs = 2.5\n").unwrap(),
            None,
        )
        .unwrap();
        assert_eq!(sup.startup_estimate_secs(&no_health), 2.5);
        let health = crate::config::resolve_app(
            "demo",
            Path::new("x.toml"),
            &toml::from_str(
                "[program.p]\ncommand='x'\nstartsecs = 2.0\nhealth_check='true'\n\
                 health_start_period = 4.0\nhealth_interval = 3.0\nhealth_retries = 5\n",
            )
            .unwrap(),
            None,
        )
        .unwrap();
        assert_eq!(sup.startup_estimate_secs(&health), 2.0 + 4.0 + 3.0 * 5.0);
        stop_adhoc(&sup, h);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 4.2: the wait succeeds once the program is Running (no health check
    /// confirms at spawn), fails fast when it exits inside startsecs, and
    /// times out when the estimate elapses.
    #[test]
    fn wait_startup_success_and_failure_paths() {
        let (sup, dir, h) = adhoc_harness("wait");
        // Success: sleep 30 with a short startsecs.
        adhoc_up(
            &sup,
            AdhocSpec {
                name: Some("ok".into()),
                ..spec_for(sleep_bin().to_str().unwrap(), &["30"])
            },
        )
        .unwrap();
        // The harness's default startsecs is 3.0; the program stays alive.
        match wait_startup(&sup, "ok", Duration::from_secs(10)) {
            StartupOutcome::Started { pid } => assert!(pid.is_some()),
            other => panic!("expected Started, got {other:?}"),
        }

        // Failure: a command that exits immediately.
        #[cfg(unix)]
        let (cmd, args) = ("/bin/sh", vec!["-c", "exit 3"]);
        #[cfg(windows)]
        let (cmd, args) = ("cmd", vec!["/c", "exit 3"]);
        adhoc_up(
            &sup,
            AdhocSpec {
                name: Some("bad".into()),
                restart: Some(crate::config::RestartPolicy::No),
                ..spec_for(cmd, &args)
            },
        )
        .unwrap();
        match wait_startup(&sup, "bad", Duration::from_secs(10)) {
            StartupOutcome::Failed { phase, reason } => {
                assert_eq!(phase, "startup");
                assert!(!reason.is_empty(), "{reason}");
            }
            other => panic!("expected Failed, got {other:?}"),
        }

        // Timeout: a program that never reaches Running (huge startsecs).
        adhoc_up(
            &sup,
            AdhocSpec {
                name: Some("slow".into()),
                ..spec_for(sleep_bin().to_str().unwrap(), &["30"])
            },
        )
        .unwrap();
        // Force a long startsecs on the running definition is not possible via
        // the spec, so use a zero-length estimate to exercise the timeout arm.
        match wait_startup(&sup, "slow", Duration::from_millis(0)) {
            StartupOutcome::TimedOut { .. } => {}
            other => panic!("expected TimedOut, got {other:?}"),
        }
        stop_adhoc(&sup, h);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 4.4: after a failed startup judgement the abort cleanup leaves no file
    /// behind and no program in state.
    #[test]
    fn failed_startup_then_abort_leaves_no_residue() {
        let (sup, dir, h) = adhoc_harness("failclean");
        #[cfg(unix)]
        let (cmd, args) = ("/bin/sh", vec!["-c", "exit 3"]);
        #[cfg(windows)]
        let (cmd, args) = ("cmd", vec!["/c", "exit 3"]);
        adhoc_up(
            &sup,
            AdhocSpec {
                name: Some("bad".into()),
                restart: Some(crate::config::RestartPolicy::No),
                ..spec_for(cmd, &args)
            },
        )
        .unwrap();
        let outcome = wait_startup(&sup, "bad", Duration::from_secs(10));
        assert!(matches!(outcome, StartupOutcome::Failed { .. }), "{outcome:?}");
        sup.cmd_adhoc_abort("bad").unwrap();
        let app_dir = dir.join("app.d");
        if app_dir.exists() {
            assert!(
                std::fs::read_dir(&app_dir).unwrap().next().is_none(),
                "no .toml or .pending residue"
            );
        }
        stop_adhoc(&sup, h);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 3.4/D4: a commit whose rename keeps failing reports a partial failure,
    /// keeps the process running, keeps the staged entry, and returns the full
    /// config text for manual landing. (unix, non-root)
    #[test]
    #[cfg(unix)]
    fn adhoc_commit_partial_failure_keeps_process() {
        if unsafe { libc::geteuid() } == 0 {
            return;
        }
        use std::os::unix::fs::PermissionsExt;
        let (sup, dir, h) = adhoc_harness("partial");
        adhoc_up(
            &sup,
            AdhocSpec {
                name: Some("web".into()),
                ..spec_for(sleep_bin().to_str().unwrap(), &["30"])
            },
        )
        .unwrap();
        let pid = sup.state.lock().unwrap().programs["web"].pid();
        assert!(pid.is_some());
        // Make the app_dir read-only so the rename cannot complete.
        let app_dir = dir.join("app.d");
        std::fs::set_permissions(&app_dir, std::fs::Permissions::from_mode(0o555)).unwrap();
        let out = adhoc_commit(&sup, "web").unwrap();
        std::fs::set_permissions(&app_dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(out.partial, "rename failure is a partial failure: {out:?}");
        assert!(!out.partial_reason.is_empty(), "{out:?}");
        assert!(out.config_text.contains("[program.web]"), "config returned");
        assert_eq!(out.pid, pid, "process kept running");
        {
            let st = sup.state.lock().unwrap();
            assert!(st.staged.contains_key("web"), "staged kept for retry");
            assert!(st.programs.contains_key("web"), "program kept running");
        }
        stop_adhoc(&sup, h);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// D4: after a partial failure, committing again (once the IO recovers)
    /// lands the record while reusing the already-running process (pid
    /// unchanged).
    #[test]
    #[cfg(unix)]
    fn adhoc_residual_commit_reuses_process() {
        if unsafe { libc::geteuid() } == 0 {
            return;
        }
        use std::os::unix::fs::PermissionsExt;
        let (sup, dir, h) = adhoc_harness("residual-commit");
        adhoc_up(
            &sup,
            AdhocSpec {
                name: Some("web".into()),
                ..spec_for(sleep_bin().to_str().unwrap(), &["30"])
            },
        )
        .unwrap();
        let pid = sup.state.lock().unwrap().programs["web"].pid();
        let app_dir = dir.join("app.d");
        std::fs::set_permissions(&app_dir, std::fs::Permissions::from_mode(0o555)).unwrap();
        let partial = adhoc_commit(&sup, "web").unwrap();
        assert!(partial.partial);
        // IO recovers; a fresh run reports residual, and committing lands it.
        std::fs::set_permissions(&app_dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        let again = adhoc_up(
            &sup,
            AdhocSpec {
                name: Some("web".into()),
                ..spec_for(sleep_bin().to_str().unwrap(), &["30"])
            },
        )
        .unwrap();
        assert!(again.residual, "residual reported after a partial failure");
        assert_eq!(again.pid, pid, "no restart while residual");
        let done = adhoc_commit(&sup, "web").unwrap();
        assert!(!done.partial, "second commit lands");
        assert_eq!(done.pid, pid, "process reused");
        assert!(dir.join("app.d").join("web.toml").exists());
        stop_adhoc(&sup, h);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// D1/shutdown: a staged (uncommitted) program is stopped by shutdown_all
    /// even though its app is not yet in `st.apps` — no orphaned child.
    #[test]
    fn shutdown_stops_staged_program() {
        let (sup, dir, h) = adhoc_harness("shutdown-staged");
        adhoc_up(
            &sup,
            AdhocSpec {
                name: Some("web".into()),
                ..spec_for(sleep_bin().to_str().unwrap(), &["30"])
            },
        )
        .unwrap();
        let pid = sup.state.lock().unwrap().programs["web"].pid().unwrap();
        // Shutdown: the loop exits and shutdown_all runs.
        sup.request_shutdown();
        h.join().unwrap();
        let st = sup.state.lock().unwrap();
        assert_eq!(
            st.programs["web"].state(),
            ProgramState::Stopped,
            "staged program stopped at shutdown"
        );
        assert!(st.programs["web"].pid().is_none(), "child reaped");
        drop(st);
        #[cfg(unix)]
        {
            // The child is gone (kill(pid, 0) fails with ESRCH).
            let alive = unsafe { libc::kill(pid as i32, 0) } == 0;
            assert!(!alive, "no orphaned child {pid}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// D1: a staged app is not recorded in `st.apps` by apply — so an abort
    /// leaves no phantom app record behind, while the program survives the
    /// apply's `keep` filter.
    #[test]
    fn staged_app_not_recorded_by_apply() {
        let (sup, dir, h) = adhoc_harness("no-phantom");
        adhoc_up(
            &sup,
            AdhocSpec {
                name: Some("web".into()),
                ..spec_for(sleep_bin().to_str().unwrap(), &["30"])
            },
        )
        .unwrap();
        // A full apply while staged must not persist the app record.
        sup.cmd_apply(&ApplyScope::All, false).unwrap();
        {
            let st = sup.state.lock().unwrap();
            assert!(
                !st.apps.iter().any(|a| a.name == "web"),
                "staged app must not enter apps: {:?}",
                st.apps.iter().map(|a| &a.name).collect::<Vec<_>>()
            );
            assert!(st.programs.contains_key("web"), "program survives the apply");
        }
        // Abort: no app record, no program, no file.
        sup.cmd_adhoc_abort("web").unwrap();
        let st = sup.state.lock().unwrap();
        assert!(!st.apps.iter().any(|a| a.name == "web"));
        assert!(!st.programs.contains_key("web"));
        drop(st);
        stop_adhoc(&sup, h);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
