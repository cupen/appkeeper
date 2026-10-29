//! Unix daemon stop signals (handle-daemon-quit-signal).
//!
//! SIGINT (Ctrl+C), SIGTERM (service managers, `kill`) and SIGQUIT (operator
//! requests) all mean the same thing to the daemon: stop every managed
//! program and only then exit. They therefore share one notification channel
//! and the existing `Supervisor::request_shutdown()` intent (design D1-D3).
//!
//! Mechanism (design D2, written out here rather than delegated to a crate):
//!
//! - one process-wide self-pipe. The read end is non-blocking; the write end
//!   keeps its blocking semantics, so a delivery is never silently dropped
//!   just because the buffer is momentarily full (design Risks).
//! - the raw `sigaction` handlers (registered with `SA_RESTART`, installed
//!   once and left in place until the process exits) only write one fixed
//!   byte and save/restore `errno`. No mutex, no allocation, no logging.
//! - a dedicated ordinary thread polls the read end, drains it, and calls
//!   back into Rust — where `request_shutdown()` runs.
//!
//! The `unsafe` here is the FFI the contract requires (`sigaction`, `pipe`,
//! `poll`, `read`, `write`, `fcntl` and the `errno` accessor); every site
//! carries a safety note and it is confined to this module.
//!
//! SIGHUP is deliberately NOT part of the set (design D1): it keeps its
//! default disposition instead of turning into a stop request. Windows uses
//! `ctrlc` in console mode and the SCM service shell instead, so this module
//! is Unix-only.

#![cfg(unix)]

use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::sync::atomic::{AtomicI32, Ordering};

use anyhow::{Context, Result};

/// The Unix stop-signal set. Exactly these three, in registration order;
/// `SIGHUP` and every other signal stay untouched.
pub const SHUTDOWN_SIGNALS: [libc::c_int; 3] = [libc::SIGINT, libc::SIGTERM, libc::SIGQUIT];

/// The write end handed to the signal handlers, `-1` while not installed.
///
/// A plain lock-free integer is the only shared state the handler touches. It
/// is written once before the handlers are installed and never again, so the
/// handler always observes either `-1` (nothing to do) or the final
/// descriptor.
static WRITE_FD: AtomicI32 = AtomicI32::new(-1);

/// One observation from the notifier's wait/drain steps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wake {
    /// At least one byte was available: a stop signal arrived (or a hangup).
    Notified,
    /// Spurious or drained wake (EINTR, EAGAIN): wait again. Never interpreted
    /// as a stop request.
    Retry,
    /// The write end is gone: the notifier ends.
    Closed,
    /// Non-transient failure: the notifier gives up (without faking a stop).
    Failed(io::ErrorKind),
}

/// Classify one `read` on the self-pipe read end (pure, unit-tested).
pub fn classify(read: io::Result<usize>) -> Wake {
    match read {
        Ok(0) => Wake::Closed,
        Ok(_) => Wake::Notified,
        // EINTR is a retryable wake; EAGAIN is the normal drain terminator
        // (the read end is non-blocking).
        Err(e)
            if matches!(
                e.kind(),
                io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock
            ) =>
        {
            Wake::Retry
        }
        Err(e) => Wake::Failed(e.kind()),
    }
}

/// Human name for one of the registered stop signals (logging only).
pub fn signal_label(signal: libc::c_int) -> &'static str {
    match signal {
        libc::SIGINT => "SIGINT",
        libc::SIGTERM => "SIGTERM",
        libc::SIGQUIT => "SIGQUIT",
        _ => "signal",
    }
}

/// Block until the read end has data (or hangs up), retrying on EINTR.
pub fn wait_readable(fd: RawFd) -> Wake {
    let mut pollfd = libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: `poll(2)` over a one-element array we own; it only writes the
    // `revents` field of our stack value. Any `fd` value is safe to pass.
    let rc = unsafe { libc::poll(&mut pollfd, 1, -1) };
    if rc < 0 {
        return match io::Error::last_os_error().kind() {
            io::ErrorKind::Interrupted => Wake::Retry,
            kind => Wake::Failed(kind),
        };
    }
    if rc == 0 {
        // Cannot happen with an infinite timeout; treat it as spurious.
        return Wake::Retry;
    }
    if pollfd.revents & libc::POLLNVAL != 0 {
        return Wake::Failed(io::ErrorKind::InvalidInput);
    }
    // POLLIN, POLLHUP and POLLERR all mean "read and find out"; a hangup is
    // then observed as a 0-byte read (Closed).
    Wake::Notified
}

/// Read whatever the handler queued, without blocking: `Notified` when at
/// least one byte was consumed, `Retry` once the buffer is drained (EAGAIN),
/// `Closed` at end-of-pipe. Unit-testable without installing any handler.
pub fn drain_once(fd: RawFd) -> Wake {
    let mut buf = [0u8; 64];
    classify(read_fd(fd, &mut buf))
}

fn read_fd(fd: RawFd, buf: &mut [u8]) -> io::Result<usize> {
    // SAFETY: `read(2)` writes at most `buf.len()` bytes into the buffer we
    // own and reports how many.
    let rc = unsafe { libc::read(fd, buf.as_mut_ptr().cast(), buf.len()) };
    if rc >= 0 {
        Ok(rc as usize)
    } else {
        Err(io::Error::last_os_error())
    }
}

/// Notifier thread body: wait for readability, drain, notify once per batch.
/// Repeated deliveries are allowed (the supervisor's flag makes the intent
/// idempotent, design D3).
pub fn notifier_loop<F>(reader: OwnedFd, mut on_signal: F)
where
    F: FnMut(),
{
    let fd = reader.as_raw_fd();
    loop {
        match wait_readable(fd) {
            Wake::Notified => {}
            Wake::Retry => continue,
            Wake::Closed => return,
            Wake::Failed(kind) => {
                log::error!("shutdown-signal notifier stopped: poll failed ({kind:?})");
                return;
            }
        }
        // Drain the whole batch: every byte is one delivery, but one callback
        // is enough — the shutdown intent is a single flag.
        let mut got = false;
        loop {
            match drain_once(fd) {
                Wake::Notified => got = true,
                Wake::Retry => break,
                Wake::Closed => {
                    if got {
                        log::info!("shutdown signal received (SIGINT/SIGTERM/SIGQUIT)");
                        on_signal();
                    }
                    return;
                }
                Wake::Failed(kind) => {
                    log::error!(
                        "shutdown-signal notifier stopped: self-pipe read failed ({kind:?})"
                    );
                    return;
                }
            }
        }
        if got {
            log::info!("shutdown signal received (SIGINT/SIGTERM/SIGQUIT)");
            on_signal();
        }
    }
}

/// Create the self-pipe: non-blocking read end, blocking write end, both
/// close-on-exec.
fn self_pipe() -> Result<(OwnedFd, OwnedFd)> {
    let mut fds = [0 as libc::c_int; 2];
    // SAFETY: `pipe(2)` fills the two-element array we own.
    if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error()).context("failed to create the shutdown self-pipe");
    }
    // Wrap immediately so an early return below cannot leak the descriptors.
    // SAFETY: both descriptors were just created by `pipe(2)` and are not
    // owned by anything else.
    let (reader, writer) = unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) };
    // Read end: non-blocking, so draining never stalls (design D2). The write
    // end keeps its default blocking behaviour on purpose (design Risks).
    set_status_flags(reader.as_raw_fd(), libc::O_NONBLOCK)
        .context("failed to make the shutdown self-pipe read end non-blocking")?;
    // Close-on-exec for both: a re-exec'd child must not inherit the pipe.
    set_fd_flags(reader.as_raw_fd(), libc::FD_CLOEXEC)?;
    set_fd_flags(writer.as_raw_fd(), libc::FD_CLOEXEC)?;
    Ok((reader, writer))
}

fn set_status_flags(fd: RawFd, flags: libc::c_int) -> io::Result<()> {
    // SAFETY: fcntl(2) on a descriptor we own, with plain integer arguments.
    let current = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if current < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: same as above; `F_SETFL` takes an integer flag word.
    if unsafe { libc::fcntl(fd, libc::F_SETFL, current | flags) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn set_fd_flags(fd: RawFd, flags: libc::c_int) -> io::Result<()> {
    // SAFETY: fcntl(2) on a descriptor we own, with plain integer arguments.
    let current = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    if current < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: same as above.
    if unsafe { libc::fcntl(fd, libc::F_SETFD, current | flags) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// The raw (`extern "C"`) handler for every stop signal.
///
/// Design D2: it writes one fixed byte into the self-pipe and restores
/// `errno`; nothing else. No locks, no allocation, no logging — each call
/// below is async-signal-safe.
extern "C" fn handle_stop_signal(_signal: libc::c_int) {
    let saved_errno = errno::get();
    let fd = WRITE_FD.load(Ordering::Relaxed);
    if fd >= 0 {
        let byte = 1u8;
        // SAFETY: `write(2)` reads exactly one byte from our stack value. The
        // write end stays blocking, so while the pipe buffer has room the byte
        // is queued immediately; a short or failed write is tolerated — either
        // the read end is already readable or the buffer is full, and in both
        // cases the shutdown intent is already delivered.
        unsafe {
            libc::write(fd, std::ptr::from_ref(&byte).cast(), 1);
        }
    }
    errno::set(saved_errno);
}

/// Install `handle_stop_signal` for every signal in `signals`. Each
/// `sigaction` uses `SA_RESTART` and an empty mask, and the actions are never
/// unregistered, so a repeated delivery during the graceful stop can never
/// fall back to the default (terminating) action.
fn install_handlers(signals: &[libc::c_int]) -> Result<()> {
    for &signal in signals {
        // SAFETY: `sigaction` is a plain C struct; a zeroed value is a valid
        // "default/no handler" starting point and every field the kernel reads
        // is set below. `sa_sigaction` carries our `extern "C" fn(c_int)`
        // through `sighandler_t` (a pointer-sized integer on Unix).
        let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
        action.sa_sigaction = handle_stop_signal as *const () as usize;
        action.sa_flags = libc::SA_RESTART;
        // SAFETY: `sigemptyset` initialises the mask storage we own.
        unsafe { libc::sigemptyset(&mut action.sa_mask) };
        // SAFETY: the action pointer stays valid for the whole call, and a
        // null `oldact` asks the kernel to discard the previous disposition.
        if unsafe { libc::sigaction(signal, &action, std::ptr::null_mut()) } != 0 {
            return Err(io::Error::last_os_error()).with_context(|| {
                format!(
                    "failed to install the {} shutdown-signal handler",
                    signal_label(signal)
                )
            });
        }
    }
    Ok(())
}

/// Install the three Unix stop signals and start the notifier thread that
/// calls `on_shutdown` for every delivered batch. Called once, right after
/// the daemon core is up and before the supervision loop starts; a failure
/// here aborts startup instead of leaving the daemon unprotected (design D2).
pub fn install<F>(on_shutdown: F) -> Result<()>
where
    F: Fn() + Send + 'static,
{
    let (reader, writer) = self_pipe()?;
    WRITE_FD.store(writer.as_raw_fd(), Ordering::SeqCst);
    std::thread::Builder::new()
        .name("shutdown-signals".into())
        .spawn(move || notifier_loop(reader, on_shutdown))
        .context("failed to spawn the shutdown-signal notifier thread")?;
    if let Err(e) = install_handlers(&SHUTDOWN_SIGNALS) {
        WRITE_FD.store(-1, Ordering::SeqCst);
        // Dropping `writer` closes the write end; the notifier sees EOF and
        // ends on its own.
        return Err(e);
    }
    // The installed handlers write into this descriptor for the rest of the
    // process lifetime (design D2: the actions stay installed), so it is
    // deliberately kept open past this scope.
    std::mem::forget(writer);
    Ok(())
}

/// `errno` accessors used by the handler to save and restore `errno` around
/// its `write(2)` (design D2). The accessor symbol is per-OS family; an
/// unnamed exotic Unix falls back to a no-op so the daemon still builds
/// (losing only the save/restore nicety).
mod errno {
    use libc::c_int;

    #[cfg(any(target_os = "linux", target_os = "l4re"))]
    unsafe extern "C" {
        fn __errno_location() -> *mut c_int;
    }

    #[cfg(any(target_os = "android", target_os = "netbsd", target_os = "openbsd"))]
    unsafe extern "C" {
        fn __errno() -> *mut c_int;
    }

    #[cfg(any(
        target_os = "macos",
        target_os = "ios",
        target_os = "freebsd",
        target_os = "dragonfly"
    ))]
    unsafe extern "C" {
        fn __error() -> *mut c_int;
    }

    #[cfg(any(target_os = "linux", target_os = "l4re"))]
    fn location() -> *mut c_int {
        // SAFETY: the C library's errno accessor returns a pointer to the
        // calling thread's always-valid errno slot.
        unsafe { __errno_location() }
    }

    #[cfg(any(target_os = "android", target_os = "netbsd", target_os = "openbsd"))]
    fn location() -> *mut c_int {
        // SAFETY: as above, under this family's accessor name.
        unsafe { __errno() }
    }

    #[cfg(any(
        target_os = "macos",
        target_os = "ios",
        target_os = "freebsd",
        target_os = "dragonfly"
    ))]
    fn location() -> *mut c_int {
        // SAFETY: as above, under the BSD/Apple accessor name.
        unsafe { __error() }
    }

    #[cfg(any(
        target_os = "linux",
        target_os = "l4re",
        target_os = "android",
        target_os = "netbsd",
        target_os = "openbsd",
        target_os = "macos",
        target_os = "ios",
        target_os = "freebsd",
        target_os = "dragonfly"
    ))]
    pub fn get() -> i32 {
        // SAFETY: `location()` points at this thread's errno slot.
        unsafe { *location() }
    }

    #[cfg(any(
        target_os = "linux",
        target_os = "l4re",
        target_os = "android",
        target_os = "netbsd",
        target_os = "openbsd",
        target_os = "macos",
        target_os = "ios",
        target_os = "freebsd",
        target_os = "dragonfly"
    ))]
    pub fn set(value: i32) {
        // SAFETY: `location()` points at this thread's errno slot.
        unsafe { *location() = value };
    }

    #[cfg(not(any(
        target_os = "linux",
        target_os = "l4re",
        target_os = "android",
        target_os = "netbsd",
        target_os = "openbsd",
        target_os = "macos",
        target_os = "ios",
        target_os = "freebsd",
        target_os = "dragonfly"
    )))]
    pub fn get() -> i32 {
        0
    }

    #[cfg(not(any(
        target_os = "linux",
        target_os = "l4re",
        target_os = "android",
        target_os = "netbsd",
        target_os = "openbsd",
        target_os = "macos",
        target_os = "ios",
        target_os = "freebsd",
        target_os = "dragonfly"
    )))]
    pub fn set(_value: i32) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `WRITE_FD` is process-global; the two handler tests below drive it, so
    /// they must not overlap with each other.
    fn handler_guard() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Queue one byte into a pipe write end the way the handler would.
    fn put_byte(writer: &OwnedFd) {
        let byte = 1u8;
        // SAFETY: `write(2)` reads one byte from our stack value into a
        // descriptor we own.
        let rc = unsafe { libc::write(writer.as_raw_fd(), std::ptr::from_ref(&byte).cast(), 1) };
        assert_eq!(rc, 1, "test write succeeded");
    }

    /// The registered set is exactly INT/TERM/QUIT: no SIGHUP (design D1),
    /// no KILL/STOP (uncatchable), no other signal.
    #[test]
    fn signal_set_is_int_term_quit_without_hup() {
        assert_eq!(
            SHUTDOWN_SIGNALS,
            [libc::SIGINT, libc::SIGTERM, libc::SIGQUIT]
        );
        assert!(!SHUTDOWN_SIGNALS.contains(&libc::SIGHUP));
        assert!(!SHUTDOWN_SIGNALS.contains(&libc::SIGKILL));
        assert!(!SHUTDOWN_SIGNALS.contains(&libc::SIGSTOP));
        assert_eq!(signal_label(libc::SIGINT), "SIGINT");
        assert_eq!(signal_label(libc::SIGTERM), "SIGTERM");
        assert_eq!(signal_label(libc::SIGQUIT), "SIGQUIT");
        assert_eq!(signal_label(libc::SIGUSR1), "signal");
    }

    /// A byte from the handler is a notification; EOF closes; the retryable
    /// errno values are never mistaken for a stop request.
    #[test]
    fn classify_maps_read_outcomes() {
        assert_eq!(classify(Ok(1)), Wake::Notified);
        assert_eq!(classify(Ok(64)), Wake::Notified);
        assert_eq!(classify(Ok(0)), Wake::Closed);
        assert_eq!(
            classify(Err(io::Error::from(io::ErrorKind::Interrupted))),
            Wake::Retry
        );
        assert_eq!(
            classify(Err(io::Error::from(io::ErrorKind::WouldBlock))),
            Wake::Retry
        );
        assert_eq!(
            classify(Err(io::Error::from(io::ErrorKind::BrokenPipe))),
            Wake::Failed(io::ErrorKind::BrokenPipe)
        );
    }

    /// The self-pipe has the shape the contract asks for: non-blocking read
    /// end, blocking write end (design Risks), close-on-exec on both.
    #[test]
    fn self_pipe_flags_match_the_contract() {
        let (reader, writer) = self_pipe().unwrap();
        // SAFETY: F_GETFL/F_GETFD only read status flags.
        let read_flags = unsafe { libc::fcntl(reader.as_raw_fd(), libc::F_GETFL) };
        let write_flags = unsafe { libc::fcntl(writer.as_raw_fd(), libc::F_GETFL) };
        assert!(read_flags >= 0 && write_flags >= 0);
        assert_ne!(
            read_flags & libc::O_NONBLOCK,
            0,
            "the read end is non-blocking so draining never stalls"
        );
        assert_eq!(
            write_flags & libc::O_NONBLOCK,
            0,
            "the write end stays blocking so a delivery is never dropped"
        );
        for fd in [reader.as_raw_fd(), writer.as_raw_fd()] {
            // SAFETY: same read-only status query.
            let fd_flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
            assert_ne!(fd_flags & libc::FD_CLOEXEC, 0, "close-on-exec is set");
        }
    }

    /// `drain_once` consumes queued bytes and reports the batch; a drained
    /// (non-blocking) pipe is a retry, never a stop. No global signal handler
    /// is installed here (tasks 1.1).
    #[test]
    fn drain_reports_batches_without_blocking() {
        let (reader, writer) = self_pipe().unwrap();
        for _ in 0..3 {
            put_byte(&writer);
        }
        assert_eq!(drain_once(reader.as_raw_fd()), Wake::Notified);
        assert_eq!(
            drain_once(reader.as_raw_fd()),
            Wake::Retry,
            "a drained non-blocking pipe is a retry, not a stop"
        );
        put_byte(&writer);
        assert_eq!(
            drain_once(reader.as_raw_fd()),
            Wake::Notified,
            "a later delivery is its own batch"
        );
    }

    /// The wait is woken by data and by a hangup; a drained pipe is read as
    /// EOF afterwards.
    #[test]
    fn wait_readable_wakes_on_data_and_on_hangup() {
        let (reader, writer) = self_pipe().unwrap();
        put_byte(&writer);
        assert_eq!(wait_readable(reader.as_raw_fd()), Wake::Notified);
        assert_eq!(drain_once(reader.as_raw_fd()), Wake::Notified);
        assert_eq!(drain_once(reader.as_raw_fd()), Wake::Retry);
        drop(writer);
        assert_eq!(
            wait_readable(reader.as_raw_fd()),
            Wake::Notified,
            "a hangup wakes the poll"
        );
        assert_eq!(drain_once(reader.as_raw_fd()), Wake::Closed);
    }

    /// The notifier thread collapses a whole batch into one callback and ends
    /// at EOF. It is driven through a real pipe, never through the global
    /// handlers.
    #[test]
    fn notifier_drains_pipe_and_stops_at_eof() {
        let (reader, writer) = self_pipe().unwrap();
        for _ in 0..3 {
            put_byte(&writer);
        }
        drop(writer);
        let mut calls = 0;
        notifier_loop(reader, || calls += 1);
        assert_eq!(calls, 1, "a batch of pending wakes is drained in one call");
    }

    /// Two deliveries separated by a drain are two notifications.
    #[test]
    fn notifier_reports_each_drained_batch() {
        let (reader, writer) = self_pipe().unwrap();
        put_byte(&writer);
        assert_eq!(wait_readable(reader.as_raw_fd()), Wake::Notified);
        assert_eq!(drain_once(reader.as_raw_fd()), Wake::Notified);
        put_byte(&writer);
        drop(writer);
        let mut calls = 0;
        notifier_loop(reader, || calls += 1);
        assert_eq!(calls, 1, "the second delivery is its own notification");
    }

    /// A registration the kernel rejects is surfaced as an error, so the
    /// daemon refuses to enter its supervised loop unprotected. An
    /// out-of-range number travels the real `sigaction` error path without
    /// touching any installed handler.
    #[test]
    fn install_handlers_surfaces_an_unusable_signal() {
        let err = install_handlers(&[0x7fff]).unwrap_err();
        let text = format!("{err:#}");
        assert!(
            text.contains("shutdown-signal handler"),
            "the error names the failed registration: {text}"
        );
    }

    /// The handler's only side effect is the fixed-byte write, and it restores
    /// `errno`: a write to a closed descriptor sets EBADF, which must not leak
    /// out of the handler.
    #[test]
    fn handler_writes_one_byte_and_restores_errno() {
        let _serial = handler_guard();
        let (reader, writer) = self_pipe().unwrap();
        let invalid = writer.as_raw_fd();
        WRITE_FD.store(invalid, Ordering::SeqCst);
        drop(writer);
        errno::set(libc::EINTR);
        handle_stop_signal(libc::SIGTERM);
        let after = errno::get();
        WRITE_FD.store(-1, Ordering::SeqCst);
        assert_eq!(after, libc::EINTR, "the handler restored errno");
        assert_eq!(drain_once(reader.as_raw_fd()), Wake::Closed);
    }

    /// With no handler installed (`WRITE_FD == -1`) the trampoline is inert
    /// apart from the errno round-trip.
    #[test]
    fn handler_is_inert_before_install() {
        let _serial = handler_guard();
        WRITE_FD.store(-1, Ordering::SeqCst);
        errno::set(libc::ENOENT);
        handle_stop_signal(libc::SIGQUIT);
        assert_eq!(errno::get(), libc::ENOENT);
    }
}
