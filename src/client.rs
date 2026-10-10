//! Thin HTTP client behind the CLI control subcommands, with the shared
//! exit-code contract (0 ok / 1 error / 2 config / 3 daemon unreachable).

use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result};

#[derive(Clone)]
pub struct Client {
    agent: ureq::Agent,
    pub base: String,
    token: String,
}

impl Client {
    /// Build a client from the daemon config (host/port/token). Works with
    /// default settings even when no daemon config file exists.
    pub fn from_config(config: &crate::config::DaemonConfig) -> Self {
        Self {
            agent: ureq::AgentBuilder::new()
                .timeout_connect(Duration::from_secs(2))
                .timeout(Duration::from_secs(70))
                .build(),
            base: format!("http://{}:{}", config.daemon.host, config.daemon.port),
            token: config.daemon.auth_token.clone(),
        }
    }

    /// Same target, but with a short total timeout — for readiness polling
    /// where one hung listener must not eat the whole budget (the default
    /// 70s total would let two probes blow a ~10s readiness window).
    pub fn probe(config: &crate::config::DaemonConfig) -> Self {
        Self {
            agent: ureq::AgentBuilder::new()
                .timeout_connect(Duration::from_millis(500))
                .timeout(Duration::from_millis(800))
                .build(),
            base: format!("http://{}:{}", config.daemon.host, config.daemon.port),
            token: config.daemon.auth_token.clone(),
        }
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
        let r = self
            .agent
            .request(method, &format!("{}{}", self.base, path));
        self.send(r, body)
    }

    /// Send a prepared request; `body` rides as JSON when non-empty.
    fn send(&self, mut r: ureq::Request, body: &serde_json::Value) -> Result<serde_json::Value> {
        if !self.token.is_empty() {
            r = r.set("Authorization", &format!("Bearer {}", self.token));
        }
        let has_body = body.as_object().map(|o| !o.is_empty()).unwrap_or(false);
        // ureq surfaces HTTP error statuses (4xx/5xx) as Error::Status — those
        // are ANSWERS from the control plane (409 conflict, 404 unknown…),
        // not transport failures; only Transport means "daemon unreachable".
        let resp = if has_body {
            match r
                .set("Content-Type", "application/json")
                .send_string(&body.to_string())
            {
                Ok(resp) => resp,
                Err(ureq::Error::Status(_, resp)) => resp,
                Err(e) => return Err(unreachable(e)),
            }
        } else {
            match r.call() {
                Ok(resp) => resp,
                Err(ureq::Error::Status(_, resp)) => resp,
                Err(e) => return Err(unreachable(e)),
            }
        };
        let status = resp.status();
        let text = resp.into_string().unwrap_or_default();
        let v: serde_json::Value = serde_json::from_str(&text).unwrap_or(serde_json::json!({}));
        if (200..300).contains(&status) {
            Ok(v)
        } else {
            let msg = v
                .get("error")
                .and_then(|e| e.as_str())
                .unwrap_or("request failed")
                .to_string();
            Err(anyhow::anyhow!("api error {status}: {msg}"))
        }
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

    /// Run a program's declared custom action. Long actions must not hit the
    /// agent's default total timeout, so the request gets its own bound.
    pub fn custom_action(&self, program: &str, action: &str) -> Result<serde_json::Value> {
        let r = self
            .agent
            .request(
                "POST",
                &format!("{}/v1/programs/{program}/actions/{action}", self.base),
            )
            .timeout(Duration::from_secs(15 * 60));
        self.send(r, &serde_json::json!({}))
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
        let mut path = format!("/v1/programs/{name}/logs?stream={stream}&tail=20&follow=1");
        if !self.token.is_empty() {
            path = format!("{path}&auth=1"); // token goes via header below
        }
        let mut r = self
            .agent
            .get(&format!("{}{}", self.base, path))
            .set("Accept", "text/plain");
        if !self.token.is_empty() {
            r = r.set("Authorization", &format!("Bearer {}", self.token));
        }
        let resp = match r.call() {
            Ok(resp) => resp,
            // 4xx/5xx are control-plane answers (e.g. unknown program 404).
            Err(ureq::Error::Status(_, resp)) => resp,
            Err(e) => return Err(unreachable(e)),
        };
        if resp.status() != 200 {
            let code = resp.status();
            let msg = resp.into_string().unwrap_or_default();
            anyhow::bail!("api error {code}: {msg}");
        }
        let mut reader = resp.into_reader();
        let mut pending = Vec::new();
        let mut buf = [0u8; 4096];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => {
                    // Flush a trailing line without a newline.
                    if !pending.is_empty() {
                        let line = String::from_utf8_lossy(&pending).into_owned();
                        if !on_line(&line) {
                            return Ok(());
                        }
                    }
                    return Ok(());
                }
                Ok(n) => {
                    pending.extend_from_slice(&buf[..n]);
                    while let Some(pos) = pending.iter().position(|b| *b == b'\n') {
                        let line_bytes: Vec<u8> = pending.drain(..=pos).collect();
                        let line = String::from_utf8_lossy(&line_bytes);
                        let line = line.trim_end_matches(['\n', '\r']);
                        if !on_line(line) {
                            return Ok(());
                        }
                    }
                }
                Err(e) => return Err(e.into()),
            }
        }
    }
}

fn unreachable(e: ureq::Error) -> anyhow::Error {
    let hint = "daemon unreachable (is `ak run` active?)";
    match e {
        ureq::Error::Transport(t) => anyhow::anyhow!("{hint}: {t}"),
        other => anyhow::anyhow!("{hint}: {other}"),
    }
}

/// Exit code class for a client error, per the CLI contract:
/// 3 = daemon unreachable, 1 = everything else (2 handled at parse time).
pub fn exit_code_of(err: &anyhow::Error) -> i32 {
    let s = format!("{err:#}");
    if s.contains("daemon unreachable") {
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

    fn config_with_port(port: u16) -> crate::config::DaemonConfig {
        toml::from_str(&format!("[daemon]\nhost = \"127.0.0.1\"\nport = {port}\n")).unwrap()
    }

    /// `follow_lines` frames the chunked stream into whole lines (a line split
    /// across chunks is reassembled; a trailing line without a newline is
    /// flushed at EOF) and stops when the callback returns false.
    #[test]
    fn follow_lines_reassembles_and_stops() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (mut sock, _) = listener.accept().unwrap();
            let mut buf = [0u8; 4096];
            let _ = std::io::Read::read(&mut sock, &mut buf);
            let head = "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\n\
                        Transfer-Encoding: chunked\r\nConnection: close\r\n\r\n";
            std::io::Write::write_all(&mut sock, head.as_bytes()).unwrap();
            // Two lines, the first split across two chunks; then a final line
            // with no trailing newline.
            for chunk in ["one\npar", "tial\ntail-no-nl"] {
                let body = chunk.as_bytes();
                let framed = format!("{:x}\r\n{}\r\n", body.len(), chunk);
                std::io::Write::write_all(&mut sock, framed.as_bytes()).unwrap();
            }
            std::io::Write::write_all(&mut sock, b"0\r\n\r\n").unwrap();
            let _ = std::io::Write::flush(&mut sock);
        });

        let c = Client::from_config(&config_with_port(port));
        let mut got = Vec::new();
        c.follow_lines("web", "out", |line| {
            got.push(line.to_string());
            true
        })
        .unwrap();
        assert_eq!(got, vec!["one", "partial", "tail-no-nl"]);
        server.join().unwrap();

        // A callback returning false stops early.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (mut sock, _) = listener.accept().unwrap();
            let mut buf = [0u8; 4096];
            let _ = std::io::Read::read(&mut sock, &mut buf);
            let head = "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\n\
                        Transfer-Encoding: chunked\r\nConnection: close\r\n\r\n";
            std::io::Write::write_all(&mut sock, head.as_bytes()).unwrap();
            for chunk in ["a\nb\nc\n"] {
                let framed = format!("{:x}\r\n{}\r\n", chunk.len(), chunk);
                std::io::Write::write_all(&mut sock, framed.as_bytes()).unwrap();
            }
            std::io::Write::write_all(&mut sock, b"0\r\n\r\n").unwrap();
            let _ = std::io::Write::flush(&mut sock);
        });
        let c = Client::from_config(&config_with_port(port));
        let mut count = 0;
        c.follow_lines("web", "out", |_| {
            count += 1;
            false // stop after the first line
        })
        .unwrap();
        assert_eq!(count, 1, "the callback stops the stream");
        server.join().unwrap();
    }

    /// A 404 answer is surfaced as an api error (the control plane answered,
    /// the daemon is reachable).
    #[test]
    fn follow_lines_surfaces_api_error() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (mut sock, _) = listener.accept().unwrap();
            let mut buf = [0u8; 4096];
            let _ = std::io::Read::read(&mut sock, &mut buf);
            let body = "{\"error\":\"unknown program\"}";
            let resp = format!(
                "HTTP/1.1 404 Not Found\r\nContent-Type: application/json\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = std::io::Write::write_all(&mut sock, resp.as_bytes());
        });
        let c = Client::from_config(&config_with_port(port));
        let err = c
            .follow_lines("ghost", "out", |_| true)
            .err()
            .expect("404 must error");
        assert!(err.to_string().contains("404"), "{err:#}");
        server.join().unwrap();
    }
}
