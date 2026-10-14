//! /v1 control-plane client (design: bench is a pure external consumer of the
//! existing control plane — no daemon changes, same endpoints the CLI uses).
//!
//! Transport: the control plane lives on the endpoint the daemon config
//! names — a local IPC endpoint (unix domain socket, named pipe on Windows)
//! or a `tcp://host:port` URL (tcp-control-endpoint) — carrying HTTP/1.1
//! over either. Bench speaks that protocol itself. The IPC form has no
//! application credential (admission is the endpoint's owner); the tcp://
//! form takes the Bearer credential passed as `--token`.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use serde_json::Value;

/// One byte-stream connection to the control endpoint.
#[derive(Clone, Debug)]
pub struct Client {
    endpoint: Endpoint,
    /// Bearer credential sent with every request when set (`--token`, only
    /// meaningful for a tcp:// endpoint; the CLI layer refuses it elsewhere).
    token: Option<String>,
}

impl Client {
    /// `endpoint` is the daemon's control endpoint: a local IPC path (or
    /// `\\.\pipe\…` on Windows) or a `tcp://host:port` URL — the same shapes
    /// `[daemon] socket` accepts (tcp-control-endpoint D5).
    pub fn new(endpoint: &str) -> Result<Self> {
        Self::with_token(endpoint, None)
    }

    /// Same, with the Bearer credential for a tcp:// endpoint.
    pub fn with_token(endpoint: &str, token: Option<String>) -> Result<Self> {
        let endpoint = endpoint.trim();
        if endpoint.is_empty() {
            return Err(anyhow!(
                "--socket needs the daemon's control endpoint: a local IPC path \
                 (e.g. /run/appkeeper/$(id -u)/ak.sock) or a tcp://host:port URL"
            ));
        }
        Ok(Client {
            endpoint: Endpoint::parse(endpoint)
                .map_err(|e| anyhow!("--socket {e}"))?,
            token,
        })
    }

    /// The endpoint as a string, for reports and error messages.
    pub fn addr(&self) -> String {
        self.endpoint.display()
    }

    /// The endpoint itself, for permission assertions.
    pub fn endpoint(&self) -> &Endpoint {
        &self.endpoint
    }

    fn connect(&self) -> Result<Conn> {
        self.endpoint.connect().map_err(|e| {
            match (e.kind(), self.endpoint.owner_uid()) {
                (std::io::ErrorKind::PermissionDenied, Some(owner)) => anyhow!(
                    "control endpoint {} belongs to uid {owner} but this bench runs as uid {} \
                     — run bench as the account the daemon runs under (or with higher privileges)",
                    self.endpoint.display(),
                    current_uid()
                ),
                _ => anyhow!(
                    "cannot connect to the daemon at control endpoint {}: {e}",
                    self.endpoint.display()
                ),
            }
            .context("daemon connection refused")
        })
    }

    /// Issue one request; a non-2xx status is a control-plane ANSWER (409
    /// conflict, 404 unknown…), not a transport failure. A 401 is its own
    /// class (benchmark spec: TCP 形态凭据被拒 → 明确报告 token 不被接受,
    /// MUST NOT 表述为「连接不上」).
    fn request(&self, method: &str, path: &str, body: Option<&Value>) -> Result<Value> {
        let mut conn = self.connect()?;
        let payload = body.map(|b| b.to_string()).unwrap_or_default();
        let mut req = format!("{method} {path} HTTP/1.1\r\nHost: localhost\r\n");
        if let Some(token) = &self.token {
            // Trim: a credential with stray whitespace is a user typo, and
            // the header must stay well-formed.
            req.push_str(&format!(
                "Authorization: Bearer {}\r\n",
                token.trim()
            ));
        }
        req.push_str("Content-Type: application/json\r\n");
        req.push_str(&format!("Content-Length: {}\r\n", payload.len()));
        req.push_str("Connection: close\r\n\r\n");
        conn.write_all(req.as_bytes())?;
        conn.write_all(payload.as_bytes())?;
        conn.flush()?;

        let mut reader = BufReader::new(conn);
        let mut line = String::new();
        reader.read_line(&mut line)?;
        let status: u16 = line
            .split_whitespace()
            .nth(1)
            .and_then(|c| c.parse().ok())
            .with_context(|| format!("malformed response: {line:?}"))?;
        let mut length: Option<usize> = None;
        let mut chunked = false;
        loop {
            let mut h = String::new();
            if reader.read_line(&mut h)? == 0 {
                break;
            }
            let t = h.trim_end_matches(['\r', '\n']);
            if t.is_empty() {
                break;
            }
            if let Some((k, v)) = t.split_once(':') {
                let k = k.trim().to_ascii_lowercase();
                let v = v.trim();
                if k == "content-length" {
                    length = v.parse().ok();
                } else if k == "transfer-encoding" && v.eq_ignore_ascii_case("chunked") {
                    chunked = true;
                }
            }
        }
        let text = if chunked {
            let mut out = Vec::new();
            loop {
                let mut size_line = Vec::new();
                let mut byte = [0u8; 1];
                while !size_line.ends_with(b"\r\n") {
                    if reader.read(&mut byte)? == 0 {
                        break;
                    }
                    size_line.push(byte[0]);
                }
                let text = String::from_utf8_lossy(&size_line);
                let size = usize::from_str_radix(text.trim(), 16).unwrap_or(0);
                if size == 0 {
                    break;
                }
                let mut body = vec![0u8; size];
                reader.read_exact(&mut body)?;
                out.extend_from_slice(&body);
                let mut crlf = [0u8; 2];
                let _ = reader.read(&mut crlf);
            }
            String::from_utf8_lossy(&out).into_owned()
        } else {
            let mut out = Vec::new();
            match length {
                Some(n) => {
                    let mut body = vec![0u8; n];
                    reader.read_exact(&mut body)?;
                    out.extend_from_slice(&body);
                }
                None => {
                    reader.read_to_end(&mut out)?;
                }
            }
            String::from_utf8_lossy(&out).into_owned()
        };
        let v: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
        if (200..300).contains(&status) {
            Ok(v)
        } else {
            let msg = v
                .get("error")
                .and_then(|e| e.as_str())
                .unwrap_or(&text)
                .to_string();
            if status == 401 {
                Err(anyhow!(
                    "the daemon rejected the bench credential (401): {msg} — pass --token \
                     matching the daemon's [daemon] auth_token (only a tcp:// endpoint takes one)"
                ))
            } else {
                Err(anyhow!("control plane answered {status}: {msg}"))
            }
        }
    }

    pub fn health(&self) -> Result<()> {
        self.request("GET", "/v1/health", None)
            .map(|_| ())
            .map_err(|e| {
                // /v1/health is credential-free, so a 401 here would mean a
                // non-conforming daemon; report it as itself, never as
                // "cannot reach".
                if e.to_string().contains("401") {
                    e
                } else {
                    e.context(format!("cannot reach the daemon at {}", self.addr()))
                }
            })
    }

    pub fn status(&self) -> Result<Value> {
        self.request("GET", "/v1/status", None)
    }

    pub fn daemon_version(&self) -> Result<String> {
        let v = self.status()?;
        Ok(v.pointer("/daemon/version")
            .and_then(|x| x.as_str())
            .unwrap_or("unknown")
            .to_string())
    }

    /// The daemon-side config path (status projection `config_source`); the
    /// connect topology reads app_dir/log_dir from that file.
    pub fn config_source(&self) -> Result<String> {
        let v = self.status()?;
        v.pointer("/daemon/config_source")
            .and_then(|x| x.as_str())
            .map(String::from)
            .ok_or_else(|| anyhow!("status projection has no daemon.config_source"))
    }

    pub fn reload(&self) -> Result<Value> {
        self.request("POST", "/v1/reload", Some(&serde_json::json!({})))
    }

    pub fn apply(&self, app: Option<&str>) -> Result<Value> {
        let body = serde_json::json!({ "app": app });
        self.request("POST", "/v1/apply", Some(&body))
    }

    pub fn stop_program(&self, name: &str) -> Result<Value> {
        self.request(
            "POST",
            &format!("/v1/programs/{name}/stop"),
            Some(&serde_json::json!({})),
        )
    }

    pub fn shutdown(&self) -> Result<()> {
        self.request("POST", "/v1/shutdown", Some(&serde_json::json!({})))
            .map(|_| ())
    }

    /// States of the named programs from one status snapshot.
    /// Missing programs are simply absent from the result.
    pub fn program_states(&self, names: &[String]) -> Result<Vec<(String, String)>> {
        let v = self.status()?;
        let mut out = Vec::new();
        if let Some(ps) = v.get("programs").and_then(|p| p.as_array()) {
            for p in ps {
                let name = p.get("name").and_then(|n| n.as_str()).unwrap_or("");
                let state = p.get("state").and_then(|s| s.as_str()).unwrap_or("");
                if names.iter().any(|n| n == name) && !state.is_empty() {
                    out.push((name.to_string(), state.to_string()));
                }
            }
        }
        Ok(out)
    }

    /// Names of running programs carrying the bench prefix (collision check).
    pub fn bench_prefixed_programs(&self) -> Result<Vec<String>> {
        let v = self.status()?;
        let mut out = Vec::new();
        if let Some(ps) = v.get("programs").and_then(|p| p.as_array()) {
            for p in ps {
                if let Some(name) = p.get("name").and_then(|n| n.as_str()) {
                    if name.starts_with(crate::cases::BENCH_PREFIX) {
                        out.push(name.to_string());
                    }
                }
            }
        }
        Ok(out)
    }
}

// -- endpoint transport ------------------------------------------------------

/// A control endpoint in one of the two `[daemon] socket` shapes (design D5):
/// a local IPC path / pipe name, or a `tcp://host:port` URL. All platform
/// detail lives here; the client above only ever sees `Read + Write` bytes.
#[derive(Clone, Debug)]
pub struct Endpoint {
    /// Local IPC form: filesystem path / pipe name.
    path: Option<PathBuf>,
    /// TCP form: `host:port` with IPv6 brackets preserved as written.
    tcp: Option<String>,
}

impl Endpoint {
    /// Parse a `--socket` value with the same grammar as
    /// `[daemon] socket`: the `tcp://` prefix selects TCP (`host:port` must
    /// resolve: non-empty host, port 1–65535), every other non-empty value
    /// is a local IPC path.
    pub fn parse(value: &str) -> Result<Endpoint, String> {
        let value = value.trim();
        if let Some(rest) = value.strip_prefix("tcp://") {
            let rest = rest.trim_end_matches('/');
            let Some((host, port_raw)) = rest.rsplit_once(':') else {
                return Err(format!(
                    "tcp:// endpoint must be host:port, got {rest:?} (missing ':port')"
                ));
            };
            if host.is_empty() {
                return Err(format!("tcp:// endpoint host must not be empty, got {rest:?}"));
            }
            let port_err = || {
                format!("tcp:// endpoint port must be an integer in 1..=65535, got {port_raw:?}")
            };
            let port: u16 = port_raw.trim().parse().map_err(|_| port_err())?;
            if port == 0 {
                return Err(port_err());
            }
            return Ok(Endpoint {
                path: None,
                tcp: Some(format!("{host}:{port}")),
            });
        }
        Ok(Endpoint {
            path: Some(PathBuf::from(value)),
            tcp: None,
        })
    }

    pub fn display(&self) -> String {
        match (&self.tcp, &self.path) {
            (Some(addr), _) => format!("tcp://{addr}"),
            (_, Some(p)) => p.display().to_string(),
            _ => unreachable!("an endpoint is exactly one of IPC|TCP"),
        }
    }

    pub fn connect(&self) -> std::io::Result<Conn> {
        if let Some(addr) = &self.tcp {
            return TcpStream::connect(addr).map(Conn::tcp);
        }
        Conn::open(self.path.as_deref().expect("IPC form carries a path"))
    }

    /// Owner uid of the endpoint file, when the platform has one. A TCP
    /// endpoint has no owner (admission is the Bearer credential).
    pub fn owner_uid(&self) -> Option<u32> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            self.path
                .as_ref()
                .and_then(|p| std::fs::symlink_metadata(p).ok().map(|m| m.uid()))
        }
        #[cfg(not(unix))]
        {
            None
        }
    }

    /// Permission bits of the endpoint file, for the isolation assertions
    /// (IPC form only).
    pub fn file_mode(&self) -> Option<u32> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            self.path
                .as_ref()
                .and_then(|p| std::fs::metadata(p).ok())
                .map(|m| m.permissions().mode() & 0o7777)
        }
        #[cfg(not(unix))]
        {
            None
        }
    }

    /// Permission bits of the endpoint's directory, likewise.
    pub fn dir_mode(&self) -> Option<u32> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            self.path
                .as_ref()
                .and_then(|p| p.parent())
                .and_then(|p| std::fs::metadata(p).ok())
                .map(|m| m.permissions().mode() & 0o7777)
        }
        #[cfg(not(unix))]
        {
            None
        }
    }
}

pub fn current_uid() -> u32 {
    #[cfg(unix)]
    {
        unsafe extern "C" {
            fn geteuid() -> u32;
        }
        unsafe { geteuid() }
    }
    #[cfg(not(unix))]
    {
        0
    }
}

/// A connected byte stream to the control endpoint.
pub struct Conn(Box<dyn Stream>);

/// The one capability the client needs from a connection.
trait Stream: Read + Write + Send {}
impl<T: Read + Write + Send> Stream for T {}

impl Conn {
    /// TCP form (cross-platform std; no platform branches).
    fn tcp(s: TcpStream) -> Conn {
        Conn(Box::new(s))
    }
}

impl Read for Conn {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.0.read(buf)
    }
}

impl Write for Conn {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.write(buf)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.0.flush()
    }
}

#[cfg(unix)]
impl Conn {
    fn open(path: &Path) -> std::io::Result<Conn> {
        use std::os::unix::net::UnixStream;
        Ok(Conn(Box::new(UnixStream::connect(path)?)))
    }
}

#[cfg(windows)]
impl Conn {
    /// std's `File::open` ends up in `CreateFileW`, which is exactly how a
    /// client attaches to a named pipe; std handles the wide-character
    /// conversion and the handle lifetime.
    fn open(path: &Path) -> std::io::Result<Conn> {
        use std::fs::OpenOptions;
        use std::os::windows::fs::OpenOptionsExt;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .share_mode(0)
            .open(path)?;
        Ok(Conn(Box::new(file)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_socket_argument_is_rejected() {
        let err = Client::new("").unwrap_err().to_string();
        assert!(err.contains("--socket"), "{err}");
        assert!(Client::new("   ").is_err(), "whitespace is not a path");
    }

    #[test]
    fn socket_path_is_kept_verbatim() {
        let c = Client::new("/run/appkeeper/1000/ak.sock").unwrap();
        assert_eq!(c.addr(), "/run/appkeeper/1000/ak.sock");
    }

    /// tcp-control-endpoint 4.3: `--socket` accepts both endpoint shapes —
    /// the same URL grammar as `[daemon] socket`. Malformed tcp:// values
    /// are refused before any connection is attempted.
    #[test]
    fn socket_accepts_both_shapes_and_rejects_bad_tcp() {
        let c = Client::new("tcp://127.0.0.1:7310").unwrap();
        assert_eq!(c.addr(), "tcp://127.0.0.1:7310");
        // IPC stays verbatim.
        let c = Client::new("/tmp/ak.sock").unwrap();
        assert_eq!(c.addr(), "/tmp/ak.sock");
        for bad in ["tcp://127.0.0.1", "tcp://127.0.0.1:99999", "tcp://:1", "tcp://127.0.0.1:0"] {
            let err = Client::new(bad).unwrap_err().to_string();
            assert!(err.contains("--socket"), "{bad}: {err}");
        }
    }

    /// The token rides as a Bearer header on every request (only a tcp://
    /// endpoint legally has one; the CLI layer refuses the IPC+token combo).
    #[test]
    fn token_rides_as_bearer_header() {
        let port = {
            let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            l.local_addr().unwrap().port()
        };
        let listener = std::net::TcpListener::bind(("127.0.0.1", port)).unwrap();
        let server = std::thread::spawn(move || {
            let (mut conn, _) = listener.accept().unwrap();
            let mut buf = [0u8; 4096];
            let n = std::io::Read::read(&mut conn, &mut buf).unwrap();
            let req = String::from_utf8_lossy(&buf[..n]).into_owned();
            let body = r#"{"daemon":{"version":"t"}}"#;
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = std::io::Write::write_all(&mut conn, resp.as_bytes());
            req
        });
        let c = Client::with_token(&format!("tcp://127.0.0.1:{port}"), Some("bt".into()))
            .unwrap();
        assert_eq!(c.daemon_version().unwrap(), "t");
        let req = server.join().unwrap();
        assert!(
            req.contains("Authorization: Bearer bt\r\n"),
            "the bench token rides along: {req}"
        );
    }

    /// A 401 from the daemon is reported as a rejected credential — never
    /// as a connection failure (benchmark spec: TCP 凭据被拒 MUST NOT
    /// 表述为「连接不上」).
    #[test]
    fn tcp_401_is_a_rejected_credential_not_unreachable() {
        let port = {
            let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            l.local_addr().unwrap().port()
        };
        let listener = std::net::TcpListener::bind(("127.0.0.1", port)).unwrap();
        let server = std::thread::spawn(move || {
            let (mut conn, _) = listener.accept().unwrap();
            let mut buf = [0u8; 4096];
            let _ = std::io::Read::read(&mut conn, &mut buf);
            let body = r#"{"error":"unauthorized"}"#;
            let resp = format!(
                "HTTP/1.1 401 Unauthorized\r\nContent-Type: application/json\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = std::io::Write::write_all(&mut conn, resp.as_bytes());
        });
        let c = Client::with_token(&format!("tcp://127.0.0.1:{port}"), Some("wrong".into()))
            .unwrap();
        let err = c.status().unwrap_err();
        let text = format!("{err:#}");
        assert!(text.contains("401"), "{text}");
        assert!(text.contains("--token"), "names the fix: {text}");
        assert!(!text.contains("cannot reach"), "{text}");
        assert!(!text.contains("connection refused"), "{text}");
        server.join().unwrap();
    }

    /// An unreachable endpoint names the endpoint it tried (benchmark spec:
    /// 目标不可达 → 非零退出并报错说明连接失败).
    #[test]
    fn unreachable_endpoint_is_reported_with_its_path() {
        let dir = std::env::temp_dir().join(format!("xk-bench-api-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let sock = dir.join("absent.sock");
        let c = Client::new(&sock.display().to_string()).unwrap();
        let err = format!("{:#}", c.health().unwrap_err());
        assert!(err.contains(&sock.display().to_string()), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A file that is not a socket is refused as a connection failure, not
    /// silently deleted or treated as a daemon.
    #[cfg(unix)]
    #[test]
    fn non_endpoint_file_is_refused() {
        let dir = std::env::temp_dir().join(format!("xk-bench-api2-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let sock = dir.join("ak.sock");
        std::fs::write(&sock, b"x").unwrap();
        let c = Client::new(&sock.display().to_string()).unwrap();
        assert!(c.health().is_err(), "a plain file is not a control endpoint");
        assert!(sock.exists(), "bench must not remove anything it did not create");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
