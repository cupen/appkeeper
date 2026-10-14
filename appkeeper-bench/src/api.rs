//! /v1 control-plane client (design: bench is a pure external consumer of the
//! existing control plane — no daemon changes, same endpoints the CLI uses).
//!
//! Transport: the control plane moved to a local IPC endpoint — a unix
//! domain socket, a named pipe on Windows — carrying HTTP/1.1. Bench speaks
//! that protocol itself; there is no `HOST:PORT` and no token any more
//! (admission is the endpoint's owner, decided by the operating system).

use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use serde_json::Value;

/// One byte-stream connection to the control endpoint.
#[derive(Clone, Debug)]
pub struct Client {
    endpoint: Endpoint,
}

impl Client {
    /// `path` is the daemon's control socket (or `\\.\pipe\…` on Windows).
    pub fn new(path: &str) -> Result<Self> {
        let path = path.trim();
        if path.is_empty() {
            return Err(anyhow!(
                "--socket needs the daemon's control endpoint path, e.g. \
                 /run/appkeeper/$(id -u)/ak.sock"
            ));
        }
        Ok(Client {
            endpoint: Endpoint::new(PathBuf::from(path)),
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
    /// conflict, 404 unknown…), not a transport failure.
    fn request(&self, method: &str, path: &str, body: Option<&Value>) -> Result<Value> {
        let mut conn = self.connect()?;
        let payload = body.map(|b| b.to_string()).unwrap_or_default();
        let mut req = format!("{method} {path} HTTP/1.1\r\nHost: localhost\r\n");
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
            Err(anyhow!("control plane answered {status}: {msg}"))
        }
    }

    pub fn health(&self) -> Result<()> {
        self.request("GET", "/v1/health", None)
            .map(|_| ())
            .with_context(|| format!("cannot reach the daemon at {}", self.addr()))
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

/// A local IPC endpoint. All platform detail lives here; the client above
/// only ever sees `Read + Write` bytes.
#[derive(Clone, Debug)]
pub struct Endpoint {
    path: PathBuf,
}

impl Endpoint {
    pub fn new(path: PathBuf) -> Self {
        Endpoint { path }
    }

    pub fn display(&self) -> String {
        self.path.display().to_string()
    }

    pub fn connect(&self) -> std::io::Result<Conn> {
        Conn::open(&self.path)
    }

    /// Owner uid of the endpoint file, when the platform has one.
    pub fn owner_uid(&self) -> Option<u32> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            std::fs::symlink_metadata(&self.path).ok().map(|m| m.uid())
        }
        #[cfg(not(unix))]
        {
            None
        }
    }

    /// Permission bits of the endpoint file, for the isolation assertions.
    pub fn file_mode(&self) -> Option<u32> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::metadata(&self.path)
                .ok()
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
                .parent()
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
