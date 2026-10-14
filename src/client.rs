//! Thin HTTP client behind the CLI control subcommands, with the shared
//! exit-code contract (0 ok / 1 error / 2 config / 3 daemon unreachable).
//!
//! Transport: the control plane lives on the endpoint `[daemon] socket`
//! names — a local IPC path (unix socket / Windows named pipe) or a
//! `tcp://host:port` URL — and carries HTTP/1.1 over either, so
//! `curl --unix-socket <path> http://localhost/v1/status` still works for
//! troubleshooting. This module owns the HTTP semantics; the endpoint itself
//! is invisible here (see [`crate::transport`]). The one shape-awareness the
//! client keeps is credential attachment (tcp-control-endpoint D4): on a
//! `tcp://` endpoint every request carries `Authorization: Bearer
//! <auth_token>` (the config's), on a path endpoint nothing is attached —
//! decided via [`transport::Endpoint::is_tcp`], never by peeking at the raw
//! value.
//!
//! **About `ureq`:** it stays in `Cargo.toml` and stays used — but only by
//! [`crate::health`], to probe the user's *own* programs (`health_check =
//! "http://..."` / `"tcp://..."`). Control-plane traffic must never go back
//! through it: the endpoint is either owner-only IPC or token-gated TCP, so
//! a general-purpose HTTP client would add nothing but a second way to
//! reach it.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result};

use crate::transport::{self, Conn};

/// Markers `exit_code_of` maps to 3. Kept as literals (not an error type) so
/// the contract stays visible in the message the operator actually reads.
const UNREACHABLE: &str = "daemon unreachable";
const DENIED: &str = "control endpoint access denied";
/// A tcp:// control endpoint answered 401: the daemon is demonstrably up and
/// reachable — it just refused our credential. Its own exit class is 3
/// (control-plane spec: TCP 凭据被拒 → 退出码 3), because the fix is on the
/// operator's side of the connection (match the daemon's `auth_token`), not
/// in the command's arguments.
const TOKEN_REJECTED: &str = "control endpoint rejected the credential";

/// Default per-operation deadline for control calls. Streaming endpoints
/// override it (see [`Client::STREAM_TIMEOUT`]).
const CALL_TIMEOUT: Duration = Duration::from_secs(70);
/// Readiness-poll budget: a listener that accepts but never answers must not
/// eat the whole pull-up window (`daemonize::READY_TIMEOUT`).
const PROBE_TIMEOUT: Duration = Duration::from_millis(800);
/// `run --app` / custom actions block until the daemon's startup judgement,
/// which can legitimately take minutes.
const STREAM_TIMEOUT: Duration = Duration::from_secs(15 * 60);

/// How the response body is framed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Framing {
    Length(usize),
    Chunked,
    /// No `Content-Length` and no chunking: the body ends when the peer
    /// closes (the server always sends `Connection: close`).
    Eof,
}

/// A parsed response head plus the connection that still carries its body.
struct Response {
    status: u16,
    framing: Framing,
    conn: BufReader<Box<dyn Conn>>,
    chunked: ChunkedState,
}

#[derive(Clone)]
pub struct Client {
    endpoint: transport::Endpoint,
    /// The Bearer credential attached to every request when the endpoint is
    /// TCP (tcp-control-endpoint D4). `None` is the path-form world — the
    /// header is not even sent there.
    token: Option<String>,
    timeout: Duration,
}

impl Client {
    /// Build a client from the daemon config's control endpoint. Works with
    /// default settings even when no daemon config file exists.
    pub fn from_config(config: &crate::config::DaemonConfig) -> Self {
        Self::new(
            config.daemon.socket.to_string_lossy().as_ref(),
            config.daemon.auth_token.clone(),
            CALL_TIMEOUT,
        )
    }

    /// Same target, but with a short total timeout — for readiness polling
    /// where one hung listener must not eat the whole budget (the default
    /// 70s total would let two probes blow a ~10s readiness window).
    pub fn probe(config: &crate::config::DaemonConfig) -> Self {
        Self::new(
            config.daemon.socket.to_string_lossy().as_ref(),
            config.daemon.auth_token.clone(),
            PROBE_TIMEOUT,
        )
    }

    fn new(endpoint: &str, token: Option<String>, timeout: Duration) -> Self {
        // The config is validated on load, so an unusable value can only come
        // from a hand-built config; fall back to the built-in default rather
        // than panicking in a CLI path.
        let endpoint = transport::Endpoint::parse(endpoint)
            .unwrap_or_else(|_| transport::Endpoint::default_endpoint());
        Self {
            endpoint,
            token,
            timeout,
        }
    }

    /// The `Authorization` header value to attach: only on a TCP endpoint
    /// (the path form has no application credential and its server ignores
    /// the header). `Some` even when the token is unconfigured — the empty
    /// bearer is simply refused with the explanatory 401.
    fn auth_header(&self) -> Option<String> {
        if !self.endpoint.is_tcp() {
            return None;
        }
        Some(format!(
            "Bearer {}",
            self.token.as_deref().unwrap_or("")
        ))
    }

    /// The endpoint this client talks to (for error messages and reports).
    pub fn endpoint(&self) -> &transport::Endpoint {
        &self.endpoint
    }

    fn connect(&self, timeout: Duration) -> Result<Box<dyn Conn>> {
        let conn = self.endpoint.connect().map_err(|e| connect_error(&self.endpoint, e))?;
        // Connect is instant on a local endpoint; the deadline that matters is
        // the per-read one.
        let _ = conn.set_write_timeout(Some(timeout));
        let _ = conn.set_read_timeout(Some(timeout));
        Ok(conn)
    }

    fn call(&self, method: &str, path: &str) -> Result<serde_json::Value> {
        self.call_with_body(method, path, &serde_json::json!({}))
    }

    fn call_with_body(
        &self,
        method: &str,
        path: &str,
        body: &serde_json::Value,
    ) -> Result<serde_json::Value> {
        self.send(method, path, body, self.timeout, None)
    }

    /// Issue one request and read the whole body. A non-2xx status is an
    /// ANSWER from the control plane (409 conflict, 404 unknown…), not a
    /// transport failure; only a failed `connect`/read means "unreachable".
    fn send(
        &self,
        method: &str,
        path: &str,
        body: &serde_json::Value,
        timeout: Duration,
        accept: Option<&str>,
    ) -> Result<serde_json::Value> {
        let mut resp = self.request(method, path, body, timeout, accept)?;
        let text = resp.read_body()?;
        let v: serde_json::Value = serde_json::from_str(&text).unwrap_or(serde_json::json!({}));
        if (200..300).contains(&resp.status) {
            Ok(v)
        } else {
            let status = resp.status;
            let msg = v
                .get("error")
                .and_then(|e| e.as_str())
                .unwrap_or("request failed")
                .to_string();
            Err(api_error(status, &msg))
        }
    }

    /// Write the request and parse the response head; the caller owns the
    /// connection from here.
    fn request(
        &self,
        method: &str,
        path: &str,
        body: &serde_json::Value,
        timeout: Duration,
        accept: Option<&str>,
    ) -> Result<Response> {
        let mut conn = self.connect(timeout)?;
        let has_body = body.as_object().map(|o| !o.is_empty()).unwrap_or(false);
        let payload = if has_body { body.to_string() } else { String::new() };
        let mut head = format!("{method} {path} HTTP/1.1\r\nHost: localhost\r\n");
        if let Some(auth) = self.auth_header() {
            head.push_str(&format!("Authorization: {auth}\r\n"));
        }
        if let Some(a) = accept {
            head.push_str(&format!("Accept: {a}\r\n"));
        }
        if has_body {
            head.push_str("Content-Type: application/json\r\n");
            head.push_str(&format!("Content-Length: {}\r\n", payload.len()));
        }
        head.push_str("Connection: close\r\n\r\n");
        conn.write_all(head.as_bytes())
            .and_then(|_| conn.write_all(payload.as_bytes()))
            .and_then(|_| conn.flush())
            .map_err(|e| read_error(&self.endpoint, e))?;
        let mut reader = BufReader::new(conn);
        let mut line = String::new();
        reader
            .read_line(&mut line)
            .map_err(|e| read_error(&self.endpoint, e))?;
        let status: u16 = line
            .split_whitespace()
            .nth(1)
            .and_then(|c| c.parse().ok())
            .with_context(|| format!("malformed response from the control endpoint: {line:?}"))?;
        let mut framing = Framing::Eof;
        let mut total = 0usize;
        loop {
            let mut h = String::new();
            let n = reader
                .read_line(&mut h)
                .map_err(|e| read_error(&self.endpoint, e))?;
            if n == 0 {
                break;
            }
            let t = h.trim_end_matches(['\r', '\n']);
            if t.is_empty() {
                break;
            }
            if let Some((k, v)) = t.split_once(':') {
                total += t.len();
                match (k.trim().to_ascii_lowercase().as_str(), v.trim()) {
                    ("transfer-encoding", v) if v.eq_ignore_ascii_case("chunked") => {
                        framing = Framing::Chunked
                    }
                    ("content-length", v) => {
                        if let Ok(len) = v.parse::<usize>() {
                            framing = Framing::Length(len)
                        }
                    }
                    _ => {}
                }
            }
            if total > 64 * 1024 {
                return Err(anyhow::anyhow!("control endpoint sent oversized headers"));
            }
        }
        Ok(Response {
            status,
            framing,
            conn: reader,
            chunked: ChunkedState::default(),
        })
    }

    /// Cheap reachability probe.
    pub fn health(&self) -> Result<()> {
        self.call("GET", "/v1/health").map(|_| ())
    }

    pub fn status(&self) -> Result<serde_json::Value> {
        self.call("GET", "/v1/status")
    }

    pub fn program(&self, name: &str) -> Result<serde_json::Value> {
        self.call("GET", &format!("/v1/programs/{name}"))
    }

    pub fn action(&self, name: &str, action: &str) -> Result<serde_json::Value> {
        self.call("POST", &format!("/v1/programs/{name}/{action}"))
    }

    /// `run <name>`: start and block until the daemon's startup judgement
    /// (`?wait=1`, run-daemon-first control-plane spec: run 阻塞至守护进程返回).
    pub fn start_and_wait(&self, name: &str) -> Result<serde_json::Value> {
        self.send(
            "POST",
            &format!("/v1/programs/{name}/start?wait=1"),
            &serde_json::json!({}),
            STREAM_TIMEOUT,
            None,
        )
    }

    /// Run a program's declared custom action. Long actions must not hit the
    /// default deadline, so the request gets its own bound.
    pub fn custom_action(&self, program: &str, action: &str) -> Result<serde_json::Value> {
        self.send(
            "POST",
            &format!("/v1/programs/{program}/actions/{action}"),
            &serde_json::json!({}),
            STREAM_TIMEOUT,
            None,
        )
    }

    /// Deliver a whitelist signal to a program's child process.
    pub fn signal(&self, program: &str, signal: &str) -> Result<serde_json::Value> {
        self.call_with_body(
            "POST",
            &format!("/v1/programs/{program}/signal"),
            &serde_json::json!({ "signal": signal }),
        )
    }

    /// App-level fan-out (start|stop|restart), expanded server-side.
    pub fn app_action(&self, app: &str, verb: &str) -> Result<serde_json::Value> {
        self.call("POST", &format!("/v1/apps/{app}/{verb}"))
    }

    /// `run --app`: fan out and block until every program's startup judgement
    /// (`?wait=1`).
    pub fn app_start_and_wait(&self, app: &str) -> Result<serde_json::Value> {
        self.send(
            "POST",
            &format!("/v1/apps/{app}/start?wait=1"),
            &serde_json::json!({}),
            STREAM_TIMEOUT,
            None,
        )
    }

    pub fn reload(&self) -> Result<serde_json::Value> {
        self.call("POST", "/v1/reload")
    }

    pub fn pending(&self) -> Result<serde_json::Value> {
        self.call("GET", "/v1/pending")
    }

    pub fn apply(
        &self,
        app: Option<&str>,
        program: Option<&str>,
        restart: bool,
    ) -> Result<serde_json::Value> {
        let mut body = serde_json::json!({ "restart": restart });
        if let Some(a) = app {
            body["app"] = serde_json::Value::String(a.to_string());
        }
        if let Some(p) = program {
            body["program"] = serde_json::Value::String(p.to_string());
        }
        self.call_with_body("POST", "/v1/apply", &body)
    }

    pub fn shutdown(&self) -> Result<serde_json::Value> {
        self.call("POST", "/v1/shutdown")
    }

    pub fn log_tail(&self, name: &str, stream: &str, tail: usize) -> Result<Vec<String>> {
        let v = self.call(
            "GET",
            &format!("/v1/programs/{name}/logs?stream={stream}&tail={tail}"),
        )?;
        Ok(v.get("lines")
            .and_then(|l| l.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default())
    }

    /// Stream `follow` output to stdout until interrupted or disconnected.
    /// Thin wrapper over [`Self::follow_lines`] with a stdout writer.
    pub fn log_follow(&self, name: &str, stream: &str) -> Result<()> {
        self.follow_lines(name, stream, |line| {
            use std::io::Write;
            let mut out = std::io::stdout();
            let ok = writeln!(out, "{line}").is_ok() && out.flush().is_ok();
            ok
        })
    }

    /// Follow one program stream, invoking `on_line` for each line. `on_line`
    /// returns false to stop (e.g. the writer failed / the consumer left).
    /// Line-buffered so a consumer can prefix/serialize whole lines.
    pub fn follow_lines(
        &self,
        name: &str,
        stream: &str,
        mut on_line: impl FnMut(&str) -> bool,
    ) -> Result<()> {
        let path = format!("/v1/programs/{name}/logs?stream={stream}&tail=20&follow=1");
        let mut resp = self.request("GET", &path, &serde_json::json!({}), STREAM_TIMEOUT, Some("text/plain"))?;
        if resp.status != 200 {
            let status = resp.status;
            let text = resp.read_body().unwrap_or_default();
            let msg = serde_json::from_str::<serde_json::Value>(&text)
                .ok()
                .and_then(|v| v.get("error").and_then(|e| e.as_str()).map(String::from))
                .unwrap_or(text);
            anyhow::bail!("{:#}", api_error(status, &msg));
        }
        resp.for_each_line(&mut on_line)
    }

    // -- ad-hoc run (run-daemon-first section 6) ----------------------------

    /// Call `/v1/run` and stream its NDJSON progress frames to `on_frame`,
    /// returning the terminal frame. Each frame is a decoded JSON value;
    /// `on_frame` is invoked in arrival order. A non-2xx answer is surfaced as
    /// an api error (the control plane answered; the daemon is reachable).
    pub fn adhoc_run(
        &self,
        body: &serde_json::Value,
        mut on_frame: impl FnMut(&serde_json::Value),
    ) -> Result<serde_json::Value> {
        let mut resp = self.request(
            "POST",
            "/v1/run",
            body,
            STREAM_TIMEOUT,
            Some("application/x-ndjson"),
        )?;
        if resp.status != 200 {
            let status = resp.status;
            let text = resp.read_body().unwrap_or_default();
            let msg = serde_json::from_str::<serde_json::Value>(&text)
                .ok()
                .and_then(|v| v.get("error").and_then(|e| e.as_str()).map(String::from))
                .unwrap_or(text);
            anyhow::bail!("{:#}", api_error(status, &msg));
        }
        let mut last = serde_json::Value::Null;
        resp.for_each_line(&mut |line| {
            let line = line.trim();
            if line.is_empty() {
                return true;
            }
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(line) {
                on_frame(&v);
                last = v;
            }
            true
        })?;
        Ok(last)
    }

    /// Confirm re-landing of a residual (staged, unpersisted) app without
    /// restarting it (run-daemon-first D4).
    pub fn adhoc_commit(&self, name: &str) -> Result<serde_json::Value> {
        self.call_with_body(
            "POST",
            "/v1/run/commit",
            &serde_json::json!({ "name": name }),
        )
    }

    /// Abort a staged app: stop the process, delete the placeholder. Part of
    /// the control-plane surface; the CLI's decline path keeps the process
    /// running instead, so this is currently only exercised by tests.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn adhoc_abort(&self, name: &str) -> Result<serde_json::Value> {
        self.call_with_body(
            "POST",
            "/v1/run/abort",
            &serde_json::json!({ "name": name }),
        )
    }
}

impl Response {
    /// The whole body as text, whatever the framing.
    fn read_body(&mut self) -> Result<String> {
        let mut out = Vec::new();
        while self.pump(&mut out)? {}
        Ok(String::from_utf8_lossy(&out).into_owned())
    }

    /// Feed the decoded body to `on_line`, whole lines only; a trailing line
    /// without a newline is flushed at EOF. Returning false from `on_line`
    /// stops the stream.
    fn for_each_line(&mut self, on_line: &mut dyn FnMut(&str) -> bool) -> Result<()> {
        let mut pending: Vec<u8> = Vec::new();
        while self.pump(&mut pending)? {
            while let Some(pos) = pending.iter().position(|b| *b == b'\n') {
                let bytes: Vec<u8> = pending.drain(..=pos).collect();
                let line = String::from_utf8_lossy(&bytes);
                if !on_line(line.trim_end_matches(['\n', '\r'])) {
                    return Ok(());
                }
            }
        }
        if !pending.is_empty() {
            let line = String::from_utf8_lossy(&pending);
            on_line(line.trim_end_matches(['\n', '\r']));
        }
        Ok(())
    }

    /// Decode one step of the body into `out`. Returns false at end of body.
    fn pump(&mut self, out: &mut Vec<u8>) -> Result<bool> {
        let mut buf = [0u8; 8192];
        match self.framing {
            Framing::Length(0) => Ok(false),
            Framing::Length(remaining) => {
                let want = remaining.min(buf.len());
                let n = self.conn.read(&mut buf[..want])?;
                if n == 0 {
                    return Ok(false);
                }
                out.extend_from_slice(&buf[..n]);
                self.framing = Framing::Length(remaining - n);
                Ok(true)
            }
            Framing::Chunked => self.chunked.next_chunk(&mut self.conn, out),
            Framing::Eof => {
                let n = self.conn.read(&mut buf)?;
                if n == 0 {
                    Ok(false)
                } else {
                    out.extend_from_slice(&buf[..n]);
                    Ok(true)
                }
            }
        }
    }
}

/// The chunked-transfer decoder state, kept beside the response so the
/// streaming and buffered paths decode identically.
#[derive(Default)]
struct ChunkedState {
    /// Bytes still to come from the current chunk.
    remaining: usize,
    /// A chunk body has been consumed at least once (its trailing CRLF has
    /// not been swallowed yet).
    mid_stream: bool,
    /// The terminating zero-length chunk was seen.
    finished: bool,
}

impl ChunkedState {
    /// Decode the next chunk into `out`. `Ok(false)` = stream complete.
    fn next_chunk(&mut self, r: &mut dyn Read, out: &mut Vec<u8>) -> Result<bool> {
        if self.finished {
            return Ok(false);
        }
        if self.remaining == 0 {
            if self.mid_stream {
                // The CRLF that terminated the previous chunk body.
                let mut crlf = [0u8; 2];
                r.read_exact(&mut crlf)?;
                self.mid_stream = false;
            }
            let size = read_chunk_size(r)?;
            if size == 0 {
                self.finished = true;
                return Ok(false);
            }
            self.remaining = size;
        }
        let want = self.remaining.min(8192);
        let mut buf = vec![0u8; want];
        let n = r.read(&mut buf)?;
        if n == 0 {
            anyhow::bail!("truncated chunked stream");
        }
        out.extend_from_slice(&buf[..n]);
        self.remaining -= n;
        if self.remaining == 0 {
            self.mid_stream = true;
        }
        Ok(true)
    }
}

/// Read one `<hex>[;ext]\r\n` chunk-size line.
fn read_chunk_size(r: &mut dyn Read) -> Result<usize> {
    let mut line = Vec::new();
    let mut byte = [0u8; 1];
    while !line.ends_with(b"\r\n") {
        if r.read(&mut byte)? == 0 {
            anyhow::bail!("truncated chunked stream (no chunk size)");
        }
        line.push(byte[0]);
        if line.len() > 64 {
            anyhow::bail!("malformed chunked stream (chunk size line too long)");
        }
    }
    let text = String::from_utf8_lossy(&line);
    let hex = text.split(';').next().unwrap_or("").trim();
    usize::from_str_radix(hex, 16).with_context(|| format!("malformed chunk size {text:?}"))
}


/// A failed `connect()`: the two causes the spec separates — "no daemon
/// there" and "not your endpoint" — get different words.
fn connect_error(ep: &transport::Endpoint, e: std::io::Error) -> anyhow::Error {
    use std::io::ErrorKind;
    match e.kind() {
        ErrorKind::PermissionDenied => {
            let owner = match ep.owner_uid() {
                Some(uid) => format!("endpoint {} is owned by uid {uid}", ep.display()),
                None => format!("endpoint {} is not accessible to this account", ep.display()),
            };
            let me = match transport::current_uid() {
                Some(uid) => format!("uid {uid}"),
                None => format!("account {:?}", transport::current_user()),
            };
            anyhow::anyhow!(
                "{DENIED}: {owner}, the current user is {me} — run this command as the account \
                 the daemon runs under, or with higher privileges"
            )
        }
        ErrorKind::NotFound | ErrorKind::ConnectionRefused | ErrorKind::ConnectionReset => {
            anyhow::anyhow!("{UNREACHABLE} (is `ak daemon` active? endpoint {}): {e}", ep.display())
        }
        _ => anyhow::anyhow!("{UNREACHABLE} (endpoint {}): {e}", ep.display()),
    }
}

/// A failed write/read on an established connection: the peer went away.
fn read_error(ep: &transport::Endpoint, e: std::io::Error) -> anyhow::Error {
    anyhow::anyhow!("{UNREACHABLE} (connection to {} failed): {e}", ep.display())
}

/// A non-2xx control-plane ANSWER (the daemon is demonstrably reachable).
/// 401 is its own class on a `tcp://` endpoint: the message carries the fix
/// (`auth_token`), and [`exit_code_of`] maps it to 3 like the other
/// "the daemon did not do what you asked because of the endpoint" answers
/// (control-plane spec: TCP 凭据被拒 → 专属提示 + 退出码 3).
fn api_error(status: u16, msg: &str) -> anyhow::Error {
    if status == 401 {
        anyhow::anyhow!(
            "{TOKEN_REJECTED} (api error 401): {msg} — a tcp:// control endpoint requires the \
             daemon's `[daemon] auth_token`, which the client sends as `Authorization: Bearer \
             <token>`; check the auth_token in your config"
        )
    } else {
        anyhow::anyhow!("api error {status}: {msg}")
    }
}

/// Exit code class for a client error, per the CLI contract:
/// 3 = daemon unreachable (or the endpoint is not yours, or its TCP form
/// rejected the credential), 1 = everything else (2 handled at parse time).
pub fn exit_code_of(err: &anyhow::Error) -> i32 {
    let s = format!("{err:#}");
    if s.contains(UNREACHABLE) || s.contains(DENIED) || s.contains(TOKEN_REJECTED) {
        3
    } else {
        1
    }
}

/// Load the daemon config for client commands (defaults when absent).
pub fn load_config(config_path: &Path) -> Result<crate::config::DaemonConfig> {
    crate::config::DaemonConfig::load_or_default(config_path)
        .with_context(|| format!("failed to load daemon config {}", config_path.display()))
        .map(|(c, _)| c)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn tmp_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("xk-client-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn config_with_socket(socket: &std::path::Path) -> crate::config::DaemonConfig {
        toml::from_str(&format!(
            "[daemon]\nsocket = {:?}\n",
            socket.to_string_lossy()
        ))
        .unwrap()
    }

    /// Bind a private endpoint and return it with its listener. Binding in
    /// the test body (not in the server thread) removes the startup race:
    /// the endpoint exists before the client is allowed to try.
    fn bound(path: &std::path::Path) -> (transport::Endpoint, transport::Listener) {
        let ep = transport::Endpoint::parse(&path.display().to_string()).unwrap();
        let listener = ep.bind().unwrap();
        (ep, listener)
    }

    /// A one-shot fake control plane on a private socket: read one request,
    /// answer with the bytes the test supplies.
    fn fake_server(
        listener: transport::Listener,
        response: String,
    ) -> std::thread::JoinHandle<String> {
        std::thread::spawn(move || {
            let mut listener = listener;
            let mut conn = listener.accept().unwrap();
            let mut req = Vec::new();
            let mut buf = [0u8; 4096];
            loop {
                let n = std::io::Read::read(&mut conn, &mut buf).unwrap();
                if n == 0 {
                    break;
                }
                req.extend_from_slice(&buf[..n]);
                if let Some((head, body)) = String::from_utf8_lossy(&req).split_once("\r\n\r\n") {
                    let len = head
                        .lines()
                        .find_map(|l| {
                            l.strip_prefix("Content-Length: ")
                                .or_else(|| l.strip_prefix("content-length: "))
                        })
                        .and_then(|v| v.trim().parse::<usize>().ok())
                        .unwrap_or(0);
                    if body.len() >= len {
                        break;
                    }
                }
            }
            let _ = std::io::Write::write_all(&mut conn, response.as_bytes());
            let _ = std::io::Write::flush(&mut conn);
            String::from_utf8_lossy(&req).into_owned()
        })
    }

    /// A Content-Length-framed response; the length is derived so a test can
    /// never lie about its own body (a mismatch shows up as a truncated read).
    fn http_response(status_line: &str, body: &str) -> String {
        format!(
            "{status_line}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
    }

    fn ok_response(body: &str) -> String {
        http_response("HTTP/1.1 200 OK", body)
    }

    /// Task 3.1: the request is a well-formed HTTP/1.1 message over the
    /// socket (request line, Host, JSON body) and a 200 is decoded.
    #[test]
    fn call_writes_http_and_decodes_json() {
        let dir = tmp_dir("ok");
        let sock = dir.join("ak.sock");
        let (_, listener) = bound(&sock);
        let server = fake_server(listener, ok_response("{\"result\":\"ok\"}"));
        let c = Client::from_config(&config_with_socket(&sock));
        let v = c.status().unwrap();
        assert_eq!(v["result"], "ok");
        let req = server.join().unwrap();
        assert!(req.starts_with("GET /v1/status HTTP/1.1\r\n"), "{req}");
        assert!(req.contains("Host: localhost\r\n"), "{req}");
        assert!(
            !req.contains("Authorization"),
            "a path-form endpoint carries no credential: {req}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A body rides as JSON with a Content-Length the server can trust.
    #[test]
    fn post_body_is_json_with_content_length() {
        let dir = tmp_dir("body");
        let sock = dir.join("ak.sock");
        let (_, listener) = bound(&sock);
        let server = fake_server(listener, ok_response("{\"ok\":true}"));
        let c = Client::from_config(&config_with_socket(&sock));
        let v = c.signal("web", "USR1").unwrap();
        assert_eq!(v["ok"], true);
        let req = server.join().unwrap();
        assert!(req.contains("Content-Type: application/json\r\n"), "{req}");
        assert!(req.contains("Content-Length: 17\r\n"), "{req}");
        assert!(req.ends_with("{\"signal\":\"USR1\"}"), "{req}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Task 3.1: a 4xx is an ANSWER (api error), not a transport failure —
    /// the daemon is demonstrably reachable.
    #[test]
    fn error_status_is_an_api_error_not_unreachable() {
        let dir = tmp_dir("404");
        let sock = dir.join("ak.sock");
        let (_, listener) = bound(&sock);
        let server = fake_server(
            listener,
            http_response("HTTP/1.1 404 Not Found", r#"{"error":"unknown program"}"#),
        );
        let c = Client::from_config(&config_with_socket(&sock));
        let err = c.program("ghost").expect_err("404 must error");
        let text = format!("{err:#}");
        assert!(text.contains("404"), "{text}");
        assert!(text.contains("unknown program"), "{text}");
        assert_eq!(exit_code_of(&err), 1, "a control-plane answer is not unreachable");
        server.join().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Task 3.3: nothing listening at the endpoint → "daemon unreachable",
    /// exit code 3.
    #[test]
    fn no_listener_reports_unreachable_with_exit_3() {
        let dir = tmp_dir("dead");
        let sock = dir.join("ak.sock");
        let c = Client::from_config(&config_with_socket(&sock));
        let err = c.status().expect_err("nothing is listening");
        let text = format!("{err:#}");
        assert!(text.contains(UNREACHABLE), "{text}");
        assert!(text.contains(&sock.display().to_string()), "{text}");
        assert_eq!(exit_code_of(&err), 3);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Task 3.3: a permission failure gets its own words — it must NOT read
    /// as "the daemon is not running".
    #[test]
    fn permission_error_is_distinguished_from_unreachable() {
        let denied = connect_error(
            &transport::Endpoint::default_endpoint(),
            std::io::Error::from(std::io::ErrorKind::PermissionDenied),
        );
        let text = format!("{denied:#}");
        assert!(text.contains(DENIED), "{text}");
        assert!(text.contains("uid"), "names the ids: {text}");
        assert!(!text.starts_with(UNREACHABLE), "{text}");
        // Still exit class 3: the operator cannot act on it here.
        assert_eq!(exit_code_of(&denied), 3);
    }

    /// Task 3.2: chunked frames are reassembled into whole lines (a line
    /// split across chunks is rejoined; a trailing line without a newline is
    /// flushed at EOF) and the callback can stop the stream.
    #[test]
    fn follow_lines_reassembles_and_stops() {
        for stop_early in [false, true] {
            let dir = tmp_dir("follow");
            let sock = dir.join("ak.sock");
            let (_ep, listener) = bound(&sock);
            let chunks: Vec<&str> = if stop_early {
                vec!["a\nb\nc\n"]
            } else {
                vec!["one\npar", "tial\ntail-no-nl"]
            };
            let server = std::thread::spawn(move || {
                let mut listener = listener;
                let mut conn = listener.accept().unwrap();
                let mut buf = [0u8; 4096];
                let _ = std::io::Read::read(&mut conn, &mut buf);
                let head = "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\n\
                            Transfer-Encoding: chunked\r\nConnection: close\r\n\r\n";
                std::io::Write::write_all(&mut conn, head.as_bytes()).unwrap();
                for chunk in chunks {
                    let body = chunk.as_bytes();
                    let framed = format!("{:x}\r\n{}\r\n", body.len(), chunk);
                    std::io::Write::write_all(&mut conn, framed.as_bytes()).unwrap();
                }
                std::io::Write::write_all(&mut conn, b"0\r\n\r\n").unwrap();
                let _ = std::io::Write::flush(&mut conn);
            });
            let c = Client::from_config(&config_with_socket(&sock));
            let mut got = Vec::new();
            let mut count = 0;
            c.follow_lines("web", "out", |line| {
                count += 1;
                if stop_early {
                    got.push(line.to_string());
                    false
                } else {
                    got.push(line.to_string());
                    true
                }
            })
            .unwrap();
            if stop_early {
                assert_eq!(got, vec!["a"], "the callback stops the stream");
            } else {
                assert_eq!(got, vec!["one", "partial", "tail-no-nl"]);
            }
            server.join().unwrap();
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    /// A 404 on a streaming endpoint is an api error, not a broken pipe.
    #[test]
    fn follow_lines_surfaces_api_error() {
        let dir = tmp_dir("follow404");
        let sock = dir.join("ak.sock");
        let (_, listener) = bound(&sock);
        let server = fake_server(
            listener,
            http_response("HTTP/1.1 404 Not Found", r#"{"error":"unknown program"}"#),
        );
        let c = Client::from_config(&config_with_socket(&sock));
        let err = c.follow_lines("ghost", "out", |_| true).expect_err("404 must error");
        assert!(err.to_string().contains("404"), "{err:#}");
        server.join().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Task 3.2: NDJSON frames (one split across chunks) come out whole, in
    /// order, and the terminal frame is returned.
    #[test]
    fn adhoc_run_parses_frames_and_returns_terminal() {
        let dir = tmp_dir("adhoc");
        let sock = dir.join("ak.sock");
        let (_ep, listener) = bound(&sock);
        let server = std::thread::spawn(move || {
            let mut listener = listener;
            let mut conn = listener.accept().unwrap();
            let mut buf = [0u8; 8192];
            let _ = std::io::Read::read(&mut conn, &mut buf);
            let head = "HTTP/1.1 200 OK\r\nContent-Type: application/x-ndjson\r\n\
                        Transfer-Encoding: chunked\r\nConnection: close\r\n\r\n";
            std::io::Write::write_all(&mut conn, head.as_bytes()).unwrap();
            for chunk in [
                "{\"phase\":\"sta",
                "ged\",\"estimate_secs\":3}\n{\"ok\":true,\"program\":\"web\",\"pid\":42}\n",
            ] {
                let framed = format!("{:x}\r\n{}\r\n", chunk.len(), chunk);
                std::io::Write::write_all(&mut conn, framed.as_bytes()).unwrap();
            }
            std::io::Write::write_all(&mut conn, b"0\r\n\r\n").unwrap();
            let _ = std::io::Write::flush(&mut conn);
        });
        let c = Client::from_config(&config_with_socket(&sock));
        let mut seen = Vec::new();
        let last = c
            .adhoc_run(&serde_json::json!({"command": "/bin/true"}), |f| {
                seen.push(f.clone())
            })
            .unwrap();
        assert_eq!(seen.len(), 2, "{seen:?}");
        assert_eq!(seen[0]["phase"], "staged");
        assert_eq!(last["ok"], true);
        assert_eq!(last["pid"], 42);
        server.join().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A 4xx answer to the ad-hoc call is an api error carrying the daemon's
    /// own explanation.
    #[test]
    fn adhoc_run_surfaces_api_error() {
        let dir = tmp_dir("adhoc400");
        let sock = dir.join("ak.sock");
        let (_, listener) = bound(&sock);
        let server = fake_server(
            listener,
            http_response(
                "HTTP/1.1 400 Bad Request",
                r#"{"error":"app name \"all\" is reserved"}"#,
            ),
        );
        let c = Client::from_config(&config_with_socket(&sock));
        let err = c
            .adhoc_run(&serde_json::json!({"command": "/bin/true"}), |_| {})
            .expect_err("400 must error");
        assert!(err.to_string().contains("400"), "{err:#}");
        assert!(err.to_string().contains("reserved"), "{err:#}");
        server.join().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The residual confirm/decline surface posts to /v1/run/commit and
    /// /v1/run/abort with the program name.
    #[test]
    fn adhoc_commit_and_abort_post_name() {
        for (path, commit) in [("/v1/run/commit", true), ("/v1/run/abort", false)] {
            let dir = tmp_dir("commit");
            let sock = dir.join("ak.sock");
            let (_, listener) = bound(&sock);
        let server = fake_server(listener, ok_response("{\"result\":\"ok\"}"));
            let c = Client::from_config(&config_with_socket(&sock));
            let v = if commit {
                c.adhoc_commit("web").unwrap()
            } else {
                c.adhoc_abort("web").unwrap()
            };
            assert_eq!(v["result"], "ok");
            let req = server.join().unwrap();
            assert!(req.contains(path), "path in request: {req}");
            assert!(req.contains("\"name\":\"web\""), "name in body: {req}");
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    /// Task 3.4: a listener that accepts but never answers must not eat the
    /// readiness budget — the probe gives up within its own short timeout.
    #[cfg(unix)]
    #[test]
    fn probe_does_not_exhaust_the_budget_on_a_silent_listener() {
        let dir = tmp_dir("probe");
        let sock = dir.join("ak.sock");
        let (_ep, mut listener) = bound(&sock);
        // Accept and then say nothing, ever; the test only needs the
        // accepted connection to exist while the probe gives up.
        let silent = std::thread::spawn(move || {
            let mut conn = listener.accept().unwrap();
            let mut buf = [0u8; 64];
            let _ = std::io::Read::read(&mut conn, &mut buf);
            drop(conn);
        });
        let cfg = config_with_socket(&sock);
        let probe = Client::probe(&cfg);
        let t0 = std::time::Instant::now();
        assert!(probe.health().is_err(), "no answer is not healthy");
        let elapsed = t0.elapsed();
        assert!(
            elapsed < Duration::from_secs(2),
            "the probe gives up on its own budget (took {elapsed:?})"
        );
        drop(silent);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Task 3.5 (module contract): the client resolves its endpoint from the
    /// config and never invents one.
    #[test]
    fn endpoint_comes_from_the_config() {
        let dir = tmp_dir("ep");
        let sock = dir.join("custom.sock");
        let c = Client::from_config(&config_with_socket(&sock));
        assert_eq!(c.endpoint().display(), sock.display().to_string());
        // The built-in default when the config has no explicit socket.
        let d: crate::config::DaemonConfig = toml::from_str("[daemon]\n").unwrap();
        assert_eq!(
            Client::from_config(&d).endpoint().display(),
            transport::Endpoint::default_endpoint().display()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    // -- TCP form (tcp-control-endpoint task 4.1) ----------------------------

    fn free_tcp_port() -> u16 {
        std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port()
    }

    fn config_with_tcp(port: u16, token: Option<&str>) -> crate::config::DaemonConfig {
        let token_line = match token {
            Some(t) => format!("auth_token = \"{t}\"\n"),
            None => String::new(),
        };
        toml::from_str(&format!(
            "[daemon]\nsocket = \"tcp://127.0.0.1:{port}\"\n{token_line}"
        ))
        .unwrap()
    }

    /// A bound private TCP endpoint for the fake-server pattern.
    fn bound_tcp(port: u16) -> (transport::Endpoint, transport::Listener) {
        let ep = transport::Endpoint::parse(&format!("tcp://127.0.0.1:{port}")).unwrap();
        let listener = ep.bind().unwrap();
        (ep, listener)
    }

    /// 4.1: on a `tcp://` endpoint the client automatically attaches
    /// `Authorization: Bearer <auth_token>` from the same config; the
    /// endpoint shape (via `is_tcp`) is the only decision input.
    #[test]
    fn tcp_client_sends_the_bearer_header() {
        let port = free_tcp_port();
        let (_, listener) = bound_tcp(port);
        let server = fake_server(listener, ok_response("{\"result\":\"ok\"}"));
        let c = Client::from_config(&config_with_tcp(port, Some("tok-1")));
        assert!(c.endpoint().is_tcp());
        let v = c.status().unwrap();
        assert_eq!(v["result"], "ok");
        let req = server.join().unwrap();
        assert!(
            req.contains("Authorization: Bearer tok-1\r\n"),
            "the config's token rides along: {req}"
        );
    }

    /// 4.1: a tcp:// config without a token sends an empty bearer (the
    /// server would 401 it) — the shape still decides attachment, uniformly.
    #[test]
    fn tcp_client_without_token_sends_empty_bearer() {
        let port = free_tcp_port();
        let (_, listener) = bound_tcp(port);
        let server = fake_server(listener, ok_response("{}"));
        let c = Client::from_config(&config_with_tcp(port, None));
        let _ = c.status();
        let req = server.join().unwrap();
        assert!(
            req.contains("Authorization: Bearer \r\n"),
            "the header is attached (shape decides), empty when unconfigured: {req}"
        );
    }

    /// 4.1: a 401 from the control plane is a business answer (the daemon is
    /// reachable): the message keeps the daemon's own words plus the fix,
    /// and the exit class is 3 — never "unreachable" wording for a refusal.
    #[test]
    fn tcp_401_is_a_business_answer_with_exit_3() {
        let port = free_tcp_port();
        let (_, listener) = bound_tcp(port);
        let server = fake_server(
            listener,
            http_response("HTTP/1.1 401 Unauthorized", r#"{"error":"unauthorized"}"#),
        );
        let c = Client::from_config(&config_with_tcp(port, Some("wrong")));
        let err = c.status().expect_err("401 must error");
        let text = format!("{err:#}");
        assert!(text.contains("401"), "{text}");
        assert!(text.contains("auth_token"), "names the fix: {text}");
        assert!(text.contains("unauthorized"), "keeps the daemon's words: {text}");
        assert_eq!(exit_code_of(&err), 3);
        server.join().unwrap();
    }

    /// 4.1: a closed TCP endpoint is the plain unreachable case (exit 3,
    /// "daemon unreachable" wording — connection refused, nothing answered).
    #[test]
    fn tcp_connect_refused_is_unreachable() {
        let port = free_tcp_port(); // bound briefly, then dropped: closed
        let c = Client::from_config(&config_with_tcp(port, Some("t")));
        let err = c.status().expect_err("nothing is listening");
        let text = format!("{err:#}");
        assert!(text.contains(UNREACHABLE), "{text}");
        assert_eq!(exit_code_of(&err), 3);
    }
}
