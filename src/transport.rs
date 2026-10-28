//! Control-plane transport (unix-socket-control-plane D9 + tcp-control-endpoint D1).
//!
//! This module is the ONLY place in the tree that knows about unix domain
//! sockets, named pipes, socket permission bits, Windows security
//! descriptors — or that the `tcp://` form of `[daemon] socket` is TCP at
//! all. Everything above it — [`crate::server`] (HTTP parsing and routing),
//! [`crate::client`] (HTTP/1.1 requests), [`crate::main`] — sees an opaque
//! [`Endpoint`] plus [`Conn`]/[`Listener`] and never sees a path, a port or
//! a platform IPC type. Conversely nothing here knows what HTTP is: the
//! bytes in and out are opaque.
//!
//! Identity is enforced by whichever model the endpoint shape picks (design
//! D2 — the shapes never mix): a local IPC path delegates to the operating
//! system (unix binds the socket `0600` inside a `0700` per-uid directory,
//! Windows builds an explicit DACL naming only the running account, SYSTEM
//! and Administrators), while a `tcp://` endpoint is reachable from the
//! network and leans on the application-level Bearer credential checked by
//! [`crate::server`] — this layer carries the bytes, not the policy.

use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// One accepted connection, split for reading and writing.
///
/// The control plane server is synchronous std with one thread per
/// connection; `try_clone_conn` gives the writer half without requiring the
/// concrete type to be a `TcpStream`.
pub trait Conn: Read + Write + Send {
    fn try_clone_conn(&self) -> io::Result<Box<dyn Conn>>;
    /// Per-read deadline. `None` = block forever. A socket that accepts but
    /// never answers must not be able to hold a thread (or the readiness
    /// budget in `daemonize`) hostage.
    fn set_read_timeout(&self, timeout: Option<Duration>) -> io::Result<()>;
    fn set_write_timeout(&self, timeout: Option<Duration>) -> io::Result<()>;
}

/// Why an endpoint value cannot be used at all (distinct from "cannot reach
/// it right now", which is an `io::Error` from [`Endpoint::connect`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EndpointError {
    /// `[daemon] socket = ""` — rejected by validation, never bound.
    Empty,
    /// Longer than the platform's endpoint path limit (`sun_path` on unix,
    /// `MAX_PATH` on Windows). Truncating silently would point the daemon at
    /// an endpoint nobody else connects to.
    TooLong { len: usize, max: usize },
    /// A `tcp://` value that does not parse as `host:port` (missing port,
    /// empty host, port outside 1–65535). The message names the defect and
    /// the fix.
    Tcp(String),
}

impl std::fmt::Display for EndpointError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EndpointError::Empty => write!(f, "must not be empty"),
            EndpointError::TooLong { len, max } => write!(
                f,
                "is {len} bytes, longer than the {max}-byte limit for a local IPC endpoint \
                 (unix sun_path / Windows pipe name)"
            ),
            EndpointError::Tcp(msg) => write!(f, "{msg}"),
        }
    }
}

impl std::error::Error for EndpointError {}

/// Why a bind did not produce a listener.
#[derive(Debug)]
pub enum BindError {
    /// Something is already serving this endpoint (the probe connect
    /// succeeded, or it exists but is not ours to touch). The endpoint is
    /// left exactly as it was.
    AlreadyRunning,
    /// The path exists but is not an IPC endpoint — a regular file, a
    /// directory, ... Never deleted (design D6).
    PathOccupied,
    /// The endpoint value itself is unusable.
    Invalid(EndpointError),
    Io(io::Error),
}

impl std::fmt::Display for BindError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BindError::AlreadyRunning => write!(f, "another daemon is already serving it"),
            BindError::PathOccupied => write!(
                f,
                "is occupied by a file that is not an IPC endpoint (left untouched)"
            ),
            BindError::Invalid(e) => write!(f, "{e}"),
            BindError::Io(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for BindError {}

impl From<io::Error> for BindError {
    fn from(e: io::Error) -> Self {
        BindError::Io(e)
    }
}

/// A control-plane endpoint, one of exactly two shapes (tcp-control-endpoint
/// D1): a local IPC path (filesystem path on unix, pipe name on Windows) or a
/// `tcp://host:port` TCP address. The `socket` value's URL prefix picks the
/// shape; the same URL is what [`Endpoint::parse`] accepts, what
/// [`Endpoint::display`] renders and what the config layer validates.
///
/// Opaque on purpose — callers format it with [`Endpoint::display`], ask it
/// which admission model it implies with [`Endpoint::is_tcp`], and pass it
/// back to [`Endpoint::connect`] / [`Endpoint::bind`]; nothing else is
/// reachable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoint {
    inner: Inner,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Inner {
    Ipc(platform::Endpoint),
    Tcp(TcpAddr),
}

/// A parsed `tcp://host:port`. `host` keeps IPv6 brackets exactly as written
/// (`[::1]`), so `format!("{host}:{port}")` is always a valid std resolver
/// string.
#[derive(Debug, Clone, PartialEq, Eq)]
struct TcpAddr {
    host: String,
    port: u16,
}

/// The per-user root every default endpoint lives under: `/run/appkeeper`
/// on unix. Created by the daemon itself when missing (task 7.2) and by
/// systemd's `RuntimeDirectory=appkeeper` in the packaged unit.
pub const DEFAULT_ROOT: &str = "/run/appkeeper";
/// The endpoint file name inside the per-uid directory.
pub const DEFAULT_NAME: &str = "ak.sock";
/// Permission bits for the per-uid directory: nobody else may even list it.
#[cfg(unix)]
pub const UID_DIR_MODE: u32 = 0o700;
/// Permission bits for the shared root: traversable, not writable.
#[cfg(unix)]
pub const ROOT_DIR_MODE: u32 = 0o755;
/// Permission bits for the endpoint itself: owner read/write only.
#[cfg(unix)]
pub const SOCKET_MODE: u32 = 0o600;

impl Endpoint {
    /// Parse a configured endpoint value. Rejects empty and unusable values
    /// up front so validation and bind agree on what is acceptable. The
    /// `tcp://` prefix selects the TCP shape (`host:port` must resolve:
    /// non-empty host, port 1–65535 — design D1); every other non-empty
    /// value is a local IPC path.
    pub fn parse(value: &str) -> Result<Endpoint, EndpointError> {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            return Err(EndpointError::Empty);
        }
        if let Some(rest) = trimmed.strip_prefix("tcp://") {
            return parse_tcp(rest).map(|tcp| Endpoint {
                inner: Inner::Tcp(tcp),
            });
        }
        platform::Endpoint::parse(trimmed).map(|inner| Endpoint {
            inner: Inner::Ipc(inner),
        })
    }

    /// The platform default: `/run/appkeeper/<uid>/ak.sock` on unix,
    /// `\\.\pipe\ak-<username>.sock` on Windows. Both carry the current
    /// uid / user name so two accounts on one machine never collide
    /// (design D5). Defaults are always the IPC shape; TCP is an explicit
    /// `tcp://` choice.
    pub fn default_path() -> PathBuf {
        platform::Endpoint::default_path()
    }

    /// The built-in default as a validated endpoint (it is valid by
    /// construction; the `expect` documents that invariant).
    pub fn default_endpoint() -> Endpoint {
        Endpoint {
            inner: Inner::Ipc(platform::Endpoint::default_endpoint()),
        }
    }

    /// Human-readable form for logs, error messages and `ak validate` hints.
    /// A TCP endpoint renders as its URL (`tcp://host:port`) — the shape is
    /// self-evident in the startup log (task 2.4).
    pub fn display(&self) -> String {
        match &self.inner {
            Inner::Ipc(e) => e.display(),
            Inner::Tcp(t) => format!("tcp://{}:{}", t.host, t.port),
        }
    }

    /// Which admission model this endpoint implies (tcp-control-endpoint D2,
    /// read-only): `true` = TCP, Bearer credential on the application layer;
    /// `false` = local IPC, the OS decides by endpoint owner. This is the
    /// only shape introspection above this layer — the client uses it to
    /// decide whether to attach an `Authorization` header without ever
    /// seeing the raw value.
    pub fn is_tcp(&self) -> bool {
        matches!(self.inner, Inner::Tcp(_))
    }

    /// Open a connection. Errors keep their `io::ErrorKind` so callers can
    /// tell "not there" from "not allowed" — the two get different operator
    /// messages (spec: 控制面连接失败的错误区分).
    pub fn connect(&self) -> io::Result<Box<dyn Conn>> {
        match &self.inner {
            Inner::Ipc(e) => e.connect().map(|c| Box::new(c) as Box<dyn Conn>),
            Inner::Tcp(t) => {
                let addr = format!("{}:{}", t.host, t.port);
                std::net::TcpStream::connect(addr)
                    .map(|s| Box::new(TcpConn(s)) as Box<dyn Conn>)
            }
        }
    }

    /// Owner uid of the endpoint, when the platform can tell us. Used to
    /// explain a permission failure ("endpoint belongs to uid 0, you are
    /// uid 1000") — the operating system already made the decision, we only
    /// report it. A TCP endpoint has no owner (task 1.4): admission is the
    /// Bearer credential, not a filesystem identity.
    pub fn owner_uid(&self) -> Option<u32> {
        match &self.inner {
            Inner::Ipc(e) => e.owner_uid(),
            Inner::Tcp(_) => None,
        }
    }

    /// Create the directories, clear a stale endpoint, and start listening —
    /// IPC shape. The TCP shape binds the address; there is no filesystem
    /// and nothing stale to clean (task 1.3).
    pub fn bind(&self) -> Result<Listener, BindError> {
        match &self.inner {
            Inner::Ipc(e) => e.bind().map(|l| Listener {
                inner: ListenerInner::Ipc(l),
            }),
            Inner::Tcp(t) => tcp_bind(t).map(|l| Listener {
                inner: ListenerInner::Tcp(l),
            }),
        }
    }
}

/// Parse the `host:port` after the `tcp://` prefix (task 1.1): host non-empty,
/// port an integer in 1–65535. Errors name the defect so a rejected startup
/// says what to fix.
fn parse_tcp(rest: &str) -> Result<TcpAddr, EndpointError> {
    // A trailing "/" is URL furniture, not part of host:port (the control
    // plane has no URL path); anything longer than that is a typo and will
    // fail the port parse with a precise message.
    let rest = rest.trim_end_matches('/');
    let Some((host, port_raw)) = rest.rsplit_once(':') else {
        return Err(EndpointError::Tcp(format!(
            "tcp:// endpoint must be host:port, got {rest:?} (missing ':port')"
        )));
    };
    if host.is_empty() {
        return Err(EndpointError::Tcp(format!(
            "tcp:// endpoint host must not be empty, got {rest:?}"
        )));
    }
    // A colon-bearing host without brackets is a bare IPv6 literal; std's
    // resolver cannot dial it, so reject at parse time with the bracket form.
    if !host.starts_with('[') && host.contains(':') {
        return Err(EndpointError::Tcp(format!(
            "tcp:// endpoint IPv6 host must be bracketed, got {rest:?} \
             (write \"[{host}]:{port_raw}\")"
        )));
    }
    let port_err = || {
        EndpointError::Tcp(format!(
            "tcp:// endpoint port must be an integer in 1..=65535, got {port_raw:?}"
        ))
    };
    let port: u16 = port_raw.trim().parse().map_err(|_| port_err())?;
    if port == 0 {
        return Err(port_err());
    }
    Ok(TcpAddr {
        host: host.to_string(),
        port,
    })
}

/// Bind a TCP control endpoint (task 1.3): there is no filesystem and no
/// stale-endpoint cleanup — an `AddrInUse` conflict is decided by probing
/// whether the occupant answers (`AlreadyRunning` when it does); every other
/// failure is the real bind error, reported as-is.
fn tcp_bind(t: &TcpAddr) -> Result<std::net::TcpListener, BindError> {
    let addr = format!("{}:{}", t.host, t.port);
    match std::net::TcpListener::bind(&addr) {
        Ok(l) => Ok(l),
        Err(e) if e.kind() == io::ErrorKind::AddrInUse => {
            match std::net::TcpStream::connect(&addr) {
                // Something answers on the address another listener holds:
                // a live daemon, not ours to disturb.
                Ok(_) => Err(BindError::AlreadyRunning),
                // Occupied but not answering: there is no stale file to
                // clear on TCP, so the bind error stands.
                Err(_) => Err(BindError::Io(e)),
            }
        }
        Err(e) => Err(BindError::Io(e)),
    }
}

/// A bound endpoint ready to hand out connections.
pub struct Listener {
    inner: ListenerInner,
}

enum ListenerInner {
    Ipc(platform::Listener),
    Tcp(std::net::TcpListener),
}

impl Listener {
    /// Block until one client connects.
    pub fn accept(&mut self) -> io::Result<Box<dyn Conn>> {
        match &mut self.inner {
            ListenerInner::Ipc(l) => l.accept().map(|c| Box::new(c) as Box<dyn Conn>),
            ListenerInner::Tcp(l) => l
                .accept()
                .map(|(s, _)| Box::new(TcpConn(s)) as Box<dyn Conn>),
        }
    }
}

/// A TCP connection of the control plane (cross-platform std — no platform
/// branches; the DACL/permission work is IPC-only, design: TCP details live
/// in this module alone). Read/write timeout semantics match the UDS
/// connection (task 1.2).
pub struct TcpConn(std::net::TcpStream);

impl Read for TcpConn {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.0.read(buf)
    }
}

impl Write for TcpConn {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.write(buf)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.0.flush()
    }
}

impl Conn for TcpConn {
    fn try_clone_conn(&self) -> io::Result<Box<dyn Conn>> {
        Ok(Box::new(TcpConn(self.0.try_clone()?)))
    }
    fn set_read_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        self.0.set_read_timeout(timeout)
    }
    fn set_write_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        self.0.set_write_timeout(timeout)
    }
}

/// Human-readable identity of the current process, for diagnostics that need
/// to name a user rather than a numeric id.
pub fn current_user() -> String {
    platform::current_user()
}

/// The current uid, where the platform has one (always `Some` on unix).
pub fn current_uid() -> Option<u32> {
    platform::current_uid()
}

/// The DACL the Windows daemon installs on its control pipe, as SDDL
/// (design D3, task 1.4).
///
/// Named pipes are created world-accessible by default, so an implicit DACL
/// would hand every local account full control — which would make D1 ("the
/// OS answers who may control the daemon") a no-op on Windows. The ACE set is
/// therefore spelled out: SYSTEM (`SY`), Administrators (`BA`) and the
/// running account (its SID), and nothing else. Everyone (`WD`), Users (`BU`)
/// and Authenticated Users (`AU`) are deliberately absent — granting them is
/// exactly what the default descriptor does.
///
/// Pure text, and compiled on every platform (still inside this module, per
/// D9) so the subject set is assertable without a Windows host; the
/// conversion to a real security descriptor happens in the `windows` platform
/// module.
#[cfg(any(windows, test))]
pub fn control_pipe_sddl(current_sid: &str) -> String {
    format!("D:(A;;GA;;;SY)(A;;GA;;;BA)(A;;GA;;;{current_sid})")
}

#[cfg(unix)]
mod platform {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::net::{UnixListener, UnixStream};

    /// `sockaddr_un.sun_path` is 108 bytes including the NUL terminator, so a
    /// path longer than 107 bytes cannot be bound at all.
    const MAX_PATH_BYTES: usize = 107;

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct Endpoint {
        path: PathBuf,
    }

    impl From<Endpoint> for PathBuf {
        fn from(e: Endpoint) -> PathBuf {
            e.path
        }
    }

    impl Endpoint {
        pub fn parse(value: &str) -> Result<Self, EndpointError> {
            let len = value.len();
            if len > MAX_PATH_BYTES {
                return Err(EndpointError::TooLong {
                    len,
                    max: MAX_PATH_BYTES,
                });
            }
            Ok(Endpoint {
                path: PathBuf::from(value),
            })
        }

        pub fn default_path() -> PathBuf {
            PathBuf::from(DEFAULT_ROOT)
                .join(current_uid().unwrap_or(0).to_string())
                .join(DEFAULT_NAME)
        }

        pub fn default_endpoint() -> Self {
            Endpoint {
                path: Endpoint::default_path(),
            }
        }

        pub fn display(&self) -> String {
            self.path.display().to_string()
        }

        fn path(&self) -> &Path {
            &self.path
        }

        pub fn connect(&self) -> io::Result<UnixConn> {
            Ok(UnixConn(UnixStream::connect(&self.path)?))
        }

        pub fn owner_uid(&self) -> Option<u32> {
            use std::os::unix::fs::MetadataExt;
            std::fs::symlink_metadata(self.path()).ok().map(|m| m.uid())
        }

        /// Is this directory one of ours (the per-uid private directory)?
        /// A configured endpoint elsewhere in the filesystem never has its
        /// parents chmod'ed — the daemon does not get to re-permission
        /// directories the operator chose.
        pub fn is_private_uid_dir(dir: &Path) -> bool {
            dir.file_name()
                .and_then(|n| n.to_str())
                .and_then(|n| n.parse::<u32>().ok())
                == current_uid()
        }

        fn ensure_dirs(path: &Path) -> io::Result<()> {
            let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) else {
                return Ok(());
            };
            // Create every missing ancestor at 0755 (the shared root,
            // /run/appkeeper included — systemd's RuntimeDirectory is a
            // convenience, not a prerequisite, see task 7.2).
            let mut missing: Vec<&Path> = Vec::new();
            let mut cur = Some(parent);
            while let Some(dir) = cur {
                if dir.exists() {
                    break;
                }
                missing.push(dir);
                cur = dir.parent().filter(|p| !p.as_os_str().is_empty());
            }
            for dir in missing.iter().rev() {
                std::fs::create_dir(dir).map_err(|e| {
                    io::Error::new(
                        e.kind(),
                        format!("cannot create directory {}: {e}", dir.display()),
                    )
                })?;
                tighten(dir, ROOT_DIR_MODE);
            }
            // The per-uid directory is private: create it 0700 and tighten an
            // existing too-permissive one.
            if Endpoint::is_private_uid_dir(parent) {
                tighten(parent, UID_DIR_MODE);
            }
            Ok(())
        }

        fn chmod_mode(path: &Path, mode: u32) -> io::Result<()> {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).map_err(|e| {
                io::Error::new(
                    e.kind(),
                    format!("cannot set mode {:o} on {}: {e}", mode, path.display()),
                )
            })
        }

        /// Is this path an IPC endpoint (a socket), rather than an arbitrary
    /// file that happens to sit where the endpoint goes? Checked before any
    /// removal — the daemon must never delete something it did not create.
    fn is_endpoint_file(meta: &std::fs::Metadata) -> bool {
        use std::os::unix::fs::FileTypeExt;
        meta.file_type().is_socket()
    }

    /// Create (or clear) the endpoint and return a listener.
    pub fn bind(&self) -> Result<Listener, BindError> {
            let path = self.path().to_path_buf();
            Endpoint::ensure_dirs(&path)?;
            if path.symlink_metadata().is_ok() {
                match self.connect() {
                    // Someone answers → a live instance owns this endpoint.
                    Ok(_) => return Err(BindError::AlreadyRunning),
                    // We may not even try to connect → a live instance owned
                    // by another account. Never delete it.
                    Err(e) if e.kind() == io::ErrorKind::PermissionDenied => {
                        return Err(BindError::AlreadyRunning)
                    }
                    // Refused / not a socket → leftover from a crash; make
                    // sure before removing anything.
                    Err(_) => {
                        let meta = std::fs::symlink_metadata(&path)?;
                        if !Endpoint::is_endpoint_file(&meta) {
                            return Err(BindError::PathOccupied);
                        }
                        std::fs::remove_file(&path)?;
                    }
                }
            }
            let listener = UnixListener::bind(&path)?;
            // Narrow the socket after bind: the initial mode is 0777 & ~umask
            // and the window between bind and chmod is exactly the window an
            // attacker would use.
            Endpoint::chmod_mode(&path, SOCKET_MODE)?;
            Ok(Listener { inner: listener })
        }
    }

    /// Set mode only when it differs — keeps the common path cheap and the
    /// error message meaningful.
    fn tighten(dir: &Path, mode: u32) {
        let already =
            std::fs::metadata(dir).is_ok_and(|meta| meta.permissions().mode() & 0o7777 == mode);
        if !already
            && let Err(e) = Endpoint::chmod_mode(dir, mode)
        {
            log::warn!("{e}");
        }
    }

    pub struct UnixConn(UnixStream);

    impl Read for UnixConn {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            self.0.read(buf)
        }
    }

    impl Write for UnixConn {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0.write(buf)
        }
        fn flush(&mut self) -> io::Result<()> {
            self.0.flush()
        }
    }

    impl Conn for UnixConn {
        fn try_clone_conn(&self) -> io::Result<Box<dyn Conn>> {
            Ok(Box::new(UnixConn(self.0.try_clone()?)))
        }
        fn set_read_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
            self.0.set_read_timeout(timeout)
        }
        fn set_write_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
            self.0.set_write_timeout(timeout)
        }
    }

    pub struct Listener {
        inner: UnixListener,
    }

    impl Listener {
        pub fn accept(&mut self) -> io::Result<UnixConn> {
            Ok(UnixConn(self.inner.accept()?.0))
        }
    }

    pub fn current_user() -> String {
        current_uid()
            .map(|u| u.to_string())
            .unwrap_or_else(|| "unknown".to_string())
    }

    pub fn current_uid() -> Option<u32> {
        Some(unsafe { libc::geteuid() })
    }
}

#[cfg(windows)]
mod platform {
    use super::*;
    use std::cell::Cell;
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::{
        CloseHandle, DUPLICATE_SAME_ACCESS, DuplicateHandle, ERROR_ACCESS_DENIED,
        ERROR_FILE_NOT_FOUND, ERROR_IO_PENDING, ERROR_PIPE_BUSY, HANDLE, INVALID_HANDLE_VALUE,
        WAIT_OBJECT_0,
    };
    use windows_sys::Win32::Security::Authorization::{
        ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
    };
    use windows_sys::Win32::Security::{
        GetTokenInformation, SECURITY_ATTRIBUTES, TokenUser, TOKEN_QUERY, TOKEN_USER,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, ReadFile, WriteFile, FILE_FLAG_OVERLAPPED, FILE_SHARE_READ, FILE_SHARE_WRITE,
        OPEN_EXISTING, PIPE_ACCESS_DUPLEX,
    };
    use windows_sys::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};
    use windows_sys::Win32::System::Pipes::{
        ConnectNamedPipe, CreateNamedPipeW, PIPE_READMODE_MESSAGE, PIPE_TYPE_MESSAGE, PIPE_WAIT,
    };
    use windows_sys::Win32::System::Threading::{
        CreateEventW, GetCurrentProcess, INFINITE, OpenProcessToken, WaitForSingleObject,
    };

    /// `CreateFileW` on a pipe name is bounded by `MAX_PATH`.
    const MAX_PATH_BYTES: usize = 260;
    const GENERIC_READ_WRITE: u32 = 0x8000_0000 | 0x4000_0000;

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct Endpoint {
        name: String,
    }

    impl From<Endpoint> for PathBuf {
        fn from(e: Endpoint) -> PathBuf {
            PathBuf::from(e.name)
        }
    }

    impl Endpoint {
        pub fn parse(value: &str) -> Result<Self, EndpointError> {
            let len = value.len();
            if len > MAX_PATH_BYTES {
                return Err(EndpointError::TooLong {
                    len,
                    max: MAX_PATH_BYTES,
                });
            }
            Ok(Endpoint {
                name: value.to_string(),
            })
        }

        pub fn default_path() -> PathBuf {
            PathBuf::from(Endpoint::default_name())
        }

        /// The user name goes into the pipe name: two accounts on one
        /// machine must never collide on — or be able to reach — each
        /// other's control pipe (design D5).
        pub fn default_name() -> String {
            format!(r"\\.\pipe\ak-{}.sock", sanitize(&current_user()))
        }

        pub fn default_endpoint() -> Self {
            Endpoint {
                name: Endpoint::default_name(),
            }
        }

        pub fn display(&self) -> String {
            self.name.clone()
        }

        pub fn connect(&self) -> io::Result<PipeConn> {
            let wide = wide_null(&self.name);
            let handle = unsafe {
                CreateFileW(
                    wide.as_ptr(),
                    GENERIC_READ_WRITE,
                    FILE_SHARE_READ | FILE_SHARE_WRITE,
                    std::ptr::null(),
                    OPEN_EXISTING,
                    FILE_FLAG_OVERLAPPED,
                    std::ptr::null_mut(),
                )
            };
            if handle != INVALID_HANDLE_VALUE {
                return Ok(PipeConn::new(handle));
            }
            let err = io::Error::last_os_error();
            let raw = err.raw_os_error().unwrap_or(0) as u32;
            // A busy pipe (every instance taken) still means "it exists".
            if raw == ERROR_PIPE_BUSY || raw == ERROR_ACCESS_DENIED {
                return Err(io::Error::from_raw_os_error(ERROR_ACCESS_DENIED as i32));
            }
            if raw == ERROR_FILE_NOT_FOUND {
                return Err(io::Error::from_raw_os_error(
                    ERROR_FILE_NOT_FOUND as i32,
                ));
            }
            Err(err)
        }

        pub fn owner_uid(&self) -> Option<u32> {
            // Windows has no uid; the DACL is the identity (D1 / D3).
            None
        }

        pub fn bind(&self) -> Result<Listener, BindError> {
            // "The path exists" on Windows == "a pipe of this name answers".
            match self.connect() {
                Ok(_) => return Err(BindError::AlreadyRunning),
                Err(e) if e.kind() == io::ErrorKind::PermissionDenied => {
                    return Err(BindError::AlreadyRunning)
                }
                Err(_) => {}
            }
            // Pipe instances die with the last handle to them, so there is
            // never a stale endpoint to clear and never a non-endpoint file
            // to refuse: CreateNamedPipeW either yields our name or fails.
            Listener::create(&self.name)
        }
    }

    /// Strip characters that are not legal in a pipe name.
    fn sanitize(user: &str) -> String {
        let cleaned: String = user
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        if cleaned.is_empty() {
            "user".to_string()
        } else {
            cleaned
        }
    }

    fn wide_null(s: &str) -> Vec<u16> {
        std::ffi::OsStr::new(s)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    }

    /// SID string of the account this process runs as.
    pub fn current_user_sid() -> Option<String> {
        unsafe {
            let mut token: HANDLE = std::ptr::null_mut();
            if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
                return None;
            }
            let mut needed: u32 = 0;
            let mut buf: Vec<u8> = Vec::new();
            let probed = GetTokenInformation(token, TokenUser, std::ptr::null_mut(), 0, &mut needed);
            let ok = if probed == 0 && needed > 0 {
                buf.resize(needed as usize, 0);
                GetTokenInformation(
                    token,
                    TokenUser,
                    buf.as_mut_ptr().cast(),
                    needed,
                    &mut needed,
                ) != 0
            } else {
                false
            };
            let _ = CloseHandle(token);
            if !ok {
                return None;
            }
            let user = buf.as_ptr().cast::<TOKEN_USER>();
            let mut sid: *mut u16 = std::ptr::null_mut();
            if ConvertSidToStringSidW((*user).User.Sid, &mut sid) == 0 || sid.is_null() {
                return None;
            }
            let mut len = 0usize;
            while *sid.add(len) != 0 {
                len += 1;
            }
            let text = String::from_utf16_lossy(std::slice::from_raw_parts(sid, len));
            let _ = windows_sys::Win32::Foundation::LocalFree(sid.cast());
            Some(text)
        }
    }

    pub fn current_user() -> String {
        std::env::var("USERNAME")
            .or_else(|_| std::env::var("USER"))
            .unwrap_or_else(|_| "user".to_string())
    }

    pub fn current_uid() -> Option<u32> {
        None
    }

    /// Wait for one overlapped operation, bounded by `timeout`.
    ///
    /// A byte-stream HANDLE has no `SetFileTimeouts`, so the deadline rides
    /// on an OVERLAPPED + event per operation. Without this a listener that
    /// accepts and never answers would hang the readiness poll (and the
    /// connection thread) forever.
    fn run_overlapped(
        handle: HANDLE,
        buf: *mut u8,
        len: usize,
        write: bool,
        timeout: Option<Duration>,
    ) -> io::Result<usize> {
        unsafe {
            let event = CreateEventW(std::ptr::null(), 1, 0, std::ptr::null());
            if event.is_null() {
                return Err(io::Error::last_os_error());
            }
            let mut ov: OVERLAPPED = std::mem::zeroed();
            ov.hEvent = event;
            let mut transferred: u32 = 0;
            let started = if write {
                WriteFile(handle, buf, len as u32, &mut transferred, &mut ov)
            } else {
                ReadFile(handle, buf, len as u32, &mut transferred, &mut ov)
            };
            if started == 0 {
                let err = io::Error::last_os_error();
                if err.raw_os_error() != Some(ERROR_IO_PENDING as i32) {
                    let _ = CloseHandle(event);
                    return Err(err);
                }
            }
            let millis = match timeout {
                Some(d) => d.as_millis().min(u32::MAX as u128) as u32,
                None => INFINITE,
            };
            if WaitForSingleObject(event, millis) != WAIT_OBJECT_0 {
                let _ = CancelIoEx(handle, &ov);
                let _ = CloseHandle(event);
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "named pipe operation timed out",
                ));
            }
            let mut n: u32 = 0;
            let ok = GetOverlappedResult(handle, &ov, &mut n, 0);
            let _ = CloseHandle(event);
            if ok == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(n as usize)
        }
    }

    pub struct PipeConn {
        handle: HANDLE,
        read_timeout: Cell<Option<Duration>>,
        write_timeout: Cell<Option<Duration>>,
    }

    // SAFETY: the handle is owned exclusively by this struct and only ever
    // used by the thread holding it; `Conn` requires `Send`, not `Sync`.
    unsafe impl Send for PipeConn {}

    impl PipeConn {
        pub fn new(handle: HANDLE) -> Self {
            PipeConn {
                handle,
                read_timeout: Cell::new(None),
                write_timeout: Cell::new(None),
            }
        }

        fn duplicate(&self) -> io::Result<PipeConn> {
            unsafe {
                let mut dup: HANDLE = std::ptr::null_mut();
                if DuplicateHandle(
                    GetCurrentProcess(),
                    self.handle,
                    GetCurrentProcess(),
                    &mut dup,
                    0,
                    0,
                    DUPLICATE_SAME_ACCESS,
                ) == 0
                {
                    return Err(io::Error::last_os_error());
                }
                Ok(PipeConn::new(dup))
            }
        }
    }

    impl Drop for PipeConn {
        fn drop(&mut self) {
            unsafe {
                let _ = CloseHandle(self.handle);
            }
        }
    }

    impl Read for PipeConn {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            if buf.is_empty() {
                return Ok(0);
            }
            let timeout = self.read_timeout.get();
            run_overlapped(self.handle, buf.as_mut_ptr(), buf.len(), false, timeout)
        }
    }

    impl Write for PipeConn {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            if buf.is_empty() {
                return Ok(0);
            }
            let timeout = self.write_timeout.get();
            run_overlapped(
                self.handle,
                buf.as_ptr() as *mut u8,
                buf.len(),
                true,
                timeout,
            )
        }
        fn flush(&mut self) -> io::Result<()> {
            // A pipe write with an event has already been flushed by the
            // kernel when the event fires; there is no userspace buffer.
            Ok(())
        }
    }

    impl Conn for PipeConn {
        fn try_clone_conn(&self) -> io::Result<Box<dyn Conn>> {
            Ok(Box::new(self.duplicate()?))
        }
        fn set_read_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
            self.read_timeout.set(timeout);
            Ok(())
        }
        fn set_write_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
            self.write_timeout.set(timeout);
            Ok(())
        }
    }

    pub struct Listener {
        name: String,
        /// The next instance waiting for a client. Pre-created so an
        /// incoming client never races the accept loop.
        next: HANDLE,
    }

    // SAFETY: `accept` needs `&mut self`, so the raw handle is touched by
    // one thread at a time.
    unsafe impl Send for Listener {}

    impl Listener {
        fn create_instance(name: &str) -> Result<HANDLE, BindError> {
            // SDDL strings are wide: build the UTF-16 form up front. Without
            // the owner SID we refuse rather than create a pipe with the
            // default (world-accessible) DACL.
            let Some(sid) = current_user_sid() else {
                return Err(BindError::Io(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "cannot determine the SID of the running account; refusing to create the \
                     control pipe with the default (world-accessible) DACL",
                )));
            };
            let sddl_wide = wide_null(&super::control_pipe_sddl(&sid));
            let mut sd: *mut std::ffi::c_void = std::ptr::null_mut();
            let mut sd_len: u32 = 0;
            unsafe {
                if ConvertStringSecurityDescriptorToSecurityDescriptorW(
                    sddl_wide.as_ptr(),
                    1, // SDDL_REVISION_1
                    &mut sd,
                    &mut sd_len,
                ) == 0
                {
                    return Err(BindError::Io(io::Error::last_os_error()));
                }
            }
            // The kernel copies the descriptor during CreateNamedPipeW, so
            // the buffer only has to outlive this call.
            let attributes = SECURITY_ATTRIBUTES {
                nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
                lpSecurityDescriptor: sd,
                bInheritHandle: 0,
            };
            let wide = wide_null(name);
            let handle = unsafe {
                CreateNamedPipeW(
                    wide.as_ptr(),
                    PIPE_ACCESS_DUPLEX | FILE_FLAG_OVERLAPPED,
                    PIPE_TYPE_MESSAGE | PIPE_READMODE_MESSAGE | PIPE_WAIT,
                    // 255 instances: one thread per connection, and the
                    // control plane is a single-account surface.
                    255,
                    64 * 1024,
                    64 * 1024,
                    0,
                    &attributes,
                )
            };
            unsafe {
                let _ = windows_sys::Win32::Foundation::LocalFree(sd.cast());
            }
            if handle == INVALID_HANDLE_VALUE {
                return Err(BindError::Io(io::Error::last_os_error()));
            }
            Ok(handle)
        }

        pub fn create(name: &str) -> Result<Self, BindError> {
            Ok(Listener {
                name: name.to_string(),
                next: Listener::create_instance(name)?,
            })
        }

        pub fn accept(&mut self) -> io::Result<PipeConn> {
            unsafe {
                let event = CreateEventW(std::ptr::null(), 1, 0, std::ptr::null());
                if event.is_null() {
                    return Err(io::Error::last_os_error());
                }
                let mut ov: OVERLAPPED = std::mem::zeroed();
                ov.hEvent = event;
                let ok = ConnectNamedPipe(self.next, &mut ov);
                if ok == 0 {
                    let err = io::Error::last_os_error();
                    let raw = err.raw_os_error().unwrap_or(0) as u32;
                    // A client that connected between CreateNamedPipeW and
                    // ConnectNamedPipe leaves ERROR_PIPE_CONNECTED behind;
                    // that is a success, not a failure.
                    if raw == 535 {
                        let _ = CloseHandle(event);
                        return Ok(self.take_next());
                    }
                    if raw != ERROR_IO_PENDING {
                        let _ = CloseHandle(event);
                        let _ = CloseHandle(self.next);
                        return Err(err);
                    }
                    if WaitForSingleObject(event, INFINITE) != WAIT_OBJECT_0 {
                        let _ = CancelIoEx(self.next, &ov);
                        let _ = CloseHandle(event);
                        let _ = CloseHandle(self.next);
                        return Err(io::Error::new(
                            io::ErrorKind::TimedOut,
                            "waiting for a control-pipe client timed out",
                        ));
                    }
                }
                let _ = CloseHandle(event);
                Ok(self.take_next())
            }
        }

        /// Hand the connected instance out and arm the next one.
        fn take_next(&mut self) -> PipeConn {
            let connected = self.next;
            // Pre-create the next instance; a failure only means the next
            // accept has to retry, which the loop reports as an accept error.
            if let Ok(h) = Listener::create_instance(&self.name) {
                self.next = h;
            }
            PipeConn::new(connected)
        }
    }

    impl Drop for Listener {
        fn drop(&mut self) {
            unsafe {
                let _ = CloseHandle(self.next);
            }
        }
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ak-test-transport-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// Task 1.5: the default endpoint carries the current uid so two users
    /// on one machine never share an endpoint.
    #[test]
    fn default_endpoint_is_uid_layered() {
        let ep = Endpoint::default_endpoint();
        let uid = current_uid().expect("unix has a uid");
        #[cfg(unix)]
        {
            assert_eq!(ep.display(), format!("{DEFAULT_ROOT}/{uid}/{DEFAULT_NAME}"));
        }
        #[cfg(windows)]
        {
            let name = ep.display();
            assert!(name.starts_with(r"\\.\pipe\ak-"), "{name}");
            assert!(name.ends_with(".sock"), "{name}");
            assert!(name.contains(&current_user()), "{name} carries the user name");
        }
    }

    /// An explicitly configured endpoint overrides the default.
    #[test]
    fn configured_endpoint_overrides_the_default() {
        let d = tmp_dir("override");
        let p = d.join("custom.sock");
        let ep = Endpoint::parse(&p.display().to_string()).unwrap();
        assert_eq!(ep.display(), p.display().to_string());
        assert_ne!(ep.display(), Endpoint::default_endpoint().display());
        let _ = std::fs::remove_dir_all(&d);
    }

    /// Task 1.5 / design risk: an endpoint longer than the platform limit is
    /// a recognizable error, never a silent truncation.
    #[test]
    fn overlong_endpoint_is_rejected() {
        let long = format!("/tmp/{}/ak.sock", "x".repeat(200));
        match Endpoint::parse(&long) {
            Err(EndpointError::TooLong { len, max }) => {
                assert!(len > max);
            }
            other => panic!("expected TooLong, got {other:?}"),
        }
    }

    #[test]
    fn empty_endpoint_is_rejected() {
        assert_eq!(Endpoint::parse(""), Err(EndpointError::Empty));
        assert_eq!(Endpoint::parse("   "), Err(EndpointError::Empty));
    }

    /// Task 1.1: a bound endpoint hands out connections that read and write
    /// in both directions.
    #[test]
    fn bind_then_connect_round_trips() {
        let d = tmp_dir("roundtrip");
        let ep = Endpoint::parse(&d.join("ak.sock").display().to_string()).unwrap();
        let mut listener = ep.bind().unwrap();
        let server = std::thread::spawn(move || {
            let mut conn = listener.accept().unwrap();
            let mut buf = [0u8; 5];
            conn.read_exact(&mut buf).unwrap();
            conn.write_all(b"pong").unwrap();
            conn.flush().unwrap();
            // The writer half is an independent clone of the same connection.
            let mut writer = conn.try_clone_conn().unwrap();
            writer.write_all(b"!").unwrap();
            writer.flush().unwrap();
            buf
        });
        let mut conn = ep.connect().unwrap();
        conn.write_all(b"hello").unwrap();
        conn.flush().unwrap();
        // Both write halves landed: the server's own writer and the clone it
        // made of the same connection.
        let mut buf = [0u8; 5];
        conn.read_exact(&mut buf).unwrap();
        assert_eq!(std::str::from_utf8(&buf).unwrap(), "pong!");
        assert_eq!(server.join().unwrap(), *b"hello");
        let _ = std::fs::remove_dir_all(&d);
    }

    /// Task 1.3 (first branch): binding an endpoint another live listener
    /// serves reports `AlreadyRunning` and leaves the endpoint alone.
    #[test]
    fn bind_reports_already_running() {
        let d = tmp_dir("busy");
        let ep = Endpoint::parse(&d.join("ak.sock").display().to_string()).unwrap();
        let _first = ep.bind().unwrap();
        match ep.bind() {
            Err(BindError::AlreadyRunning) => {}
            Err(other) => panic!("expected AlreadyRunning, got {other}"),
            Ok(_) => panic!("a live endpoint must not be re-bindable"),
        }
        assert!(d.join("ak.sock").exists(), "the live endpoint is untouched");
        let _ = std::fs::remove_dir_all(&d);
    }

    /// Task 1.3 (second branch): a leftover socket with no listener is stale
    /// and gets removed so the daemon can start.
    #[cfg(unix)]
    #[test]
    fn stale_endpoint_is_cleared_and_rebound() {
        let d = tmp_dir("stale");
        let path = d.join("ak.sock");
        // Bind then drop the listener: the socket file survives, nothing
        // answers on it — exactly a crashed daemon's residue.
        {
            let ep = Endpoint::parse(&path.display().to_string()).unwrap();
            let l = ep.bind().unwrap();
            drop(l);
        }
        assert!(path.exists(), "the crashed daemon left its endpoint behind");
        let ep = Endpoint::parse(&path.display().to_string()).unwrap();
        let listener = ep.bind().expect("a stale endpoint must be cleaned up");
        let conn = ep.connect().expect("the rebound endpoint answers");
        drop(conn);
        drop(listener);
        let _ = std::fs::remove_dir_all(&d);
    }

    /// Task 1.3 (third branch): a regular file at the endpoint path is never
    /// deleted — the daemon refuses to start and says why.
    #[test]
    fn non_endpoint_path_is_never_deleted() {
        let d = tmp_dir("occupied");
        let path = d.join("ak.sock");
        std::fs::write(&path, b"not a socket").unwrap();
        let ep = Endpoint::parse(&path.display().to_string()).unwrap();
        match ep.bind() {
            Err(BindError::PathOccupied) => {}
            Err(other) => panic!("expected PathOccupied, got {other}"),
            Ok(_) => panic!("a non-socket path must not be bindable"),
        }
        assert!(path.exists(), "a non-endpoint file must survive");
        assert_eq!(std::fs::read(&path).unwrap(), b"not a socket");
        let _ = std::fs::remove_dir_all(&d);
    }

    /// Task 1.2: directories are created when missing, and the socket itself
    /// is 0600 — that pair is what makes the kernel refuse other accounts.
    #[cfg(unix)]
    #[test]
    fn bind_creates_dirs_and_narrows_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let d = tmp_dir("perms");
        // Nested, missing: the walk has to create both levels.
        let root = d.join("run");
        let uid_dir = root.join(current_uid().unwrap().to_string());
        let path = uid_dir.join(DEFAULT_NAME);
        let ep = Endpoint::parse(&path.display().to_string()).unwrap();
        let listener = ep.bind().unwrap();
        let meta = std::fs::metadata(&path).unwrap();
        assert_eq!(
            meta.permissions().mode() & 0o777,
            SOCKET_MODE,
            "the control endpoint is owner-only"
        );
        assert_eq!(
            std::fs::metadata(&uid_dir).unwrap().permissions().mode() & 0o777,
            UID_DIR_MODE,
            "the per-uid directory is private"
        );
        drop(listener);
        let _ = std::fs::remove_dir_all(&d);
    }

    /// Task 1.2 (third case): an existing but too-permissive uid directory is
    /// tightened rather than trusted.
    #[cfg(unix)]
    #[test]
    fn bind_tightens_an_over_permissive_uid_dir() {
        use std::os::unix::fs::PermissionsExt;
        let d = tmp_dir("loose");
        let root = d.join("run");
        let uid_dir = root.join(current_uid().unwrap().to_string());
        std::fs::create_dir_all(&uid_dir).unwrap();
        std::fs::set_permissions(&uid_dir, std::fs::Permissions::from_mode(0o777)).unwrap();
        let path = uid_dir.join(DEFAULT_NAME);
        let ep = Endpoint::parse(&path.display().to_string()).unwrap();
        let listener = ep.bind().unwrap();
        assert_eq!(
            std::fs::metadata(&uid_dir).unwrap().permissions().mode() & 0o777,
            UID_DIR_MODE
        );
        drop(listener);
        let _ = std::fs::remove_dir_all(&d);
    }

    /// A configured endpoint outside our own namespace never has its parent
    /// re-permissioned — the daemon does not touch operator-owned dirs.
    #[cfg(unix)]
    #[test]
    fn foreign_parent_directory_is_left_alone() {
        use std::os::unix::fs::PermissionsExt;
        let d = tmp_dir("foreign");
        std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o777)).unwrap();
        let path = d.join("ak.sock");
        let ep = Endpoint::parse(&path.display().to_string()).unwrap();
        let listener = ep.bind().unwrap();
        assert_eq!(
            std::fs::metadata(&d).unwrap().permissions().mode() & 0o777,
            0o777,
            "an operator-chosen directory keeps its mode"
        );
        drop(listener);
        let _ = std::fs::remove_dir_all(&d);
    }

    /// 7.2: with no `RuntimeDirectory=appkeeper` in the unit (an older
    /// install), the daemon creates the whole chain itself.
    #[cfg(unix)]
    #[test]
    fn missing_root_directory_is_created_by_the_daemon() {
        use std::os::unix::fs::PermissionsExt;
        let d = tmp_dir("self-create");
        let root = d.join("run").join("appkeeper");
        let uid_dir = root.join(current_uid().unwrap().to_string());
        let path = uid_dir.join(DEFAULT_NAME);
        assert!(!root.exists(), "precondition: nothing exists yet");
        let ep = Endpoint::parse(&path.display().to_string()).unwrap();
        let listener = ep.bind().unwrap();
        assert_eq!(
            std::fs::metadata(&root).unwrap().permissions().mode() & 0o777,
            ROOT_DIR_MODE,
            "the shared root is traversable, not writable"
        );
        assert_eq!(
            std::fs::metadata(&uid_dir).unwrap().permissions().mode() & 0o777,
            UID_DIR_MODE
        );
        assert!(path.exists(), "the endpoint itself was created");
        drop(listener);
        let _ = std::fs::remove_dir_all(&d);
    }

    /// 7.3: a `User=<name>` unit derives the endpoint from the RUNNING
    /// account's uid, not from whoever installed the unit.
    #[cfg(unix)]
    #[test]
    fn endpoint_follows_the_running_account_uid() {
        // The derivation is per-process (the running account), so the
        // assertion is that the uid layer matches this process's uid and
        // that changing the parent path keeps the uid layer intact.
        let uid = current_uid().unwrap();
        let ep = Endpoint::default_endpoint();
        assert_eq!(ep.display(), format!("{DEFAULT_ROOT}/{uid}/{DEFAULT_NAME}"));
        assert!(
            Path::new(&ep.display())
                .parent()
                .and_then(|p| p.file_name())
                .and_then(|n| n.to_str())
                .map(|n| n.parse::<u32>() == Ok(uid))
                .unwrap_or(false),
            "the uid directory names the running account"
        );
    }

    /// The per-uid directory recognition is what keeps `/tmp` from being
    /// chmod'ed into a private dir when an operator points `socket` there.
    #[test]
    fn uid_dir_recognition_matches_only_numeric_our_uid() {
        #[cfg(unix)]
        {
            let d = tmp_dir("recognise");
            assert!(platform::Endpoint::is_private_uid_dir(
                &PathBuf::from("/run/appkeeper").join(current_uid().unwrap().to_string())
            ));
            assert!(!platform::Endpoint::is_private_uid_dir(&d));
            assert!(!platform::Endpoint::is_private_uid_dir(Path::new("/tmp")));
            let _ = std::fs::remove_dir_all(&d);
        }
    }

    /// Task 1.4: the control pipe's DACL names exactly SYSTEM,
    /// Administrators and the running account — never Everyone, Users or
    /// Authenticated Users. The SDDL text is platform-independent, so the
    /// subject set is asserted on every host; only the conversion into a real
    /// security descriptor needs Windows (verified on `windows-latest` CI).
    #[test]
    fn control_pipe_dacl_subjects_are_exactly_sy_ba_and_running_account() {
        let sid = "S-1-5-21-3623811015-3361044348-30300820-1013";
        let sddl = control_pipe_sddl(sid);
        // Exactly three allow ACEs, each granting generic-all (GA).
        assert_eq!(sddl, format!("D:(A;;GA;;;SY)(A;;GA;;;BA)(A;;GA;;;{sid})"));
        // The owner set: SYSTEM, Administrators, running account.
        assert!(sddl.contains("(A;;GA;;;SY)"), "SYSTEM must be granted: {sddl}");
        assert!(
            sddl.contains("(A;;GA;;;BA)"),
            "Administrators must be granted: {sddl}"
        );
        assert!(
            sddl.contains(&format!("(A;;GA;;;{sid})")),
            "the running account must be granted: {sddl}"
        );
        // The deliberately absent subjects: Everyone (WD), Users (BU),
        // Authenticated Users (AU). A well-formed SID never spells those
        // tokens, so substring absence is a faithful subject-set assertion.
        for absent in ["WD", "BU", "AU"] {
            assert!(
                !sddl.contains(&format!(";;;{absent})")),
                "{absent} must not appear as a DACL subject: {sddl}"
            );
        }
    }

    // -- TCP form (tcp-control-endpoint tasks 1.1–1.4) -----------------------

    /// An ephemeral free TCP port on localhost: bind :0, read the assigned
    /// port, drop the listener. (A tiny race window remains between the drop
    /// and the re-bind; standard test practice.)
    fn free_tcp_port() -> u16 {
        std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port()
    }

    /// Task 1.1: `tcp://host:port` parses into the TCP shape and displays
    /// back as the same URL; the default derivation is untouched (still the
    /// per-uid IPC path, not TCP).
    #[test]
    fn tcp_parse_accepts_well_formed_urls() {
        let ep = Endpoint::parse("tcp://127.0.0.1:7310").unwrap();
        assert!(ep.is_tcp());
        assert_eq!(ep.display(), "tcp://127.0.0.1:7310");
        // Whitespace around the value is trimmed like every other form.
        assert_eq!(
            Endpoint::parse("  tcp://localhost:1  ").unwrap().display(),
            "tcp://localhost:1"
        );
        // A trailing slash is URL furniture, kept out of the port.
        assert_eq!(
            Endpoint::parse("tcp://127.0.0.1:9/").unwrap().display(),
            "tcp://127.0.0.1:9"
        );
        // The defaults remain the IPC shape (缺省推导不变).
        let def = Endpoint::default_endpoint();
        assert!(!def.is_tcp());
        assert_eq!(def.display(), Endpoint::default_path().display().to_string());
    }

    /// Task 1.1: malformed `tcp://` values are rejected with a readable
    /// error naming the defect — missing port, empty host, port out of range
    /// (including 0), non-numeric port.
    #[test]
    fn tcp_parse_rejects_malformed_urls() {
        for (bad, frag) in [
            ("tcp://", "missing"),
            ("tcp://127.0.0.1", "missing"),
            ("tcp://:7310", "host must not be empty"),
            ("tcp://127.0.0.1:99999", "1..=65535"),
            ("tcp://127.0.0.1:0", "1..=65535"),
            ("tcp://127.0.0.1:x", "1..=65535"),
            ("tcp://127.0.0.1:-1", "1..=65535"),
            ("tcp://::1:7310", "must be bracketed"),
        ] {
            match Endpoint::parse(bad) {
                Err(EndpointError::Tcp(msg)) => {
                    assert!(
                        msg.contains(frag),
                        "{bad:?} must name the defect ({frag:?}): {msg}"
                    );
                }
                other => panic!("{bad:?} must be a Tcp parse error, got {other:?}"),
            }
        }
        // The TCP shape never trips the IPC path-length rule (no sun_path).
        let long = format!("tcp://host.example:1/{}", "x".repeat(300));
        assert!(
            matches!(Endpoint::parse(&long), Err(EndpointError::Tcp(_))),
            "an over-long tcp:// value is a parse error, not TooLong"
        );
    }

    /// Task 1.2: a TCP listener hands out connections that read and write in
    /// both directions, with the same clone and timeout affordances as the
    /// IPC connection.
    #[test]
    fn tcp_bind_connect_round_trips() {
        let port = free_tcp_port();
        let ep = Endpoint::parse(&format!("tcp://127.0.0.1:{port}")).unwrap();
        let mut listener = ep.bind().unwrap();
        let server = std::thread::spawn(move || {
            let mut conn = listener.accept().unwrap();
            let mut buf = [0u8; 5];
            conn.read_exact(&mut buf).unwrap();
            conn.write_all(b"pong").unwrap();
            conn.flush().unwrap();
            let mut writer = conn.try_clone_conn().unwrap();
            writer.write_all(b"!").unwrap();
            writer.flush().unwrap();
            buf
        });
        let mut conn = ep.connect().unwrap();
        conn.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        conn.set_write_timeout(Some(Duration::from_secs(5))).unwrap();
        conn.write_all(b"hello").unwrap();
        conn.flush().unwrap();
        let mut buf = [0u8; 5];
        conn.read_exact(&mut buf).unwrap();
        assert_eq!(std::str::from_utf8(&buf).unwrap(), "pong!");
        assert_eq!(server.join().unwrap(), *b"hello");
    }

    /// Task 1.3 (branch 1): binding a TCP address a live listener already
    /// serves reports `AlreadyRunning` — the probe connect answered.
    #[test]
    fn tcp_bind_reports_already_running() {
        let port = free_tcp_port();
        let ep = Endpoint::parse(&format!("tcp://127.0.0.1:{port}")).unwrap();
        let _first = ep.bind().unwrap();
        match ep.bind() {
            Err(BindError::AlreadyRunning) => {}
            Err(other) => panic!("expected AlreadyRunning, got {other}"),
            Ok(_) => panic!("a live TCP endpoint must not be re-bindable"),
        }
    }

    /// Task 1.3 (branch 2): a bind failure that is not an occupancy is the
    /// real error — never phrased as "another daemon" and never mapped into
    /// a false positive. The address probe first establishes, with the same
    /// std primitive the transport uses, that this environment does refuse
    /// non-local unassigned binds; on a host with `ip_nonlocal_bind` (or an
    /// equivalent quirk) the premise does not exist and there is nothing to
    /// assert.
    #[test]
    fn tcp_bind_unassignable_address_is_a_real_error() {
        let addr = "240.0.0.1:7310"; // reserved class E, never a local address
        match std::net::TcpListener::bind(addr) {
            Err(e) if e.kind() != io::ErrorKind::AddrInUse => {
                let ep = Endpoint::parse(&format!("tcp://{addr}")).unwrap();
                match ep.bind() {
                    Err(BindError::Io(bound_err)) => {
                        assert_eq!(
                            bound_err.kind(),
                            e.kind(),
                            "the real bind error passes through unchanged"
                        );
                    }
                    Err(BindError::AlreadyRunning) => {
                        panic!("a refused bind must not read as another daemon")
                    }
                    Err(other) => panic!("expected Io, got {other}"),
                    Ok(_) => panic!("a refused address must not bind"),
                }
            }
            // This host accepts the address (non-local bind enabled): the
            // branch's premise does not hold here.
            _ => {}
        }
    }

    /// Task 1.3 (branch 3) / task 2.4: TCP has no filesystem presence — a
    /// bound endpoint creates no files anywhere and never deletes anything
    /// (the stale-endpoint cleanup is IPC-only).
    #[test]
    fn tcp_endpoint_never_touches_the_filesystem_and_has_no_owner() {
        let d = tmp_dir("tcp-fs");
        std::fs::write(d.join("keep.txt"), b"precious").unwrap();
        let before: Vec<_> = std::fs::read_dir(&d).unwrap().collect();
        let ep = Endpoint::parse(&format!("tcp://127.0.0.1:{}", free_tcp_port())).unwrap();
        assert_eq!(ep.owner_uid(), None, "TCP has no owner (task 1.4)");
        let listener = ep.bind().unwrap();
        drop(listener);
        let after: Vec<_> = std::fs::read_dir(&d).unwrap().collect();
        assert_eq!(before.len(), after.len(), "no files created: {after:?}");
        assert!(d.join("keep.txt").exists(), "existing files untouched");
        assert_eq!(std::fs::read(d.join("keep.txt")).unwrap(), b"precious");
        let _ = std::fs::remove_dir_all(&d);
    }
}
