//! The local control plane: HTTP/1.1 JSON API over the configured control
//! endpoint (local IPC, or `tcp://` — tcp-control-endpoint D1).
//!
//! The transport underneath is owned by [`crate::transport`]; this module
//! never sees a path, a port or a platform IPC type — it accepts
//! [`transport::Conn`]s and speaks HTTP/1.1 over them. Admission follows the
//! endpoint shape (design D2/D3 — the shapes never mix): the local IPC form
//! was decided by the operating system before the request was ever parsed,
//! so an `Authorization` header is ignored there; the `tcp://` form leans on
//! the Bearer credential from `[daemon] auth_token`, required for every
//! request except `/v1/health`. The shape is a daemon-startup fact, so the
//! gate is derived from the config per connection — there is no per-request
//! dual-track.
//!
//! Hand-rolled (instead of a framework) for one reason: the `log --follow`
//! endpoint must stream lines as they are produced and flush after every
//! write; generic HTTP stacks buffer responses and make that impossible
//! (see design D2). The request surface we need is deliberately tiny:
//! GET/POST, no request bodies, `Connection: close`.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::time::Duration;

use anyhow::Result;
use log::{info, warn};
use serde::{Deserialize, Serialize};

use crate::supervisor::{Command, Supervisor};
use crate::transport::{self, Conn};

const MAX_FOLLOWS: usize = 8;
const MAX_HEADER_BYTES: usize = 32 * 1024;

static FOLLOWS: AtomicUsize = AtomicUsize::new(0);

/// Shared status projection: served as JSON by the control plane and
/// re-used verbatim by the web console (`/api/*` + WS, MessagePack).
///
/// Metric fields (`cpu_percent`/`mem_bytes`/`log_rate`/`system`) are
/// incremental, backwards-compatible additions sourced from the sideband
/// sampler ([`crate::metrics`]): `None`/default means "no data", never 0.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct ProgramInfo {
    pub app: String,
    pub name: String,
    pub state: String,
    pub pid: Option<u32>,
    pub unhealthy: bool,
    pub uptime_secs: f64,
    pub total_exits: u64,
    pub restart_backoff: f64,
    pub last_exit: Option<String>,
    pub fatal_reason: Option<String>,
    pub wait_reason: Option<String>,
    /// CPU% normalized to the whole machine (0..=100).
    pub cpu_percent: Option<f64>,
    /// Resident set size in bytes.
    pub mem_bytes: Option<u64>,
    /// Per-stream log line rates (lines/sec at the display windows).
    pub log_rate: crate::metrics::LogRates,
    /// Declared command and args (console detail display).
    pub command: String,
    pub args: Vec<String>,
    /// Working directory (empty = inherited).
    pub work_dir: String,
    /// Path of the app's config file (the registration record). New in
    /// run-daemon-first (D7); `None` when unknown.
    #[serde(default)]
    pub config_path: Option<String>,
    /// True once the program's health has been positively confirmed (D6):
    /// no-health programs confirm at spawn, checked ones on the first
    /// successful probe. Backward-compatible addition.
    #[serde(default)]
    pub health_confirmed: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct StatusDoc {
    pub daemon: DaemonInfo,
    pub programs: Vec<ProgramInfo>,
    /// Detected-but-not-applied config changes (apply-workflow spec);
    /// backward-compatible addition sourced from the detection pass.
    pub pending: crate::supervisor::PendingDoc,
}

/// Build identity of this binary (show-build-version D2), injected by
/// `build.rs` at compile time. Any probe that failed at build time is the
/// literal `unknown` — consumers render it verbatim rather than guessing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct BuildInfo {
    /// Short git hash, `-dirty` when tracked files were modified.
    pub git_commit: String,
    /// RFC 3339 UTC build timestamp.
    pub built_at: String,
    /// Cargo target triple.
    pub target: String,
    /// `rustc --version` output.
    pub rustc: String,
}

impl BuildInfo {
    /// The metadata compiled into this binary (never fails: missing probes
    /// were already folded into `unknown` by the build script).
    pub(crate) fn current() -> Self {
        BuildInfo {
            git_commit: env!("AK_BUILD_GIT").to_string(),
            built_at: env!("AK_BUILD_TIME").to_string(),
            target: env!("AK_BUILD_TARGET").to_string(),
            rustc: env!("AK_BUILD_RUSTC").to_string(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct DaemonInfo {
    pub version: String,
    /// This binary's build identity (show-build-version D2).
    pub build: BuildInfo,
    /// Kept for wire compatibility with existing consumers, always 0: the
    /// control plane no longer listens on any TCP port
    /// (unix-socket-control-plane D2). New consumers read `socket`.
    pub port: u16,
    /// The control endpoint (socket path / pipe name) this daemon serves.
    pub socket: String,
    pub apps: usize,
    /// Host-level resource overview.
    pub system: crate::metrics::SystemMetrics,
    /// Configured monitor interval (console overview).
    pub monitor_interval: f64,
    /// Daemon wall-clock runtime in seconds.
    pub uptime_secs: f64,
    /// Where the daemon config was loaded from.
    pub config_source: String,
    /// The service manager running this daemon (harden-daemon-autostart
    /// 4.3): `systemd`, `scm`, or `null` for manual starts. Backward-
    /// compatible addition — consumers that ignore it are unaffected.
    #[serde(default)]
    pub managed_by: Option<String>,
}

/// The single `DaemonInfo` constructor: `status_doc()` (WS/API) and the
/// `/v1/status` handler both call this, so the three surfaces cannot drift
/// (show-build-version D2).
fn daemon_info(st: &crate::supervisor::SupervisorState, sup: &Arc<Supervisor>) -> DaemonInfo {
    DaemonInfo {
        version: env!("CARGO_PKG_VERSION").to_string(),
        build: BuildInfo::current(),
        port: 0,
        socket: st.config.daemon.socket.to_string_lossy().into_owned(),
        apps: st.apps.len(),
        system: sup.metrics.system(),
        monitor_interval: st.config.daemon.monitor_interval,
        uptime_secs: (sup.uptime_secs() * 100.0).round() / 100.0,
        config_source: sup.config_path().display().to_string(),
        managed_by: st.managed_by.map(String::from),
    }
}

/// One status snapshot for any consumer (control plane + web console).
pub(crate) fn status_doc(sup: &Arc<Supervisor>) -> StatusDoc {
    let st = sup.state.lock().unwrap();
    let mut programs: Vec<ProgramInfo> = st
        .programs
        .values()
        .map(|p| program_info_state(&st, p, Some(&sup.metrics)))
        .collect();
    programs.sort_by(|a, b| (&a.app, &a.name).cmp(&(&b.app, &b.name)));
    StatusDoc {
        daemon: daemon_info(&st, sup),
        programs,
        pending: st.pending.clone(),
    }
}

/// `GET /v1/webui` (add-webui-launch-command D2/D10): the console's runtime
/// status for the CLI. `enabled` is the config intent, `serving` the manager's
/// last applied outcome, and `token` exists only in `auto` mode — a fixed
/// token is never echoed. The token comes from the live [`ConsoleAuth`], so
/// the answer reflects what the router actually enforces; the intent's
/// resolved token is the fallback while a degraded console is not serving.
///
/// This is the only surface that carries the token: it is not part of
/// [`StatusDoc`], `/v1/programs*` or any `/api/*` response.
pub(crate) fn webui_status_json(sup: &Arc<Supervisor>) -> serde_json::Value {
    let snapshot = sup.webui.snapshot();
    let intent = &snapshot.intent;
    // The absent section still answers with the built-in default address, so
    // a client can render the "enable it here" hint (control-plane spec).
    let listen = if intent.enabled {
        intent.listen.clone()
    } else {
        crate::config::DEFAULT_WEBUI_LISTEN.to_string()
    };
    // The token is only ever handed out for the intent the manager has
    // actually converged on (`serving`). During a reload window the new auto
    // intent may already be published while the live credential is still the
    // old fixed token; echoing either value would hand `ak webui` a stale
    // bootstrap URL, so the window reports no token and the CLI exits 1.
    let token = match intent.mode {
        crate::config::WebuiTokenMode::Auto if snapshot.serving => {
            let live = sup.console_auth.lock().unwrap().token().to_string();
            let token = if live.is_empty() {
                intent.token.clone()
            } else {
                live
            };
            (!token.is_empty()).then_some(token)
        }
        _ => None,
    };
    let mut doc = serde_json::json!({
        "enabled": intent.enabled,
        "serving": snapshot.serving,
        "listen": listen,
        "url": webui_console_url(&listen),
        "auth": intent.mode.as_str(),
    });
    if let Some(token) = token {
        doc["token"] = serde_json::Value::String(token);
    }
    doc
}

/// Browser-facing base URL for a `webui.listen` value. Wildcard binds are
/// rendered as loopback — a browser cannot use `0.0.0.0`/`::`
/// (control-plane spec: MUST NOT output an unreachable wildcard address).
pub(crate) fn webui_console_url(listen: &str) -> String {
    let (host, port) = match listen.rsplit_once(':') {
        Some((host, port)) => (host, Some(port)),
        None => (listen, None),
    };
    let host = match host {
        "0.0.0.0" | "::" | "[::]" | "[::0]" => "127.0.0.1".to_string(),
        h if h.contains(':') && !h.starts_with('[') => format!("[{h}]"),
        h => h.to_string(),
    };
    match port {
        Some(port) => format!("http://{host}:{port}"),
        None => format!("http://{host}"),
    }
}

/// Build the status projection of one program. `metrics` is the shared
/// sampler table; passing `None` leaves metric fields empty (used by tests).
/// `config_path` is the app's config file path (run-daemon-first D7), looked
/// up by the caller from the app records.
pub(crate) fn program_info(
    p: &crate::program::ManagedProgram,
    metrics: Option<&crate::metrics::MetricsTable>,
    config_path: Option<&str>,
) -> ProgramInfo {
    let m = metrics
        .and_then(|t| t.program(&p.def.name))
        .unwrap_or_default();
    ProgramInfo {
        app: p.def.app.clone(),
        name: p.def.name.clone(),
        state: p.state().to_string(),
        pid: p.pid(),
        unhealthy: p.unhealthy(),
        uptime_secs: (p.uptime_secs() * 100.0).round() / 100.0,
        total_exits: p.total_exits(),
        restart_backoff: p.def.restart_backoff,
        last_exit: p.last_exit().map(String::from),
        fatal_reason: p.fatal_reason().map(String::from),
        wait_reason: p.wait_reason(),
        cpu_percent: m.cpu_percent,
        mem_bytes: m.mem_bytes,
        log_rate: p.log_rates(),
        command: p.def.command.clone(),
        args: p.def.args.clone(),
        work_dir: p.def.work_dir.display().to_string(),
        config_path: config_path.map(String::from),
        health_confirmed: p.health_confirmed(),
    }
}

/// Look up an app's config path by app name from the live state.
pub(crate) fn app_config_path(
    st: &crate::supervisor::SupervisorState,
    app: &str,
) -> Option<String> {
    st.apps
        .iter()
        .find(|a| a.name == app)
        .map(|a| a.path.display().to_string())
}

/// Startup-judgement budget for one program (D5): `startsecs`, extended by the
/// health window when the program declares a health check. A small slack is
/// added so a borderline tick does not produce a spurious timeout.
fn startup_estimate_for(sup: &Arc<Supervisor>, program: &str) -> Duration {
    let st = sup.state.lock().unwrap();
    let secs = st
        .programs
        .get(program)
        .map(|p| match &p.def.health {
            None => p.def.startsecs,
            Some(h) => p.def.startsecs + h.start_period + h.interval * f64::from(h.retries),
        })
        .unwrap_or(0.0);
    Duration::from_secs_f64(secs.max(0.1) + 5.0)
}

/// [`program_info`] with the app's config path resolved from the live state.
pub(crate) fn program_info_state(
    st: &crate::supervisor::SupervisorState,
    p: &crate::program::ManagedProgram,
    metrics: Option<&crate::metrics::MetricsTable>,
) -> ProgramInfo {
    let config_path = app_config_path(st, &p.def.app);
    program_info(p, metrics, config_path.as_deref())
}

fn json_bytes(status: u16, body: impl Serialize) -> Vec<u8> {
    let data = serde_json::to_vec(&body).unwrap_or_else(|_| b"{}".to_vec());
    let reason = match status {
        200..=299 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        404 => "Not Found",
        409 => "Conflict",
        429 => "Too Many Requests",
        431 => "Request Header Fields Too Large",
        _ => "Internal Server Error",
    };
    format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        data.len()
    )
    .into_bytes()
    .into_iter()
    .chain(data)
    .collect()
}

fn err_bytes(status: u16, msg: &str) -> Vec<u8> {
    json_bytes(status, serde_json::json!({ "error": msg }))
}

/// Bind the control endpoint. Called on the startup path so a conflict (a
/// live instance, or a path some other file occupies) fails fast with a
/// message that says which. The shape — local IPC or `tcp://` — comes from
/// `[daemon] socket` and is picked inside [`crate::transport`]; the startup
/// log names the endpoint URL, which is self-describing (task 2.4). See
/// [`transport::Endpoint::bind`].
pub fn bind(sup: &Arc<Supervisor>) -> Result<transport::Listener> {
    let endpoint = {
        let st = sup.state.lock().unwrap();
        st.config
            .control_endpoint()
            .map_err(|e| anyhow::anyhow!("invalid daemon.socket: {e}"))?
    };
    let listener = endpoint.bind().map_err(|e| match e {
        transport::BindError::AlreadyRunning => anyhow::anyhow!(
            "another ak daemon is already running on control endpoint {}",
            endpoint.display()
        ),
        transport::BindError::PathOccupied => anyhow::anyhow!(
            "control endpoint {} is occupied by a file that is not a socket; move it away \
             (the daemon will not delete it for you)",
            endpoint.display()
        ),
        other => anyhow::anyhow!(
            "failed to bind control endpoint {}: {other}",
            endpoint.display()
        ),
    })?;
    info!("control API listening on {}", endpoint.display());
    Ok(listener)
}

/// Accept loop; run on a background thread after `bind` succeeded.
pub fn serve(sup: Arc<Supervisor>, mut listener: transport::Listener) {
    loop {
        match listener.accept() {
            Ok(stream) => {
                let sup = sup.clone();
                std::thread::Builder::new()
                    .name("api-conn".into())
                    .spawn(move || handle_connection(&sup, stream))
                    .ok();
            }
            Err(e) => {
                warn!("api accept error: {e}");
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    }
}

/// One connection's admission policy, derived from the daemon config
/// (tcp-control-endpoint D2/D3): the endpoint shape is fixed at daemon
/// startup, so every connection of a daemon shares one policy — there is no
/// mixed state.
enum Admission {
    /// Local IPC: admission was the kernel's (endpoint owner, decided at
    /// `connect()`); any `Authorization` header is noise and is ignored.
    OwnerOnly,
    /// TCP: a Bearer credential gate on everything except `/v1/health`.
    /// The token is non-empty in every runnable config (the combination
    /// check rejects `tcp://` without one at startup), but an empty token
    /// still fails closed below.
    Bearer(String),
}

/// Derive the admission policy for one connection from the live config.
fn admission_for(sup: &Supervisor) -> Admission {
    let st = sup.state.lock().unwrap();
    match st.config.control_endpoint() {
        Ok(ep) if ep.is_tcp() => {
            Admission::Bearer(st.config.daemon.auth_token.clone().unwrap_or_default())
        }
        _ => Admission::OwnerOnly,
    }
}

/// The Bearer check for the TCP form (tcp-control-endpoint D3): a
/// scheme-insensitive `Bearer` prefix, then a plain — non-constant-time —
/// token comparison, the same sensitivity class as the webui console token.
/// An empty expected token never matches (fail closed).
fn bearer_ok(headers: &HashMap<String, String>, expected: &str) -> bool {
    if expected.is_empty() {
        return false;
    }
    let Some(value) = headers.get("authorization") else {
        return false;
    };
    let Some((scheme, token)) = value.split_once(' ') else {
        return false;
    };
    scheme.eq_ignore_ascii_case("bearer") && token.trim() == expected
}

fn handle_connection(sup: &Arc<Supervisor>, stream: Box<dyn Conn>) {
    let mut writer = match stream.try_clone_conn() {
        Ok(w) => w,
        Err(_) => return,
    };
    let mut reader = BufReader::new(stream);

    // Request line.
    let mut line = String::new();
    if reader.read_line(&mut line).is_err() || line.is_empty() {
        return;
    }
    let mut parts = line.split_whitespace();
    let method = parts.next().unwrap_or("").to_string();
    let target = parts.next().unwrap_or("").to_string();
    if method.is_empty() || target.is_empty() {
        let _ = writer.write_all(&err_bytes(400, "bad request"));
        return;
    }

    // Headers (bounded). Only /v1/apply accepts a body; elsewhere a small
    // body is drained.
    let mut total = 0usize;
    let mut headers = HashMap::new();
    loop {
        let mut h = String::new();
        if reader.read_line(&mut h).is_err() {
            return;
        }
        total += h.len();
        if total > MAX_HEADER_BYTES {
            let _ = writer.write_all(&err_bytes(431, "headers too large"));
            return;
        }
        let trimmed = h.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            break;
        }
        if let Some((k, v)) = trimmed.split_once(':') {
            headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
        }
    }
    let body: Vec<u8> = match headers
        .get("content-length")
        .and_then(|v| v.parse::<usize>().ok())
    {
        Some(len) if len > 0 => {
            if len > 64 * 1024 {
                let _ = writer.write_all(&err_bytes(431, "body too large"));
                return;
            }
            let mut buf = vec![0u8; len];
            if reader.read_exact(&mut buf).is_err() {
                return;
            }
            buf
        }
        _ => Vec::new(),
    };

    let (path, query) = match target.split_once('?') {
        Some((p, q)) => (p.to_string(), q.to_string()),
        None => (target.clone(), String::new()),
    };

    let segments: Vec<&str> = path.trim_matches('/').split('/').collect();

    // Admission gate (tcp-control-endpoint D3, tasks 2.2/2.3): on the TCP
    // form every request except the health probe needs the Bearer token;
    // the 401 carries the fix in its body. On the IPC form the header is
    // ignored — the OS already decided who this is.
    if let Admission::Bearer(token) = admission_for(sup) {
        let exempt = method == "GET" && segments.as_slice() == ["v1", "health"];
        if !exempt && !bearer_ok(&headers, &token) {
            let _ = writer.write_all(&err_bytes(
                401,
                "unauthorized: a tcp:// control endpoint requires `Authorization: Bearer <token>` \
                 (the daemon's [daemon] auth_token) for every request except /v1/health",
            ));
            return;
        }
    }

    let resp: Vec<u8> = match (method.as_str(), segments.as_slice()) {
        ("GET", ["v1", "health"]) => json_bytes(200, serde_json::json!({"status": "ok"})),
        ("GET", ["v1", "status"]) => {
            let st = sup.state.lock().unwrap();
            let mut programs: Vec<ProgramInfo> = st
                .programs
                .values()
                .map(|p| program_info_state(&st, p, Some(&sup.metrics)))
                .collect();
            programs.sort_by(|a, b| (&a.app, &a.name).cmp(&(&b.app, &b.name)));
            json_bytes(
                200,
                StatusDoc {
                    daemon: daemon_info(&st, sup),
                    programs,
                    pending: st.pending.clone(),
                },
            )
        }
        // Console bootstrap endpoint (add-webui-launch-command D2): the only
        // projection that may carry the auto token, and only under the same
        // admission as every other `/v1/*` route.
        ("GET", ["v1", "webui"]) => json_bytes(200, webui_status_json(sup)),
        ("GET", ["v1", "programs"]) => {
            let st = sup.state.lock().unwrap();
            let mut programs: Vec<ProgramInfo> = st
                .programs
                .values()
                .map(|p| program_info_state(&st, p, Some(&sup.metrics)))
                .collect();
            programs.sort_by(|a, b| (&a.app, &a.name).cmp(&(&b.app, &b.name)));
            json_bytes(200, serde_json::json!({ "programs": programs }))
        }
        ("GET", ["v1", "programs", name]) => {
            let st = sup.state.lock().unwrap();
            match st.programs.get(*name) {
                Some(p) => json_bytes(200, program_info_state(&st, p, Some(&sup.metrics))),
                None => err_bytes(404, &format!("unknown program {name:?}")),
            }
        }
        (
            "POST",
            [
                "v1",
                "programs",
                name,
                action @ ("start" | "stop" | "restart"),
            ],
        ) => {
            let (tx, rx) = mpsc::channel();
            let cmd = match *action {
                "start" => Command::Start {
                    name: name.to_string(),
                    reply: Some(tx),
                },
                "stop" => Command::Stop {
                    name: name.to_string(),
                    reply: Some(tx),
                },
                _ => Command::Restart {
                    name: name.to_string(),
                    reply: Some(tx),
                },
            };
            sup.enqueue(cmd);
            let wait = query
                .split('&')
                .any(|kv| kv == "wait=1" || kv == "wait=true");
            match rx.recv_timeout(Duration::from_secs(60)) {
                Ok(Ok(msg)) => {
                    // run-daemon-first: the CLI's `run <name>` blocks until the
                    // startup judgement (`?wait=1`); the shell/webui keep the
                    // immediate-return behavior (no query flag).
                    if wait && *action == "start" {
                        let estimate = startup_estimate_for(sup, name);
                        match crate::supervisor::wait_startup(sup, name, estimate) {
                            crate::supervisor::StartupOutcome::Started { .. } => {}
                            crate::supervisor::StartupOutcome::Failed { phase, reason } => {
                                let _ = writer
                                    .write_all(&err_bytes(409, &format!("{phase}: {reason}")));
                                return;
                            }
                            crate::supervisor::StartupOutcome::TimedOut { waited_secs } => {
                                let _ = writer.write_all(&err_bytes(
                                    500,
                                    &format!("startup was not confirmed within {waited_secs:.1}s"),
                                ));
                                return;
                            }
                        }
                    }
                    // run-daemon-first D7: keep the legacy `result` text AND add
                    // structured per-program info {name, config_path, pid}.
                    let st = sup.state.lock().unwrap();
                    let (config_path, pid) = st
                        .programs
                        .get(*name)
                        .map(|p| (app_config_path(&st, &p.def.app), p.pid()))
                        .unwrap_or((None, None));
                    json_bytes(
                        200,
                        serde_json::json!({
                            "result": msg,
                            "name": name,
                            "config_path": config_path,
                            "pid": pid,
                        }),
                    )
                }
                Ok(Err(e)) => {
                    // "already running" style refusals are 409; an unknown
                    // program name is a missing target and must be 404.
                    let msg = format!("{e:#}");
                    let code = if msg.starts_with("unknown program") {
                        404
                    } else {
                        409
                    };
                    err_bytes(code, &msg)
                }
                Err(_) => err_bytes(500, "supervisor did not answer in time"),
            }
        }
        // signal 内置动作：投递白名单信号到该程序的子进程。
        ("POST", ["v1", "programs", name, "signal"]) => post_signal(sup, name, &body),
        // 自定义动作：worker 线程同步执行，等待至完成或超时。
        ("POST", ["v1", "programs", name, "actions", action]) => {
            run_custom_action(sup, name, action)
        }
        // app 级扇出：服务端按排序规则展开为逐程序动作。
        ("POST", ["v1", "apps", name, verb @ ("start" | "stop" | "restart")]) => {
            let wait = query
                .split('&')
                .any(|kv| kv == "wait=1" || kv == "wait=true");
            post_app_fanout(sup, name, verb, wait)
        }
        // guard-remove-until-stopped D1: the one online removal path — the
        // Supervisor command owns the liveness guard, the record deletion and
        // the state cleanup (this thread never touches files or processes).
        ("POST", ["v1", "apps", name, "remove"]) => post_app_remove(sup, name),
        ("GET", ["v1", "programs", name, "logs"]) => {
            // Streaming response: owns the connection until the client leaves.
            respond_logs(sup, name, &query, Box::new(writer));
            return;
        }
        // run-daemon-first: the ad-hoc "register and start" endpoint. Streams
        // NDJSON progress frames (chunked) and owns the connection.
        ("POST", ["v1", "run"]) => {
            respond_adhoc_run(sup, &body, Box::new(writer));
            return;
        }
        // add-compose-up D3: the zero-side-effect preview half of the compose
        // up protocol (render + compare + classify on this thread).
        ("POST", ["v1", "compose", "up", "preview"]) => respond_compose_preview(sup, &body),
        // add-compose-up D3/D4: the execute half — the classification decision
        // runs in the command handler; the pipeline streams NDJSON progress
        // (the /v1/run transport mode) and owns the connection.
        ("POST", ["v1", "compose", "up"]) => {
            respond_compose_up(sup, &body, Box::new(writer));
            return;
        }
        // Residual-state confirmation (D4): commit a staged-but-unpersisted
        // app without restarting it.
        ("POST", ["v1", "run", "commit"]) => {
            let req: serde_json::Value = serde_json::from_slice(&body).unwrap_or_default();
            let Some(name) = req.get("name").and_then(|n| n.as_str()) else {
                let _ = writer.write_all(&err_bytes(400, "request body must carry \"name\""));
                return;
            };
            match commit_via_command(sup, name) {
                Ok(c) if c.partial => err_bytes(500, &c.partial_reason),
                Ok(c) => json_bytes(
                    200,
                    serde_json::json!({
                        "result": format!("app[{}] registered", c.app),
                        "program": c.program,
                        "config_path": c.config_path,
                        "pid": c.pid,
                        "unchanged": c.unchanged,
                    }),
                ),
                Err(e) => err_bytes(404, &format!("{e:#}")),
            }
        }
        ("POST", ["v1", "run", "abort"]) => {
            let req: serde_json::Value = serde_json::from_slice(&body).unwrap_or_default();
            let Some(name) = req.get("name").and_then(|n| n.as_str()) else {
                let _ = writer.write_all(&err_bytes(400, "request body must carry \"name\""));
                return;
            };
            match abort_via_command(sup, name) {
                Ok(msg) => json_bytes(200, serde_json::json!({ "result": msg })),
                Err(e) => err_bytes(404, &format!("{e:#}")),
            }
        }
        ("POST", ["v1", "reload"]) => {
            let (tx, rx) = mpsc::channel();
            sup.enqueue(Command::Reload { reply: Some(tx) });
            match rx.recv_timeout(Duration::from_secs(60)) {
                // The reply text is the rendered preview; the structured
                // doc rides along in state.pending (same source as /v1/pending).
                Ok(Ok(msg)) => {
                    let pending = sup.state.lock().unwrap().pending.clone();
                    json_bytes(
                        200,
                        serde_json::json!({ "result": msg, "pending": pending }),
                    )
                }
                Ok(Err(e)) => err_bytes(400, &format!("{e:#}")),
                Err(_) => err_bytes(500, "supervisor did not answer in time"),
            }
        }
        ("GET", ["v1", "pending"]) => {
            let pending = sup.state.lock().unwrap().pending.clone();
            json_bytes(200, pending)
        }
        ("POST", ["v1", "apply"]) => {
            let req: serde_json::Value = serde_json::from_slice(&body).unwrap_or_default();
            let app = req.get("app").and_then(|v| v.as_str()).map(String::from);
            let program = req
                .get("program")
                .and_then(|v| v.as_str())
                .map(String::from);
            let restart = req
                .get("restart")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            // Scope validation + the `all` keyword resolve in the supervisor
            // (single source shared with /api/apply).
            let scope = match crate::supervisor::resolve_apply_scope(
                sup,
                app.as_deref(),
                program.as_deref(),
            ) {
                Ok(s) => s,
                Err(msg) => {
                    let _ = writer.write_all(&err_bytes(404, &msg));
                    return;
                }
            };
            let (tx, rx) = mpsc::channel();
            sup.enqueue(Command::Apply {
                scope,
                restart,
                reply: Some(tx),
            });
            match rx.recv_timeout(Duration::from_secs(120)) {
                Ok(Ok(msg)) => json_bytes(200, serde_json::json!({ "result": msg })),
                Ok(Err(e)) => err_bytes(409, &format!("{e:#}")),
                Err(_) => err_bytes(500, "supervisor did not answer in time"),
            }
        }
        ("POST", ["v1", "shutdown"]) => {
            // 服务托管时的停机命令语义 (harden-daemon-autostart 4.4): a
            // service-managed daemon refuses here, BEFORE anything is
            // enqueued — no program stops, the daemon does not exit; the
            // answer names the stop command the service manager owns. This
            // is the single source both /v1 and the webui's shared gate ride.
            let managed = sup.state.lock().unwrap().managed_by;
            if let Some(source) = managed {
                let _ =
                    writer.write_all(&err_bytes(409, &crate::service::managed_stop_hint(source)));
                return;
            }
            let (tx, rx) = mpsc::channel();
            let (ftx, frx) = mpsc::channel();
            sup.enqueue(Command::Shutdown {
                reply: Some(tx),
                flushed: Some(frx),
            });
            let _ = rx.recv_timeout(Duration::from_secs(5));
            // Write the response BEFORE acknowledging: the supervisor waits on
            // the ack, so the bytes are on the wire before the process exits
            // (otherwise the client sees an EOF and reports exit 3).
            let resp = json_bytes(200, serde_json::json!({ "result": "shutting down" }));
            let _ = writer.write_all(&resp);
            let _ = writer.flush();
            let _ = ftx.send(());
            return;
        }
        _ => err_bytes(404, &format!("no route {method} {path}")),
    };
    let _ = writer.write_all(&resp);
}

// -- action / signal / fan-out endpoints -------------------------------------

/// `POST /v1/programs/{name}/signal` — deliver one whitelist signal to the
/// program's child. Errors map: bad signal name → 400, unknown program →
/// 404, no live child → 409 (incl. the windows "not supported" refusal).
fn post_signal(sup: &Arc<Supervisor>, name: &str, body: &[u8]) -> Vec<u8> {
    let req: serde_json::Value = serde_json::from_slice(body).unwrap_or_default();
    let sig_name = req
        .get("signal")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .unwrap_or("");
    if sig_name.is_empty() {
        return err_bytes(400, "request body must be {\"signal\": \"NAME\"}");
    }
    let sig = match crate::platform::Signal::parse(sig_name) {
        Ok(s) => s,
        Err(msg) => return err_bytes(400, &msg),
    };
    match crate::supervisor::signal_program(sup, name, sig) {
        Ok(msg) => json_bytes(200, serde_json::json!({ "result": msg })),
        Err(e) => err_bytes(e.http_status(), &e.to_string()),
    }
}

/// `POST /v1/programs/{name}/actions/{action}` — run a declared custom
/// action on a worker thread and wait synchronously until it finishes or
/// times out. Errors map: unknown program/action → 404, same action already
/// running → 409. A timed-out action is reported in the result body
/// (`timed_out: true`) — the tree was killed, the daemon stayed responsive.
fn run_custom_action(sup: &Arc<Supervisor>, program: &str, action: &str) -> Vec<u8> {
    let (spec, ctx) = {
        let st = sup.state.lock().unwrap();
        let Some(p) = st.programs.get(program) else {
            return err_bytes(404, &format!("unknown program {program:?}"));
        };
        let Some(spec) = p.def.actions.get(action) else {
            return err_bytes(
                404,
                &format!("program {program:?} declares no action {action:?}"),
            );
        };
        (spec.clone(), crate::supervisor::run_context(&st))
    };
    let Ok(guard) = sup.actions.try_begin(program, action) else {
        return err_bytes(
            409,
            &format!(
                "action {program}.{action} is already running (mutual exclusion, no queueing)"
            ),
        );
    };
    let (tx, rx) = mpsc::channel();
    let (prog, act) = (program.to_string(), action.to_string());
    let work_dir = ctx
        .programs
        .get(program)
        .map(|v| v.work_dir.clone())
        .unwrap_or_default();
    let timeout_secs = spec.timeout;
    // Worker thread (design D1): substitute at spawn time, run the shell,
    // reap — the supervisor loop and command queue are never involved.
    let spawned = std::thread::Builder::new()
        .name("action-worker".into())
        .spawn(move || {
            let command = ctx.substitute(&spec.command);
            let result = crate::action::execute(&spec, &prog, &act, &command, &work_dir);
            let _ = tx.send(result);
            drop(guard); // free the (program, action) slot after reaping
        })
        .is_ok();
    if !spawned {
        return err_bytes(500, "failed to spawn action worker");
    }
    let wait = Duration::from_secs(timeout_secs.max(1))
        .saturating_add(Duration::from_secs(15))
        .min(Duration::from_secs(3600));
    match rx.recv_timeout(wait) {
        Ok(result) => json_bytes(200, result),
        Err(_) => err_bytes(500, "action worker did not answer in time"),
    }
}

/// `POST /v1/apps/{name}/start|stop|restart` — app-level fan-out expanded
/// by the supervisor in the established order. Unknown app → 404.
fn post_app_fanout(sup: &Arc<Supervisor>, app: &str, verb: &str, wait: bool) -> Vec<u8> {
    let kind = match verb {
        "start" => crate::supervisor::FanoutKind::Start,
        "stop" => crate::supervisor::FanoutKind::Stop,
        _ => crate::supervisor::FanoutKind::Restart,
    };
    let (tx, rx) = mpsc::channel();
    sup.enqueue(Command::AppFanout {
        kind,
        app: app.to_string(),
        reply: Some(tx),
    });
    match rx.recv_timeout(Duration::from_secs(120)) {
        Ok(Ok(json)) => match serde_json::from_str::<crate::supervisor::FanoutResult>(&json) {
            Ok(fr) => {
                // run-daemon-first: `start <app>` (wait=1) blocks per program until its
                // startup judgement; the shell/webui keep the immediate reply.
                if wait && kind == crate::supervisor::FanoutKind::Start {
                    for s in &fr.programs {
                        if s.result.starts_with("error") {
                            continue; // already failed to start
                        }
                        let estimate = startup_estimate_for(sup, &s.program);
                        match crate::supervisor::wait_startup(sup, &s.program, estimate) {
                            crate::supervisor::StartupOutcome::Started { .. } => {}
                            crate::supervisor::StartupOutcome::Failed { phase, reason } => {
                                // Record the per-program failure in the reply.
                                let _ = (phase, reason);
                            }
                            crate::supervisor::StartupOutcome::TimedOut { .. } => {}
                        }
                    }
                }
                // run-daemon-first D7: augment each step with {name, config_path,
                // pid} while keeping the legacy `programs` steps and `result`.
                let st = sup.state.lock().unwrap();
                let enriched: Vec<serde_json::Value> = fr
                    .programs
                    .iter()
                    .map(|s| {
                        let (config_path, pid, state) = st
                            .programs
                            .get(&s.program)
                            .map(|p| {
                                (
                                    app_config_path(&st, &p.def.app),
                                    p.pid(),
                                    p.state().to_string(),
                                )
                            })
                            .unwrap_or((None, None, String::new()));
                        // With `wait=1`, reclassify a program that did not reach
                        // Running as a failure so the CLI exits non-zero, and
                        // refresh a success text to the post-wait state.
                        let failed = wait
                            && kind == crate::supervisor::FanoutKind::Start
                            && !s.result.starts_with("error")
                            && state != "running"
                            && state != "starting";
                        let result = if failed {
                            format!(
                                "error: program {} is {state} (startup not confirmed)",
                                s.program
                            )
                        } else if wait
                            && kind == crate::supervisor::FanoutKind::Start
                            && state == "running"
                        {
                            format!("program {} is running", s.program)
                        } else {
                            s.result.clone()
                        };
                        serde_json::json!({
                            "program": s.program,
                            "result": result,
                            "name": s.program,
                            "config_path": config_path,
                            "pid": pid,
                        })
                    })
                    .collect();
                let failed = enriched.iter().any(|p| {
                    p["result"]
                        .as_str()
                        .map(|r| r.starts_with("error"))
                        .unwrap_or(false)
                });
                json_bytes(
                    200,
                    serde_json::json!({
                        "result": crate::supervisor::render_fanout(&fr),
                        "app": fr.app,
                        "action": fr.action,
                        "programs": enriched,
                        "ok": !failed,
                    }),
                )
            }
            Err(_) => err_bytes(500, "unreadable fan-out result"),
        },
        Ok(Err(e)) => err_bytes(404, &format!("{e:#}")),
        Err(_) => err_bytes(500, "supervisor did not answer in time"),
    }
}

/// `POST /v1/apps/{name}/remove` — the one online removal path
/// (guard-remove-until-stopped). Admission, status mapping and the result
/// shape are the Supervisor's; this function only marshals the command and
/// renders the shared outcome, so the `/api` endpoint stays isomorphic.
///
/// Status mapping: unknown app → 404, a live/backoff program → 409,
/// a registration-deletion failure → 500 (nothing was cleaned up).
fn post_app_remove(sup: &Arc<Supervisor>, app: &str) -> Vec<u8> {
    let (tx, rx) = mpsc::channel();
    sup.enqueue(Command::Remove {
        app: app.to_string(),
        reply: tx,
    });
    match rx.recv_timeout(Duration::from_secs(120)) {
        Ok(Ok(outcome)) => json_bytes(200, remove_json(&outcome)),
        Ok(Err(e)) => err_bytes(e.http_status(), &e.to_string()),
        Err(_) => err_bytes(500, "supervisor did not answer in time"),
    }
}

/// The wire shape of one successful removal, shared by `/v1` and `/api`.
pub(crate) fn remove_json(o: &crate::supervisor::RemoveOutcome) -> serde_json::Value {
    serde_json::json!({
        "app": o.app,
        "config_path": o.config_path,
        "deleted_file": o.deleted_file,
        "kept_config": o.kept_config,
        "already_unregistered": o.already_unregistered,
        "programs": o.programs,
        "applied": o.applied,
        "result": o.render(),
    })
}

// -- ad-hoc register-and-start (run-daemon-first) ---------------------------

/// Parse the `/v1/run` request body into an [`AdhocSpec`]. The CLI has already
/// resolved `command` to an absolute path; everything else rides as-is.
fn parse_adhoc_spec(body: &[u8]) -> std::result::Result<crate::supervisor::AdhocSpec, String> {
    let v: serde_json::Value =
        serde_json::from_slice(body).map_err(|e| format!("invalid JSON body: {e}"))?;
    let command = v
        .get("command")
        .and_then(|c| c.as_str())
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| "request body must carry a non-empty \"command\"".to_string())?;
    let args = v
        .get("args")
        .and_then(|a| a.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    let env = v
        .get("env")
        .and_then(|e| e.as_object())
        .map(|o| {
            o.iter()
                .filter_map(|(k, val)| val.as_str().map(|s| (k.clone(), s.to_string())))
                .collect()
        })
        .unwrap_or_default();
    let restart = match v.get("restart").and_then(|r| r.as_str()) {
        Some(s) => Some(crate::config::parse_restart_cli(s)?),
        None => None,
    };
    Ok(crate::supervisor::AdhocSpec {
        name: v.get("name").and_then(|n| n.as_str()).map(String::from),
        command: std::path::PathBuf::from(command),
        args,
        env,
        work_dir: v
            .get("work_dir")
            .and_then(|w| w.as_str())
            .filter(|s| !s.is_empty())
            .map(std::path::PathBuf::from),
        description: v
            .get("description")
            .and_then(|d| d.as_str())
            .map(String::from),
        restart,
        restart_backoff: v.get("restart_backoff").and_then(|b| b.as_f64()),
        health_check: v
            .get("health_check")
            .and_then(|h| h.as_str())
            .filter(|s| !s.trim().is_empty())
            .map(String::from),
        register_only: v
            .get("register_only")
            .and_then(|b| b.as_bool())
            .unwrap_or(false),
        force: v.get("force").and_then(|b| b.as_bool()).unwrap_or(false),
    })
}

/// Write one NDJSON frame as a chunked-transfer chunk (line + CRLF).
fn write_ndjson_chunk(writer: &mut dyn Write, value: &serde_json::Value) -> bool {
    let line = value.to_string();
    let body = line.as_bytes();
    let head = format!("{:x}\r\n", body.len() + 1);
    writer.write_all(head.as_bytes()).is_ok()
        && writer.write_all(body).is_ok()
        && writer.write_all(b"\n").is_ok()
        && writer.write_all(b"\r\n").is_ok()
        && writer.flush().is_ok()
}

/// Finish a chunked response (zero-length terminator).
fn finish_chunked(writer: &mut dyn Write) {
    let _ = writer.write_all(b"0\r\n\r\n");
    let _ = writer.flush();
}

/// `POST /v1/run` — ad-hoc register-and-start (run-daemon-first D3/D5).
///
/// Streams NDJSON progress frames over `Transfer-Encoding: chunked` and owns
/// the connection:
///
/// ```text
/// {"phase":"staged","estimate_secs":3}
/// {"phase":"waiting_startsecs"}
/// {"phase":"waiting_health","estimate_secs":35}
/// {"ok":true,"program":"web","config_path":"...","pid":1234}
/// ```
///
/// The startup judgement (polling until `Running && health_confirmed`) runs
/// HERE, on the connection thread — the supervision loop only does the fast
/// validate/stage/spawn (`AdhocUp`) and the commit/abort.
fn respond_adhoc_run(sup: &Arc<Supervisor>, body: &[u8], mut writer: Box<dyn Write>) {
    let spec = match parse_adhoc_spec(body) {
        Ok(s) => s,
        Err(msg) => {
            let _ = writer.write_all(&err_bytes(400, &msg));
            return;
        }
    };
    // Phase 1: fast validate → stage → spawn on the supervision loop. This
    // happens BEFORE the chunked head so validation/stage failures can answer
    // a proper status code (400/409/500) instead of a 200 stream (task 5.5).
    let (tx, rx) = mpsc::channel();
    sup.enqueue(Command::AdhocUp { spec, reply: tx });
    let up = match rx.recv_timeout(Duration::from_secs(30)) {
        Ok(Ok(u)) => u,
        Ok(Err(e)) => {
            let body = serde_json::json!({ "error": e.to_string(), "phase": e.phase() });
            let _ = writer.write_all(&json_bytes(e.http_status(), body));
            return;
        }
        Err(_) => {
            let _ = writer.write_all(&err_bytes(500, "supervisor did not answer in time"));
            return;
        }
    };
    // From here on the response is a chunked NDJSON stream.
    let head = "HTTP/1.1 200 OK\r\nContent-Type: application/x-ndjson\r\n\
                Transfer-Encoding: chunked\r\nConnection: close\r\n\r\n";
    if writer.write_all(head.as_bytes()).is_err() || writer.flush().is_err() {
        return;
    }

    // Residual state (D4): the app is already started but unpersisted. Do not
    // restart; let the CLI prompt and call /v1/run/commit or abort.
    if up.residual {
        let _ = write_ndjson_chunk(
            &mut writer,
            &serde_json::json!({
                "ok": false,
                "phase": "residual",
                "residual": true,
                "program": up.program,
                "config_path": up.config_path,
                "pid": up.pid,
                "error": "the program is already started but not registered",
            }),
        );
        finish_chunked(&mut writer);
        return;
    }

    // `--force` + byte-identical record (refuse-run-existing-app D6): nothing
    // was written and nothing was started or stopped. No staged entry exists
    // and no startup judgement applies — answer the terminal frame directly.
    if up.unchanged {
        let _ = write_ndjson_chunk(
            &mut writer,
            &serde_json::json!({
                "ok": true,
                "program": up.program,
                "config_path": up.config_path,
                "pid": up.pid,
                "unchanged": true,
                "partial": false,
                "error": "",
                "config_text": "",
            }),
        );
        finish_chunked(&mut writer);
        return;
    }

    // `--restart=no` (D8): no startup judgement. A `--force` overwrite landed
    // the record before this point (D7), so there is no placeholder to commit;
    // a fresh registration still commits here.
    if up.register_only {
        if up.landed {
            let _ = write_ndjson_chunk(
                &mut writer,
                &serde_json::json!({
                    "ok": true,
                    "program": up.program,
                    "config_path": up.config_path,
                    "pid": serde_json::Value::Null,
                    "unchanged": false,
                    "partial": false,
                    "error": "",
                    "config_text": "",
                }),
            );
            finish_chunked(&mut writer);
            return;
        }
        let result = commit_via_command(sup, &up.app);
        match result {
            Ok(c) => {
                let _ = write_ndjson_chunk(
                    &mut writer,
                    &serde_json::json!({
                        "ok": !c.partial,
                        "program": c.program,
                        "config_path": c.config_path,
                        "pid": serde_json::Value::Null,
                        "unchanged": c.unchanged,
                        "partial": c.partial,
                        "error": if c.partial { c.partial_reason.clone() } else { String::new() },
                        "config_text": if c.partial { c.config_text.clone() } else { String::new() },
                    }),
                );
            }
            Err(e) => {
                let _ = write_ndjson_chunk(
                    &mut writer,
                    &serde_json::json!({
                        "ok": false,
                        "phase": "commit",
                        "error": format!("{e:#}"),
                    }),
                );
            }
        }
        finish_chunked(&mut writer);
        return;
    }

    // Phase 2: stream progress, then wait for the startup judgement. Per the
    // control-plane spec ("无健康探测时 SHALL 直接回传最终结果，无需中间帧")
    // the intermediate frames are emitted only when a health check exists;
    // without one the endpoint answers the terminal frame alone.
    if up.has_health {
        let _ = write_ndjson_chunk(
            &mut writer,
            &serde_json::json!({
                "phase": "staged",
                "estimate_secs": up.estimate_secs,
                "program": up.program,
            }),
        );
        let _ = write_ndjson_chunk(
            &mut writer,
            &serde_json::json!({ "phase": "waiting_startsecs" }),
        );
        let _ = write_ndjson_chunk(
            &mut writer,
            &serde_json::json!({
                "phase": "waiting_health",
                "estimate_secs": up.estimate_secs,
            }),
        );
    }

    let estimate = Duration::from_secs_f64(up.estimate_secs.max(0.1) + 5.0);
    let outcome = crate::supervisor::wait_startup(sup, &up.program, estimate);
    match outcome {
        crate::supervisor::StartupOutcome::Started { pid } => {
            // A `--force` overwrite already landed the record (D7): there is no
            // staged entry to commit — answer directly with the judged pid.
            if up.landed {
                let _ = write_ndjson_chunk(
                    &mut writer,
                    &serde_json::json!({
                        "ok": true,
                        "program": up.program,
                        "config_path": up.config_path,
                        "pid": pid,
                        "unchanged": false,
                        "partial": false,
                        "error": "",
                        "config_text": "",
                    }),
                );
                finish_chunked(&mut writer);
                return;
            }
            let result = commit_via_command(sup, &up.app);
            match result {
                Ok(c) => {
                    let _ = write_ndjson_chunk(
                        &mut writer,
                        &serde_json::json!({
                            "ok": !c.partial,
                            "program": c.program,
                            "config_path": c.config_path,
                            "pid": pid,
                            "unchanged": c.unchanged,
                            "partial": c.partial,
                            "error": if c.partial { c.partial_reason.clone() } else { String::new() },
                            "config_text": if c.partial { c.config_text.clone() } else { String::new() },
                        }),
                    );
                }
                Err(e) => {
                    let _ = write_ndjson_chunk(
                        &mut writer,
                        &serde_json::json!({
                            "ok": false,
                            "phase": "commit",
                            "error": format!("{e:#}"),
                        }),
                    );
                }
            }
        }
        crate::supervisor::StartupOutcome::Failed { phase, reason } => {
            // Startup judgement failed: abort (stop process, delete
            // placeholder, clear staged) and report the structured failure.
            // A `--force` overwrite has no staged entry: its new record stays
            // on disk (the overwrite already took effect) and the failed
            // process is simply stopped (refuse-run-existing-app D7).
            if up.landed {
                let _ = stop_via_command(sup, &up.app);
            } else {
                let _ = abort_via_command(sup, &up.app);
            }
            let _ = write_ndjson_chunk(
                &mut writer,
                &serde_json::json!({
                    "ok": false,
                    "phase": phase,
                    "error": reason,
                }),
            );
        }
        crate::supervisor::StartupOutcome::TimedOut { waited_secs } => {
            if up.landed {
                let _ = stop_via_command(sup, &up.app);
            } else {
                let _ = abort_via_command(sup, &up.app);
            }
            let _ = write_ndjson_chunk(
                &mut writer,
                &serde_json::json!({
                    "ok": false,
                    "phase": "timeout",
                    "error": format!(
                        "startup was not confirmed within {:.1}s (estimated upper bound)",
                        waited_secs
                    ),
                }),
            );
        }
    }
    finish_chunked(&mut writer);
}

/// Run an `AdhocCommit` through the command queue (the single writer).
fn commit_via_command(
    sup: &Arc<Supervisor>,
    name: &str,
) -> Result<crate::supervisor::AdhocCommitResult> {
    let (tx, rx) = mpsc::channel();
    sup.enqueue(Command::AdhocCommit {
        name: name.to_string(),
        reply: tx,
    });
    match rx.recv_timeout(Duration::from_secs(60)) {
        Ok(r) => r,
        Err(_) => anyhow::bail!("supervisor did not answer the commit in time"),
    }
}

/// Run an `AdhocAbort` through the command queue; best-effort.
fn abort_via_command(sup: &Arc<Supervisor>, name: &str) -> Result<String> {
    let (tx, rx) = mpsc::channel();
    sup.enqueue(Command::AdhocAbort {
        name: name.to_string(),
        reply: tx,
    });
    match rx.recv_timeout(Duration::from_secs(30)) {
        Ok(r) => r,
        Err(_) => anyhow::bail!("supervisor did not answer the abort in time"),
    }
}

/// Stop a program through the command queue; best-effort. Used when a
/// `--force` overwritten ad-hoc program fails its startup judgement: there is
/// no staged entry to abort, so the new record stays and only the failed
/// process is stopped.
fn stop_via_command(sup: &Arc<Supervisor>, name: &str) -> Result<String> {
    let (tx, rx) = mpsc::channel();
    sup.enqueue(Command::Stop {
        name: name.to_string(),
        reply: Some(tx),
    });
    match rx.recv_timeout(Duration::from_secs(30)) {
        Ok(r) => r,
        Err(_) => anyhow::bail!("supervisor did not answer the stop in time"),
    }
}

// -- compose up (add-compose-up D2/D3) ----------------------------------------

/// Parse the `/v1/compose/up(/preview)` request body. The request carries the
/// declaration CONTENT, the base directory, the caller's PATH and the app
/// name — never a file path (control-plane spec: TCP 形态下 CLI 与守护进程可
/// 不在同一台机器).
fn parse_compose_request(
    body: &[u8],
) -> std::result::Result<crate::supervisor::ComposeUpRequest, String> {
    let v: serde_json::Value =
        serde_json::from_slice(body).map_err(|e| format!("invalid JSON body: {e}"))?;
    let name = v
        .get("name")
        .and_then(|n| n.as_str())
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| "request body must carry a non-empty \"name\"".to_string())?;
    let content = v.get("content").and_then(|c| c.as_str()).ok_or_else(|| {
        "request body must carry the declaration \"content\" (a file path is not accepted — \
         the client and the daemon may live on different machines)"
            .to_string()
    })?;
    let base_dir = v
        .get("base_dir")
        .and_then(|b| b.as_str())
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| {
            "request body must carry a non-empty \"base_dir\" (the declaration file's \
             directory, used to absolutize relative paths)"
                .to_string()
        })?;
    Ok(crate::supervisor::ComposeUpRequest {
        name: name.to_string(),
        content: content.to_string(),
        base_dir: std::path::PathBuf::from(base_dir),
        caller_path: v
            .get("caller_path")
            .and_then(|p| p.as_str())
            .filter(|s| !s.is_empty())
            .map(std::ffi::OsString::from),
        force: v.get("force").and_then(|b| b.as_bool()).unwrap_or(false),
        vars: v
            .get("vars")
            .and_then(|m| m.as_object())
            .map(|m| {
                m.iter()
                    .filter_map(|(k, val)| val.as_str().map(|s| (k.clone(), s.to_string())))
                    .collect()
            })
            .unwrap_or_default(),
    })
}

/// The wire shape of a compose preview, shared by the preview endpoint and
/// the conflict errors of the submit endpoint.
fn compose_preview_json(pv: &crate::supervisor::ComposePreview) -> serde_json::Value {
    serde_json::json!({
        "classification": pv.class.as_str(),
        "record_path": pv.record_path.display().to_string(),
        "declared": pv.declared,
        "diff": pv.diff,
        "hint": pv.hint,
        "programs": pv.programs,
        "warnings": pv.warnings,
    })
}

/// `POST /v1/compose/up/preview` — the zero-side-effect half of the protocol
/// (add-compose-up D3). Rendering + comparison + classification happen HERE on
/// the HTTP thread (all reads); a validation failure is a 400 and any of the
/// five classifications is a 200 with the details attached.
fn respond_compose_preview(sup: &Arc<Supervisor>, body: &[u8]) -> Vec<u8> {
    let req = match parse_compose_request(body) {
        Ok(r) => r,
        Err(m) => return err_bytes(400, &m),
    };
    let rendered = match crate::config::render_declaration_with_vars(
        &req.content,
        &req.base_dir,
        &req.name,
        req.caller_path.as_deref(),
        &req.vars,
    ) {
        Ok(t) => t,
        Err(e) => return err_bytes(400, &format!("{e:#}")),
    };
    let pv = match sup.compose_preview(&req.name, &rendered) {
        Ok(pv) => pv,
        Err(m) => return err_bytes(400, &m),
    };
    json_bytes(200, compose_preview_json(&pv))
}

/// `POST /v1/compose/up` — the execute half (add-compose-up D3/D4). The
/// classification decision runs in the command handler (MUST NOT rely on a
/// client pre-check); the pipeline streams NDJSON progress frames over a
/// chunked response (the `/v1/run` transport mode):
///
/// ```text
/// {"phase":"staged","app":"shop","programs":["web","worker"],"skipped":[]}
/// {"phase":"starting","program":"web"}
/// {"phase":"starting","program":"worker"}
/// {"phase":"started","program":"web","pid":123}
/// {"phase":"started","program":"worker","pid":124}
/// {"ok":true,"app":"shop","config_path":"...","programs":[...]}
/// ```
///
/// The multi-program startup wait runs HERE on the connection thread (each
/// program against its own deadline); only when ALL pass is the record
/// committed. Any failure aborts server-side: the spawned programs are
/// stopped in reverse, the placeholder is discarded, and the structured
/// error names the failed phase and program.
fn respond_compose_up(sup: &Arc<Supervisor>, body: &[u8], mut writer: Box<dyn Write>) {
    let req = match parse_compose_request(body) {
        Ok(r) => r,
        Err(m) => {
            let _ = writer.write_all(&err_bytes(400, &m));
            return;
        }
    };
    // Phase 1: the fast turn (validate → classify → stage → spawn) on the
    // supervision loop. Refusals answer proper status codes BEFORE the chunked
    // head, with the preview details attached for the conflict classes.
    let (tx, rx) = mpsc::channel();
    sup.enqueue(Command::ComposeUp { req, reply: tx });
    let staged = match rx.recv_timeout(Duration::from_secs(60)) {
        Ok(Ok(s)) => s,
        Ok(Err(e)) => {
            let status = e.http_status();
            match e.preview() {
                Some(pv) => {
                    let mut body = compose_preview_json(pv);
                    body["error"] = serde_json::Value::String(e.to_string());
                    body["phase"] = serde_json::Value::String(e.phase().into());
                    let _ = writer.write_all(&json_bytes(status, body));
                }
                None => {
                    let body = serde_json::json!({ "error": e.to_string(), "phase": e.phase() });
                    let _ = writer.write_all(&json_bytes(status, body));
                }
            }
            return;
        }
        Err(_) => {
            let _ = writer.write_all(&err_bytes(500, "supervisor did not answer in time"));
            return;
        }
    };

    // From here on the response is a chunked NDJSON stream.
    let head = "HTTP/1.1 200 OK\r\nContent-Type: application/x-ndjson\r\n\
                Transfer-Encoding: chunked\r\nConnection: close\r\n\r\n";
    if writer.write_all(head.as_bytes()).is_err() || writer.flush().is_err() {
        return;
    }

    let max_estimate = staged
        .waits
        .iter()
        .map(|w| w.estimate_secs)
        .fold(0.0_f64, f64::max);

    // `identical` with nothing to pull up: the no-operation result (D5).
    if staged.identical && staged.unchanged {
        let _ = write_ndjson_chunk(
            &mut writer,
            &serde_json::json!({
                "ok": true,
                "app": staged.app,
                "config_path": staged.config_path,
                "unchanged": true,
                "identical": true,
                "programs": [],
                "skipped": staged.skipped,
                "warnings": staged.warnings,
            }),
        );
        finish_chunked(&mut writer);
        return;
    }

    let _ = write_ndjson_chunk(
        &mut writer,
        &serde_json::json!({
            "phase": if staged.identical { "ensuring" } else { "staged" },
            "app": staged.app,
            "estimate_secs": max_estimate,
            "programs": staged.waits.iter().map(|w| w.program.clone()).collect::<Vec<_>>(),
            "skipped": staged.skipped,
        }),
    );
    for w in &staged.waits {
        let _ = write_ndjson_chunk(
            &mut writer,
            &serde_json::json!({ "phase": "starting", "program": w.program }),
        );
    }

    // Phase 2: wait for every startup judgement (per-program deadlines).
    let mut tracker = crate::supervisor::ComposeWaitTracker::new(&staged.waits);
    let mut failure: Option<(String, crate::supervisor::StartupOutcome)> = None;
    let mut started: Vec<serde_json::Value> = Vec::new();
    while !tracker.finished() {
        for (program, outcome) in tracker.poll(sup) {
            match outcome {
                crate::supervisor::StartupOutcome::Started { pid } => {
                    let _ = write_ndjson_chunk(
                        &mut writer,
                        &serde_json::json!({
                            "phase": "started",
                            "program": program,
                            "pid": pid,
                        }),
                    );
                    started.push(serde_json::json!({ "program": program, "pid": pid }));
                }
                crate::supervisor::StartupOutcome::Failed { phase, reason } => {
                    failure = Some((
                        program,
                        crate::supervisor::StartupOutcome::Failed { phase, reason },
                    ));
                }
                crate::supervisor::StartupOutcome::TimedOut { waited_secs } => {
                    failure = Some((
                        program,
                        crate::supervisor::StartupOutcome::Failed {
                            phase: "timeout",
                            reason: format!(
                                "startup was not confirmed within {waited_secs:.1}s (estimated upper bound)"
                            ),
                        },
                    ));
                }
            }
            if failure.is_some() {
                break;
            }
        }
        if failure.is_some() {
            break;
        }
        std::thread::sleep(crate::supervisor::STARTUP_POLL);
    }

    if let Some((program, outcome)) = failure {
        let crate::supervisor::StartupOutcome::Failed { phase, reason } = outcome else {
            unreachable!("failure is only set to Failed above");
        };
        // Server-side abort (D4): stop what this round spawned (reverse order
        // — the shared staged abort) and discard the placeholder. An
        // `identical` submission staged nothing, so there is nothing to abort.
        if !staged.identical {
            let _ = abort_via_command(sup, &staged.app);
        }
        let _ = write_ndjson_chunk(
            &mut writer,
            &serde_json::json!({
                "ok": false,
                "phase": phase,
                "program": program,
                "error": reason,
            }),
        );
        finish_chunked(&mut writer);
        return;
    }

    if staged.identical {
        // Ensure-running finished: the record was never touched.
        let _ = write_ndjson_chunk(
            &mut writer,
            &serde_json::json!({
                "ok": true,
                "app": staged.app,
                "config_path": staged.config_path,
                "unchanged": false,
                "identical": true,
                "programs": started,
                "skipped": staged.skipped,
            }),
        );
        finish_chunked(&mut writer);
        return;
    }

    // Phase 3: all judgements passed — commit (placeholder → record).
    match commit_via_command(sup, &staged.app) {
        Ok(c) => {
            let _ = write_ndjson_chunk(
                &mut writer,
                &serde_json::json!({
                    "ok": !c.partial,
                    "app": c.app,
                    "config_path": c.config_path,
                    "unchanged": c.unchanged,
                    "partial": c.partial,
                    "error": if c.partial { c.partial_reason.clone() } else { String::new() },
                    "programs": started,
                    "skipped": staged.skipped,
                }),
            );
        }
        Err(e) => {
            let _ = write_ndjson_chunk(
                &mut writer,
                &serde_json::json!({
                    "ok": false,
                    "phase": "commit",
                    "error": format!("{e:#}"),
                }),
            );
        }
    }
    finish_chunked(&mut writer);
}

fn respond_logs(sup: &Arc<Supervisor>, name: &str, query: &str, mut writer: Box<dyn Write>) {
    let mut stream = "out";
    let mut tail = 100usize;
    let mut follow = false;
    for kv in query.split('&') {
        let mut it = kv.splitn(2, '=');
        let k = it.next().unwrap_or("");
        let v = it.next().unwrap_or("");
        match k {
            "stream" if v == "err" => stream = "err",
            "stream" if v == "out" => stream = "out",
            "tail" => tail = v.parse().unwrap_or(100),
            "follow" => follow = v == "1" || v == "true",
            _ => {}
        }
    }
    let ring = {
        let st = sup.state.lock().unwrap();
        match st.programs.get(name) {
            Some(p) => Some(p.ring(if stream == "err" {
                crate::pump::Stream::Err
            } else {
                crate::pump::Stream::Out
            })),
            None => None,
        }
    };
    let Some(ring) = ring else {
        let _ = writer.write_all(&err_bytes(404, &format!("unknown program {name:?}")));
        return;
    };

    if !follow {
        let lines = ring.tail(tail);
        let _ = writer.write_all(&json_bytes(
            200,
            serde_json::json!({ "program": name, "stream": stream, "lines": lines }),
        ));
        return;
    }

    if FOLLOWS.load(Ordering::SeqCst) >= MAX_FOLLOWS {
        let _ = writer.write_all(&err_bytes(429, "too many concurrent log followers"));
        return;
    }
    FOLLOWS.fetch_add(1, Ordering::SeqCst);

    // Subscribe first, then snapshot: worst case a line appears twice, never
    // goes missing.
    let rx = ring.subscribe();
    let snapshot = ring.tail(tail);

    let head = "HTTP/1.1 200 OK\r\nContent-Type: text/plain; charset=utf-8\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n";
    if writer.write_all(head.as_bytes()).is_err() || writer.flush().is_err() {
        FOLLOWS.fetch_sub(1, Ordering::SeqCst);
        return;
    }

    let write_chunk = |writer: &mut dyn Write, line: &str| -> bool {
        let body = line.as_bytes();
        // Chunk data is the line plus one trailing '\n'; then the CRLF terminator.
        let head = format!("{:x}\r\n", body.len() + 1);
        writer.write_all(head.as_bytes()).is_ok()
            && writer.write_all(body).is_ok()
            && writer.write_all(b"\n").is_ok()
            && writer.write_all(b"\r\n").is_ok()
            && writer.flush().is_ok()
    };

    let mut client_gone = false;
    for l in snapshot {
        if !write_chunk(&mut writer, &l) {
            client_gone = true;
            break;
        }
    }
    if !client_gone {
        // Bounded subscription: Lines batches, Gap = this follower fell
        // behind and missed N lines (ring/file still complete — show an
        // explicit marker instead of silently lying).
        for item in rx {
            let ok = match item {
                crate::pump::SubItem::Lines(lines) => {
                    lines.iter().all(|l| write_chunk(&mut writer, l))
                }
                crate::pump::SubItem::Gap(n) => {
                    write_chunk(&mut writer, &format!("[[ak: skipped {n} lines]]"))
                }
            };
            if !ok {
                client_gone = true;
                break; // client disconnected
            }
        }
    }
    let _ = writer.write_all(b"0\r\n\r\n");
    let _ = writer.flush();
    FOLLOWS.fetch_sub(1, Ordering::SeqCst);
    if client_gone {
        info!("log follow for program[{name}] ended (client disconnected)");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{AppRaw, resolve_app};
    use crate::metrics::{ProgramMetrics, StreamRate, SystemMetrics};
    use crate::program::ManagedProgram;
    use std::path::Path;

    fn sample_doc() -> StatusDoc {
        let info = ProgramInfo {
            app: "app".into(),
            name: "a".into(),
            state: "running".into(),
            pid: Some(1),
            unhealthy: false,
            uptime_secs: 1.0,
            total_exits: 0,
            restart_backoff: 1.0,
            last_exit: None,
            fatal_reason: None,
            wait_reason: None,
            cpu_percent: Some(42.1),
            mem_bytes: Some(8192),
            log_rate: crate::metrics::LogRates {
                out: StreamRate {
                    w1: 5.0,
                    w10: 5.0,
                    w60: 4.9,
                    w300: 5.1,
                },
                err: StreamRate::default(),
            },
            command: "true".into(),
            args: vec![],
            work_dir: String::new(),
            config_path: Some("/etc/appkeeper/app.d/a.toml".into()),
            health_confirmed: true,
        };
        StatusDoc {
            daemon: DaemonInfo {
                version: "t".into(),
                build: BuildInfo {
                    git_commit: "abc1234".into(),
                    built_at: "2026-01-01T00:00:00Z".into(),
                    target: "x86_64-unknown-linux-gnu".into(),
                    rustc: "rustc 1.90.0 (x)".into(),
                },
                port: 0,
                socket: "/run/appkeeper/0/ak.sock".into(),
                apps: 1,
                system: SystemMetrics {
                    cpu_percent: Some(7.3),
                    mem_used_bytes: 10,
                    mem_total_bytes: 100,
                },
                monitor_interval: 1.0,
                uptime_secs: 5.0,
                config_source: "/tmp/appkeeper.toml".into(),
                managed_by: None,
            },
            programs: vec![info],
            pending: crate::supervisor::PendingDoc::default(),
        }
    }

    /// control-plane spec (refine-cli-verbs 2.2): `ak ps --out json` prints the
    /// `/v1/status` projection verbatim, and the pending-change document
    /// already rides inside it — no extra JSON field is added by the CLI.
    #[test]
    fn status_projection_carries_pending_for_ps_json() {
        let mut doc = sample_doc();
        doc.pending = crate::supervisor::PendingDoc {
            programs: vec![crate::supervisor::PendingProgram {
                app: "app".into(),
                program: "a".into(),
                running: true,
            }],
            apps_added: vec!["newapp".into()],
            apps_removed: vec!["gone".into()],
            daemon_hints: vec!["[daemon] host".into()],
            errors: vec!["bad.toml: parse error".into()],
        };
        let v: serde_json::Value =
            serde_json::from_slice(&serde_json::to_vec(&doc).unwrap()).unwrap();
        let pending = &v["pending"];
        assert_eq!(pending["programs"][0]["app"], "app");
        assert_eq!(pending["programs"][0]["program"], "a");
        assert_eq!(pending["programs"][0]["running"], true);
        assert_eq!(pending["apps_added"][0], "newapp");
        assert_eq!(pending["apps_removed"][0], "gone");
        assert_eq!(pending["daemon_hints"][0], "[daemon] host");
        assert_eq!(pending["errors"][0], "bad.toml: parse error");
    }

    /// webui-api: WS structured payloads and REST JSON must come from the
    /// same serde structs — decoding either encoding yields equal values.
    #[test]
    fn projection_json_msgpack_isomorphic() {
        let doc = sample_doc();
        let j = serde_json::to_vec(&doc).unwrap();
        let m = rmp_serde::to_vec(&doc).unwrap();
        let dj: StatusDoc = serde_json::from_slice(&j).unwrap();
        let dm: StatusDoc = rmp_serde::from_slice(&m).unwrap();
        assert_eq!(dj, dm);
        assert_eq!(dj, doc);
        assert!(
            m.len() < j.len(),
            "msgpack should not be larger than json here"
        );
    }

    /// harden-daemon-autostart 4.3: `managed_by` rides the one `DaemonInfo`
    /// struct — a manual start projects `null`, systemd projects `systemd`,
    /// SCM projects `scm`; the field is backward-compatible (JSON written
    /// without it still decodes).
    #[test]
    fn managed_by_projects_null_systemd_and_scm() {
        // Manual start (sample doc): null.
        let doc = sample_doc();
        let v = serde_json::to_value(&doc).unwrap();
        assert!(v["daemon"]["managed_by"].is_null(), "{v}");
        // Managed: the source string round-trips through JSON and msgpack.
        for (source, wire) in [("systemd", "systemd"), ("scm", "scm")] {
            let mut doc = sample_doc();
            doc.daemon.managed_by = Some(source.into());
            let j = serde_json::to_vec(&doc).unwrap();
            let m = rmp_serde::to_vec(&doc).unwrap();
            let dj: StatusDoc = serde_json::from_slice(&j).unwrap();
            let dm: StatusDoc = rmp_serde::from_slice(&m).unwrap();
            assert_eq!(dj.daemon.managed_by.as_deref(), Some(wire));
            assert_eq!(dm.daemon.managed_by.as_deref(), Some(wire));
        }
        // Backward compatibility: a projection written without the field
        // still decodes — to `null` (existing consumers/writers unaffected).
        let mut legacy = serde_json::to_value(sample_doc()).unwrap();
        legacy["daemon"]
            .as_object_mut()
            .unwrap()
            .remove("managed_by");
        let old: StatusDoc = serde_json::from_value(legacy).unwrap();
        assert_eq!(old.daemon.managed_by, None);
    }

    /// harden-daemon-autostart 4.3: the injection lands before projection —
    /// `set_managed_by` feeds the same `daemon_info` constructor /v1, /api
    /// and the WS snapshot share.
    #[test]
    fn managed_by_injection_reaches_the_projection() {
        let dir = std::env::temp_dir().join(format!("ak-test-mgd-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let cfg_path = dir.join("daemon.toml");
        std::fs::write(
            &cfg_path,
            format!(
                "[daemon]\nsocket = {:?}\nlog_dir = {:?}\nstate_dir = {:?}\nmonitor_interval = 0.05\n",
                dir.join("ak.sock").to_string_lossy(),
                dir.join("logs").to_string_lossy(),
                dir.join("state").to_string_lossy(),
            ),
        )
        .unwrap();
        let (config, _) = crate::config::DaemonConfig::load_or_default(&cfg_path).unwrap();
        let sup = Supervisor::new(config, &dir).unwrap();
        // Manual start: unmanaged until injected.
        assert!(status_doc(&sup).daemon.managed_by.is_none());
        sup.set_managed_by(Some("systemd"));
        let doc = status_doc(&sup);
        assert_eq!(doc.daemon.managed_by.as_deref(), Some("systemd"));
        // 4.4: the gate reads the same state — the shutdown guard flips with it.
        sup.set_managed_by(None);
        assert!(status_doc(&sup).daemon.managed_by.is_none());
        sup.request_shutdown();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// metrics/control-plane: a supervised program's projection carries the
    /// metric fields from the shared sampler table; not-running → null.
    #[test]
    fn program_info_merges_metrics_from_table() {
        let dir = std::env::temp_dir().join(format!("ak-test-proj-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let config = crate::config::DaemonConfig::default();
        let sup = Supervisor::new(config, &dir).unwrap();
        let raw: AppRaw = toml::from_str("[program.p]\ncommand = 'true'\n").unwrap();
        let app = resolve_app("demo", Path::new("x.toml"), &raw, None).unwrap();
        let def = app.programs.into_iter().next().unwrap();
        {
            let mut st = sup.state.lock().unwrap();
            st.programs.insert(
                def.name.clone(),
                ManagedProgram::new(def.clone(), dir.clone(), 10),
            );
        }
        // No metrics yet → fields empty (not 0).
        let doc = status_doc(&sup);
        let info = doc.programs.iter().find(|p| p.name == def.name).unwrap();
        assert_eq!(info.cpu_percent, None);
        assert_eq!(info.mem_bytes, None);

        sup.metrics.set_program(
            &def.name,
            ProgramMetrics {
                cpu_percent: Some(12.5),
                mem_bytes: Some(2048),
            },
        );
        let doc = status_doc(&sup);
        let info = doc.programs.iter().find(|p| p.name == def.name).unwrap();
        assert_eq!(info.cpu_percent, Some(12.5));
        assert_eq!(info.mem_bytes, Some(2048));
        assert_eq!(
            doc.daemon.system.cpu_percent, None,
            "system untouched by program entry"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    // -- ad-hoc run endpoint (run-daemon-first section 5) -------------------

    /// A control endpoint on a private per-test socket path. Short, because
    /// the whole harness lives under `std::env::temp_dir()` and `sun_path`
    /// is only ~108 bytes.
    struct TestEndpoint(crate::transport::Endpoint);

    impl TestEndpoint {
        fn post(&self, path: &str, body: &serde_json::Value) -> String {
            self.request("POST", path, Some(body), Some("application/json"))
        }

        fn get(&self, path: &str) -> String {
            self.request("GET", path, None, None)
        }

        fn request(
            &self,
            method: &str,
            path: &str,
            body: Option<&serde_json::Value>,
            content_type: Option<&str>,
        ) -> String {
            let mut conn = self.0.connect().expect("connect to the control endpoint");
            let payload = body.map(|b| b.to_string()).unwrap_or_default();
            let mut req = format!("{method} {path} HTTP/1.1\r\nHost: localhost\r\n");
            if let Some(ct) = content_type {
                req.push_str(&format!("Content-Type: {ct}\r\n"));
            }
            req.push_str(&format!("Content-Length: {}\r\n", payload.len()));
            req.push_str("Connection: close\r\n\r\n");
            conn.write_all(req.as_bytes()).unwrap();
            conn.write_all(payload.as_bytes()).unwrap();
            conn.flush().unwrap();
            let mut out = String::new();
            conn.read_to_string(&mut out).unwrap();
            out
        }
    }

    /// Boot a real supervisor + control plane on a private socket with a
    /// private app_dir/log_dir; returns (sup, dir, endpoint). Caller shuts
    /// down.
    fn serve_harness(tag: &str) -> (Arc<Supervisor>, std::path::PathBuf, TestEndpoint) {
        let dir = std::env::temp_dir().join(format!("ak-test-srv-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("logs")).unwrap();
        let socket = dir.join("ak.sock");
        let cfg_path = dir.join("daemon.toml");
        std::fs::write(
            &cfg_path,
            format!(
                "[daemon]\nsocket = {:?}\nlog_dir = {:?}\nstate_dir = {:?}\nmonitor_interval = 0.05\n",
                socket.to_string_lossy(),
                dir.join("logs").to_string_lossy(),
                dir.join("state").to_string_lossy(),
            ),
        )
        .unwrap();
        let (config, _) = crate::config::DaemonConfig::load_or_default(&cfg_path).unwrap();
        let sup = Supervisor::new(config, &dir).unwrap();
        sup.set_config_path(&cfg_path);
        // The health-checker thread, as run_daemon starts it — health-checked
        // programs only confirm when a probe actually runs.
        {
            let sup_h = sup.clone();
            let tasks = sup.health_tasks.clone();
            std::thread::spawn(move || {
                crate::health::checker_loop(tasks, move |name, ok| {
                    let restart = {
                        let mut st = sup_h.state.lock().unwrap();
                        st.programs
                            .get_mut(&name)
                            .map(|p| p.record_health(ok, std::time::Instant::now()))
                            .unwrap_or(false)
                    };
                    if restart {
                        sup_h.enqueue(crate::supervisor::Command::HealthRestart { name });
                    }
                });
            });
        }
        let listener = bind(&sup).unwrap();
        let sup2 = sup.clone();
        std::thread::spawn(move || serve(sup2, listener));
        let sup3 = sup.clone();
        std::thread::spawn(move || sup3.run());
        let endpoint =
            TestEndpoint(crate::transport::Endpoint::parse(&socket.to_string_lossy()).unwrap());
        (sup, dir, endpoint)
    }

    /// De-chunk a chunked response body into the concatenated payload.
    fn dechunk(body: &str) -> String {
        let mut out = String::new();
        let mut rest = body;
        while let Some(pos) = rest.find("\r\n") {
            let size = usize::from_str_radix(rest[..pos].trim(), 16).unwrap_or(0);
            rest = &rest[pos + 2..];
            if size == 0 {
                break;
            }
            if rest.len() < size {
                out.push_str(rest);
                break;
            }
            out.push_str(&rest[..size]);
            rest = &rest[size..];
            if rest.starts_with("\r\n") {
                rest = &rest[2..];
            }
        }
        out
    }

    /// The JSON body of a non-streaming control-plane response.
    fn response_json(resp: &str) -> serde_json::Value {
        let body = resp.split_once("\r\n\r\n").map(|(_, b)| b).unwrap_or("");
        serde_json::from_str(body).unwrap_or_else(|e| panic!("body is JSON ({e}): {resp}"))
    }

    /// Publish an intent and converge a real webui manager on it (the same
    /// thread `run_daemon` spawns). Returns the manager handle; stop it with
    /// [`stop_console_manager`].
    fn converge_console(
        sup: &Arc<Supervisor>,
        listen: &str,
        mode: crate::config::WebuiTokenMode,
        token: &str,
    ) -> std::thread::JoinHandle<()> {
        let sup2 = sup.clone();
        let mgr = std::thread::spawn(move || crate::supervisor::webui_manager_loop(sup2));
        let generation = sup.webui.set_desired(crate::supervisor::WebuiIntent {
            enabled: true,
            listen: listen.to_string(),
            mode,
            token: token.to_string(),
        });
        let (ok, note) = sup.webui.wait_applied(generation, Duration::from_secs(10));
        assert!(ok, "console converged: {note}");
        mgr
    }

    fn stop_console_manager(sup: &Arc<Supervisor>, mgr: std::thread::JoinHandle<()>) {
        sup.request_shutdown();
        mgr.join().unwrap();
    }

    /// add-webui-launch-command 3.1: `GET /v1/webui` reports the auto token
    /// in auto mode, withholds it for a fixed token, and falls back to the
    /// built-in default address when the console is off. The wildcard URL
    /// mapping is part of the same contract.
    #[test]
    fn webui_endpoint_reports_auto_fixed_and_disabled() {
        let (sup, dir, ep) = serve_harness("v1-webui-modes");
        let listen = format!("127.0.0.1:{}", free_tcp_port());
        let token = crate::web::generate_console_token().unwrap();
        let mgr = converge_console(&sup, &listen, crate::config::WebuiTokenMode::Auto, &token);

        // auto: every documented field, token included and matching the live
        // credential the router enforces.
        let doc = response_json(&ep.get("/v1/webui"));
        assert_eq!(doc["enabled"], true);
        assert_eq!(doc["serving"], true);
        assert_eq!(doc["listen"], listen);
        assert_eq!(doc["url"], format!("http://{listen}"));
        assert_eq!(doc["auth"], "auto");
        assert_eq!(doc["token"], token);
        assert_eq!(sup.console_auth.lock().unwrap().token(), token);

        // fixed: `auth` flips and the token is absent — never echoed.
        let generation = sup.webui.set_desired(crate::supervisor::WebuiIntent {
            enabled: true,
            listen: listen.clone(),
            mode: crate::config::WebuiTokenMode::Fixed("s3cr3t".into()),
            token: "s3cr3t".into(),
        });
        let (ok, note) = sup.webui.wait_applied(generation, Duration::from_secs(10));
        assert!(ok, "fixed-token console converged: {note}");
        let doc = response_json(&ep.get("/v1/webui"));
        assert_eq!(doc["enabled"], true);
        assert_eq!(doc["serving"], true);
        assert_eq!(doc["auth"], "token");
        assert!(
            doc.get("token").is_none(),
            "fixed token must not be echoed: {doc}"
        );

        // disabled: intent off, nothing serving, built-in default address.
        let generation = sup
            .webui
            .set_desired(crate::supervisor::WebuiIntent::disabled());
        let (ok, note) = sup.webui.wait_applied(generation, Duration::from_secs(10));
        assert!(ok, "console disabled: {note}");
        let doc = response_json(&ep.get("/v1/webui"));
        assert_eq!(doc["enabled"], false);
        assert_eq!(doc["serving"], false);
        assert_eq!(doc["listen"], crate::config::DEFAULT_WEBUI_LISTEN);
        assert_eq!(
            doc["url"],
            format!("http://{}", crate::config::DEFAULT_WEBUI_LISTEN)
        );
        assert!(doc.get("token").is_none(), "no token when off: {doc}");

        stop_console_manager(&sup, mgr);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// add-webui-launch-command fix: while a new intent is published but not
    /// yet converged, `/v1/webui` must report `serving: false` and withhold
    /// the token. This is the fixed→auto leak the review found: the live
    /// console still enforces the old fixed credential, so echoing anything
    /// would hand `ak webui` a stale bootstrap URL. No manager thread runs
    /// here, which pins the window deterministically.
    #[test]
    fn webui_status_withholds_the_token_while_a_mode_switch_is_pending() {
        let (sup, dir, ep) = serve_harness("v1-webui-switch-pending");
        let listen = format!("127.0.0.1:{}", free_tcp_port());
        let live_token = crate::web::generate_console_token().unwrap();
        // The console is still serving the previous (fixed) intent.
        *sup.console_auth.lock().unwrap() = Arc::new(crate::web::ConsoleAuth::new("s3cr3t"));

        let new_token = crate::web::generate_console_token().unwrap();
        sup.webui.set_desired(crate::supervisor::WebuiIntent {
            enabled: true,
            listen: listen.clone(),
            mode: crate::config::WebuiTokenMode::Auto,
            token: new_token.clone(),
        });

        let snapshot = sup.webui.snapshot();
        assert!(!snapshot.serving, "pending generation is not serving");
        assert!(snapshot.error.is_none(), "a pending switch is not an error");
        let doc = response_json(&ep.get("/v1/webui"));
        assert_eq!(doc["enabled"], true);
        assert_eq!(doc["serving"], false);
        assert_eq!(doc["auth"], "auto");
        assert!(
            doc.get("token").is_none(),
            "no token during the reload window: {doc}"
        );
        assert_eq!(
            sup.console_auth.lock().unwrap().token(),
            "s3cr3t",
            "the live console still enforces the old fixed token"
        );
        assert_ne!(new_token, live_token);

        // auto→fixed symmetry: the window is closed the same way (and fixed
        // mode never echoes a token, so `serving: false` is what stops the CLI
        // from opening anything).
        sup.webui.set_desired(crate::supervisor::WebuiIntent {
            enabled: true,
            listen: listen.clone(),
            mode: crate::config::WebuiTokenMode::Fixed("n3w".into()),
            token: "n3w".into(),
        });
        let doc = response_json(&ep.get("/v1/webui"));
        assert_eq!(doc["serving"], false);
        assert_eq!(doc["auth"], "token");
        assert!(doc.get("token").is_none(), "fixed mode never echoes: {doc}");

        let _ = std::fs::remove_dir_all(dir);
    }

    /// After the manager converges on the new auto generation, serving
    /// resumes and the endpoint returns the new live token (fixed → auto).
    #[test]
    fn webui_status_returns_the_new_token_after_a_mode_switch_converges() {
        let (sup, dir, ep) = serve_harness("v1-webui-switch-converged");
        let listen = format!("127.0.0.1:{}", free_tcp_port());
        let mgr = converge_console(
            &sup,
            &listen,
            crate::config::WebuiTokenMode::Fixed("s3cr3t".into()),
            "s3cr3t",
        );
        let doc = response_json(&ep.get("/v1/webui"));
        assert_eq!(doc["serving"], true);
        assert_eq!(doc["auth"], "token");
        assert!(doc.get("token").is_none(), "fixed mode never echoes: {doc}");

        let new_token = crate::web::generate_console_token().unwrap();
        let generation = sup.webui.set_desired(crate::supervisor::WebuiIntent {
            enabled: true,
            listen: listen.clone(),
            mode: crate::config::WebuiTokenMode::Auto,
            token: new_token.clone(),
        });
        let (ok, note) = sup.webui.wait_applied(generation, Duration::from_secs(10));
        assert!(ok, "auto console converged: {note}");

        let doc = response_json(&ep.get("/v1/webui"));
        assert_eq!(doc["serving"], true);
        assert_eq!(doc["auth"], "auto");
        assert_eq!(doc["token"], new_token);
        assert_eq!(sup.console_auth.lock().unwrap().token(), new_token);

        stop_console_manager(&sup, mgr);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// The wildcard binds are rendered as loopback (a browser cannot use
    /// `0.0.0.0` / `::`), IPv6 literals get brackets, and a host:port passes
    /// through otherwise.
    #[test]
    fn webui_url_maps_wildcards_and_brackets_ipv6() {
        assert_eq!(webui_console_url("0.0.0.0:9877"), "http://127.0.0.1:9877");
        assert_eq!(webui_console_url("[::]:9877"), "http://127.0.0.1:9877");
        assert_eq!(webui_console_url("::1:9877"), "http://[::1]:9877");
        assert_eq!(webui_console_url("[::1]:9877"), "http://[::1]:9877");
        assert_eq!(webui_console_url("localhost:8080"), "http://localhost:8080");
    }

    /// add-webui-launch-command 3.2: the auto token is reachable only through
    /// `/v1/webui`; the regular status projections never carry it.
    #[test]
    fn console_token_never_appears_in_status_projections() {
        let (sup, dir, ep) = serve_harness("v1-webui-token-leak");
        let listen = format!("127.0.0.1:{}", free_tcp_port());
        let token = crate::web::generate_console_token().unwrap();
        let mgr = converge_console(&sup, &listen, crate::config::WebuiTokenMode::Auto, &token);

        // Sanity: the bootstrap endpoint does hand it out.
        let doc = response_json(&ep.get("/v1/webui"));
        assert_eq!(doc["token"], token);

        for path in [
            "/v1/status",
            "/v1/programs",
            "/v1/programs/ghost",
            "/v1/pending",
            "/v1/health",
        ] {
            let resp = ep.get(path);
            assert!(
                !resp.contains(&token),
                "{path} leaked the console token: {resp}"
            );
        }

        stop_console_manager(&sup, mgr);
        let _ = std::fs::remove_dir_all(dir);
    }

    fn sleep_cmd() -> (&'static str, Vec<String>) {
        if cfg!(windows) {
            (
                "C:\\Windows\\System32\\ping.exe",
                vec!["-n".into(), "30".into(), "127.0.0.1".into()],
            )
        } else {
            ("/bin/sleep", vec!["30".into()])
        }
    }

    /// 5.1: the projection carries `config_path` and `health_confirmed`, and
    /// keeps every legacy field.
    #[test]
    fn projection_has_config_path_and_health_confirmed() {
        let doc = sample_doc();
        let j = serde_json::to_value(&doc).unwrap();
        let p = &j["programs"][0];
        assert_eq!(p["config_path"], "/etc/appkeeper/app.d/a.toml");
        assert_eq!(p["health_confirmed"], true);
        // Legacy fields unchanged.
        for f in ["app", "name", "state", "pid", "unhealthy"] {
            assert!(p.get(f).is_some(), "legacy field {f} present");
        }
    }

    /// refuse-run-existing-app 1.3: `parse_adhoc_spec` lifts `force` from the
    /// request body and defaults it to false when absent.
    #[test]
    fn parse_adhoc_spec_reads_force() {
        let with = parse_adhoc_spec(
            &serde_json::to_vec(&serde_json::json!({
                "name": "web", "command": "/bin/sleep", "args": ["30"], "force": true,
            }))
            .unwrap(),
        )
        .unwrap();
        assert!(with.force);
        assert_eq!(with.name.as_deref(), Some("web"));

        let without = parse_adhoc_spec(
            &serde_json::to_vec(&serde_json::json!({
                "name": "web", "command": "/bin/sleep",
            }))
            .unwrap(),
        )
        .unwrap();
        assert!(!without.force, "absent force defaults to false");
        // A non-boolean `force` is ignored (defaults to false), like the other
        // loosely-typed fields.
        let bogus = parse_adhoc_spec(
            &serde_json::to_vec(&serde_json::json!({
                "name": "web", "command": "/bin/sleep", "force": "yes",
            }))
            .unwrap(),
        )
        .unwrap();
        assert!(!bogus.force);
    }

    /// 5.2/5.3: a successful ad-hoc run (no health check) streams only the
    /// terminal frame (control-plane spec: 无健康探测时无需中间帧), commits
    /// the record and returns {program, config_path, pid}.
    #[test]
    fn run_endpoint_streams_and_commits() {
        let (sup, dir, ep) = serve_harness("run-ok");
        let (cmd, args) = sleep_cmd();
        let resp = ep.post(
            "/v1/run",
            &serde_json::json!({
                "name": "web",
                "command": cmd,
                "args": args,
                "register_only": false,
            }),
        );
        assert!(resp.starts_with("HTTP/1.1 200 OK"), "{resp}");
        assert!(
            resp.contains("Transfer-Encoding: chunked"),
            "chunked response: {resp}"
        );
        let body = dechunk(resp.split("\r\n\r\n").nth(1).unwrap_or(""));
        let frames: Vec<serde_json::Value> = body
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(
            frames.len(),
            1,
            "no health check → terminal frame only: {frames:?}"
        );
        let last = frames.last().unwrap();
        assert_eq!(last["ok"], true, "{frames:?}");
        assert_eq!(last["program"], "web");
        assert!(last["pid"].as_u64().unwrap() > 0);
        assert!(last["config_path"].as_str().unwrap().ends_with("web.toml"));
        assert!(
            dir.join("app.d").join("adhoc").join("web.toml").exists(),
            "record written"
        );
        assert!(!dir.join("app.d").join("web.toml.pending").exists());
        sup.request_shutdown();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 5.3: a program WITH a health check streams the staged/waiting frames
    /// (with the estimate) before the terminal frame (control-plane spec:
    /// 有健康探测时流式回传进度帧).
    #[test]
    fn run_endpoint_with_health_streams_progress() {
        let (sup, dir, ep) = serve_harness("run-health");
        let (cmd, args) = sleep_cmd();
        // `health_check = "true"` is an exec check that succeeds immediately.
        let resp = ep.post(
            "/v1/run",
            &serde_json::json!({
                "name": "web",
                "command": cmd,
                "args": args,
                "health_check": "true",
            }),
        );
        assert!(resp.starts_with("HTTP/1.1 200"), "{resp}");
        assert!(resp.contains("Transfer-Encoding: chunked"), "{resp}");
        let body = dechunk(resp.split("\r\n\r\n").nth(1).unwrap_or(""));
        let frames: Vec<serde_json::Value> = body
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        // Progress frames first (staged + waiting_startsecs + waiting_health).
        let phases: Vec<&str> = frames
            .iter()
            .filter_map(|f| f.get("phase").and_then(|p| p.as_str()))
            .collect();
        assert!(phases.contains(&"staged"), "{frames:?}");
        assert!(phases.contains(&"waiting_health"), "{frames:?}");
        // The staged frame carries the estimate.
        let staged = frames.iter().find(|f| f["phase"] == "staged").unwrap();
        assert!(staged["estimate_secs"].as_f64().unwrap() > 0.0, "{staged}");
        // The terminal frame reports success and a pid.
        let last = frames.last().unwrap();
        assert_eq!(last["ok"], true, "{frames:?}");
        assert!(last["pid"].as_u64().unwrap() > 0, "{last}");
        assert!(dir.join("app.d").join("adhoc").join("web.toml").exists());
        sup.request_shutdown();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 5.3: `--restart=no` emits only the terminal frame (no intermediate
    /// progress) and registers with pid = null.
    #[test]
    fn run_endpoint_register_only_emits_only_terminal_frame() {
        let (sup, dir, ep) = serve_harness("run-no");
        let (cmd, args) = sleep_cmd();
        let resp = ep.post(
            "/v1/run",
            &serde_json::json!({
                "name": "web",
                "command": cmd,
                "args": args,
                "restart": "no",
                "register_only": true,
            }),
        );
        let body = dechunk(resp.split("\r\n\r\n").nth(1).unwrap_or(""));
        let frames: Vec<serde_json::Value> = body
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(frames.len(), 1, "only the terminal frame: {frames:?}");
        assert_eq!(frames[0]["ok"], true);
        assert!(frames[0]["pid"].is_null(), "pid null on register-only");
        assert!(dir.join("app.d").join("adhoc").join("web.toml").exists());
        sup.request_shutdown();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 5.5: validation errors answer a real status code, not a 200 stream.
    #[test]
    fn run_endpoint_error_codes() {
        let (sup, dir, ep) = serve_harness("run-err");
        let (cmd, args) = sleep_cmd();
        // Reserved name `all` → 400.
        let resp = ep.post(
            "/v1/run",
            &serde_json::json!({ "name": "all", "command": cmd, "args": args }),
        );
        assert!(resp.starts_with("HTTP/1.1 400"), "{resp}");
        assert!(resp.contains("reserved"), "{resp}");
        // Missing command → 400.
        let resp = ep.post("/v1/run", &serde_json::json!({ "name": "web" }));
        assert!(resp.starts_with("HTTP/1.1 400"), "{resp}");
        sup.request_shutdown();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// refuse-run-existing-app 3.1: without `force` a same-name submission is a
    /// 409 state conflict answered before the stream starts — no record write,
    /// no new process, and the existing PID is untouched.
    #[test]
    fn run_endpoint_refuses_same_name_without_force() {
        let (sup, dir, ep) = serve_harness("run-conflict");
        let (cmd, args) = sleep_cmd();
        let first = ep.post(
            "/v1/run",
            &serde_json::json!({
                "name": "web", "command": cmd, "args": args,
                "register_only": true, "restart": "no",
            }),
        );
        assert!(first.starts_with("HTTP/1.1 200"), "{first}");
        let record = dir.join("app.d").join("adhoc").join("web.toml");
        let before = std::fs::read_to_string(&record).unwrap();
        let start = ep.post("/v1/programs/web/start", &serde_json::json!({}));
        assert!(start.starts_with("HTTP/1.1 200"), "{start}");
        let pid = sup.state.lock().unwrap().programs["web"].pid();
        assert!(pid.is_some());

        let resp = ep.post(
            "/v1/run",
            &serde_json::json!({ "name": "web", "command": cmd, "args": args }),
        );
        assert!(resp.starts_with("HTTP/1.1 409"), "{resp}");
        assert!(resp.contains("\"phase\":\"conflict\""), "{resp}");
        assert!(resp.contains("web"), "{resp}");
        assert!(resp.contains("--force"), "offers the escape: {resp}");
        assert_eq!(std::fs::read_to_string(&record).unwrap(), before);
        assert_eq!(
            sup.state.lock().unwrap().programs["web"].pid(),
            pid,
            "the running process is untouched"
        );
        sup.request_shutdown();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// refuse-run-existing-app 3.1/D6: `force` + byte-identical config answers a
    /// single terminal `unchanged` frame, never entering the startup judgement.
    #[test]
    fn run_endpoint_force_unchanged_answers_one_frame() {
        let (sup, dir, ep) = serve_harness("run-unchanged");
        let (cmd, args) = sleep_cmd();
        let first = ep.post(
            "/v1/run",
            &serde_json::json!({
                "name": "web", "command": cmd, "args": args,
                "register_only": true, "restart": "no",
            }),
        );
        assert!(first.starts_with("HTTP/1.1 200"), "{first}");
        let record = dir.join("app.d").join("adhoc").join("web.toml");
        let before = std::fs::read_to_string(&record).unwrap();
        let before_mtime = std::fs::metadata(&record).unwrap().modified().unwrap();

        let resp = ep.post(
            "/v1/run",
            &serde_json::json!({
                "name": "web", "command": cmd, "args": args,
                "force": true, "register_only": true, "restart": "no",
            }),
        );
        assert!(resp.starts_with("HTTP/1.1 200"), "{resp}");
        let body = dechunk(resp.split("\r\n\r\n").nth(1).unwrap_or(""));
        let frames: Vec<serde_json::Value> = body
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(
            frames.len(),
            1,
            "unchanged → one terminal frame: {frames:?}"
        );
        assert_eq!(frames[0]["ok"], true, "{frames:?}");
        assert_eq!(frames[0]["unchanged"], true, "{frames:?}");
        assert!(
            frames[0]["pid"].is_null(),
            "no process is started: {frames:?}"
        );
        assert!(
            frames[0]["config_path"]
                .as_str()
                .unwrap()
                .ends_with("web.toml")
        );
        // Byte and mtime identical: nothing was rewritten.
        assert_eq!(std::fs::read_to_string(&record).unwrap(), before);
        assert_eq!(
            std::fs::metadata(&record).unwrap().modified().unwrap(),
            before_mtime
        );
        assert!(!dir.join("app.d").join("web.toml.pending").exists());
        sup.request_shutdown();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// refuse-run-existing-app 3.2/3.3: `force` with different content lands the
    /// record before the startup judgement, then answers the terminal frame
    /// without a second commit (`register_only` answers immediately, pid null).
    #[test]
    fn run_endpoint_force_landed_skips_commit() {
        let (sup, dir, ep) = serve_harness("run-landed");
        let (cmd, args) = sleep_cmd();
        let first = ep.post(
            "/v1/run",
            &serde_json::json!({
                "name": "web", "command": cmd, "args": args,
                "register_only": true, "restart": "no",
            }),
        );
        assert!(first.starts_with("HTTP/1.1 200"), "{first}");
        let record = dir.join("app.d").join("adhoc").join("web.toml");
        let before = std::fs::read_to_string(&record).unwrap();
        // Different args, register-only: the record is rewritten and nothing is
        // started (pid null), no staged entry is left behind.
        let mut new_args = args.clone();
        new_args.push("extra".into());
        let resp = ep.post(
            "/v1/run",
            &serde_json::json!({
                "name": "web", "command": cmd, "args": new_args,
                "force": true, "register_only": true, "restart": "no",
            }),
        );
        assert!(resp.starts_with("HTTP/1.1 200"), "{resp}");
        let body = dechunk(resp.split("\r\n\r\n").nth(1).unwrap_or(""));
        let frames: Vec<serde_json::Value> = body
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(frames.len(), 1, "terminal frame only: {frames:?}");
        assert_eq!(frames[0]["ok"], true, "{frames:?}");
        assert_ne!(frames[0]["unchanged"], true, "{frames:?}");
        assert!(frames[0]["pid"].is_null(), "register-only: {frames:?}");
        let after = std::fs::read_to_string(&record).unwrap();
        assert_ne!(after, before, "the record was rewritten");
        assert!(after.contains("extra"), "{after}");
        assert!(!dir.join("app.d").join("web.toml.pending").exists());
        assert!(
            !sup.state.lock().unwrap().staged.contains_key("web"),
            "the force path never stages (no commit step)"
        );
        sup.request_shutdown();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// refuse-run-existing-app 3.2/D7: `force` with different content that then
    /// FAILS its startup judgement gets a structured error and the new process
    /// is stopped, while the landed record stays on disk.
    #[test]
    fn run_endpoint_force_failed_startup_stops_new_process() {
        let (sup, dir, ep) = serve_harness("run-force-fail");
        let record = register_only_app(&ep, &dir, "web");
        assert!(record.exists());
        // A definition that exits inside startsecs: the startup judgement fails.
        #[cfg(unix)]
        let (cmd, args) = ("/bin/sh", vec!["-c".to_string(), "exit 3".to_string()]);
        #[cfg(windows)]
        let (cmd, args) = ("cmd", vec!["/c".to_string(), "exit 3".to_string()]);
        let resp = ep.post(
            "/v1/run",
            &serde_json::json!({
                "name": "web", "command": cmd, "args": args, "force": true,
            }),
        );
        assert!(resp.starts_with("HTTP/1.1 200"), "{resp}");
        let body = dechunk(resp.split("\r\n\r\n").nth(1).unwrap_or(""));
        let frames: Vec<serde_json::Value> = body
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        let last = frames.last().unwrap();
        assert_eq!(last["ok"], false, "{frames:?}");
        assert_eq!(last["phase"], "startup", "{frames:?}");
        assert!(!last["error"].as_str().unwrap().is_empty(), "{frames:?}");
        // The landed record stays (the overwrite took effect, no rollback) and
        // the failed process is stopped.
        assert!(record.exists(), "the new record is kept");
        assert!(
            sup.state.lock().unwrap().programs["web"].pid().is_none(),
            "the failed new process was stopped"
        );
        assert!(!dir.join("app.d").join("web.toml.pending").exists());
        sup.request_shutdown();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// refuse-run-existing-app 3.2: `force` + different content that passes the
    /// startup judgement answers the terminal frame with the new PID.
    #[test]
    fn run_endpoint_force_restarts_and_reports_new_pid() {
        let (sup, dir, ep) = serve_harness("run-force-ok");
        let (cmd, args) = sleep_cmd();
        let first = ep.post(
            "/v1/run",
            &serde_json::json!({
                "name": "web", "command": cmd, "args": args,
                "register_only": true, "restart": "no",
            }),
        );
        assert!(first.starts_with("HTTP/1.1 200"), "{first}");
        let pid1 = sup
            .state
            .lock()
            .unwrap()
            .programs
            .get("web")
            .and_then(|p| p.pid());
        // Different content + spawn: the startup judgement runs (3s startsecs).
        let mut new_args = args.clone();
        new_args.push("60".into());
        let resp = ep.post(
            "/v1/run",
            &serde_json::json!({
                "name": "web", "command": cmd, "args": new_args, "force": true,
            }),
        );
        assert!(resp.starts_with("HTTP/1.1 200"), "{resp}");
        let body = dechunk(resp.split("\r\n\r\n").nth(1).unwrap_or(""));
        let frames: Vec<serde_json::Value> = body
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        let last = frames.last().unwrap();
        assert_eq!(last["ok"], true, "{frames:?}");
        let pid2 = last["pid"].as_u64().expect("new pid");
        if let Some(p1) = pid1 {
            assert_ne!(pid2 as u32, p1, "restarted with a new pid");
        }
        assert!(dir.join("app.d").join("adhoc").join("web.toml").exists());
        assert!(!dir.join("app.d").join("web.toml.pending").exists());
        sup.request_shutdown();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 2.1 / API 准入控制: there is no credential to present. A request with
    /// no `Authorization` header and one carrying a leftover Bearer token are
    /// answered identically, and neither answer mentions authentication —
    /// admission is the endpoint owner's, decided by the OS before a request
    /// is ever parsed.
    #[test]
    fn no_credentials_are_required_and_authorization_is_ignored() {
        let (sup, dir, ep) = serve_harness("open");
        let bare = ep.get("/v1/status");
        assert!(bare.starts_with("HTTP/1.1 200"), "{bare}");
        let with_token = ep.request("GET", "/v1/status", None, Some("application/json"));
        assert!(with_token.starts_with("HTTP/1.1 200"), "{with_token}");
        // Two calls are only comparable with time-variant fields (uptime,
        // metrics) stripped — under parallel load they straddle a boundary.
        let strip = |v: &mut serde_json::Value| {
            v["daemon"]["uptime_secs"] = serde_json::json!(0);
            v["daemon"]["system"] = serde_json::Value::Null;
            if let Some(ps) = v["programs"].as_array_mut() {
                for p in ps {
                    p["uptime_secs"] = serde_json::json!(0);
                    p["cpu_percent"] = serde_json::Value::Null;
                    p["mem_bytes"] = serde_json::Value::Null;
                    p["log_rate"] = serde_json::Value::Null;
                }
            }
        };
        let mut a: serde_json::Value =
            serde_json::from_str(bare.split("\r\n\r\n").nth(1).unwrap_or("")).unwrap();
        let mut b: serde_json::Value =
            serde_json::from_str(with_token.split("\r\n\r\n").nth(1).unwrap_or("")).unwrap();
        strip(&mut a);
        strip(&mut b);
        assert_eq!(a, b, "the header changes nothing");
        let body = bare.split("\r\n\r\n").nth(1).unwrap_or("");
        for field in ["token", "auth", "unauthorized"] {
            assert!(
                !body.contains(field),
                "no {field:?} field in the projection: {body}"
            );
        }
        sup.request_shutdown();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 2.1 / API 准入控制 (second half): `/v1/run` streams to its owner, with
    /// no 401 branch in front of it.
    #[test]
    fn run_endpoint_needs_no_credential() {
        let (sup, dir, ep) = serve_harness("run-open");
        let (cmd, args) = sleep_cmd();
        let resp = ep.post(
            "/v1/run",
            &serde_json::json!({ "name": "web", "command": cmd, "args": args }),
        );
        assert!(
            resp.starts_with("HTTP/1.1 200"),
            "the stream is served without an auth gate: {resp}"
        );
        // The admission gate is a status-line decision: matching "401"
        // anywhere in the body would also hit a temp path or pid that happens
        // to contain those digits.
        assert!(
            !resp.starts_with("HTTP/1.1 401"),
            "no 401 branch in front of the stream: {resp}"
        );
        sup.request_shutdown();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 2.3: the control plane answers on the socket and on nothing else —
    /// no TCP listener exists to probe.
    #[test]
    fn control_plane_listens_on_no_tcp_port() {
        let (sup, dir, ep) = serve_harness("no-tcp");
        assert!(ep.get("/v1/status").starts_with("HTTP/1.1 200"));
        // The daemon config carries no host/port to have rebound, and the
        // projection advertises the endpoint instead.
        let st = sup.state.lock().unwrap();
        assert!(st.config.control_endpoint().is_ok());
        drop(st);
        let body = ep.get("/v1/status");
        let v: serde_json::Value =
            serde_json::from_str(body.split("\r\n\r\n").nth(1).unwrap()).unwrap();
        assert_eq!(v["daemon"]["port"], 0, "no control TCP port is advertised");
        assert!(
            v["daemon"]["socket"].as_str().unwrap().ends_with("ak.sock"),
            "the endpoint path is what the projection carries now: {v}"
        );
        sup.request_shutdown();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// show-build-version 1.3: the projection carries this binary's build
    /// identity, the version stays the package version, and `/v1/status`
    /// serves exactly what `status_doc()` produces (one constructor, no drift).
    #[test]
    fn status_projection_carries_build_identity() {
        let (sup, dir, ep) = serve_harness("build-identity");
        let doc = status_doc(&sup);
        assert_eq!(doc.daemon.version, env!("CARGO_PKG_VERSION"));
        for (field, value) in [
            ("git_commit", &doc.daemon.build.git_commit),
            ("built_at", &doc.daemon.build.built_at),
            ("target", &doc.daemon.build.target),
            ("rustc", &doc.daemon.build.rustc),
        ] {
            assert!(!value.is_empty(), "daemon.build.{field} must be non-empty");
        }
        let body = ep.get("/v1/status");
        let v: serde_json::Value =
            serde_json::from_str(body.split("\r\n\r\n").nth(1).unwrap()).unwrap();
        assert_eq!(
            v["daemon"]["build"]["git_commit"],
            doc.daemon.build.git_commit
        );
        assert_eq!(v["daemon"]["build"]["built_at"], doc.daemon.build.built_at);
        assert_eq!(v["daemon"]["build"]["target"], doc.daemon.build.target);
        assert_eq!(v["daemon"]["build"]["rustc"], doc.daemon.build.rustc);
        assert_eq!(v["daemon"]["version"], env!("CARGO_PKG_VERSION"));
        sup.request_shutdown();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 5.4: the start endpoint keeps the legacy `result` and adds structured
    /// {name, config_path, pid}.
    #[test]
    fn start_endpoint_adds_structured_fields() {
        let (sup, dir, ep) = serve_harness("start-fields");
        // Register + start an app through the ad-hoc path first.
        let (cmd, args) = sleep_cmd();
        let resp = ep.post(
            "/v1/run",
            &serde_json::json!({ "name": "web", "command": cmd, "args": args, "register_only": true, "restart": "no" }),
        );
        assert!(resp.starts_with("HTTP/1.1 200"), "{resp}");
        // Stop it so start is legal, then start via the standard endpoint.
        let _ = ep.post("/v1/programs/web/stop", &serde_json::json!({}));
        let resp = ep.post("/v1/programs/web/start", &serde_json::json!({}));
        assert!(resp.starts_with("HTTP/1.1 200"), "{resp}");
        let body = resp.split("\r\n\r\n").nth(1).unwrap_or("");
        let v: serde_json::Value = serde_json::from_str(body).unwrap();
        assert!(v.get("result").is_some(), "legacy result kept: {v}");
        assert_eq!(v["name"], "web");
        assert!(
            v["config_path"].as_str().unwrap().ends_with("web.toml"),
            "{v}"
        );
        assert!(v["pid"].as_u64().is_some(), "pid present: {v}");
        // /v1/status carries the new projection fields too.
        let status = ep.get("/v1/status");
        let sbody = status.split("\r\n\r\n").nth(1).unwrap_or("");
        let sv: serde_json::Value = serde_json::from_str(sbody).unwrap();
        let p = sv["programs"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["name"] == "web")
            .unwrap();
        assert!(p.get("config_path").is_some(), "{p}");
        assert!(p.get("health_confirmed").is_some(), "{p}");
        sup.request_shutdown();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 5.4/control-plane: `run <name>` uses `?wait=1`, which blocks until the
    /// startup judgement; the response then carries the final pid.
    #[test]
    fn start_endpoint_wait_blocks_until_running() {
        let (sup, dir, ep) = serve_harness("start-wait");
        let (cmd, args) = sleep_cmd();
        let _ = ep.post(
            "/v1/run",
            &serde_json::json!({ "name": "web", "command": cmd, "args": args, "register_only": true, "restart": "no" }),
        );
        // The registered program uses the built-in 3s startsecs; measure that
        // the wait=1 call blocks at least ~3s and returns a pid.
        let t0 = std::time::Instant::now();
        let resp = ep.post("/v1/programs/web/start?wait=1", &serde_json::json!({}));
        let elapsed = t0.elapsed();
        assert!(resp.starts_with("HTTP/1.1 200"), "{resp}");
        assert!(
            elapsed >= std::time::Duration::from_secs(2),
            "wait=1 blocks until startsecs elapses (elapsed {elapsed:?})"
        );
        let body = resp.split("\r\n\r\n").nth(1).unwrap_or("");
        let v: serde_json::Value = serde_json::from_str(body).unwrap();
        assert!(v["pid"].as_u64().is_some(), "{v}");
        // Without wait the endpoint answers immediately.
        let _ = ep.post("/v1/programs/web/stop", &serde_json::json!({}));
        let t0 = std::time::Instant::now();
        let resp = ep.post("/v1/programs/web/start", &serde_json::json!({}));
        assert!(resp.starts_with("HTTP/1.1 200"), "{resp}");
        assert!(
            t0.elapsed() < std::time::Duration::from_secs(2),
            "without wait the reply is immediate"
        );
        sup.request_shutdown();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 5.4: the app fan-out response keeps per-program `result` and adds
    /// `config_path`/`pid`.
    #[test]
    fn fanout_endpoint_adds_structured_fields() {
        let (sup, dir, ep) = serve_harness("fanout-fields");
        let (cmd, args) = sleep_cmd();
        let _ = ep.post(
            "/v1/run",
            &serde_json::json!({ "name": "web", "command": cmd, "args": args, "register_only": true, "restart": "no" }),
        );
        let resp = ep.post("/v1/apps/web/start", &serde_json::json!({}));
        assert!(resp.starts_with("HTTP/1.1 200"), "{resp}");
        let body = resp.split("\r\n\r\n").nth(1).unwrap_or("");
        let v: serde_json::Value = serde_json::from_str(body).unwrap();
        assert!(v.get("result").is_some(), "legacy result kept: {v}");
        let p = &v["programs"][0];
        assert_eq!(p["program"], "web");
        assert!(p.get("result").is_some());
        assert!(p.get("config_path").is_some(), "{v}");
        assert!(p.get("pid").is_some(), "{v}");
        sup.request_shutdown();
        let _ = std::fs::remove_dir_all(&dir);
    }

    // -- app removal endpoint (guard-remove-until-stopped 2.1) ---------------

    /// Register a `restart = "no"` app through the ad-hoc endpoint and return
    /// its registration record path. `register_only` means it is never started.
    fn register_only_app(ep: &TestEndpoint, dir: &Path, name: &str) -> std::path::PathBuf {
        let (cmd, args) = sleep_cmd();
        let resp = ep.post(
            "/v1/run",
            &serde_json::json!({
                "name": name, "command": cmd, "args": args,
                "register_only": true, "restart": "no",
            }),
        );
        assert!(resp.starts_with("HTTP/1.1 200"), "{resp}");
        dir.join("app.d").join("adhoc").join(format!("{name}.toml"))
    }

    /// 2.1: a stopped app is removed immediately with the structured result;
    /// the status projection no longer contains it.
    #[test]
    fn remove_endpoint_succeeds_for_a_stopped_app() {
        let (sup, dir, ep) = serve_harness("remove-ok");
        let record = register_only_app(&ep, &dir, "web");
        assert!(record.exists());
        let resp = ep.post("/v1/apps/web/remove", &serde_json::json!({}));
        assert!(resp.starts_with("HTTP/1.1 200"), "{resp}");
        let v: serde_json::Value =
            serde_json::from_str(resp.split("\r\n\r\n").nth(1).unwrap_or("")).unwrap();
        assert_eq!(v["app"], "web");
        assert_eq!(v["deleted_file"], true, "generated record deleted: {v}");
        assert_eq!(v["applied"], true);
        assert!(v["programs"].as_array().is_some());
        assert!(
            v["result"].as_str().unwrap().contains("no apply needed"),
            "{v}"
        );
        assert!(!record.exists(), "registration record deleted");
        let status = ep.get("/v1/status");
        let sv: serde_json::Value =
            serde_json::from_str(status.split("\r\n\r\n").nth(1).unwrap_or("")).unwrap();
        assert!(
            !sv["programs"]
                .as_array()
                .unwrap()
                .iter()
                .any(|p| p["app"] == "web"),
            "web's program projection is gone: {sv}"
        );
        // A second removal is a 404 (nothing is registered any more).
        let again = ep.post("/v1/apps/web/remove", &serde_json::json!({}));
        assert!(again.starts_with("HTTP/1.1 404"), "{again}");
        sup.request_shutdown();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 2.1: a running program is a 409; nothing changes.
    #[test]
    fn remove_endpoint_conflict_for_a_running_app() {
        let (sup, dir, ep) = serve_harness("remove-busy");
        let _record = register_only_app(&ep, &dir, "web");
        // Start it (register=false, restart=no means it stays stopped until a
        // start command).
        let start = ep.post("/v1/programs/web/start", &serde_json::json!({}));
        assert!(start.starts_with("HTTP/1.1 200"), "{start}");
        let resp = ep.post("/v1/apps/web/remove", &serde_json::json!({}));
        assert!(resp.starts_with("HTTP/1.1 409"), "{resp}");
        // The program is either still starting or already running — both block.
        assert!(
            resp.contains("web (running)") || resp.contains("web (starting)"),
            "{resp}"
        );
        assert!(resp.contains("ak stop web"), "{resp}");
        assert!(
            dir.join("app.d/adhoc/web.toml").exists(),
            "record untouched"
        );
        let status = ep.get("/v1/status");
        assert!(status.contains("\"web\""), "program still projected");
        sup.request_shutdown();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 2.1: an unknown app is a 404.
    #[test]
    fn remove_endpoint_unknown_app_is_404() {
        let (sup, dir, ep) = serve_harness("remove-404");
        let resp = ep.post("/v1/apps/not-registered/remove", &serde_json::json!({}));
        assert!(resp.starts_with("HTTP/1.1 404"), "{resp}");
        assert!(resp.contains("unknown app"), "{resp}");
        sup.request_shutdown();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// harden-daemon-autostart 4.4: a service-managed daemon refuses
    /// `POST /v1/shutdown` with 409 BEFORE anything is enqueued — the message
    /// names the service manager's stop command, no program stops and the
    /// daemon stays up.
    #[test]
    fn shutdown_is_refused_when_service_managed() {
        let (sup, dir, ep) = serve_harness("shutdown-managed");
        sup.set_managed_by(Some("systemd"));
        let resp = ep.post("/v1/shutdown", &serde_json::json!({}));
        assert!(resp.starts_with("HTTP/1.1 409"), "{resp}");
        let body = resp.split("\r\n\r\n").nth(1).unwrap_or("");
        assert!(
            body.contains("systemctl stop") && body.contains("systemd"),
            "the 409 names the manager and its stop command: {body}"
        );
        // Nothing was enqueued: no shutdown intent, daemon still serving.
        assert!(
            !sup.state
                .lock()
                .unwrap()
                .shutdown
                .load(std::sync::atomic::Ordering::SeqCst),
            "the refusal must not latch the shutdown intent"
        );
        assert!(ep.get("/v1/health").starts_with("HTTP/1.1 200"));
        assert!(ep.get("/v1/status").starts_with("HTTP/1.1 200"));
        sup.request_shutdown();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// harden-daemon-autostart 4.4: the manual-start path keeps its original
    /// answer and order — 200 "shutting down" on the wire, THEN the daemon
    /// stops everything and exits.
    #[test]
    fn shutdown_unmanaged_still_replies_then_stops() {
        let (sup, dir, ep) = serve_harness("shutdown-manual");
        let resp = ep.post("/v1/shutdown", &serde_json::json!({}));
        assert!(resp.starts_with("HTTP/1.1 200"), "{resp}");
        assert!(
            resp.contains("shutting down"),
            "the manual path keeps its response body: {resp}"
        );
        assert!(
            sup.state
                .lock()
                .unwrap()
                .shutdown
                .load(std::sync::atomic::Ordering::SeqCst),
            "the shutdown intent was accepted"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 2.1: the TCP form serves the same endpoint under the Bearer gate.
    #[test]
    fn remove_endpoint_works_over_the_tcp_form() {
        let (sup, dir, ep) = serve_harness_tcp("remove-tcp", "secret-1");
        let (cmd, args) = sleep_cmd();
        let resp = ep.post_auth(
            "/v1/run",
            &serde_json::json!({
                "name": "web", "command": cmd, "args": args,
                "register_only": true, "restart": "no",
            }),
            Some("secret-1"),
        );
        assert!(resp.starts_with("HTTP/1.1 200"), "{resp}");
        // No token → 401, and the record survives.
        let denied = ep.post_auth("/v1/apps/web/remove", &serde_json::json!({}), None);
        assert!(denied.starts_with("HTTP/1.1 401"), "{denied}");
        assert!(dir.join("app.d/adhoc/web.toml").exists());
        let ok = ep.post_auth(
            "/v1/apps/web/remove",
            &serde_json::json!({}),
            Some("secret-1"),
        );
        assert!(ok.starts_with("HTTP/1.1 200"), "{ok}");
        assert!(!dir.join("app.d/adhoc/web.toml").exists());
        sup.request_shutdown();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 3.2: `POST /v1/apply` maps the batch refusal for a live hand-deleted
    /// app/program to 409 with the blocker list — the same message `/api/apply`
    /// serves (both ride `Command::Apply`).
    #[test]
    fn apply_endpoint_maps_a_live_deletion_to_409() {
        let (sup, dir, ep) = serve_harness("apply-guard");
        let record = register_only_app(&ep, &dir, "web");
        let start = ep.post("/v1/programs/web/start", &serde_json::json!({}));
        assert!(start.starts_with("HTTP/1.1 200"), "{start}");
        // Hand-delete the registration while the program still runs.
        std::fs::remove_file(&record).unwrap();
        let resp = ep.post("/v1/apply", &serde_json::json!({}));
        assert!(
            resp.starts_with("HTTP/1.1 409"),
            "a live deleted app refuses the whole apply: {resp}"
        );
        for needle in ["apply refused, nothing was stopped or restarted", "ak stop"] {
            assert!(resp.contains(needle), "the 409 names {needle:?}: {resp}");
        }
        // The blocker is reported with its live state (`starting` until
        // startsecs elapses, `running` after — both are guard states).
        assert!(
            resp.contains("web.web (starting)") || resp.contains("web.web (running)"),
            "the 409 names the blocker program and state: {resp}"
        );
        assert!(!record.exists(), "the on-disk deletion is not rolled back");
        assert!(
            sup.state.lock().unwrap().programs.contains_key("web"),
            "the live program was not dropped by the refused apply"
        );
        sup.request_shutdown();
        let _ = std::fs::remove_dir_all(&dir);
    }

    // -- TCP form + Bearer admission (tcp-control-endpoint tasks 2.1–2.4) ----

    /// An ephemeral free localhost TCP port (bind :0, read, drop — the
    /// standard small race between drop and re-bind is accepted).
    fn free_tcp_port() -> u16 {
        std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port()
    }

    /// TCP-form harness: the same supervisor + control plane as
    /// [`serve_harness`], but the control endpoint is
    /// `tcp://127.0.0.1:<port>` with a Bearer token — the `tcp://` shape of
    /// `[daemon] socket` (task 2.1).
    fn serve_harness_tcp(
        tag: &str,
        token: &str,
    ) -> (Arc<Supervisor>, std::path::PathBuf, TestEndpoint) {
        let dir =
            std::env::temp_dir().join(format!("ak-test-srv-tcp-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("logs")).unwrap();
        let port = free_tcp_port();
        let cfg_path = dir.join("daemon.toml");
        std::fs::write(
            &cfg_path,
            format!(
                "[daemon]\nsocket = \"tcp://127.0.0.1:{port}\"\nauth_token = {token:?}\nlog_dir = {:?}\nstate_dir = {:?}\nmonitor_interval = 0.05\n",
                dir.join("logs").to_string_lossy(),
                dir.join("state").to_string_lossy(),
            ),
        )
        .unwrap();
        let (config, _) = crate::config::DaemonConfig::load_or_default(&cfg_path).unwrap();
        let sup = Supervisor::new(config, &dir).unwrap();
        sup.set_config_path(&cfg_path);
        let listener = bind(&sup).unwrap();
        let sup2 = sup.clone();
        std::thread::spawn(move || serve(sup2, listener));
        let sup3 = sup.clone();
        std::thread::spawn(move || sup3.run());
        let endpoint = TestEndpoint(
            crate::transport::Endpoint::parse(&format!("tcp://127.0.0.1:{port}")).unwrap(),
        );
        (sup, dir, endpoint)
    }

    impl TestEndpoint {
        /// GET with an optional `Authorization: Bearer <token>` header.
        fn get_auth(&self, path: &str, token: Option<&str>) -> String {
            let mut conn = self.0.connect().expect("connect to the control endpoint");
            let mut req = format!("GET {path} HTTP/1.1\r\nHost: localhost\r\n");
            if let Some(t) = token {
                req.push_str(&format!("Authorization: Bearer {t}\r\n"));
            }
            req.push_str("Connection: close\r\n\r\n");
            conn.write_all(req.as_bytes()).unwrap();
            conn.flush().unwrap();
            let mut out = String::new();
            conn.read_to_string(&mut out).unwrap();
            out
        }

        /// POST JSON with an optional Bearer token (the tcp:// form gate).
        fn post_auth(&self, path: &str, body: &serde_json::Value, token: Option<&str>) -> String {
            let mut conn = self.0.connect().expect("connect to the control endpoint");
            let payload = body.to_string();
            let mut req = format!(
                "POST {path} HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\n"
            );
            if let Some(t) = token {
                req.push_str(&format!("Authorization: Bearer {t}\r\n"));
            }
            req.push_str(&format!(
                "Content-Length: {}\r\nConnection: close\r\n\r\n{payload}",
                payload.len()
            ));
            conn.write_all(req.as_bytes()).unwrap();
            conn.flush().unwrap();
            let mut out = String::new();
            conn.read_to_string(&mut out).unwrap();
            out
        }
    }

    /// Task 2.1: the TCP form serves the same endpoint set with byte-equal
    /// semantics — `/v1/status` answers with the same projection (and the
    /// projection advertises the `tcp://` URL as the endpoint).
    #[test]
    fn tcp_form_serves_the_same_status_projection() {
        let (sup, dir, ep) = serve_harness_tcp("status", "secret-1");
        let resp = ep.get_auth("/v1/status", Some("secret-1"));
        assert!(resp.starts_with("HTTP/1.1 200"), "{resp}");
        let v: serde_json::Value =
            serde_json::from_str(resp.split("\r\n\r\n").nth(1).unwrap()).unwrap();
        assert_eq!(
            v["daemon"]["socket"],
            ep.0.display(),
            "the endpoint URL is what the projection carries"
        );
        // And /v1/health on the same form.
        let health = ep.get_auth("/v1/health", Some("secret-1"));
        assert!(health.starts_with("HTTP/1.1 200"), "{health}");
        sup.request_shutdown();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Tasks 2.2/2.3: the Bearer gate on the TCP form — missing and wrong
    /// tokens are 401 (program state never leaks), the correct token passes,
    /// and `/v1/health` is exempt with or without credentials.
    #[test]
    fn tcp_bearer_gate_three_ways() {
        let (sup, dir, ep) = serve_harness_tcp("gate", "secret-2");
        // No credential → 401, and the body names the fix.
        let bare = ep.get_auth("/v1/status", None);
        assert!(bare.starts_with("HTTP/1.1 401"), "{bare}");
        let body = bare.split("\r\n\r\n").nth(1).unwrap_or("");
        assert!(body.contains("auth_token"), "{body}");
        assert!(
            !body.contains("programs"),
            "no program state leaks through the gate: {body}"
        );
        // Wrong credential → 401.
        let wrong = ep.get_auth("/v1/status", Some("wrong"));
        assert!(wrong.starts_with("HTTP/1.1 401"), "{wrong}");
        // Correct credential → 200 (the plain == comparison, non-constant-time).
        let right = ep.get_auth("/v1/status", Some("secret-2"));
        assert!(right.starts_with("HTTP/1.1 200"), "{right}");
        // Health probe: exempt with no token and with a wrong token.
        assert!(ep.get_auth("/v1/health", None).starts_with("HTTP/1.1 200"));
        assert!(
            ep.get_auth("/v1/health", Some("wrong"))
                .starts_with("HTTP/1.1 200")
        );
        // Bearer scheme is case-insensitive; a non-Bearer scheme never is.
        let mut conn = ep.0.connect().unwrap();
        let req = "GET /v1/status HTTP/1.1\r\nHost: localhost\r\n\
                   Authorization: bearer secret-2\r\nConnection: close\r\n\r\n";
        std::io::Write::write_all(&mut conn, req.as_bytes()).unwrap();
        let mut out = String::new();
        conn.read_to_string(&mut out).unwrap();
        assert!(
            out.starts_with("HTTP/1.1 200"),
            "lowercase bearer passes: {out}"
        );
        sup.request_shutdown();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Task 2.3 (direct): the comparison function itself — right token
    /// passes, wrong token, wrong scheme, missing header and the fail-closed
    /// empty expected token are all rejected.
    #[test]
    fn bearer_comparison_right_and_wrong() {
        let mut h = HashMap::new();
        assert!(!bearer_ok(&h, "tok"), "no header at all");
        h.insert("authorization".into(), "Bearer tok".into());
        assert!(bearer_ok(&h, "tok"), "exact match passes");
        assert!(!bearer_ok(&h, "other"), "wrong token rejected");
        h.insert("authorization".into(), "Basic tok".into());
        assert!(!bearer_ok(&h, "tok"), "wrong scheme rejected");
        h.insert("authorization".into(), "Bearer".into());
        assert!(!bearer_ok(&h, "tok"), "scheme without token rejected");
        h.insert("authorization".into(), "Bearer  ".into());
        assert!(!bearer_ok(&h, "tok"), "empty presented token rejected");
        // Fail closed: an empty expected token (a tcp:// config without one
        // can never legally run, but the gate must not open either).
        h.insert("authorization".into(), "Bearer ".into());
        assert!(!bearer_ok(&h, ""), "empty expected token never matches");
    }

    /// Task 2.2 (IPC half): the path form ignores the Authorization header —
    /// a leftover Bearer header changes nothing (the OS decided admission).
    #[test]
    fn ipc_form_ignores_the_authorization_header() {
        let (sup, dir, ep) = serve_harness("ipc-ignore");
        // The existing harness endpoint is a local IPC path; any header must
        // be ignored, including a token that matches nothing.
        let resp = ep.get_auth("/v1/status", Some("leftover-token"));
        assert!(
            resp.starts_with("HTTP/1.1 200"),
            "the header is ignored: {resp}"
        );
        // Status-line only: "401" inside the body could be a temp path or pid.
        assert!(
            !resp.starts_with("HTTP/1.1 401"),
            "no 401 for an ignored header: {resp}"
        );
        sup.request_shutdown();
        let _ = std::fs::remove_dir_all(&dir);
    }

    // -- compose up endpoints (add-compose-up section 3) ----------------------

    /// A two-program declaration (startsecs 0, always) whose programs start
    /// instantly.
    fn compose_decl() -> String {
        let (cmd, args) = sleep_cmd();
        let a0: Vec<String> = args.clone();
        let a1: Vec<String> = args.to_vec();
        format!(
            "[program.web]\ncommand = '{cmd}'\nargs = {a0:?}\nstartsecs = 0.0\nrestart = 'always'\n\n\
             [program.worker]\ncommand = '{cmd}'\nargs = {a1:?}\nstartsecs = 0.0\nrestart = 'always'\ndepends_on = ['web']\n"
        )
        .replace('"', "'")
    }

    /// The request body; the base_dir is the harness dir (it exists, and the
    /// undeclared work_dir lands there — the spawn cwd must be real).
    fn compose_body(
        dir: &Path,
        name: &str,
        content: &str,
        extra: serde_json::Value,
    ) -> serde_json::Value {
        let mut v = serde_json::json!({
            "name": name,
            "content": content,
            "base_dir": dir.display().to_string(),
        });
        for (k, val) in extra.as_object().unwrap() {
            v[k] = val.clone();
        }
        v
    }

    fn frames_of(resp: &str) -> Vec<serde_json::Value> {
        let body = dechunk(resp.split("\r\n\r\n").nth(1).unwrap_or(""));
        body.lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    /// 3.1: the preview classifies all five situations with zero side effects.
    #[test]
    fn compose_preview_classifies_all_five() {
        let (sup, dir, ep) = serve_harness("compose-preview");
        let decl = compose_decl();

        // fresh: no record anywhere.
        let resp = ep.post(
            "/v1/compose/up/preview",
            &compose_body(&dir, "shop", &decl, serde_json::json!({})),
        );
        assert!(resp.starts_with("HTTP/1.1 200"), "{resp}");
        let v: serde_json::Value =
            serde_json::from_str(resp.split("\r\n\r\n").nth(1).unwrap()).unwrap();
        assert_eq!(v["classification"], "fresh", "{v}");
        assert!(v["programs"].as_array().unwrap().is_empty(), "{v}");
        assert!(
            !dir.join("app.d").join("shop.toml").exists(),
            "zero side effects"
        );
        assert!(!dir.join("app.d").join("shop.toml.pending").exists());

        // needs-update: a differing top-level record with a diff summary.
        std::fs::create_dir_all(dir.join("app.d")).unwrap();
        std::fs::write(
            dir.join("app.d").join("shop.toml"),
            "[program.web]\ncommand = 'old'\n",
        )
        .unwrap();
        let resp = ep.post(
            "/v1/compose/up/preview",
            &compose_body(&dir, "shop", &decl, serde_json::json!({})),
        );
        let v: serde_json::Value =
            serde_json::from_str(resp.split("\r\n\r\n").nth(1).unwrap()).unwrap();
        assert_eq!(v["classification"], "needs-update", "{v}");
        assert!(!v["diff"].as_array().unwrap().is_empty(), "{v}");
        assert_eq!(
            std::fs::read_to_string(dir.join("app.d").join("shop.toml")).unwrap(),
            "[program.web]\ncommand = 'old'\n",
            "preview does not rewrite"
        );

        // identical: the record IS the rendered snapshot.
        let rendered = crate::config::render_declaration(&decl, &dir, "shop", None).unwrap();
        std::fs::write(dir.join("app.d").join("shop.toml"), &rendered).unwrap();
        let resp = ep.post(
            "/v1/compose/up/preview",
            &compose_body(&dir, "shop", &decl, serde_json::json!({})),
        );
        let v: serde_json::Value =
            serde_json::from_str(resp.split("\r\n\r\n").nth(1).unwrap()).unwrap();
        assert_eq!(v["classification"], "identical", "{v}");

        // adhoc-takeover: a run product in app_dir/adhoc/.
        std::fs::create_dir_all(dir.join("app.d").join("adhoc")).unwrap();
        std::fs::write(
            dir.join("app.d").join("adhoc").join("take.toml"),
            "[program.t]\ncommand = 'x'\n",
        )
        .unwrap();
        let resp = ep.post(
            "/v1/compose/up/preview",
            &compose_body(&dir, "take", &decl, serde_json::json!({})),
        );
        let v: serde_json::Value =
            serde_json::from_str(resp.split("\r\n\r\n").nth(1).unwrap()).unwrap();
        assert_eq!(v["classification"], "adhoc-takeover", "{v}");

        // external-refused: a symlink record (top level).
        let body = dir.join("external.toml");
        std::fs::write(&body, "[program.e]\ncommand = 'x'\n").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&body, dir.join("app.d").join("ext.toml")).unwrap();
        #[cfg(windows)]
        std::os::windows::fs::symlink_file(&body, dir.join("app.d").join("ext.toml")).unwrap();
        let resp = ep.post(
            "/v1/compose/up/preview",
            &compose_body(&dir, "ext", &decl, serde_json::json!({})),
        );
        let v: serde_json::Value =
            serde_json::from_str(resp.split("\r\n\r\n").nth(1).unwrap()).unwrap();
        assert_eq!(v["classification"], "external-refused", "{v}");
        assert!(v["hint"].as_str().unwrap().contains("ak remove"), "{v}");
        sup.request_shutdown();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 3.4: the preview always carries `warnings`; under root the
    /// "runs as root" notice is present, and under a non-root daemon an
    /// unswitchable identity is a 400 validation error (zero side effects).
    #[cfg(unix)]
    #[test]
    fn compose_preview_carries_identity_warnings() {
        let (sup, dir, ep) = serve_harness("compose-preview-identity");
        let euid = unsafe { libc::geteuid() };

        // No `user`: feasible everywhere, warnings only for root execution.
        let resp = ep.post(
            "/v1/compose/up/preview",
            &compose_body(&dir, "shop", &compose_decl(), serde_json::json!({})),
        );
        assert!(resp.starts_with("HTTP/1.1 200"), "{resp}");
        let v: serde_json::Value =
            serde_json::from_str(resp.split("\r\n\r\n").nth(1).unwrap()).unwrap();
        let warnings = v["warnings"].as_array().expect("warnings is an array");
        assert_eq!(
            !warnings.is_empty(),
            euid == 0,
            "root warning must track the daemon euid: {v}"
        );
        assert_eq!(v["classification"], "fresh", "warnings never change class");

        // `user = "root"` from a non-root daemon cannot be honored.
        let as_root = format!("{}user = \"root\"\n", compose_decl());
        let resp = ep.post(
            "/v1/compose/up/preview",
            &compose_body(&dir, "shop", &as_root, serde_json::json!({})),
        );
        if euid == 0 {
            assert!(resp.starts_with("HTTP/1.1 200"), "{resp}");
            let v: serde_json::Value =
                serde_json::from_str(resp.split("\r\n\r\n").nth(1).unwrap()).unwrap();
            assert!(
                v["warnings"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|w| w.as_str().unwrap().contains("runs as root")),
                "{v}"
            );
        } else {
            assert!(resp.starts_with("HTTP/1.1 400"), "{resp}");
            assert!(resp.contains("cannot switch users"), "{resp}");
        }
        assert!(
            !dir.join("app.d").join("shop.toml").exists(),
            "preview has zero side effects"
        );
        sup.request_shutdown();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 3.1: validation failures answer 400 with zero side effects; a body
    /// without the declaration content is a 400 (a file path is not accepted).
    #[test]
    fn compose_preview_rejects_invalid_requests() {
        let (sup, dir, ep) = serve_harness("compose-preview-bad");
        // Unknown field in the declaration.
        let resp = ep.post(
            "/v1/compose/up/preview",
            &compose_body(
                &dir,
                "shop",
                "[program.web]\ncommand = 'x'\nbogus = 1\n",
                serde_json::json!({}),
            ),
        );
        assert!(resp.starts_with("HTTP/1.1 400"), "{resp}");
        // Reserved app name.
        let resp = ep.post(
            "/v1/compose/up/preview",
            &compose_body(&dir, "all", &compose_decl(), serde_json::json!({})),
        );
        assert!(resp.starts_with("HTTP/1.1 400"), "{resp}");
        assert!(resp.contains("reserved"), "{resp}");
        // A file path instead of content is refused by contract (the body
        // carries the declaration TEXT; a path would not round-trip over tcp).
        let resp = ep.post(
            "/v1/compose/up/preview",
            &serde_json::json!({
                "name": "shop",
                "content": "/opt/shop/.appkeeper.toml",
                "base_dir": "/opt/shop"
            }),
        );
        assert!(resp.starts_with("HTTP/1.1 400"), "{resp}");
        assert!(!dir.join("app.d").join("shop.toml").exists());
        sup.request_shutdown();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 3.5: a fresh multi-program submission streams the staged/starting
    /// frames, one started frame per program, and lands the record only after
    /// every judgement passed.
    #[test]
    fn compose_up_streams_frames_and_commits() {
        let (sup, dir, ep) = serve_harness("compose-up-ok");
        let resp = ep.post(
            "/v1/compose/up",
            &compose_body(&dir, "shop", &compose_decl(), serde_json::json!({})),
        );
        assert!(resp.starts_with("HTTP/1.1 200"), "{resp}");
        assert!(resp.contains("Transfer-Encoding: chunked"), "{resp}");
        let frames = frames_of(&resp);
        let phases: Vec<&str> = frames
            .iter()
            .filter_map(|f| f.get("phase").and_then(|p| p.as_str()))
            .collect();
        assert_eq!(phases[0], "staged", "{frames:?}");
        assert_eq!(
            phases.iter().filter(|p| **p == "starting").count(),
            2,
            "{frames:?}"
        );
        assert_eq!(
            phases.iter().filter(|p| **p == "started").count(),
            2,
            "{frames:?}"
        );
        let last = frames.last().unwrap();
        assert_eq!(last["ok"], true, "{frames:?}");
        assert_eq!(last["app"], "shop");
        assert_eq!(last["unchanged"], false);
        assert!(last["config_path"].as_str().unwrap().ends_with("shop.toml"));
        assert_eq!(last["programs"].as_array().unwrap().len(), 2, "{last}");
        // The record landed; no placeholder residue; the snapshot is exact.
        assert!(dir.join("app.d").join("shop.toml").exists());
        assert!(!dir.join("app.d").join("shop.toml.pending").exists());
        let rendered =
            crate::config::render_declaration(&compose_decl(), &dir, "shop", None).unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.join("app.d").join("shop.toml")).unwrap(),
            rendered
        );
        sup.request_shutdown();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 3.4: a needs-update submission without force is a 409 carrying the
    /// preview; with force it stops the old, lands the new and streams the
    /// pull-up.
    #[test]
    fn compose_up_needs_update_gate_then_force() {
        let (sup, dir, ep) = serve_harness("compose-up-update");
        std::fs::create_dir_all(dir.join("app.d")).unwrap();
        std::fs::write(
            dir.join("app.d").join("shop.toml"),
            "[program.web]\ncommand = 'old'\n",
        )
        .unwrap();

        // Without force: 409 with the classification and preview attached.
        let resp = ep.post(
            "/v1/compose/up",
            &compose_body(&dir, "shop", &compose_decl(), serde_json::json!({})),
        );
        assert!(resp.starts_with("HTTP/1.1 409"), "{resp}");
        let v: serde_json::Value =
            serde_json::from_str(resp.split("\r\n\r\n").nth(1).unwrap()).unwrap();
        assert_eq!(v["classification"], "needs-update", "{v}");
        assert_eq!(v["phase"], "conflict", "{v}");
        assert!(!v["diff"].as_array().unwrap().is_empty(), "{v}");
        assert_eq!(
            std::fs::read_to_string(dir.join("app.d").join("shop.toml")).unwrap(),
            "[program.web]\ncommand = 'old'\n",
            "nothing written on refusal"
        );
        assert!(
            sup.state.lock().unwrap().programs.is_empty(),
            "nothing started"
        );

        // With force: the whole pipeline runs.
        let resp = ep.post(
            "/v1/compose/up",
            &compose_body(
                &dir,
                "shop",
                &compose_decl(),
                serde_json::json!({ "force": true }),
            ),
        );
        assert!(resp.starts_with("HTTP/1.1 200"), "{resp}");
        let frames = frames_of(&resp);
        let last = frames.last().unwrap();
        assert_eq!(last["ok"], true, "{frames:?}");
        assert!(dir.join("app.d").join("shop.toml").exists());
        assert!(
            !dir.join("app.d").join("shop.toml.pending").exists(),
            "no placeholder residue"
        );
        let after = std::fs::read_to_string(dir.join("app.d").join("shop.toml")).unwrap();
        assert!(
            after.contains("[program.worker]"),
            "the new definition landed: {after}"
        );
        assert!(sup.state.lock().unwrap().programs.contains_key("web"));
        sup.request_shutdown();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 3.4: an external deployment link is refused with a 409 even when
    /// `force` is set, and nothing is touched.
    #[test]
    fn compose_up_refuses_external_record_even_with_force() {
        let (sup, dir, ep) = serve_harness("compose-up-ext");
        let body = dir.join("external.toml");
        std::fs::write(&body, "[program.web]\ncommand = 'old'\n").unwrap();
        std::fs::create_dir_all(dir.join("app.d")).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&body, dir.join("app.d").join("shop.toml")).unwrap();
        #[cfg(windows)]
        std::os::windows::fs::symlink_file(&body, dir.join("app.d").join("shop.toml")).unwrap();

        let resp = ep.post(
            "/v1/compose/up",
            &compose_body(
                &dir,
                "shop",
                &compose_decl(),
                serde_json::json!({ "force": true }),
            ),
        );
        assert!(resp.starts_with("HTTP/1.1 409"), "{resp}");
        let v: serde_json::Value =
            serde_json::from_str(resp.split("\r\n\r\n").nth(1).unwrap()).unwrap();
        assert_eq!(v["classification"], "external-refused", "{v}");
        assert!(v["hint"].as_str().unwrap().contains("ak remove"), "{v}");
        assert!(
            std::fs::read_to_string(&body)
                .unwrap()
                .contains("[program.web]"),
            "the external config is untouched"
        );
        assert!(sup.state.lock().unwrap().programs.is_empty());
        sup.request_shutdown();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 3.3/3.5: an identical submission with everything running answers the
    /// single `unchanged` frame — no write, no spawn.
    #[test]
    fn compose_up_identical_all_running_answers_unchanged() {
        let (sup, dir, ep) = serve_harness("compose-up-idem");
        // Land a real app first.
        let first = ep.post(
            "/v1/compose/up",
            &compose_body(&dir, "shop", &compose_decl(), serde_json::json!({})),
        );
        assert!(first.starts_with("HTTP/1.1 200"), "{first}");
        let record = dir.join("app.d").join("shop.toml");
        let before = std::fs::read_to_string(&record).unwrap();

        let resp = ep.post(
            "/v1/compose/up",
            &compose_body(&dir, "shop", &compose_decl(), serde_json::json!({})),
        );
        assert!(resp.starts_with("HTTP/1.1 200"), "{resp}");
        let frames = frames_of(&resp);
        assert_eq!(
            frames.len(),
            1,
            "the no-op is one terminal frame: {frames:?}"
        );
        assert_eq!(frames[0]["ok"], true, "{frames:?}");
        assert_eq!(frames[0]["unchanged"], true, "{frames:?}");
        assert_eq!(frames[0]["identical"], true, "{frames:?}");
        assert_eq!(std::fs::read_to_string(&record).unwrap(), before);
        sup.request_shutdown();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 3.5: a failing startup judgement aborts server-side — a structured
    /// failure frame, every spawned program stopped, zero record residue.
    #[test]
    fn compose_up_failure_frame_and_zero_residue() {
        let (sup, dir, ep) = serve_harness("compose-up-fail");
        let mut decl = compose_decl();
        #[cfg(unix)]
        decl.push_str("\n[program.bad]\ncommand = '/bin/sh'\nargs = ['-c', 'exit 3']\nstartsecs = 0.05\nrestart = 'always'\n");
        #[cfg(windows)]
        decl.push_str("\n[program.bad]\ncommand = 'cmd'\nargs = ['/c', 'exit 3']\nstartsecs = 0.05\nrestart = 'always'\n");
        let resp = ep.post(
            "/v1/compose/up",
            &compose_body(&dir, "shop", &decl, serde_json::json!({})),
        );
        assert!(resp.starts_with("HTTP/1.1 200"), "{resp}");
        let frames = frames_of(&resp);
        let last = frames.last().unwrap();
        assert_eq!(last["ok"], false, "{frames:?}");
        assert_eq!(last["phase"], "startup", "{frames:?}");
        assert_eq!(last["program"], "bad", "{frames:?}");
        // Zero residue: no record, no placeholder, no programs left.
        assert!(!dir.join("app.d").join("shop.toml").exists());
        assert!(!dir.join("app.d").join("shop.toml.pending").exists());
        let st = sup.state.lock().unwrap();
        assert!(!st.staged.contains_key("shop"));
        assert!(
            st.programs.is_empty(),
            "every program was dropped: {:?}",
            st.programs.keys().collect::<Vec<_>>()
        );
        drop(st);
        sup.request_shutdown();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `-e/--env` values reach the render: the `vars` object in the request
    /// body substitutes `${NAME}` before the declaration is parsed, and the
    /// rendered record holds the value (not the placeholder).
    #[test]
    fn compose_up_substitutes_env_vars() {
        let (sup, dir, ep) = serve_harness("compose-up-vars");
        let (cmd, args) = sleep_cmd();
        let decl = format!(
            "[program.web]\ncommand = '{cmd}'\nargs = {args:?}\nstartsecs = 0.0\n\
             restart = 'always'\nwork_dir = '${{WD}}'\n\n\
             [program.web.env]\nPORT = '${{PORT}}'\n"
        );
        std::fs::create_dir_all(dir.join("work")).unwrap();
        let resp = ep.post(
            "/v1/compose/up",
            &compose_body(
                &dir,
                "shop",
                &decl,
                serde_json::json!({ "vars": { "WD": "./work", "PORT": "9000" } }),
            ),
        );
        assert!(resp.starts_with("HTTP/1.1 200"), "{resp}");
        let frames = frames_of(&resp);
        assert_eq!(frames.last().unwrap()["ok"], true, "{frames:?}");
        let record = std::fs::read_to_string(dir.join("app.d").join("shop.toml")).unwrap();
        assert!(!record.contains("${"), "no placeholder survives: {record}");
        assert!(record.contains("9000"), "env var substituted: {record}");
        assert!(
            record.contains(&format!("{}/./work", dir.display())),
            "work_dir substituted then absolutized: {record}"
        );
        drop(sup.state.lock().unwrap());
        sup.request_shutdown();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
