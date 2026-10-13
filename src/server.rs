//! The local control plane: loopback HTTP/1.1 JSON API on std::TcpListener.
//!
//! Hand-rolled (instead of a framework) for one reason: the `log --follow`
//! endpoint must stream lines as they are produced and flush after every
//! write; generic HTTP stacks buffer responses and make that impossible
//! (see design D2). The request surface we need is deliberately tiny:
//! GET/POST, no request bodies, `Connection: close`.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::time::Duration;

use anyhow::{Context, Result};
use log::{info, warn};
use serde::{Deserialize, Serialize};

use crate::supervisor::{Command, Supervisor};

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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct DaemonInfo {
    pub version: String,
    pub port: u16,
    pub apps: usize,
    /// Host-level resource overview.
    pub system: crate::metrics::SystemMetrics,
    /// Configured monitor interval (console overview).
    pub monitor_interval: f64,
    /// Daemon wall-clock runtime in seconds.
    pub uptime_secs: f64,
    /// Where the daemon config was loaded from.
    pub config_source: String,
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
        daemon: DaemonInfo {
            version: env!("CARGO_PKG_VERSION").to_string(),
            port: st.config.daemon.port,
            apps: st.apps.len(),
            system: sup.metrics.system(),
            monitor_interval: st.config.daemon.monitor_interval,
            uptime_secs: (sup.uptime_secs() * 100.0).round() / 100.0,
            config_source: sup.config_path().display().to_string(),
        },
        programs,
        pending: st.pending.clone(),
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
pub(crate) fn app_config_path(st: &crate::supervisor::SupervisorState, app: &str) -> Option<String> {
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

/// Bind the API port. Called on the startup path so a port clash fails fast.
pub fn bind(sup: &Arc<Supervisor>) -> Result<TcpListener> {
    let (host, port) = {
        let st = sup.state.lock().unwrap();
        (st.config.daemon.host.clone(), st.config.daemon.port)
    };
    let addr = format!("{host}:{port}");
    let listener = TcpListener::bind(&addr).with_context(|| {
        format!("failed to bind control API on {addr} (is another ak running?)")
    })?;
    info!("control API listening on http://{addr}");
    Ok(listener)
}

/// Accept loop; run on a background thread after `bind` succeeded.
pub fn serve(sup: Arc<Supervisor>, listener: TcpListener) {
    let token = {
        let st = sup.state.lock().unwrap();
        st.config.daemon.auth_token.clone()
    };
    for conn in listener.incoming() {
        match conn {
            Ok(stream) => {
                let sup = sup.clone();
                let token = token.clone();
                std::thread::Builder::new()
                    .name("api-conn".into())
                    .spawn(move || handle_connection(&sup, &token, stream))
                    .ok();
            }
            Err(e) => {
                warn!("api accept error: {e}");
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    }
}

fn handle_connection(sup: &Arc<Supervisor>, token: &str, stream: TcpStream) {
    let mut writer = match stream.try_clone() {
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

    if !authorized(&headers, token, &path) {
        let _ = writer.write_all(&err_bytes(
            401,
            "unauthorized: missing or invalid bearer token",
        ));
        return;
    }

    let segments: Vec<&str> = path.trim_matches('/').split('/').collect();
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
                    daemon: DaemonInfo {
                        version: env!("CARGO_PKG_VERSION").to_string(),
                        port: st.config.daemon.port,
                        apps: st.apps.len(),
                        system: sup.metrics.system(),
                        monitor_interval: st.config.daemon.monitor_interval,
                        uptime_secs: (sup.uptime_secs() * 100.0).round() / 100.0,
                        config_source: sup.config_path().display().to_string(),
                    },
                    programs,
                    pending: st.pending.clone(),
                },
            )
        }
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
                                let _ = writer.write_all(&err_bytes(
                                    409,
                                    &format!("{phase}: {reason}"),
                                ));
                                return;
                            }
                            crate::supervisor::StartupOutcome::TimedOut { waited_secs } => {
                                let _ = writer.write_all(&err_bytes(
                                    500,
                                    &format!(
                                        "startup was not confirmed within {waited_secs:.1}s"
                                    ),
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
        ("GET", ["v1", "programs", name, "logs"]) => {
            // Streaming response: owns the connection until the client leaves.
            respond_logs(sup, name, &query, writer);
            return;
        }
        // run-daemon-first: the ad-hoc "register and start" endpoint. Streams
        // NDJSON progress frames (chunked) and owns the connection.
        ("POST", ["v1", "run"]) => {
            respond_adhoc_run(sup, &body, writer);
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
                // run-daemon-first: `run --app` blocks per program until its
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
                            format!("error: program {} is {state} (startup not confirmed)", s.program)
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
                let failed = enriched
                    .iter()
                    .any(|p| p["result"].as_str().map(|r| r.starts_with("error")).unwrap_or(false));
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

fn authorized(headers: &HashMap<String, String>, token: &str, path: &str) -> bool {
    if token.is_empty() || path == "/v1/health" {
        return true;
    }
    headers
        .get("authorization")
        .map(|v| v == &format!("Bearer {token}"))
        .unwrap_or(false)
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
    })
}

/// Write one NDJSON frame as a chunked-transfer chunk (line + CRLF).
fn write_ndjson_chunk(writer: &mut TcpStream, value: &serde_json::Value) -> bool {
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
fn finish_chunked(writer: &mut TcpStream) {
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
fn respond_adhoc_run(sup: &Arc<Supervisor>, body: &[u8], mut writer: TcpStream) {
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

    // Idempotent upsert (control-plane spec: 同名重复提交走再生, 无变化幂等):
    // the record already held identical content, so nothing was written and the
    // process was not restarted. No staged entry exists — answer directly.
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

    // `--restart=no` (D8): no startup judgement, commit directly.
    if up.register_only {
        // A regeneration already wrote the record; there is no placeholder to
        // commit, so just answer.
        if up.committed {
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
            // A regenerated (already committed) app has no staged entry to
            // commit — the new record is already on disk. Answer directly.
            if up.committed {
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
            // A regenerated app has no staged entry: its record stays on disk
            // and the (failed) process is simply stopped.
            if up.committed {
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
            if up.committed {
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
/// REGENERATED (already committed) ad-hoc program fails its startup judgement:
/// there is no staged entry to abort, so the record stays and only the failed
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

fn respond_logs(sup: &Arc<Supervisor>, name: &str, query: &str, mut writer: TcpStream) {
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

    let write_chunk = |writer: &mut TcpStream, line: &str| -> bool {
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
                port: 1,
                apps: 1,
                system: SystemMetrics {
                    cpu_percent: Some(7.3),
                    mem_used_bytes: 10,
                    mem_total_bytes: 100,
                },
                monitor_interval: 1.0,
                uptime_secs: 5.0,
                config_source: "/tmp/appkeeper.toml".into(),
            },
            programs: vec![info],
            pending: crate::supervisor::PendingDoc::default(),
        }
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

    /// metrics/control-plane: a supervised program's projection carries the
    /// metric fields from the shared sampler table; not-running → null.
    #[test]
    fn program_info_merges_metrics_from_table() {
        let dir = std::env::temp_dir().join(format!("xk-proj-{}", std::process::id()));
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

    use std::net::TcpStream;

    /// Boot a real supervisor + control plane on a free port with a private
    /// app_dir/log_dir; returns (sup, dir, port). Caller shuts down.
    fn serve_harness(tag: &str) -> (Arc<Supervisor>, std::path::PathBuf, u16) {
        let dir = std::env::temp_dir().join(format!("xk-srv-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("logs")).unwrap();
        let port = {
            let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            l.local_addr().unwrap().port()
        };
        let cfg_path = dir.join("daemon.toml");
        std::fs::write(
            &cfg_path,
            format!(
                "[daemon]\nport = {port}\nlog_dir = {:?}\nmonitor_interval = 0.05\n",
                dir.join("logs").to_string_lossy()
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
        (sup, dir, port)
    }

    /// Send a POST with a JSON body and read the whole response.
    fn post(port: u16, path: &str, body: &serde_json::Value) -> String {
        let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
        let body = body.to_string();
        let req = format!(
            "POST {path} HTTP/1.1\r\nHost: x\r\nContent-Type: application/json\r\n\
             Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        s.write_all(req.as_bytes()).unwrap();
        let mut out = String::new();
        s.read_to_string(&mut out).unwrap();
        out
    }

    fn get(port: u16, path: &str) -> String {
        let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
        let req = format!("GET {path} HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n");
        s.write_all(req.as_bytes()).unwrap();
        let mut out = String::new();
        s.read_to_string(&mut out).unwrap();
        out
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

    fn sleep_cmd() -> (&'static str, Vec<String>) {
        if cfg!(windows) {
            ("C:\\Windows\\System32\\ping.exe", vec!["-n".into(), "30".into(), "127.0.0.1".into()])
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

    /// 5.2/5.3: a successful ad-hoc run (no health check) streams only the
    /// terminal frame (control-plane spec: 无健康探测时无需中间帧), commits
    /// the record and returns {program, config_path, pid}.
    #[test]
    fn run_endpoint_streams_and_commits() {
        let (sup, dir, port) = serve_harness("run-ok");
        let (cmd, args) = sleep_cmd();
        let resp = post(
            port,
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
        assert_eq!(frames.len(), 1, "no health check → terminal frame only: {frames:?}");
        let last = frames.last().unwrap();
        assert_eq!(last["ok"], true, "{frames:?}");
        assert_eq!(last["program"], "web");
        assert!(last["pid"].as_u64().unwrap() > 0);
        assert!(last["config_path"].as_str().unwrap().ends_with("web.toml"));
        assert!(dir.join("app.d").join("web.toml").exists(), "record written");
        assert!(!dir.join("app.d").join("web.toml.pending").exists());
        sup.request_shutdown();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 5.3: a program WITH a health check streams the staged/waiting frames
    /// (with the estimate) before the terminal frame (control-plane spec:
    /// 有健康探测时流式回传进度帧).
    #[test]
    fn run_endpoint_with_health_streams_progress() {
        let (sup, dir, port) = serve_harness("run-health");
        let (cmd, args) = sleep_cmd();
        // `health_check = "true"` is an exec check that succeeds immediately.
        let resp = post(
            port,
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
        assert!(dir.join("app.d").join("web.toml").exists());
        sup.request_shutdown();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 5.3: `--restart=no` emits only the terminal frame (no intermediate
    /// progress) and registers with pid = null.
    #[test]
    fn run_endpoint_register_only_emits_only_terminal_frame() {
        let (sup, dir, port) = serve_harness("run-no");
        let (cmd, args) = sleep_cmd();
        let resp = post(
            port,
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
        assert!(dir.join("app.d").join("web.toml").exists());
        sup.request_shutdown();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 5.5: validation errors answer a real status code, not a 200 stream.
    #[test]
    fn run_endpoint_error_codes() {
        let (sup, dir, port) = serve_harness("run-err");
        let (cmd, args) = sleep_cmd();
        // Reserved name `all` → 400.
        let resp = post(
            port,
            "/v1/run",
            &serde_json::json!({ "name": "all", "command": cmd, "args": args }),
        );
        assert!(resp.starts_with("HTTP/1.1 400"), "{resp}");
        assert!(resp.contains("reserved"), "{resp}");
        // Missing command → 400.
        let resp = post(port, "/v1/run", &serde_json::json!({ "name": "web" }));
        assert!(resp.starts_with("HTTP/1.1 400"), "{resp}");
        sup.request_shutdown();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 5.5: with a Bearer token configured, the new endpoint requires auth.
    #[test]
    fn run_endpoint_requires_bearer_when_configured() {
        let dir = std::env::temp_dir().join(format!("xk-srv-auth-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("logs")).unwrap();
        let port = {
            let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            l.local_addr().unwrap().port()
        };
        let cfg_path = dir.join("daemon.toml");
        std::fs::write(
            &cfg_path,
            format!(
                "[daemon]\nport = {port}\nauth_token = \"s3cr3t\"\nlog_dir = {:?}\n",
                dir.join("logs").to_string_lossy()
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

        let resp = post(port, "/v1/run", &serde_json::json!({ "name": "web", "command": "/bin/sleep" }));
        assert!(resp.starts_with("HTTP/1.1 401"), "{resp}");
        sup.request_shutdown();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 5.4: the start endpoint keeps the legacy `result` and adds structured
    /// {name, config_path, pid}.
    #[test]
    fn start_endpoint_adds_structured_fields() {
        let (sup, dir, port) = serve_harness("start-fields");
        // Register + start an app through the ad-hoc path first.
        let (cmd, args) = sleep_cmd();
        let resp = post(
            port,
            "/v1/run",
            &serde_json::json!({ "name": "web", "command": cmd, "args": args, "register_only": true, "restart": "no" }),
        );
        assert!(resp.starts_with("HTTP/1.1 200"), "{resp}");
        // Stop it so start is legal, then start via the standard endpoint.
        let _ = post(port, "/v1/programs/web/stop", &serde_json::json!({}));
        let resp = post(port, "/v1/programs/web/start", &serde_json::json!({}));
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
        let status = get(port, "/v1/status");
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
        let (sup, dir, port) = serve_harness("start-wait");
        let (cmd, args) = sleep_cmd();
        let _ = post(
            port,
            "/v1/run",
            &serde_json::json!({ "name": "web", "command": cmd, "args": args, "register_only": true, "restart": "no" }),
        );
        // The registered program uses the built-in 3s startsecs; measure that
        // the wait=1 call blocks at least ~3s and returns a pid.
        let t0 = std::time::Instant::now();
        let resp = post(port, "/v1/programs/web/start?wait=1", &serde_json::json!({}));
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
        let _ = post(port, "/v1/programs/web/stop", &serde_json::json!({}));
        let t0 = std::time::Instant::now();
        let resp = post(port, "/v1/programs/web/start", &serde_json::json!({}));
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
        let (sup, dir, port) = serve_harness("fanout-fields");
        let (cmd, args) = sleep_cmd();
        let _ = post(
            port,
            "/v1/run",
            &serde_json::json!({ "name": "web", "command": cmd, "args": args, "register_only": true, "restart": "no" }),
        );
        let resp = post(port, "/v1/apps/web/start", &serde_json::json!({}));
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
}
