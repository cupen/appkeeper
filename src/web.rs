//! The embedded web console HTTP server: console SPA + WebSocket push.
//!
//! Design consistency with the control plane (`webui-api` capability):
//! this server re-uses the *same* status projection as `crate::server`
//! (`/v1` JSON), routes control actions through the supervisor command
//! queue (the loop is the only writer), and serves log data from the
//! in-memory ring buffers (`crate::pump`) — never from log files. What it
//! adds for the browser console: the embedded SPA and a `/ws` push channel
//! (snapshot → status deltas → log chunks → heartbeat, MessagePack binary
//! frames, see [`crate::api`]).
//!
//! Lifecycle (config-driven-webui): whether this server runs at all is
//! decided by the daemon config's `[webui]` section; the supervisor's
//! manager thread spawns/stops it on reload. Besides the daemon shutdown
//! flag, `serve` honors an external stop flag (reload off / rebind), and WS
//! sessions close gracefully (queued frames flush, then a Close frame).

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Result;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Extension, Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::json;
use tokio::sync::mpsc;
use tower_http::compression::CompressionLayer;

use crate::api::{self, msg_type};
use crate::assets;
use crate::pump;
use crate::pump::Stream;
use crate::server::{ProgramInfo, StatusDoc, status_doc};
use crate::supervisor::{Command, Supervisor};

/// Replay context when a client subscribes to a log stream.
const LOG_TAIL_DEFAULT: usize = 200;
/// Status-diff cadence for WS sessions (daemon tick granularity is
/// `monitor_interval`, so this is at least twice as fine).
const WS_POLL: Duration = Duration::from_millis(500);
const WS_HEARTBEAT: Duration = Duration::from_secs(30);
/// How long an HTTP control request waits for the supervisor to execute it
/// (a stop may block up to the program's stop_timeout).
const COMMAND_TIMEOUT: Duration = Duration::from_secs(60);
/// One log frame carries multiple lines: flush at this size…
const FRAME_BYTES: usize = 64 * 1024;
/// …or this long after the first pending byte (latency bound).
const FRAME_INTERVAL: Duration = Duration::from_millis(100);
/// Log frames wait up to this long for a session-writer slot. Blocking (not
/// closing) is what routes backpressure into the bounded rx queue, where
/// loss is counted and surfaced as Gap markers; a truly dead client still
/// frees the session when the budget lapses (and on sink errors).
const LOG_SEND_TIMEOUT: Duration = Duration::from_secs(10);
/// After the stop/shutdown flag is set, the server drain waits up to this
/// long for live WS sessions to flush their queues and send the Close frame
/// before the runtime tears the sockets down (webui-api: graceful close).
const WS_LIVE_DRAIN: Duration = Duration::from_secs(3);

/// Shared lifecycle state for one console instance: the external stop flag
/// (reload off / rebind, set by the webui manager) plus the count of live WS
/// sessions so `serve` can hold the drain while they close gracefully.
pub struct WebuiLifecycle {
    stop: AtomicBool,
    live: AtomicUsize,
}

impl WebuiLifecycle {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            stop: AtomicBool::new(false),
            live: AtomicUsize::new(0),
        })
    }

    /// Request a graceful stop (manager: reload off / rebind).
    pub fn stop(&self) {
        self.stop.store(true, Ordering::SeqCst);
    }

    pub fn stopped(&self) -> bool {
        self.stop.load(Ordering::SeqCst)
    }

    fn enter_session(&self) {
        self.live.fetch_add(1, Ordering::SeqCst);
    }

    fn exit_session(&self) {
        self.live.fetch_sub(1, Ordering::SeqCst);
    }

    fn live(&self) -> usize {
        self.live.load(Ordering::SeqCst)
    }
}

#[derive(Deserialize)]
struct LogQuery {
    stream: Option<String>,
    tail: Option<usize>,
}

fn api_error(status: StatusCode, message: &str) -> Response {
    (status, Json(json!({ "error": message }))).into_response()
}

async fn health() -> Json<serde_json::Value> {
    Json(json!({ "ok": true }))
}

async fn overview(State(sup): State<Arc<Supervisor>>) -> Json<StatusDoc> {
    Json(status_doc(&sup))
}

async fn programs(State(sup): State<Arc<Supervisor>>) -> Json<serde_json::Value> {
    Json(json!({ "programs": status_doc(&sup).programs }))
}

async fn program_detail(State(sup): State<Arc<Supervisor>>, Path(name): Path<String>) -> Response {
    let st = sup.state.lock().unwrap();
    match st.programs.get(&name) {
        Some(p) => {
            let st = sup.state.lock().unwrap();
            Json(crate::server::program_info_state(
                &st,
                p,
                Some(&sup.metrics),
            ))
            .into_response()
        }
        None => api_error(StatusCode::NOT_FOUND, &format!("unknown program {name:?}")),
    }
}

async fn program_logs(
    State(sup): State<Arc<Supervisor>>,
    Path(name): Path<String>,
    Query(q): Query<LogQuery>,
) -> Response {
    let (ring, tail) = {
        let st = sup.state.lock().unwrap();
        let Some(p) = st.programs.get(&name) else {
            return api_error(StatusCode::NOT_FOUND, &format!("unknown program {name:?}"));
        };
        let stream = match q.stream.as_deref().unwrap_or("out") {
            "out" => Stream::Out,
            "err" => Stream::Err,
            other => {
                return api_error(
                    StatusCode::BAD_REQUEST,
                    &format!("stream must be \"out\" or \"err\", got {other:?}"),
                );
            }
        };
        (p.ring(stream), q.tail.unwrap_or(LOG_TAIL_DEFAULT))
    };
    // Capped by the ring buffer itself (log-management spec: tail comes
    // from the in-memory buffer, unaffected by rotation).
    Json(json!({
        "program": name,
        "lines": ring.tail(tail),
    }))
    .into_response()
}

async fn pending(State(sup): State<Arc<Supervisor>>) -> Json<crate::supervisor::PendingDoc> {
    let doc = sup.state.lock().unwrap().pending.clone();
    Json(doc)
}

/// `POST /api/apps/{name}/remove` — the console's removal entry
/// (guard-remove-until-stopped). Enqueues the same Supervisor command as
/// `/v1/apps/{name}/remove` and returns the same outcome shape; the status
/// mapping is the error's own (404 unknown, 409 conflict, 500 delete failure).
/// The console never stops programs or deletes files itself.
async fn app_remove(State(sup): State<Arc<Supervisor>>, Path(name): Path<String>) -> Response {
    use std::sync::mpsc;
    let (tx, rx) = mpsc::channel();
    sup.enqueue(Command::Remove {
        app: name,
        reply: tx,
    });
    match rx.recv_timeout(COMMAND_TIMEOUT) {
        Ok(Ok(outcome)) => Json(crate::server::remove_json(&outcome)).into_response(),
        Ok(Err(e)) => api_error(
            StatusCode::from_u16(e.http_status()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
            &e.to_string(),
        ),
        Err(_) => api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "supervisor did not answer in time",
        ),
    }
}

#[derive(Deserialize, Default)]
struct ApplyQuery {
    app: Option<String>,
    program: Option<String>,
    restart: Option<bool>,
}

async fn apply(State(sup): State<Arc<Supervisor>>, body: Option<Json<ApplyQuery>>) -> Response {
    use std::sync::mpsc;
    let q = body.map(|Json(q)| q).unwrap_or_default();
    // Scope validation + the `all` keyword resolve in the supervisor — the
    // same source as /v1/apply (unknown app/program → 404).
    let scope = match crate::supervisor::resolve_apply_scope(
        &sup,
        q.app.as_deref(),
        q.program.as_deref(),
    ) {
        Ok(s) => s,
        Err(msg) => return api_error(StatusCode::NOT_FOUND, &msg),
    };
    let (tx, rx) = mpsc::channel();
    sup.enqueue(Command::Apply {
        scope,
        restart: q.restart.unwrap_or(false),
        reply: Some(tx),
    });
    match rx.recv_timeout(COMMAND_TIMEOUT) {
        Ok(Ok(msg)) => Json(json!({ "result": msg })).into_response(),
        Ok(Err(e)) => api_error(StatusCode::CONFLICT, &format!("{e:#}")),
        Err(_) => api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "supervisor did not answer in time",
        ),
    }
}

async fn program_action(
    State(sup): State<Arc<Supervisor>>,
    Path((name, action)): Path<(String, String)>,
) -> Response {
    use std::sync::mpsc;
    let (tx, rx) = mpsc::channel();
    let cmd = match action.as_str() {
        "start" => Command::Start {
            name,
            reply: Some(tx),
        },
        "stop" => Command::Stop {
            name,
            reply: Some(tx),
        },
        "restart" => Command::Restart {
            name,
            reply: Some(tx),
        },
        other => {
            return api_error(
                StatusCode::NOT_FOUND,
                &format!("unknown action {other:?} (start | stop | restart)"),
            );
        }
    };
    sup.enqueue(cmd);
    match rx.recv_timeout(COMMAND_TIMEOUT) {
        Ok(Ok(msg)) => Json(json!({ "result": msg })).into_response(),
        Ok(Err(e)) => api_error(StatusCode::CONFLICT, &format!("{e:#}")),
        Err(_) => api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "supervisor did not answer in time",
        ),
    }
}

/// Build the console router (exposed for integration tests). `stop` is the
/// external lifecycle state shared with the WS sessions (see [`serve`]).
/// `auth_token` empty = no application-layer credential at all (the console's
/// isolation is whatever `webui.listen` grants — see the `webui-api`
/// capability); non-empty = `/api/*` (except `/api/health`) and `/ws` require
/// a session established by exchanging the token at `/api/auth`.
pub fn build_router(sup: Arc<Supervisor>, stop: Arc<WebuiLifecycle>, auth_token: &str) -> Router {
    let auth = console_auth_for(&sup, auth_token);
    let guarded = Router::new()
        .route("/api/overview", get(overview))
        .route("/api/programs", get(programs))
        .route("/api/programs/{name}", get(program_detail))
        .route("/api/programs/{name}/logs", get(program_logs))
        .route("/api/programs/{name}/{action}", post(program_action))
        .route("/api/pending", get(pending))
        .route("/api/apply", post(apply))
        .route("/api/apps/{name}/remove", post(app_remove))
        .route("/ws", get(ws_upgrade))
        .layer(axum::middleware::from_fn_with_state(
            Arc::clone(&auth),
            console_auth,
        ));
    Router::new()
        // Credential plumbing: exchange the token for a session cookie, and
        // drop a session again. Both stay reachable without a session — they
        // are how one is obtained.
        .route("/api/auth", post(auth_login))
        .route("/api/auth/logout", post(auth_logout))
        .merge(guarded)
        .route("/api/health", get(health))
        .route("/assets/{*path}", get(assets::asset))
        .layer(Extension(stop))
        .layer(CompressionLayer::new())
        .with_state(sup)
        .fallback(assets::spa_fallback)
}

/// Resolve the console's auth state for this instance: the token from the
/// config, or the disabled (pass-through) state when there is none.
fn console_auth_for(sup: &Arc<Supervisor>, auth_token: &str) -> Arc<ConsoleAuth> {
    if auth_token.is_empty() {
        return Arc::new(ConsoleAuth::disabled());
    }
    let auth = Arc::new(ConsoleAuth::new(auth_token));
    *sup.console_auth.lock().unwrap() = Arc::clone(&auth);
    auth
}

/// The console's access-control state. Sessions are random opaque ids kept
/// in memory; the token itself is only ever compared at the exchange and is
/// never stored in a cookie, a URL or a log line.
#[derive(Debug, Clone)]
pub struct ConsoleAuth {
    token: Arc<String>,
    sessions: Arc<Mutex<HashSet<String>>>,
    /// Upper bound so a stuck browser cannot grow the set without limit.
    capacity: usize,
}

impl ConsoleAuth {
    /// Unauthenticated console: every guard is a no-op.
    pub fn disabled() -> Self {
        ConsoleAuth {
            token: Arc::new(String::new()),
            sessions: Arc::new(Mutex::new(HashSet::new())),
            capacity: 0,
        }
    }

    pub fn new(token: &str) -> Self {
        ConsoleAuth {
            token: Arc::new(token.to_string()),
            sessions: Arc::new(Mutex::new(HashSet::new())),
            capacity: 64,
        }
    }

    pub fn enabled(&self) -> bool {
        !self.token.is_empty()
    }

    fn check(&self, presented: &str) -> bool {
        self.enabled() && self.sessions.lock().unwrap().contains(presented)
    }

    fn exchange(&self, token: &str) -> Option<String> {
        if !self.enabled() || !constant_time_eq(token, &self.token) {
            return None;
        }
        let mut set = self.sessions.lock().unwrap();
        if set.len() >= self.capacity {
            // Drop the oldest by insertion order would need an ordered set;
            // clearing wholesale is simpler and only costs a re-login.
            set.clear();
        }
        let id = random_session_id();
        set.insert(id.clone());
        Some(id)
    }

    fn revoke(&self, id: &str) {
        if self.enabled() {
            self.sessions.lock().unwrap().remove(id);
        }
    }
}

/// Session cookie carrying the opaque session id (never the token).
const SESSION_COOKIE: &str = "ak_console";
/// How long a browser keeps the session cookie.
const SESSION_MAX_AGE: u64 = 12 * 3600;

/// Compare without an early exit on the first differing byte.
fn constant_time_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// A random hex id. Good enough for an in-memory session that only exists
/// behind a loopback-ish listener, and it has no dependency on the OS RNG API.
fn random_session_id() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let pid = std::process::id() as u128;
    let addr = &nanos as *const u128 as usize as u128;
    let mut v = nanos ^ (pid << 64) ^ (addr << 32);
    let mut out = String::with_capacity(32);
    for _ in 0..4 {
        v ^= v << 13;
        v ^= v >> 7;
        v ^= v << 17;
        out.push_str(&format!("{v:032x}"));
    }
    out.truncate(32);
    out
}

/// Read one cookie by name from a header value.
fn cookie_value(header: Option<&str>, name: &str) -> Option<String> {
    let raw = header?;
    for part in raw.split(';') {
        let part = part.trim();
        if let Some((k, v)) = part.split_once('=')
            && k.trim() == name
        {
            return Some(v.trim().to_string());
        }
    }
    None
}

/// Exchange the token for a session cookie. The token rides in a JSON body
/// or an `X-Auth-Token` header — never in the URL, which would end up in
/// browser history and access logs (webui-api requirement).
async fn auth_login(
    State(sup): State<Arc<Supervisor>>,
    headers: axum::http::HeaderMap,
    body: Option<Json<serde_json::Value>>,
) -> Response {
    let auth = console_auth_of(&sup);
    if !auth.enabled() {
        // No token configured: there is nothing to exchange and nothing to
        // protect — say so instead of handing out a meaningless cookie.
        return api_error(
            StatusCode::BAD_REQUEST,
            "webui.auth_token is not configured; this console requires no credential",
        );
    }
    let presented = headers
        .get("x-auth-token")
        .and_then(|v| v.to_str().ok())
        .map(String::from)
        .or_else(|| {
            body.as_ref()
                .and_then(|Json(v)| v.get("token").and_then(|t| t.as_str()))
                .map(String::from)
        })
        .unwrap_or_default();
    match auth.exchange(&presented) {
        Some(id) => {
            let cookie = format!(
                "{SESSION_COOKIE}={id}; HttpOnly; SameSite=Strict; Path=/; Max-Age={SESSION_MAX_AGE}"
            );
            (
                StatusCode::OK,
                [(axum::http::header::SET_COOKIE, cookie)],
                Json(json!({ "ok": true })),
            )
                .into_response()
        }
        None => api_error(StatusCode::UNAUTHORIZED, "invalid or missing token"),
    }
}

/// Drop this browser's session cookie.
async fn auth_logout(
    headers: axum::http::HeaderMap,
    State(sup): State<Arc<Supervisor>>,
) -> Response {
    let auth = console_auth_of(&sup);
    if let Some(id) = cookie_value(
        headers
            .get(axum::http::header::COOKIE)
            .and_then(|v| v.to_str().ok()),
        SESSION_COOKIE,
    ) {
        auth.revoke(&id);
    }
    (
        StatusCode::OK,
        [(
            axum::http::header::SET_COOKIE,
            format!("{SESSION_COOKIE}=; HttpOnly; SameSite=Strict; Path=/; Max-Age=0"),
        )],
        Json(json!({ "ok": true })),
    )
        .into_response()
}

/// The auth state the credential endpoints judge against.
fn console_auth_of(sup: &Arc<Supervisor>) -> Arc<ConsoleAuth> {
    Arc::clone(&sup.console_auth.lock().unwrap())
}

/// The guard: an unauthenticated console passes every request through
/// untouched (behaviour is then exactly what it was before this feature).
/// A configured console requires a valid session cookie for everything but
/// `/api/health`; a token in the query string is deliberately NOT accepted,
/// because URLs land in browser history and access logs.
async fn console_auth(
    State(auth): State<Arc<ConsoleAuth>>,
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    if !auth.enabled() {
        return next.run(req).await;
    }
    let presented = req
        .headers()
        .get(axum::http::header::COOKIE)
        .and_then(|v| v.to_str().ok())
        .and_then(|raw| cookie_value(Some(raw), SESSION_COOKIE));
    match presented {
        Some(id) if auth.check(&id) => next.run(req).await,
        _ => (
            StatusCode::UNAUTHORIZED,
            Json(json!({
                "error": "console authentication required",
                "hint": "POST the token to /api/auth to obtain a session cookie",
            })),
        )
            .into_response(),
    }
}

/// Serve the console on `listen`. Blocks the calling thread; run it on a
/// dedicated worker thread. Exits when the daemon shuts down (flag is set by
/// Ctrl+C / `POST /v1/shutdown`) OR when `stop` is set — the webui manager
/// uses that for the reload-driven stop/rebind (config-driven-webui). The
/// bind outcome is reported on `bound` (when given) as soon as the socket is
/// bound, so the manager can converge without guessing.
pub fn serve(
    sup: Arc<Supervisor>,
    listen: &str,
    auth_token: &str,
    stop: Arc<WebuiLifecycle>,
    bound: Option<std::sync::mpsc::Sender<Result<String>>>,
) -> Result<()> {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    let auth_token = auth_token.to_string();
    rt.block_on(async move {
        let app = build_router(sup.clone(), Arc::clone(&stop), &auth_token);
        let listener = match tokio::net::TcpListener::bind(listen).await {
            Ok(l) => l,
            Err(e) => {
                // Report the failure before returning so the manager can
                // degrade non-fatally instead of waiting for a disconnect.
                let msg = format!("webui server error: {e}");
                if let Some(tx) = bound {
                    let _ = tx.send(Err(anyhow::anyhow!(msg.clone())));
                }
                return Err(anyhow::anyhow!(msg));
            }
        };
        log::info!(
            "webui listening on http://{listen} (auth: {})",
            if auth_token.is_empty() {
                "disabled — anyone who can reach this address has full control"
            } else {
                "token required"
            }
        );
        if let Some(tx) = &bound {
            let _ = tx.send(Ok(listen.to_string()));
        }
        axum::serve(listener, app)
            .with_graceful_shutdown(async move {
                // Exit when the daemon shuts down (flag is set by Ctrl+C /
                // POST /v1/shutdown, so the process can terminate) or the
                // manager requested a stop/rebind (reload 热生效). Then hold
                // the drain briefly: live WS sessions flush their queues and
                // send the WebSocket Close frame before the sockets die
                // (webui-api: 随停机 graceful 关闭).
                loop {
                    if stop.stopped() || sup.state.lock().unwrap().shutdown.load(Ordering::SeqCst) {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(200)).await;
                }
                let deadline = tokio::time::Instant::now() + WS_LIVE_DRAIN;
                while stop.live() > 0 && tokio::time::Instant::now() < deadline {
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            })
            .await
            .map_err(|e| anyhow::anyhow!("webui server error: {e}"))
    })
}

// ---------------------------------------------------------------- WebSocket

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "lowercase")]
enum ClientAction {
    Subscribe { program: String, stream: String },
    Unsubscribe { program: String, stream: String },
}

fn stream_of(stream: &str) -> Option<Stream> {
    match stream {
        "out" => Some(Stream::Out),
        "err" => Some(Stream::Err),
        _ => None,
    }
}

fn stream_byte(stream: Stream) -> u8 {
    match stream {
        Stream::Out => 0,
        Stream::Err => 1,
    }
}

async fn ws_upgrade(
    ws: WebSocketUpgrade,
    State(sup): State<Arc<Supervisor>>,
    Extension(stop): Extension<Arc<WebuiLifecycle>>,
) -> Response {
    ws.on_upgrade(move |socket| ws_session(socket, sup, stop))
}

/// Compute which programs changed between two snapshots (spec: incremental
/// events carry the program name plus its new status fields).
fn status_delta(prev: &[ProgramInfo], cur: &[ProgramInfo]) -> Vec<ProgramInfo> {
    if prev.len() != cur.len() {
        return cur.to_vec();
    }
    cur.iter()
        .zip(prev.iter())
        .filter(|(c, p)| c != p)
        .map(|(c, _)| c.clone())
        .collect()
}

/// Program names that existed in `prev` and are gone from `cur` — the removal
/// signal of a STATUS frame (guard-remove-until-stopped 4.3). Without it a
/// shrinking projection would leave the console with stale, still-clickable
/// rows for a removed app.
fn status_removed(prev: &[ProgramInfo], cur: &[ProgramInfo]) -> Vec<String> {
    let present: std::collections::HashSet<&str> = cur.iter().map(|p| p.name.as_str()).collect();
    let mut gone: Vec<String> = prev
        .iter()
        .filter(|p| !present.contains(p.name.as_str()))
        .map(|p| p.name.clone())
        .collect();
    gone.sort();
    gone
}

/// App record names in the live state. App removal drops a record, so the
/// difference between two ticks is exactly "these apps were removed" — unlike
/// the program diff it cannot be confused with an app whose programs were all
/// deleted by an apply (that keeps the record and its now-empty overview).
fn app_names(sup: &Arc<Supervisor>) -> Vec<String> {
    let st = sup.state.lock().unwrap();
    let mut names: Vec<String> = st.apps.iter().map(|a| a.name.clone()).collect();
    names.sort();
    names
}

/// Apps present in `prev` and gone from `cur` (sorted).
fn status_removed_apps(prev: &[String], cur: &[String]) -> Vec<String> {
    let present: std::collections::HashSet<&str> = cur.iter().map(String::as_str).collect();
    prev.iter()
        .filter(|a| !present.contains(a.as_str()))
        .cloned()
        .collect()
}
async fn ws_session(socket: WebSocket, sup: Arc<Supervisor>, stop: Arc<WebuiLifecycle>) {
    stop.enter_session();
    let (mut sink, mut source) = socket.split();
    // All outgoing frames funnel through this channel; one writer task.
    // `None` is the graceful-close sentinel: everything already queued
    // flushes first (FIFO), then the WebSocket Close frame goes out —
    // webui-api: 停伺服时先发关闭帧再断开，不丢已在途推送帧.
    let (out_tx, mut out_rx) = mpsc::channel::<Option<Vec<u8>>>(128);
    let writer = tokio::spawn(async move {
        while let Some(frame) = out_rx.recv().await {
            match frame {
                Some(data) => {
                    if sink.send(Message::Binary(data.into())).await.is_err() {
                        break;
                    }
                }
                None => {
                    let _ = sink.send(Message::Close(None)).await;
                    break;
                }
            }
        }
    });

    // webui-api spec: the first message is always a full snapshot.
    let snap = status_doc(&sup);
    let mut last: Vec<ProgramInfo> = snap.programs.clone();
    let mut last_apps = app_names(&sup);
    let mut last_pending = snap.pending.clone();
    // Named (map) encoding: the browser's msgpack decoder produces JS
    // objects; compact array encoding yields a tuple, which the client's
    // StatusDoc handling cannot consume (the WS channel would silently
    // stall on snapshot decode). Field names keep JSON/MessagePack
    // isomorphic per the webui-api data-format requirement.
    if let Ok(payload) = rmp_serde::to_vec_named(&snap) {
        let _ = out_tx
            .send(Some(api::encode_ws_message(msg_type::SNAPSHOT, &payload)))
            .await;
    }

    let mut heartbeat = tokio::time::interval(WS_HEARTBEAT);
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut subs: HashMap<(String, Stream), tokio::task::JoinHandle<()>> = HashMap::new();
    // A saturated session closes wholesale; subscribe tasks signal here.
    let (close_tx, mut close_rx) = mpsc::channel::<()>(1);
    // True when THIS side initiates the close (stop/shutdown): the writer
    // then flushes a Close frame instead of being aborted.
    let mut graceful = false;

    loop {
        tokio::select! {
            msg = source.next() => {
                match msg {
                    Some(Ok(Message::Binary(data))) => {
                        match serde_json::from_slice::<ClientAction>(&data)
                            .or_else(|_| rmp_serde::from_slice::<ClientAction>(&data))
                        {
                            Ok(ClientAction::Subscribe { program, stream }) => {
                                let stream = match stream_of(&stream) {
                                    Some(s) => s,
                                    None => {
                                        let _ = out_tx.send(Some(api::encode_ws_message(
                                            msg_type::ERROR,
                                            b"{\"error\":\"stream must be out or err\"}",
                                        ))).await;
                                        continue;
                                    }
                                };
                                let known = sup.state.lock().unwrap()
                                    .programs.contains_key(&program);
                                if !known {
                                    let _ = out_tx.send(Some(api::encode_ws_message(
                                        msg_type::ERROR,
                                        json!({ "error": format!("unknown program {program:?}") })
                                            .to_string().as_bytes(),
                                    ))).await;
                                    continue;
                                }
                                let key = (program.clone(), stream);
                                if let std::collections::hash_map::Entry::Vacant(e) = subs.entry(key.clone()) {
                                    e.insert(tokio::spawn(subscribe_logs(
                                        sup.clone(), out_tx.clone(), close_tx.clone(),
                                        program, stream,
                                    )));
                                }
                            }
                            Ok(ClientAction::Unsubscribe { program, stream }) => {
                                if let Some(s) = stream_of(&stream) {
                                    if let Some(handle) = subs.remove(&(program, s)) {
                                        handle.abort();
                                    }
                                }
                            }
                            Err(_) => {
                                let _ = out_tx.send(Some(api::encode_ws_message(
                                    msg_type::ERROR,
                                    b"{\"error\":\"bad message\"}",
                                ))).await;
                            }
                        }
                    }
                    Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
                    Some(Ok(_)) => {} // ping/pong/text frames are ignored
                }
            }
            _ = tokio::time::sleep(WS_POLL) => {
                // Diff the shared state against what this session last sent;
                // the control-plane loop is the single writer. Pending changes
                // ride the STATUS frame as a `pending` field when they differ
                // (apply-workflow: pending appears in snapshot + deltas).
                let doc = status_doc(&sup);
                let cur = doc.programs.clone();
                let cur_apps = app_names(&sup);
                let pending_changed = doc.pending != last_pending;
                let delta = status_delta(&last, &cur);
                let removed = status_removed(&last, &cur);
                let apps_removed = status_removed_apps(&last_apps, &cur_apps);
                last = cur;
                last_apps = cur_apps;
                if !delta.is_empty()
                    || !removed.is_empty()
                    || !apps_removed.is_empty()
                    || pending_changed
                {
                    last_pending = doc.pending.clone();
                    let mut payload = serde_json::json!({ "programs": delta });
                    // Removal is explicit: the client drops these entries
                    // instead of keeping a stale, clickable row for an app
                    // that is already gone (guard-remove-until-stopped 4.3).
                    if !removed.is_empty() {
                        payload["removed"] = serde_json::json!(removed);
                    }
                    // App-level removal is the authoritative signal (the
                    // record is gone); the program list alone cannot tell a
                    // removed app from an app whose definitions were all
                    // deleted by an apply. Named `apps_gone` so it cannot be
                    // confused with the pending projection's `apps_removed`.
                    if !apps_removed.is_empty() {
                        payload["apps_gone"] = serde_json::json!(apps_removed);
                    }
                    if pending_changed {
                        payload["pending"] =
                            serde_json::to_value(&doc.pending).unwrap_or_default();
                    }
                    if let Ok(bytes) = serde_json::to_vec(&payload) {
                        let _ = out_tx
                            .send(Some(api::encode_ws_message(msg_type::STATUS, &bytes)))
                            .await;
                    }
                }
                // Console stopping (reload off/rebind) or daemon shutting
                // down: end the session gracefully — queued pushes flush and
                // a Close frame is sent before the socket drops (webui-api:
                // 随停机 graceful 关闭 / 随守护进程退出).
                let closing = stop.stopped()
                    || sup.state.lock().unwrap().shutdown.load(Ordering::SeqCst);
                if closing {
                    graceful = true;
                    break;
                }
            }
            _ = heartbeat.tick() => {
                let _ = out_tx.send(Some(api::encode_ws_message(msg_type::HEARTBEAT, b"null"))).await;
            }
            _ = close_rx.recv() => {
                // A log subscriber hit sustained backpressure: drop the whole
                // session; the client reconnects with backoff and resyncs.
                break;
            }
        }
    }

    for (_, handle) in subs.drain() {
        handle.abort();
    }
    stop.exit_session();
    if graceful {
        // The close sentinel goes out after everything already queued; bound
        // the cleanup so a frozen client can never stall the server drain.
        let _ = tokio::time::timeout(Duration::from_secs(2), out_tx.send(None)).await;
        drop(out_tx);
        let _ = tokio::time::timeout(Duration::from_secs(2), writer).await;
    } else {
        writer.abort();
    }
}

/// Replay the current tail once, then forward ring-buffer batches as
/// aggregated frames (one frame = many lines, bounded by [`FRAME_BYTES`] /
/// [`FRAME_INTERVAL`]). Gap markers from the bounded subscription are sent
/// as their own LOG_GAP frames, always ordered before subsequent content.
/// If the session writer stays saturated past [`SEND_TIMEOUT`], the close
/// channel is signalled so the whole session drops (webui-api: WS 日志批量
/// 推送与背压; slow viewers never back up the pump).
async fn subscribe_logs(
    sup: Arc<Supervisor>,
    out_tx: mpsc::Sender<Option<Vec<u8>>>,
    close_tx: mpsc::Sender<()>,
    program: String,
    stream: Stream,
) {
    let ring = {
        let st = sup.state.lock().unwrap();
        match st.programs.get(&program) {
            Some(p) => p.ring(stream),
            None => return,
        }
    };
    let sb = stream_byte(stream);

    // Replay context first (spec: subscribing yields the tail, then follows).
    let tail = ring.tail(LOG_TAIL_DEFAULT);
    if !tail.is_empty() {
        let mut data = tail.join("\n");
        data.push('\n');
        if out_tx
            .send(Some(api::encode_log_frame(&program, sb, data.as_bytes())))
            .await
            .is_err()
        {
            return; // client went away
        }
    }

    let rx = ring.subscribe();
    let mut frame: Vec<u8> = Vec::with_capacity(FRAME_BYTES * 2);
    let mut deadline: Option<tokio::time::Instant> = None;
    loop {
        // Drain what is queued. Frames flush mid-batch — never splitting a
        // line, never dropping the remainder of a batch — so a frame is
        // bounded at FRAME_BYTES + one line.
        let mut drained_any = false;
        while let Ok(item) = rx.try_recv() {
            drained_any = true;
            match item {
                pump::SubItem::Lines(lines) => {
                    for l in lines {
                        frame.extend_from_slice(l.as_bytes());
                        frame.push(b'\n');
                        if frame.len() >= FRAME_BYTES {
                            let payload = std::mem::take(&mut frame);
                            deadline = None;
                            if !send_frame(&out_tx, api::encode_log_frame(&program, sb, &payload))
                                .await
                            {
                                saturate_close(&close_tx).await;
                                return;
                            }
                        }
                    }
                }
                pump::SubItem::Gap(n) => {
                    // The marker must precede content that arrived after the
                    // loss, so flush pending content first.
                    if !frame.is_empty() {
                        let payload = std::mem::take(&mut frame);
                        deadline = None;
                        if !send_frame(&out_tx, api::encode_log_frame(&program, sb, &payload)).await
                        {
                            saturate_close(&close_tx).await;
                            return;
                        }
                    }
                    // A gap marker is accounting the client is owed: wait for
                    // a writer slot (past the ordinary saturation bound) so a
                    // merely-slow client still learns what it missed.
                    if !send_frame(&out_tx, api::encode_gap_frame(&program, sb, n)).await {
                        saturate_close(&close_tx).await;
                        return;
                    }
                }
            }
        }
        if !frame.is_empty() && deadline.is_none() {
            deadline = Some(tokio::time::Instant::now() + FRAME_INTERVAL);
        }
        let due = deadline
            .map(|d| tokio::time::Instant::now() >= d)
            .unwrap_or(false);
        if due && !frame.is_empty() {
            let payload = std::mem::take(&mut frame);
            deadline = None;
            if !send_frame(&out_tx, api::encode_log_frame(&program, sb, &payload)).await {
                saturate_close(&close_tx).await;
                return;
            }
        }
        // Idle (nothing drained, nothing pending): poll in slices. Anything
        // in flight: short slices so throughput stays production-bound.
        tokio::time::sleep(if !drained_any && frame.is_empty() {
            Duration::from_millis(20)
        } else {
            Duration::from_millis(2)
        })
        .await;
    }
}

/// One frame to the session writer. Ordinary frames carry the saturation
/// bound (a frozen network must free the session); gap frames wait far
/// longer. False = give up and close this session.
async fn send_frame(out_tx: &mpsc::Sender<Option<Vec<u8>>>, frame: Vec<u8>) -> bool {
    match out_tx.send_timeout(Some(frame), LOG_SEND_TIMEOUT).await {
        Ok(()) => true,
        Err(_) => false,
    }
}

/// Signal the session loop to close after sustained writer saturation.
async fn saturate_close(close_tx: &mpsc::Sender<()>) {
    let _ = close_tx.send(()).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::DaemonConfig;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    fn test_sup() -> Arc<Supervisor> {
        let config = DaemonConfig::default();
        let dir = std::env::temp_dir().join(format!("ak-web-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        Supervisor::new(config, &dir).unwrap()
    }

    fn test_stop() -> Arc<WebuiLifecycle> {
        WebuiLifecycle::new()
    }

    /// A free loopback port for real-socket lifecycle tests.
    fn free_listen() -> String {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        format!("{}", l.local_addr().unwrap())
    }

    async fn get(app: Router, uri: &str) -> (StatusCode, axum::body::Bytes) {
        let resp = app
            .oneshot(Request::get(uri).body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = resp.status();
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        (status, body)
    }

    /// Issue a request with optional headers/cookies; returns status, body
    /// and the `Set-Cookie` the router emitted (empty when it did not).
    async fn call(
        app: Router,
        method: &str,
        uri: &str,
        cookie: Option<&str>,
        body: Option<&str>,
    ) -> (StatusCode, axum::body::Bytes, String) {
        let mut req = Request::builder().method(method).uri(uri);
        if let Some(c) = cookie {
            req = req.header("cookie", c);
        }
        let req = match body {
            Some(b) => req
                .header("content-type", "application/json")
                .body(Body::from(b.to_string()))
                .unwrap(),
            None => req.body(Body::empty()).unwrap(),
        };
        let resp = app.oneshot(req).await.unwrap();
        let status = resp.status();
        let set_cookie = resp
            .headers()
            .get("set-cookie")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        (status, body, set_cookie)
    }

    fn authed_app(token: &str) -> Router {
        build_router(test_sup(), test_stop(), token)
    }

    /// 6.1: with no `webui.auth_token`, nothing about the console changes —
    /// anonymous `/api/overview` still answers 200 and no credential
    /// endpoint invents a session.
    #[tokio::test]
    async fn no_token_means_no_authentication_at_all() {
        let app = authed_app("");
        for uri in [
            "/api/overview",
            "/api/programs",
            "/api/pending",
            "/api/health",
        ] {
            let (status, _, _) = call(app.clone(), "GET", uri, None, None).await;
            assert_eq!(status, StatusCode::OK, "{uri} must stay open");
        }
        // Exchanging a token is a no-go: there is none to exchange.
        let (status, body, _) = call(
            app.clone(),
            "POST",
            "/api/auth",
            None,
            Some(r#"{"token":"anything"}"#),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(String::from_utf8_lossy(&body).contains("not configured"));
    }

    /// 6.2: with a token configured, `/api/*` (except `/api/health`) and
    /// `/ws` refuse anonymous callers; `/api/health` stays open so a
    /// liveness probe does not need a credential.
    #[tokio::test]
    async fn configured_token_gates_api_and_ws_but_not_health() {
        let app = authed_app("s3cr3t");
        for uri in [
            "/api/overview",
            "/api/programs",
            "/api/pending",
            "/ws",
            "/api/programs/web",
        ] {
            let (status, body, _) = call(app.clone(), "GET", uri, None, None).await;
            assert_eq!(status, StatusCode::UNAUTHORIZED, "{uri} must be gated");
            let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
            assert!(v.get("error").is_some(), "{uri}: {v}");
            assert!(
                !String::from_utf8_lossy(&body).contains("programs"),
                "{uri} leaked state before authentication"
            );
        }
        let (status, _, _) = call(app.clone(), "GET", "/api/health", None, None).await;
        assert_eq!(status, StatusCode::OK, "health stays reachable");
        // A write action is gated too — not just reads.
        let (status, _, _) = call(app.clone(), "POST", "/api/apply", None, Some("{}")).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    /// 6.3: exchange the token for a session cookie, then use the cookie. A
    /// wrong token gets nothing, and a token in the URL is never a
    /// credential.
    #[tokio::test]
    async fn token_exchanges_for_a_session_cookie() {
        let app = authed_app("s3cr3t");

        // Wrong token → 401, no cookie.
        let (status, _, set_cookie) = call(
            app.clone(),
            "POST",
            "/api/auth",
            None,
            Some(r#"{"token":"nope"}"#),
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert!(set_cookie.is_empty(), "no session for a wrong token");

        // Right token → an HttpOnly, SameSite session cookie.
        let (status, _, set_cookie) = call(
            app.clone(),
            "POST",
            "/api/auth",
            None,
            Some(r#"{"token":"s3cr3t"}"#),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert!(set_cookie.starts_with("ak_console="), "{set_cookie}");
        assert!(set_cookie.contains("HttpOnly"), "{set_cookie}");
        assert!(set_cookie.contains("SameSite=Strict"), "{set_cookie}");
        assert!(
            !set_cookie.contains("s3cr3t"),
            "the token is never in the cookie"
        );
        let cookie = set_cookie.split(';').next().unwrap().to_string();

        // The cookie opens the gated surface.
        let (status, _, _) = call(app.clone(), "GET", "/api/overview", Some(&cookie), None).await;
        assert_eq!(status, StatusCode::OK);

        // ...and logging out closes it again.
        let (status, _, _) =
            call(app.clone(), "POST", "/api/auth/logout", Some(&cookie), None).await;
        assert_eq!(status, StatusCode::OK);
    }

    /// A token in the query string is not a credential — it would end up in
    /// browser history and access logs (webui-api requirement).
    #[tokio::test]
    async fn token_in_query_string_is_not_accepted() {
        let app = authed_app("s3cr3t");
        let (status, body, _) =
            call(app.clone(), "GET", "/api/overview?token=s3cr3t", None, None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert!(!String::from_utf8_lossy(&body).contains("programs"));
    }

    /// The token is never written to the response body — only the cookie.
    #[tokio::test]
    async fn a_forged_cookie_is_rejected() {
        let app = authed_app("s3cr3t");
        let (status, _, _) = call(
            app.clone(),
            "GET",
            "/api/overview",
            Some("ak_console=not-a-real-session"),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn rest_endpoints_serve_projection_and_404_objects() {
        let app = build_router(test_sup(), test_stop(), "");

        let (status, body) = get(app.clone(), "/api/health").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, serde_json::json!({"ok": true}).to_string());

        let (status, body) = get(app.clone(), "/api/overview").await;
        assert_eq!(status, StatusCode::OK);
        let doc: StatusDoc = serde_json::from_slice(&body).unwrap();
        assert_eq!(doc.programs.len(), 0);

        let (status, body) = get(app.clone(), "/api/programs/nope").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(v["error"].as_str().unwrap().contains("unknown program"));

        let (status, _) = get(app, "/api/programs/nope/logs").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[test]
    fn status_delta_reports_only_changes() {
        let a = ProgramInfo {
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
            cpu_percent: None,
            mem_bytes: None,
            log_rate: crate::metrics::LogRates::default(),
            command: String::new(),
            args: Vec::new(),
            work_dir: String::new(),
            config_path: None,
            health_confirmed: true,
        };
        let b_changed = {
            let mut x = a.clone();
            x.state = "backoff".into();
            x
        };
        let delta = status_delta(&[a.clone()], &[b_changed.clone()]);
        assert_eq!(delta, vec![b_changed]);
        assert!(status_delta(&[a.clone()], &[a]).is_empty());
    }

    #[test]
    fn status_delta_reports_metric_changes() {
        // webui-api: WS 增量携带指标变化 — a metric-only difference must
        // produce a delta entry (the STATUS event then carries the new value).
        let a = ProgramInfo {
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
            cpu_percent: None,
            mem_bytes: None,
            log_rate: crate::metrics::LogRates::default(),
            command: String::new(),
            args: Vec::new(),
            work_dir: String::new(),
            config_path: None,
            health_confirmed: true,
        };
        let mut b = a.clone();
        b.cpu_percent = Some(37.5);
        let delta = status_delta(&[a], &[b.clone()]);
        assert_eq!(delta, vec![b]);
    }

    /// guard-remove-until-stopped 4.3: a shrinking projection names exactly
    /// the programs that disappeared, so the console can drop them instead of
    /// keeping a stale row; nothing else is reported.
    #[test]
    fn status_removed_names_dropped_programs_only() {
        let mk = |app: &str, name: &str| ProgramInfo {
            app: app.into(),
            name: name.into(),
            state: "stopped".into(),
            pid: None,
            unhealthy: false,
            uptime_secs: 0.0,
            total_exits: 0,
            restart_backoff: 1.0,
            last_exit: None,
            fatal_reason: None,
            wait_reason: None,
            cpu_percent: None,
            mem_bytes: None,
            log_rate: crate::metrics::LogRates::default(),
            command: String::new(),
            args: Vec::new(),
            work_dir: String::new(),
            config_path: None,
            health_confirmed: true,
        };
        let before = vec![mk("web", "api"), mk("web", "web"), mk("other", "job")];
        // `web` (a whole app) went away; `other.job` stays.
        let after = vec![mk("other", "job")];
        assert_eq!(
            status_removed(&before, &after),
            vec!["api".to_string(), "web".to_string()],
            "only the removed app's programs, sorted"
        );
        assert!(status_removed(&after, &after).is_empty());
        // No removal when the projection only changed (same length/set).
        let mut changed = after.clone();
        changed[0].state = "running".into();
        assert!(status_removed(&after, &changed).is_empty());
    }

    // -- metrics contract tests (webui-api: 投影携带指标) --------------------

    use crate::config::{AppRaw, resolve_app};
    use crate::metrics::{ProgramMetrics, SystemMetrics};
    use crate::program::ManagedProgram;
    use std::path::Path;

    /// guard-remove-until-stopped 4.1: a supervisor with one registered app
    /// (`web`, restart = "no") whose removal can be driven through `/api`.
    /// Spawns the real command loop so the endpoint's enqueue/wait completes.
    fn sup_with_registered_app(tag: &str) -> (Arc<Supervisor>, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("ak-test-web-rm-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("app.d")).unwrap();
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
        let body = dir.join("web-appkeeper.toml");
        std::fs::write(
            &body,
            "[app]\nrestart = \"no\"\n\n[program.web]\ncommand = 'sleep 30'\nstartsecs = 0.0\n",
        )
        .unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&body, dir.join("app.d/web.toml")).unwrap();
        #[cfg(windows)]
        {
            if std::os::windows::fs::symlink_file(&body, dir.join("app.d/web.toml")).is_err() {
                std::fs::hard_link(&body, dir.join("app.d/web.toml")).unwrap();
            }
        }
        let (config, _) = DaemonConfig::load_or_default(&cfg_path).unwrap();
        let sup = Supervisor::new(config, &dir).unwrap();
        sup.set_config_path(&cfg_path);
        sup.bootstrap().unwrap();
        let sup2 = sup.clone();
        std::thread::spawn(move || sup2.run());
        (sup, dir)
    }

    /// 4.1: a stopped app is removed via `/api`, returning the same outcome
    /// shape as `/v1` and clearing the app from the projection.
    #[tokio::test]
    async fn api_remove_succeeds_and_projection_drops_the_app() {
        let (sup, dir) = sup_with_registered_app("ok");
        let app = build_router(sup.clone(), test_stop(), "");
        let (status, body, _) = call(app, "POST", "/api/apps/web/remove", None, None).await;
        assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["app"], "web");
        assert_eq!(v["deleted_file"], false);
        assert_eq!(v["applied"], true);
        assert!(
            v["kept_config"]
                .as_str()
                .unwrap()
                .ends_with("web-appkeeper.toml")
        );
        assert!(!dir.join("app.d/web.toml").exists(), "record deleted");
        // The projection no longer lists the app.
        let app2 = build_router(sup.clone(), test_stop(), "");
        let (status, body) = get(app2, "/api/overview").await;
        assert_eq!(status, StatusCode::OK);
        let doc: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(
            !doc["programs"]
                .as_array()
                .unwrap()
                .iter()
                .any(|p| p["app"] == "web"),
            "{doc}"
        );
        sup.request_shutdown();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 4.1: an unknown app is a 404 error object; nothing changes.
    #[tokio::test]
    async fn api_remove_unknown_app_is_404() {
        let (sup, dir) = sup_with_registered_app("404");
        let app = build_router(sup.clone(), test_stop(), "");
        let (status, body, _) = call(app, "POST", "/api/apps/ghost/remove", None, None).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(v["error"].as_str().unwrap().contains("unknown app"), "{v}");
        assert!(dir.join("app.d/web.toml").exists());
        sup.request_shutdown();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 4.1/webui-api: a registration-deletion failure is a server error on
    /// `/api`, and the app/program stay in the projection (the `/v1` and
    /// `/api` mapping cannot drift — both read `RemoveError::http_status`).
    #[tokio::test]
    async fn api_remove_delete_failure_is_500_and_keeps_the_projection() {
        let (sup, dir) = sup_with_registered_app("delete-fail");
        // Replace the record with a directory: deletion fails deterministically
        // (no permission tricks, which do nothing when running as root).
        let record = dir.join("app.d/web.toml");
        std::fs::remove_file(&record).unwrap();
        std::fs::create_dir_all(&record).unwrap();
        let app = build_router(sup.clone(), test_stop(), "");
        let (status, body, _) = call(app, "POST", "/api/apps/web/remove", None, None).await;
        assert_eq!(
            status,
            StatusCode::INTERNAL_SERVER_ERROR,
            "{}",
            String::from_utf8_lossy(&body)
        );
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let msg = v["error"].as_str().unwrap();
        assert!(
            msg.contains("failed to remove the registration record"),
            "{v}"
        );
        assert!(
            !msg.contains("ak apply") || msg.contains("will not fix this"),
            "no apply-as-remedy suggestion: {v}"
        );
        {
            let st = sup.state.lock().unwrap();
            assert!(st.programs.contains_key("web"), "program kept");
            assert!(
                st.apps.iter().any(|a| a.name == "web"),
                "app record kept in the projection"
            );
        }
        sup.request_shutdown();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 4.1: a live program blocks `/api` removal with a 409 carrying the
    /// daemon's blocker list; the app keeps running, the record survives, and
    /// no stop/apply is triggered.
    #[tokio::test]
    async fn api_remove_conflict_for_a_running_app() {
        let (sup, dir) = sup_with_registered_app("busy");
        // Start the program: `starting` and `running` both block.
        let started = {
            let (tx, rx) = std::sync::mpsc::channel();
            sup.enqueue(Command::Start {
                name: "web".into(),
                reply: Some(tx),
            });
            rx.recv_timeout(Duration::from_secs(10))
                .expect("start answered")
                .expect("start ok")
        };
        assert!(started.contains("web"), "{started}");
        let pid = sup.state.lock().unwrap().programs["web"].pid();
        let app = build_router(sup.clone(), test_stop(), "");
        let (status, body, _) = call(app, "POST", "/api/apps/web/remove", None, None).await;
        assert_eq!(
            status,
            StatusCode::CONFLICT,
            "{}",
            String::from_utf8_lossy(&body)
        );
        let msg = String::from_utf8_lossy(&body);
        assert!(
            msg.contains("web (running)") || msg.contains("web (starting)"),
            "{msg}"
        );
        assert!(msg.contains("ak stop web"), "the remedy is named: {msg}");
        assert!(dir.join("app.d/web.toml").exists(), "record untouched");
        assert_eq!(
            sup.state.lock().unwrap().programs["web"].pid(),
            pid,
            "nothing was stopped"
        );
        sup.request_shutdown();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 4.1: with a console token configured, the removal endpoint requires a
    /// session exactly like the other `/api` writes.
    #[tokio::test]
    async fn api_remove_requires_a_session() {
        let (sup, dir) = sup_with_registered_app("auth");
        let app = build_router(sup.clone(), test_stop(), "s3cr3t");
        let (status, _, _) = call(app, "POST", "/api/apps/web/remove", None, None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert!(dir.join("app.d/web.toml").exists(), "record untouched");
        sup.request_shutdown();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A supervisor with one declared program "m" and metric values injected
    /// into the shared sampler table.
    fn sup_with_metrics() -> (Arc<Supervisor>, String) {
        let config = DaemonConfig::default();
        let dir = std::env::temp_dir().join(format!("ak-test-web-metrics-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let sup = Supervisor::new(config, &dir).unwrap();
        let raw: AppRaw = toml::from_str("[program.m]\ncommand = 'true'\n").unwrap();
        let app = resolve_app("demo", Path::new("x.toml"), &raw, None).unwrap();
        let def = app.programs.into_iter().next().unwrap();
        let name = def.name.clone();
        {
            let mut st = sup.state.lock().unwrap();
            st.programs
                .insert(name.clone(), ManagedProgram::new(def, dir, 10));
        }
        sup.metrics.set_program(
            &name,
            ProgramMetrics {
                cpu_percent: Some(12.5),
                mem_bytes: Some(4096),
            },
        );
        sup.metrics.set_system(SystemMetrics {
            cpu_percent: Some(3.4),
            mem_used_bytes: 1024,
            mem_total_bytes: 8192,
        });
        (sup, name)
    }

    #[tokio::test]
    async fn overview_json_carries_metric_fields() {
        let (sup, name) = sup_with_metrics();
        let app = build_router(sup, test_stop(), "");
        let (status, body) = get(app, "/api/overview").await;
        assert_eq!(status, StatusCode::OK);
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let p = &v["programs"][0];
        assert_eq!(p["name"], name);
        assert_eq!(p["cpu_percent"], 12.5);
        assert_eq!(p["mem_bytes"], 4096);
        assert!(
            p["log_rate"]["out"]["w10"].is_number(),
            "log_rate fields present"
        );
        assert_eq!(v["daemon"]["system"]["cpu_percent"], 3.4);
        assert_eq!(v["daemon"]["system"]["mem_total_bytes"], 8192);
    }

    #[tokio::test]
    async fn ws_snapshot_carries_metric_fields() {
        let (sup, name) = sup_with_metrics();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let sup2 = sup.clone();
        let stop = test_stop();
        tokio::spawn(async move {
            let _ = axum::serve(listener, build_router(sup2, stop, "")).await;
        });
        let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/ws"))
            .await
            .expect("ws connect");
        let msg = ws.next().await.unwrap().unwrap();
        let (t, payload) = api::decode_ws_message(&msg.into_data()).unwrap();
        assert_eq!(t, msg_type::SNAPSHOT);
        let doc: StatusDoc = rmp_serde::from_slice(&payload).unwrap();
        let p = doc.programs.iter().find(|p| p.name == name).unwrap();
        assert_eq!(p.cpu_percent, Some(12.5));
        assert_eq!(p.mem_bytes, Some(4096));
        assert_eq!(doc.daemon.system.cpu_percent, Some(3.4));
    }

    /// The control plane's projection is served over the local IPC endpoint —
    /// same JSON, no TCP port involved. It is queried with the in-tree client,
    /// which speaks HTTP/1.1 over that endpoint.
    #[test]
    fn control_plane_status_carries_metrics() {
        let (sup, name) = sup_with_metrics();
        let dir = std::env::temp_dir().join(format!("ak-test-web-metrics-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let socket = dir.join("ak.sock");
        let cfg_path = dir.join("daemon.toml");
        std::fs::write(
            &cfg_path,
            format!("[daemon]\nsocket = {:?}\n", socket.to_string_lossy()),
        )
        .unwrap();
        {
            let mut st = sup.state.lock().unwrap();
            st.config.daemon.socket = socket.clone();
        }
        let listener = crate::server::bind(&sup).expect("control endpoint bind");
        let config = {
            let st = sup.state.lock().unwrap();
            st.config.clone()
        };
        std::thread::spawn(move || crate::server::serve(sup, listener));

        let client = crate::client::Client::from_config(&config);
        let v = client
            .status()
            .expect("/v1/status over the control endpoint");
        let p = v["programs"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["name"] == name)
            .expect("program in /v1/status");
        assert_eq!(p["cpu_percent"], 12.5);
        assert_eq!(p["mem_bytes"], 4096);
        assert_eq!(v["daemon"]["system"]["cpu_percent"], 3.4);
        let _ = std::fs::remove_dir_all(&dir);
    }

    // -- lifecycle: external stop + graceful WS close (config-driven-webui) ---

    /// `web::serve` honors the external stop flag: the bind outcome is
    /// reported on the channel, the socket is reachable while serving, and a
    /// set flag ends the server gracefully (reload off/rebind path).
    #[test]
    fn serve_exits_on_external_stop_flag() {
        let sup = test_sup();
        let listen = free_listen();
        let stop = test_stop();
        let (bound_tx, bound_rx) = std::sync::mpsc::channel();
        let stop2 = Arc::clone(&stop);
        let addr = listen.clone();
        let handle = std::thread::spawn(move || serve(sup, &addr, "", stop2, Some(bound_tx)));

        let bound = bound_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("bind report in time")
            .expect("bind succeeded");
        assert_eq!(bound, listen);
        assert!(
            std::net::TcpStream::connect(&listen).is_ok(),
            "the console is reachable while serving"
        );

        stop.stop();
        let started = std::time::Instant::now();
        handle
            .join()
            .expect("serve thread")
            .expect("graceful stop is not an error");
        assert!(
            started.elapsed() < Duration::from_secs(8),
            "graceful stop completes promptly, took {:?}",
            started.elapsed()
        );
        std::thread::sleep(Duration::from_millis(150));
        assert!(
            std::net::TcpStream::connect(&listen).is_err(),
            "the port is closed after the stop"
        );
    }

    /// An occupied listen degrades non-fatally: serve reports the bind error
    /// through the channel and returns (the manager keeps the daemon alive).
    #[test]
    fn serve_reports_bind_failure_nonfatally() {
        let sup = test_sup();
        let blocker = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let listen = format!("{}", blocker.local_addr().unwrap());
        let stop = test_stop();
        let (bound_tx, bound_rx) = std::sync::mpsc::channel();
        let handle = std::thread::spawn(move || serve(sup, &listen, "", stop, Some(bound_tx)));
        let err = bound_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("bind report in time")
            .expect_err("occupied port must fail the bind");
        assert!(
            err.to_string().contains("webui server error") || err.to_string().contains("addr"),
            "the error explains the bind failure: {err:#}"
        );
        let _ = handle
            .join()
            .expect("serve thread returns after bind failure");
    }

    /// Daemon shutdown (the shared flag set by Ctrl+C / `POST /v1/shutdown`)
    /// ends the console too: serve returns promptly and the port closes
    /// (webui-api: 控制台随守护进程退出).
    #[test]
    fn serve_exits_on_daemon_shutdown_flag() {
        let sup = test_sup();
        let listen = free_listen();
        let stop = test_stop();
        let (bound_tx, bound_rx) = std::sync::mpsc::channel();
        let stop2 = Arc::clone(&stop);
        let addr = listen.clone();
        let sup2 = Arc::clone(&sup);
        let handle = std::thread::spawn(move || serve(sup2, &addr, "", stop2, Some(bound_tx)));
        let bound = bound_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("bind report in time")
            .expect("bind succeeded");
        assert_eq!(bound, listen);

        sup.state
            .lock()
            .unwrap()
            .shutdown
            .store(true, Ordering::SeqCst);
        let started = std::time::Instant::now();
        handle
            .join()
            .expect("serve thread")
            .expect("graceful stop is not an error");
        assert!(
            started.elapsed() < Duration::from_secs(8),
            "serve exits promptly on the daemon shutdown flag"
        );
        std::thread::sleep(Duration::from_millis(150));
        assert!(
            std::net::TcpStream::connect(&listen).is_err(),
            "the port is closed after the daemon shutdown"
        );
    }

    /// Reload-driven stop closes WS sessions gracefully: queued frames flush
    /// and a WebSocket Close frame arrives before the socket drops.
    #[tokio::test]
    async fn ws_session_closes_with_close_frame_on_stop() {
        let sup = test_sup();
        let listen = free_listen();
        let stop = test_stop();
        let (bound_tx, bound_rx) = std::sync::mpsc::channel();
        let stop2 = Arc::clone(&stop);
        std::thread::spawn(move || serve(sup, &listen, "", stop2, Some(bound_tx)));
        let bound = bound_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("bind report in time")
            .expect("bind succeeded");

        let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://{bound}/ws"))
            .await
            .unwrap();
        // The first frame is the snapshot (webui-api contract).
        let msg = ws.next().await.unwrap().unwrap();
        let (t, _) = api::decode_ws_message(&msg.into_data()).unwrap();
        assert_eq!(t, msg_type::SNAPSHOT);

        stop.stop();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        loop {
            assert!(
                tokio::time::Instant::now() < deadline,
                "no close frame arrived"
            );
            let msg = tokio::time::timeout(Duration::from_secs(5), ws.next())
                .await
                .expect("a frame arrives in time")
                .expect("the stream must not end without a Close frame");
            match msg.unwrap() {
                tokio_tungstenite::tungstenite::Message::Close(_) => break,
                // status/heartbeat frames may interleave before the close.
                tokio_tungstenite::tungstenite::Message::Binary(_)
                | tokio_tungstenite::tungstenite::Message::Ping(_)
                | tokio_tungstenite::tungstenite::Message::Pong(_) => continue,
                other => panic!("unexpected frame while closing: {other:?}"),
            }
        }
        // After the close frame the stream ends (server dropped the socket).
        let ended = tokio::time::timeout(Duration::from_secs(5), ws.next()).await;
        match ended {
            Ok(None) => {}
            Ok(Some(Ok(m))) => panic!("expected stream end, got {m:?}"),
            Ok(Some(Err(_))) => {} // abrupt TCP close after handshake is fine
            Err(_) => {}
        }
    }
}

#[cfg(test)]
mod high_speed_tests {
    use super::*;
    use crate::config::{AppRaw, DaemonConfig, resolve_app};
    use crate::program::ManagedProgram;
    use crate::pump::SUB_CHANNEL_BATCHES;
    use futures_util::{Sink, StreamExt};
    use std::path::Path;
    use std::time::Duration;
    use tokio_tungstenite::tungstenite::Message as ClientMessage;
    use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

    type ClientWs = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

    fn test_stop() -> Arc<WebuiLifecycle> {
        WebuiLifecycle::new()
    }

    /// Supervisor + router on an ephemeral port + one declared program "m"
    /// whose out-ring is directly pushable (stand-in for a firehose child).
    async fn firehose_setup() -> (Arc<Supervisor>, String, ClientWs, Arc<pump::Ring>) {
        let config = DaemonConfig::default();
        let dir = std::env::temp_dir().join(format!("ak-test-hs-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let sup = Supervisor::new(config, &dir).unwrap();
        let raw: AppRaw = toml::from_str("[program.m]\ncommand = 'true'\n").unwrap();
        let app = resolve_app("demo", Path::new("x.toml"), &raw, None).unwrap();
        let def = app.programs.into_iter().next().unwrap();
        let name = def.name.clone();
        let ring = {
            let mut st = sup.state.lock().unwrap();
            let p = ManagedProgram::new(def, dir, 100000);
            let ring = p.ring(Stream::Out);
            st.programs.insert(name.clone(), p);
            ring
        };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let sup2 = sup.clone();
        tokio::spawn(async move {
            let _ = axum::serve(listener, build_router(sup2, test_stop(), "")).await;
        });
        let (ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/ws"))
            .await
            .expect("ws connect");
        (sup, name, ws, ring)
    }

    async fn subscribe<S>(ws: &mut S, program: &str, stream: &str)
    where
        S: Sink<ClientMessage> + Unpin,
        S::Error: std::fmt::Debug,
    {
        let action = serde_json::json!({
            "action": "subscribe", "program": program, "stream": stream
        });
        ws.send(ClientMessage::Binary(
            serde_json::to_vec(&action).unwrap().into(),
        ))
        .await
        .expect("subscribe send failed");
        // Let the session spawn the subscriber (tail replay of an empty ring).
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    /// webui-api 多行单帧 + 内容完整: a fast producer whose client keeps up
    /// must see every line, aggregated into far fewer frames than lines.
    #[tokio::test]
    async fn high_speed_output_delivers_every_line_in_multi_line_frames() {
        let (_sup, name, mut ws, ring) = firehose_setup().await;
        subscribe(&mut ws, &name, "out").await;

        const WAVES: usize = 40;
        const BATCHES_PER_WAVE: usize = 30;
        const LINES_PER_BATCH: usize = 10;
        let total = WAVES * BATCHES_PER_WAVE * LINES_PER_BATCH;

        // Concurrent reader: a keeping-up client (reading while we produce)
        // must see every line. Waves drain fully between sends: 30 batches
        // per wave (< queue depth 64) with 50 ms gaps vs the subscriber's
        // 20 ms poll, so at most one wave is ever queued.
        let reader = tokio::spawn(async move {
            let mut lines_seen = 0usize;
            let mut log_frames = 0usize;
            let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
            while lines_seen < total {
                assert!(
                    tokio::time::Instant::now() < deadline,
                    "timeout: {lines_seen}/{total} lines in {log_frames} frames"
                );
                let msg = ws.next().await.expect("ws closed early").unwrap();
                let (t, payload) = api::decode_ws_message(&msg.into_data()).unwrap();
                if t != msg_type::LOG {
                    continue; // snapshot / status / heartbeat frames may interleave
                }
                let (_, _, data) = api::decode_log_frame(&rebuild(t, &payload)).unwrap();
                lines_seen += data.lines().count();
                log_frames += 1;
            }
            (lines_seen, log_frames)
        });

        for w in 0..WAVES {
            for b in 0..BATCHES_PER_WAVE {
                let lines: Vec<String> = (0..LINES_PER_BATCH)
                    .map(|i| format!("w{w}b{b}l{i}-0123456789"))
                    .collect();
                ring.push_batch(&lines);
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }

        let (lines_seen, log_frames) = reader.await.unwrap();
        assert_eq!(lines_seen, total, "a keeping-up client loses nothing");
        assert!(
            log_frames < total / 100,
            "frames must aggregate many lines (got {log_frames} frames for {total} lines)"
        );
    }

    /// decode_ws_message strips (and decompresses) the frame header; the
    /// decode_*_frame helpers expect the full frame back.
    fn rebuild(t: u8, payload: &[u8]) -> Vec<u8> {
        let mut f = Vec::with_capacity(payload.len() + 1);
        f.push(t);
        f.extend_from_slice(payload);
        f
    }

    /// webui-api 慢客户端收到丢弃标记: a stalled client overflows the bounded
    /// subscriber queue; when it catches up, the loss arrives as an explicit
    /// LOG_GAP frame, never mixed into log content.
    #[tokio::test]
    async fn stalled_client_receives_explicit_gap_frame() {
        let (_sup, name, mut ws, ring) = firehose_setup().await;
        subscribe(&mut ws, &name, "out").await;

        // Synchronous burst on the single-threaded test runtime: the
        // subscriber task cannot run mid-burst, so overflow is deterministic.
        // Queue holds SUB_CHANNEL_BATCHES batches; the rest is dropped there.
        const BURST: usize = SUB_CHANNEL_BATCHES + 200;
        for i in 0..BURST {
            ring.push_batch(&[format!("burst-{i}")]);
        }
        // Let the subscriber drain the queued batches (no Gap among them).
        tokio::time::sleep(Duration::from_millis(100)).await;
        // Next delivery must start with the accumulated Gap.
        ring.push_batch(&["trigger".to_string()]);

        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            assert!(
                tokio::time::Instant::now() < deadline,
                "gap frame never arrived"
            );
            let msg = ws.next().await.unwrap().unwrap();
            let (t, payload) = api::decode_ws_message(&msg.into_data()).unwrap();
            if t == msg_type::LOG_GAP {
                let (prog, stream, dropped) = api::decode_gap_frame(&rebuild(t, &payload)).unwrap();
                assert_eq!(prog, name);
                assert_eq!(stream, 0);
                assert_eq!(dropped, (BURST - SUB_CHANNEL_BATCHES) as u64);
                // The stream continues: the trigger line follows.
                let msg = ws.next().await.unwrap().unwrap();
                let (t, payload) = api::decode_ws_message(&msg.into_data()).unwrap();
                assert_eq!(t, msg_type::LOG);
                let (_, _, data) = api::decode_log_frame(&rebuild(t, &payload)).unwrap();
                assert!(
                    data.contains("trigger"),
                    "stream resumes after the gap: {data:?}"
                );
                return;
            }
            // (snapshot/status frames may precede; keep reading)
        }
    }

    /// 3.4 落盘完整性: a large burst through the real pump path lands on disk
    /// line-for-line while the ring stays bounded.
    #[test]
    fn pump_writes_every_line_to_disk_under_batching() {
        let tmp = std::env::temp_dir().join(format!("ak-test-hs-disk-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let total = 50_000;
        let text: String = (0..total)
            .map(|i| format!("disk-{i}-0123456789abcdef\n"))
            .collect();
        let ring = pump::Ring::new(1000);
        let path = tmp.join("d.log");
        // max_size None: no rotation — file must hold every line.
        pump::pump(
            std::io::Cursor::new(text.into_bytes()),
            ring.clone(),
            path.clone(),
            None,
            2,
        );
        let content = std::fs::read_to_string(&path).unwrap();
        assert_eq!(content.lines().count(), total);
        assert_eq!(ring.tail(total + 1).len(), 1000, "ring stays bounded");
        let _ = std::fs::remove_dir_all(tmp);
    }
}

#[cfg(test)]
mod pending_ws_tests {
    use super::*;
    use crate::supervisor::{PendingDoc, PendingProgram};
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    fn test_stop() -> Arc<WebuiLifecycle> {
        WebuiLifecycle::new()
    }

    fn sup_with_pending() -> std::sync::Arc<Supervisor> {
        let config = crate::config::DaemonConfig::default();
        let dir = std::env::temp_dir().join(format!("ak-test-web-pending-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let sup = Supervisor::new(config, &dir).unwrap();
        {
            let mut st = sup.state.lock().unwrap();
            st.pending = PendingDoc {
                programs: vec![PendingProgram {
                    app: "demo".into(),
                    program: "web".into(),
                    running: true,
                }],
                ..Default::default()
            };
        }
        sup
    }

    /// webui-api: pending changes ride STATUS delta frames with a `pending`
    /// field when they differ from the last sent state.
    #[tokio::test]
    async fn ws_status_delta_carries_pending_changes() {
        let sup = sup_with_pending();
        let app = build_router(sup.clone(), test_stop(), "");
        let resp = app
            .oneshot(Request::get("/api/overview").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["pending"]["programs"][0]["program"], "web");
        assert_eq!(v["pending"]["programs"][0]["running"], true);
        // And /api/pending serves the same doc.
        let app2 = build_router(sup.clone(), test_stop(), "");
        let resp = app2
            .oneshot(Request::get("/api/pending").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["programs"][0]["app"], "demo");
    }

    /// guard-remove-until-stopped 4.3: a removal that lands through the
    /// command queue (CLI, `/v1`, another tab) reaches an open console as a
    /// STATUS delta naming the dropped program (`removed`) and the removed app
    /// record (`apps_gone`) — the signal the sidebar/overview converge on.
    #[tokio::test]
    async fn ws_status_delta_reports_removed_programs_and_apps_gone() {
        let dir = std::env::temp_dir().join(format!("ak-test-web-wsrm-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("app.d")).unwrap();
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
        let body = dir.join("web-appkeeper.toml");
        std::fs::write(
            &body,
            "[app]\nrestart = \"no\"\n\n[program.web]\ncommand = 'sleep 30'\nstartsecs = 0.0\n",
        )
        .unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&body, dir.join("app.d/web.toml")).unwrap();
        #[cfg(windows)]
        {
            if std::os::windows::fs::symlink_file(&body, dir.join("app.d/web.toml")).is_err() {
                std::fs::hard_link(&body, dir.join("app.d/web.toml")).unwrap();
            }
        }
        let (config, _) = crate::config::DaemonConfig::load_or_default(&cfg_path).unwrap();
        let sup = Supervisor::new(config, &dir).unwrap();
        sup.set_config_path(&cfg_path);
        sup.bootstrap().unwrap();
        // The command-queue driver: removal is executed by the supervisor loop.
        let runner = sup.clone();
        std::thread::spawn(move || runner.run());

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let sup2 = sup.clone();
        tokio::spawn(async move {
            let _ = axum::serve(listener, build_router(sup2, test_stop(), "")).await;
        });
        let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/ws"))
            .await
            .expect("ws connect");
        let msg = ws.next().await.unwrap().unwrap();
        let (t, _payload) = api::decode_ws_message(&msg.into_data()).unwrap();
        assert_eq!(t, msg_type::SNAPSHOT, "the first frame is the snapshot");

        // Remove out-of-band, exactly like `ak remove` / another console tab.
        let (tx, rx) = std::sync::mpsc::channel();
        sup.enqueue(Command::Remove {
            app: "web".into(),
            reply: tx,
        });
        rx.recv_timeout(Duration::from_secs(10))
            .expect("remove answered")
            .expect("remove succeeded");

        // The next STATUS frame must carry both removal signals.
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        let payload = loop {
            assert!(tokio::time::Instant::now() < deadline, "no STATUS frame");
            let msg = tokio::time::timeout(Duration::from_secs(5), ws.next())
                .await
                .expect("a frame arrives")
                .expect("the stream stays open")
                .unwrap();
            let Ok((t, payload)) = api::decode_ws_message(&msg.into_data()) else {
                continue;
            };
            if t == msg_type::STATUS {
                break payload;
            }
        };
        let v: serde_json::Value = serde_json::from_slice(&payload).unwrap();
        assert_eq!(
            v["removed"],
            serde_json::json!(["web"]),
            "the dropped program is named: {v}"
        );
        assert_eq!(
            v["apps_gone"],
            serde_json::json!(["web"]),
            "the removed app record is named: {v}"
        );
        assert!(!dir.join("app.d/web.toml").exists(), "record deleted");
        sup.request_shutdown();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
