//! Log pumps: one thread per child output stream. Each line is pushed into
//! an in-memory ring buffer (for `log --tail/--follow` and the web console),
//! broadcast to bounded subscribers, and appended to the on-disk log file
//! with size-based rotation.
//!
//! Backpressure boundary (log-management capability, 输出排空零反压): the
//! pump thread is the only thing draining the child's pipe. Nothing below it
//! — file writes, rotation, ring contention, slow or stalled subscribers —
//! may stop it for longer than one bounded flush. File work is batched
//! (≥[`FLUSH_BYTES`] or a drained pipe), and subscriber delivery is
//! non-blocking: a slow viewer only loses its own copy (surfaced as a Gap),
//! never the child's throughput.

use std::collections::VecDeque;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use log::{debug, warn};

use crate::metrics::RateCounter;

/// Pending bytes (or a drained pipe) trigger one flush: a single file write
/// plus a single ring push per batch instead of per line.
const FLUSH_BYTES: usize = 256 * 1024;
/// A pipe read that took at least this long had to block (nothing was
/// available): the quiet-period burst just processed is flushed immediately
/// so followers are not delayed until the next line.
const BLOCK_THRESHOLD: Duration = Duration::from_millis(1);
/// Subscriber queue depth in batches. Depth × batch ≈ 16 MiB in flight per
/// subscriber — a normally-consuming client never triggers a Gap.
pub(crate) const SUB_CHANNEL_BATCHES: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Stream {
    Out,
    Err,
}

impl Stream {
    pub fn suffix(self) -> &'static str {
        match self {
            Stream::Out => "out",
            Stream::Err => "err",
        }
    }
}

/// One delivery unit for a live subscriber: a batch of complete lines, or a
/// loss marker telling the consumer that N earlier lines will never arrive
/// on this channel (they remain intact in the ring and on disk).
#[derive(Debug, Clone, PartialEq)]
pub enum SubItem {
    Lines(Vec<String>),
    Gap(u64),
}

struct Subscriber {
    tx: SyncSender<SubItem>,
    /// Lines dropped because this subscriber's queue was full; delivered as
    /// a Gap before the next successful batch.
    dropped: u64,
}

impl Subscriber {
    /// Non-blocking delivery (零反压: the pump never waits on a viewer). A
    /// full or disconnected queue only drops THIS subscriber's copy.
    fn deliver(&mut self, lines: &[String]) -> bool {
        if self.dropped > 0 {
            let n = std::mem::replace(&mut self.dropped, 0);
            match self.tx.try_send(SubItem::Gap(n)) {
                Ok(()) => {}
                Err(TrySendError::Full(_)) => {
                    self.dropped = n + lines.len() as u64;
                    return true;
                }
                Err(TrySendError::Disconnected(_)) => return false,
            }
        }
        match self.tx.try_send(SubItem::Lines(lines.to_vec())) {
            Ok(()) => true,
            Err(TrySendError::Full(_)) => {
                self.dropped += lines.len() as u64;
                true
            }
            Err(TrySendError::Disconnected(_)) => false,
        }
    }
}

/// Bounded ring of recent lines plus live-follow subscribers. Each ring also
/// counts its lines for the log-rate metrics (hot path: one atomic add per
/// line — no lock, no viewer required).
pub struct Ring {
    inner: Mutex<RingInner>,
    rate: RateCounter,
}

struct RingInner {
    lines: VecDeque<String>,
    cap: usize,
    subs: Vec<Subscriber>,
}

impl Ring {
    pub fn new(cap: usize) -> Arc<Ring> {
        Arc::new(Ring {
            inner: Mutex::new(RingInner {
                lines: VecDeque::with_capacity(cap),
                cap,
                subs: Vec::new(),
            }),
            rate: RateCounter::new(),
        })
    }

    /// Append a batch of complete lines: one rate add, one lock, one
    /// delivery attempt per subscriber.
    pub fn push_batch(&self, lines: &[String]) {
        if lines.is_empty() {
            return;
        }
        self.rate.bump_by(lines.len() as u64);
        let mut g = self.inner.lock().unwrap();
        for line in lines {
            if g.lines.len() == g.cap {
                g.lines.pop_front();
            }
            g.lines.push_back(line.clone());
        }
        g.subs.retain_mut(|s| s.deliver(lines));
    }

    #[allow(dead_code)] // single-line convenience; hot path is push_batch
    pub fn push(&self, line: String) {
        self.push_batch(std::slice::from_ref(&line));
    }

    pub fn tail(&self, n: usize) -> Vec<String> {
        let g = self.inner.lock().unwrap();
        g.lines.iter().rev().take(n).rev().cloned().collect()
    }

    /// Subscribe to live batches. The channel is bounded: consuming slower
    /// than the pump produces loses only this subscriber's undelivered
    /// batches (signalled via [`SubItem::Gap`]); it can never slow the
    /// child, the ring, or the log files.
    pub fn subscribe(&self) -> Receiver<SubItem> {
        let (tx, rx) = mpsc::sync_channel(SUB_CHANNEL_BATCHES);
        self.inner
            .lock()
            .unwrap()
            .subs
            .push(Subscriber { tx, dropped: 0 });
        rx
    }

    /// The rate counter of this stream (rotation is driven by the metrics
    /// sampler; reading works at any time).
    pub fn rate(&self) -> &RateCounter {
        &self.rate
    }

    /// Clear the rate window (program restart: the previous process's rates
    /// must not bleed into the new one).
    pub fn reset_rate(&self) {
        self.rate.reset();
    }
}

pub struct PumpSet {
    handles: Vec<JoinHandle<()>>,
}

impl PumpSet {
    /// Wait for both pump threads. Only safe when the child's pipe writers
    /// are known dead (normal EOF); see [`Drop`][PumpSet::drop] for why the
    /// teardown path must NOT join.
    #[allow(dead_code)]
    pub fn join(&mut self) {
        for h in self.handles.drain(..) {
            let _ = h.join();
        }
    }
}

impl Drop for PumpSet {
    fn drop(&mut self) {
        // DETACH, never join. Joining here hangs the supervisor when a
        // child's pipeline grandchildren escape the process-group kill
        // (e.g. `timeout` puts its command in a new group): they keep the
        // pipe open, the pump never sees EOF, and stop()/shutdown would
        // block forever (线上事故级). A detached pump simply exits on EOF —
        // or with process exit — and holds only bounded resources
        // (Arc<Ring> + the log file handle).
        for h in self.handles.drain(..) {
            let _ = h; // detached
        }
    }
}

pub fn log_path(log_dir: &Path, name: &str, stream: Stream) -> PathBuf {
    log_dir.join(format!("{}.{}.log", name, stream.suffix()))
}

/// Start one pump thread per stream. Called right after spawn with the
/// child's piped stdout/stderr. The rings outlive the child so tails and
/// followers keep working after restarts.
pub fn start(
    out: std::process::ChildStdout,
    err: std::process::ChildStderr,
    name: &str,
    out_ring: Arc<Ring>,
    err_ring: Arc<Ring>,
    log_dir: &Path,
    max_size: Option<u64>,
    rotate_keep: u32,
) -> PumpSet {
    let out_path = log_path(log_dir, name, Stream::Out);
    let err_path = log_path(log_dir, name, Stream::Err);
    let h_out = std::thread::Builder::new()
        .name(format!("pump-{name}-out"))
        .spawn(move || pump(out, out_ring, out_path, max_size, rotate_keep))
        .expect("failed to spawn pump thread");
    let h_err = std::thread::Builder::new()
        .name(format!("pump-{name}-err"))
        .spawn(move || pump(err, err_ring, err_path, max_size, rotate_keep))
        .expect("failed to spawn pump thread");
    PumpSet {
        handles: vec![h_out, h_err],
    }
}

/// Split a raw pipe chunk into complete lines, carrying an incomplete tail
/// over to the next chunk. Appends to `batch`; `bytes` counts line bytes
/// (+ newline) for flush thresholds.
fn collect_lines(chunk: &[u8], leftover: &mut Vec<u8>, batch: &mut Vec<String>, bytes: &mut usize) {
    let mut start = 0usize;
    for (i, b) in chunk.iter().enumerate() {
        if *b == b'\n' {
            leftover.extend_from_slice(&chunk[start..i]);
            start = i + 1;
            let mut s = String::from_utf8_lossy(leftover).into_owned();
            if s.ends_with('\r') {
                s.pop();
            }
            *bytes += s.len() + 1;
            batch.push(s);
            leftover.clear();
        }
    }
    leftover.extend_from_slice(&chunk[start..]);
}

/// Commit one batch: rotate if needed, append to the ring (memory is the
/// source of truth for tails/followers), then one file write. Write failure
/// degrades to memory-only and never blocks the pipe drain.
fn flush_batch(
    ring: &Ring,
    batch: &mut Vec<String>,
    file: &mut File,
    size: &mut u64,
    path: &Path,
    limit: Option<u64>,
    keep: u32,
    warned: &mut bool,
) {
    if batch.is_empty() {
        return;
    }
    if let Some(l) = limit
        && *size >= l
    {
        match rotate_now(path, keep) {
            Ok(f) => {
                *file = f;
                *size = 0;
            }
            Err(e) => {
                if !*warned {
                    warn!("log rotation failed for {}: {e}", path.display());
                    *warned = true;
                }
            }
        }
    }
    ring.push_batch(batch);
    let mut data = batch.join("\n");
    data.push('\n');
    match file.write_all(data.as_bytes()) {
        Ok(()) => *size += data.len() as u64,
        Err(e) => {
            if !*warned {
                warn!("cannot write log file {}: {e}", path.display());
                *warned = true;
            }
        }
    }
    batch.clear();
}

/// `pub(crate)` for integration tests that push synthetic bursts through the
/// real flush path.
pub(crate) fn pump(
    reader: impl Read,
    ring: Arc<Ring>,
    path: PathBuf,
    max_size: Option<u64>,
    keep: u32,
) {
    let mut file = match open_append(&path) {
        Ok(f) => f,
        Err(e) => {
            warn!(
                "cannot open log file {}: {e}; output goes to memory only",
                path.display()
            );
            drain_only(reader, ring);
            return;
        }
    };
    let mut size = file.seek(SeekFrom::End(0)).unwrap_or(0);
    let mut reader = reader;
    let mut buf = vec![0u8; 64 * 1024];
    let mut leftover: Vec<u8> = Vec::new();
    let mut batch: Vec<String> = Vec::new();
    let mut batch_bytes = 0usize;
    let mut warned = false;
    // First iteration behaves as if the previous read blocked, so a batch
    // carried over from anywhere is flushed before the first blocking read.
    let mut prev_read_blocked = true;
    loop {
        if prev_read_blocked {
            flush_batch(
                &ring,
                &mut batch,
                &mut file,
                &mut size,
                &path,
                max_size,
                keep,
                &mut warned,
            );
            batch_bytes = 0;
        }
        let t0 = Instant::now();
        let n = match reader.read(&mut buf) {
            Ok(0) => break, // EOF
            Ok(n) => n,
            Err(e) => {
                debug!("log pump read error: {e}");
                break;
            }
        };
        // A slow read means the pipe ran dry: the child paused. Flush right
        // away so live viewers see the burst now, not at the next burst.
        prev_read_blocked = t0.elapsed() >= BLOCK_THRESHOLD;
        collect_lines(&buf[..n], &mut leftover, &mut batch, &mut batch_bytes);
        // During sustained output the read keeps returning instantly; cap
        // the batch so file writes and ring locks stay bounded.
        if batch_bytes >= FLUSH_BYTES {
            flush_batch(
                &ring,
                &mut batch,
                &mut file,
                &mut size,
                &path,
                max_size,
                keep,
                &mut warned,
            );
            batch_bytes = 0;
        }
    }
    // EOF: the trailing partial line (no final newline) still counts, then
    // flush whatever is pending.
    if !leftover.is_empty() {
        let s = String::from_utf8_lossy(&leftover).into_owned();
        batch.push(s);
    }
    flush_batch(
        &ring,
        &mut batch,
        &mut file,
        &mut size,
        &path,
        max_size,
        keep,
        &mut warned,
    );
}

/// Best-effort drain when the file cannot be opened at all.
fn drain_only(reader: impl Read, ring: Arc<Ring>) {
    let mut reader = reader;
    let mut buf = vec![0u8; 64 * 1024];
    let mut leftover: Vec<u8> = Vec::new();
    let mut batch: Vec<String> = Vec::new();
    let mut bytes = 0usize;
    loop {
        match reader.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                collect_lines(&buf[..n], &mut leftover, &mut batch, &mut bytes);
                if batch.len() >= 4096 {
                    ring.push_batch(&batch);
                    batch.clear();
                }
            }
        }
    }
    if !leftover.is_empty() {
        batch.push(String::from_utf8_lossy(&leftover).into_owned());
    }
    ring.push_batch(&batch);
}

fn open_append(path: &Path) -> std::io::Result<File> {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    OpenOptions::new().create(true).append(true).open(path)
}

/// Archive name for rotation slot `i` (`.1.gz`, `.2.gz`, …).
fn archive_path(path: &Path, i: u32) -> PathBuf {
    let os = path.as_os_str().to_string_lossy();
    PathBuf::from(format!("{os}.{i}.gz"))
}

/// The pre-upgrade (uncompressed) archive name, cleaned up on the next shift.
fn numbered(path: &Path, i: u32) -> PathBuf {
    let os = path.as_os_str().to_string_lossy();
    PathBuf::from(format!("{os}.{i}"))
}

// -- rotation + archive compression -----------------------------------------
//
// The pump thread only takes the full file out of the write path, reopens the
// current file and enqueues the archive; gzip and the archive shift run on a
// single worker (design D2), so `flush_batch` never blocks on compression
// (log-management spec: 压缩不阻塞排空). A full or disconnected queue degrades
// to an uncompressed `.1` — content is never dropped.

/// Bound on pending compression jobs; past this a rotation degrades in place.
const ROTATION_QUEUE: usize = 1024;

struct RotationJob {
    /// The renamed full file awaiting compression.
    tmp: PathBuf,
    /// The live log path the archive belongs to (`…out.log`).
    current: PathBuf,
    keep: u32,
}

/// Queue entries: compression work, or a barrier used at shutdown to prove
/// the backlog was applied (so no archive is left as a `.tmp` on exit).
enum RotationMessage {
    Compress(RotationJob),
    Barrier(mpsc::Sender<()>),
}

static ROTATION_TX: std::sync::OnceLock<SyncSender<RotationMessage>> = std::sync::OnceLock::new();

fn rotation_sender() -> &'static SyncSender<RotationMessage> {
    ROTATION_TX.get_or_init(|| {
        let (tx, rx) = mpsc::sync_channel::<RotationMessage>(ROTATION_QUEUE);
        std::thread::Builder::new()
            .name("ak-log-compress".to_string())
            .spawn(move || {
                for msg in rx {
                    match msg {
                        RotationMessage::Compress(job) => compress_archive(job),
                        RotationMessage::Barrier(ack) => {
                            let _ = ack.send(());
                        }
                    }
                }
            })
            .expect("failed to spawn the log compression worker");
        tx
    })
}

/// Best-effort drain of the compression backlog, for the shutdown path: waits
/// until every job queued *so far* is applied. Returns `false` on timeout or
/// when the worker is gone (the jobs then stay as `.tmp`, never lost silently).
/// Bounded by `timeout` in total, so a stuck worker cannot hang the daemon.
pub fn flush_rotations(timeout: Duration) -> bool {
    if ROTATION_TX.get().is_none() {
        return true; // nothing was ever rotated: no worker, no backlog
    }
    let deadline = Instant::now() + timeout;
    let (ack_tx, ack_rx) = mpsc::channel();
    // A full queue means the worker is behind; wait for room rather than
    // degrading (this is the exit path, not the pump's write path).
    let mut msg = RotationMessage::Barrier(ack_tx);
    loop {
        match rotation_sender().try_send(msg) {
            Ok(()) => break,
            Err(TrySendError::Full(returned)) => {
                if Instant::now() >= deadline {
                    return false;
                }
                msg = returned;
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(TrySendError::Disconnected(_)) => return false,
        }
    }
    let remaining = deadline.saturating_duration_since(Instant::now());
    ack_rx.recv_timeout(remaining).is_ok()
}

/// A unique name for the full file while the worker compresses it (never
/// collides with an archive, so the shift cannot see it).
fn rotation_tmp(path: &Path) -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let os = path.as_os_str().to_string_lossy();
    PathBuf::from(format!("{os}.{seq}.{}.tmp", std::process::id()))
}

/// Rotate one stream: `keep = 0` drops the full file (no archive); otherwise
/// the file is renamed out of the way, reopened, and the archive is queued.
fn rotate_now(path: &Path, keep: u32) -> std::io::Result<File> {
    if keep == 0 {
        // Explicit opt-out: delete and reopen, no archive is produced.
        let _ = std::fs::remove_file(path);
        return open_append(path);
    }
    let tmp = rotation_tmp(path);
    std::fs::rename(path, &tmp)?;
    let file = match open_append(path) {
        Ok(f) => f,
        Err(e) => {
            // Put the file back: nothing was rotated after all.
            let _ = std::fs::rename(&tmp, path);
            return Err(e);
        }
    };
    let rejected = match rotation_sender().try_send(RotationMessage::Compress(RotationJob {
        tmp,
        current: path.to_path_buf(),
        keep,
    })) {
        Ok(()) => None,
        Err(TrySendError::Full(RotationMessage::Compress(job)))
        | Err(TrySendError::Disconnected(RotationMessage::Compress(job))) => Some(job),
        // Unreachable: only `Compress` is ever sent on this path.
        Err(_) => None,
    };
    if let Some(job) = rejected {
        // Never block the pump: keep the newest archive uncompressed.
        warn!(
            "log compression queue is busy; keeping an uncompressed archive for {}",
            job.current.display()
        );
        keep_uncompressed(&job.tmp, &job.current, job.keep);
    }
    Ok(file)
}

/// Degraded rotation: park the archive as `.1` (no gzip). Best effort — a
/// failure here still leaves the current file writable.
fn keep_uncompressed(tmp: &Path, current: &Path, keep: u32) {
    if keep == 0 {
        let _ = std::fs::remove_file(tmp);
        return;
    }
    let target = numbered(current, 1);
    let _ = std::fs::remove_file(&target);
    if let Err(e) = std::fs::rename(tmp, &target) {
        warn!(
            "cannot keep the uncompressed log archive {}: {e}",
            tmp.display()
        );
    }
}

/// Worker: drop the oldest, shift `.N.gz` -> `.N+1.gz`, then compress the
/// newest archive into `.1.gz` (falling back to an uncompressed `.1`).
fn compress_archive(job: RotationJob) {
    let archive = |i: u32| archive_path(&job.current, i);
    let legacy = |i: u32| numbered(&job.current, i);

    // Oldest slot: both the compressed archive and a pre-upgrade `.N`.
    let _ = std::fs::remove_file(archive(job.keep));
    let _ = std::fs::remove_file(legacy(job.keep));
    for i in (1..job.keep).rev() {
        let to = archive(i + 1);
        let _ = std::fs::remove_file(&to);
        let _ = std::fs::remove_file(legacy(i + 1));
        let from = archive(i);
        if from.exists()
            && let Err(e) = std::fs::rename(&from, &to)
        {
            warn!("cannot shift log archive {}: {e}", from.display());
        }
    }
    // Newest slot: compress, then clean the pre-upgrade `.1`.
    let target = archive(1);
    let _ = std::fs::remove_file(&target);
    let _ = std::fs::remove_file(legacy(1));
    match gzip_file(&job.tmp, &target) {
        Ok(()) => {
            let _ = std::fs::remove_file(&job.tmp);
        }
        Err(e) => {
            warn!(
                "log compression failed for {}: {e}; keeping an uncompressed archive",
                job.tmp.display()
            );
            keep_uncompressed(&job.tmp, &job.current, job.keep);
        }
    }
}

/// gzip `from` into `to` through a temp file, so readers never observe a
/// partially written archive.
fn gzip_file(from: &Path, to: &Path) -> std::io::Result<()> {
    let mut input = std::io::BufReader::new(File::open(from)?);
    let tmp = PathBuf::from(format!("{}.tmp", to.display()));
    {
        let out = File::create(&tmp)?;
        let mut encoder = flate2::write::GzEncoder::new(
            std::io::BufWriter::new(out),
            flate2::Compression::default(),
        );
        std::io::copy(&mut input, &mut encoder)?;
        let mut out = encoder.finish()?;
        out.flush()?;
    }
    std::fs::rename(&tmp, to)
}

/// Env var the detached launcher sets so the daemon rotates its own log
/// (log-management spec: 后台守护日志按大小轮转). Absent in foreground and
/// service modes, which keep writing to stderr.
pub const DAEMON_LOG_ENV: &str = "AK_DAEMON_LOG_FILE";

/// A `Write` sink that rotates by size and enqueues gzip compression on the
/// shared worker. Used as `env_logger`'s `Target::Pipe` for the daemon's own
/// log, reusing the exact same archive contract as the child-log pumps.
pub struct RotatingWriter {
    file: File,
    size: u64,
    path: PathBuf,
    limit: u64,
    keep: u32,
}

impl RotatingWriter {
    /// Open (or create) `path` for appending and start tracking its size.
    pub fn open(path: PathBuf, limit: u64, keep: u32) -> std::io::Result<Self> {
        let file = open_append(&path)?;
        let size = file.metadata().map(|m| m.len()).unwrap_or(0);
        Ok(Self {
            file,
            size,
            path,
            limit,
            keep,
        })
    }
}

impl Write for RotatingWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if self.limit > 0 && self.size >= self.limit {
            match rotate_now(&self.path, self.keep) {
                Ok(file) => {
                    self.file = file;
                    self.size = 0;
                }
                Err(e) => {
                    // Keep logging: a failed rotation must not silence the
                    // daemon (and must not recurse into `log::`).
                    eprintln!(
                        "ak: warning: cannot rotate the daemon log {}: {e}",
                        self.path.display()
                    );
                }
            }
        }
        let written = self.file.write(buf)?;
        self.size += written as u64;
        Ok(written)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.file.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ring_tail_and_subscribe() {
        let r = Ring::new(3);
        for i in 0..5 {
            r.push(format!("line{i}"));
        }
        assert_eq!(r.tail(10), vec!["line2", "line3", "line4"]);
        assert_eq!(r.tail(2), vec!["line3", "line4"]);
        let rx = r.subscribe();
        r.push("live".to_string());
        assert_eq!(
            rx.recv_timeout(std::time::Duration::from_millis(100))
                .unwrap(),
            SubItem::Lines(vec!["live".to_string()])
        );
    }

    #[test]
    fn rate_counts_without_viewers_and_resets() {
        let r = Ring::new(10);
        for _ in 0..5 {
            r.push("x".to_string());
        }
        // Sampler normally rotates at 1 Hz; do it by hand here.
        r.rate().rotate();
        assert!(
            (r.rate().rate(1) - 5.0).abs() < 1e-9,
            "counts with no subscriber"
        );
        // Restart semantics: window clears, old lines don't count afterwards.
        r.reset_rate();
        assert_eq!(r.rate().rate(1), 0.0);
        r.push("after-restart".to_string());
        r.rate().rotate();
        assert!((r.rate().rate(1) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn slow_subscriber_gets_gap_ring_and_disk_stay_complete() {
        let tmp = std::env::temp_dir().join(format!("ak-test-gap-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&tmp);
        let ring = Ring::new(1000);
        let path = tmp.join("g.log");
        let file = open_append(&path).unwrap();

        let rx = ring.subscribe();
        // Fill the subscriber's bounded queue, then overflow it.
        let mut file = file;
        let mut size = 0u64;
        let mut warned = false;
        let total_batches = SUB_CHANNEL_BATCHES + 8;
        for b in 0..total_batches {
            let lines: Vec<String> = (0..10).map(|i| format!("b{b}l{i}")).collect();
            flush_batch(
                &ring,
                &mut lines.clone(),
                &mut file,
                &mut size,
                &path,
                None,
                2,
                &mut warned,
            );
        } // Ring has every line even though the subscriber queue overflowed.
        assert_eq!(ring.tail(10000).len(), total_batches * 10);
        // Drain the queue: batches only — no Gap ever made it in while the
        // queue was full; the loss is accounted on the subscriber.
        let mut seen_lines = 0usize;
        let mut seen_gaps = 0u64;
        while let Ok(item) = rx.try_recv() {
            match item {
                SubItem::Lines(v) => seen_lines += v.len(),
                SubItem::Gap(n) => seen_gaps += n,
            }
        }
        assert_eq!(seen_lines, SUB_CHANNEL_BATCHES * 10);
        assert_eq!(seen_gaps, 0);
        // One more batch: the pending loss is delivered as a Gap BEFORE it.
        let fresh = vec!["fresh".to_string()];
        flush_batch(
            &ring,
            &mut fresh.clone(),
            &mut file,
            &mut size,
            &path,
            None,
            2,
            &mut warned,
        );
        match rx
            .recv_timeout(std::time::Duration::from_millis(100))
            .unwrap()
        {
            SubItem::Gap(n) => assert_eq!(n, 8 * 10, "exactly the dropped lines"),
            SubItem::Lines(_) => panic!("gap must precede the next delivered batch"),
        }
        match rx
            .recv_timeout(std::time::Duration::from_millis(100))
            .unwrap()
        {
            SubItem::Lines(v) => assert_eq!(v, fresh),
            SubItem::Gap(_) => panic!("unexpected second gap"),
        }
        // Disk copy is complete: all 720 batch lines plus the fresh one.
        let content = std::fs::read_to_string(&path).unwrap();
        assert_eq!(content.lines().count(), total_batches * 10 + 1);
        let _ = std::fs::remove_dir_all(tmp);
    }

    #[test]
    fn gap_precedes_next_delivered_batch() {
        let r = Ring::new(1000);
        let rx = r.subscribe();
        // Overflow the queue without draining it.
        for b in 0..(SUB_CHANNEL_BATCHES + 3) {
            let line = format!("overflow-{b}");
            r.push_batch(&[line]);
        }
        // Drain the queued batches; none of them is a Gap.
        while let Ok(item) = rx.try_recv() {
            assert!(
                matches!(item, SubItem::Lines(_)),
                "no gap while queue was full"
            );
        }
        // The next successful delivery starts with the accumulated Gap.
        r.push_batch(&["fresh".to_string()]);
        match rx
            .recv_timeout(std::time::Duration::from_millis(100))
            .unwrap()
        {
            SubItem::Gap(n) => assert_eq!(n, 3),
            SubItem::Lines(_) => panic!("gap must precede the next delivered batch"),
        }
        match rx
            .recv_timeout(std::time::Duration::from_millis(100))
            .unwrap()
        {
            SubItem::Lines(v) => assert_eq!(v, vec!["fresh"]),
            SubItem::Gap(_) => panic!("gap must not be re-delivered after catch-up"),
        }
    }

    #[test]
    fn dead_subscriber_is_dropped_silently() {
        let r = Ring::new(10);
        let rx = r.subscribe();
        drop(rx);
        r.push("nobody listens".to_string());
        assert_eq!(r.tail(10).len(), 1, "ring unaffected by dead subscriber");
    }

    #[test]
    fn collect_lines_handles_split_chunks_and_crlf() {
        let mut leftover: Vec<u8> = Vec::new();
        let mut batch: Vec<String> = Vec::new();
        let mut bytes = 0usize;
        collect_lines(b"hel", &mut leftover, &mut batch, &mut bytes);
        assert!(batch.is_empty());
        collect_lines(b"lo\r\nwor", &mut leftover, &mut batch, &mut bytes);
        assert_eq!(batch, vec!["hello"]);
        collect_lines(b"ld\n", &mut leftover, &mut batch, &mut bytes);
        assert_eq!(batch, vec!["hello", "world"]);
        assert_eq!(bytes, "hello\n".len() + "world\n".len());
    }

    #[test]
    fn pump_batches_lines_and_flushes_on_eof() {
        let tmp = std::env::temp_dir().join(format!("ak-test-pump-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&tmp);
        let data = "l1\nl2\nl3\n";
        let reader = std::io::Cursor::new(data.as_bytes().to_vec());
        let ring = Ring::new(10);
        let path = tmp.join("p.log");
        pump(reader, ring.clone(), path.clone(), Some(4), 2);
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("l"));
        assert_eq!(ring.tail(10), vec!["l1", "l2", "l3"]);
        let _ = std::fs::remove_dir_all(tmp);
    }

    #[test]
    fn pump_flushes_partial_line_at_eof() {
        let tmp = std::env::temp_dir().join(format!("ak-test-pump-eof-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&tmp);
        let reader = std::io::Cursor::new(b"complete\npartial-no-newline".to_vec());
        let ring = Ring::new(10);
        let path = tmp.join("p.log");
        pump(reader, ring.clone(), path.clone(), None, 2);
        assert_eq!(ring.tail(10), vec!["complete", "partial-no-newline"]);
        let _ = std::fs::remove_dir_all(tmp);
    }

    /// log-management 输出排空零反压: an unwritable log location must not
    /// stop the drain — output still reaches the ring (memory is the source
    /// of truth for tails/followers) and the pump keeps going. Unix-only:
    /// root bypasses permission bits, and Windows has no chmod.
    #[test]
    #[cfg(unix)]
    fn unwritable_log_dir_degrades_to_memory_only() {
        if unsafe { libc::geteuid() } == 0 {
            return; // root can write anywhere; the scenario cannot be built
        }
        let tmp = std::env::temp_dir().join(format!("ak-test-ro-{}", std::process::id()));
        let dir = tmp.join("readonly");
        std::fs::create_dir_all(&dir).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o500)).unwrap();
        }
        let path = dir.join("p.log");
        let ring = Ring::new(100);
        let data = "line-1\nline-2\n";
        // open_append fails (read-only dir) → drain_only path.
        pump(
            std::io::Cursor::new(data.as_bytes().to_vec()),
            ring.clone(),
            path.clone(),
            None,
            2,
        );
        assert_eq!(
            ring.tail(10),
            vec!["line-1", "line-2"],
            "output reaches the ring"
        );
        assert!(!path.exists(), "no file may appear in the read-only dir");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
        }
        let _ = std::fs::remove_dir_all(tmp);
    }

    #[test]
    fn rotation_archives_as_gzip_and_shifts() {
        let tmp = std::env::temp_dir().join(format!("ak-test-rot-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&tmp);
        let p = tmp.join("t.log");
        std::fs::write(&p, "current").unwrap();
        // Pre-upgrade uncompressed archives: cleaned up on the next rotation.
        std::fs::write(numbered(&p, 1), "one").unwrap();
        std::fs::write(numbered(&p, 2), "two").unwrap();

        rotate_now(&p, 2).unwrap();
        assert!(p.exists(), "the current file is reopened immediately");
        let newest = archive_path(&p, 1);
        wait_for(|| newest.exists());
        assert_eq!(read_gzip(&newest), "current");
        assert!(!numbered(&p, 1).exists(), "legacy .1 is cleaned up");
        assert!(!numbered(&p, 2).exists(), "legacy .2 is cleaned up");

        // A second rotation shifts the previous archive to `.2.gz`.
        std::fs::write(&p, "second").unwrap();
        rotate_now(&p, 2).unwrap();
        let older = archive_path(&p, 2);
        wait_for(|| older.exists() && newest.exists());
        assert_eq!(read_gzip(&newest), "second");
        assert_eq!(read_gzip(&older), "current");

        // keep = 0 drops the full file without producing an archive.
        let _ = std::fs::remove_file(&older);
        let _ = std::fs::remove_file(&newest);
        rotate_now(&p, 0).unwrap();
        assert!(p.exists() && std::fs::metadata(&p).unwrap().len() == 0);
        assert!(!archive_path(&p, 1).exists());
        let _ = std::fs::remove_dir_all(&tmp);
    }

    fn wait_for(mut done: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !done() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// 2.2/2.4: the shutdown barrier applies everything queued so far, so a
    /// graceful stop leaves archives (never `.tmp` files) on disk.
    #[test]
    fn flush_rotations_drains_the_backlog() {
        let tmp = std::env::temp_dir().join(format!("ak-test-rotflush-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let p = tmp.join("t.log");
        std::fs::write(&p, "first").unwrap();
        rotate_now(&p, 2).unwrap();
        std::fs::write(&p, "second").unwrap();
        rotate_now(&p, 2).unwrap();

        assert!(
            flush_rotations(Duration::from_secs(10)),
            "the worker must acknowledge the barrier"
        );
        assert_eq!(read_gzip(&archive_path(&p, 1)), "second");
        assert_eq!(read_gzip(&archive_path(&p, 2)), "first");
        let leftovers: Vec<String> = std::fs::read_dir(&tmp)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".tmp"))
            .collect();
        assert!(
            leftovers.is_empty(),
            "no .tmp left after the drain: {leftovers:?}"
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    fn read_gzip(path: &Path) -> String {
        use std::io::Read;
        let mut text = String::new();
        flate2::read::GzDecoder::new(File::open(path).unwrap())
            .read_to_string(&mut text)
            .expect("archive must be valid gzip");
        text
    }

    /// 2.3: gzip failure degrades to an uncompressed `.1` — content survives,
    /// no `.1.gz` is left behind, and the pump keeps draining.
    #[test]
    fn compression_failure_keeps_an_uncompressed_archive() {
        let tmp = std::env::temp_dir().join(format!("ak-test-rotfail-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let current = tmp.join("t.log");
        let src = tmp.join("full.tmp");
        std::fs::write(&src, "payload").unwrap();
        // A directory where the gzip temp file would go makes compression fail.
        std::fs::create_dir(tmp.join("t.log.1.gz.tmp")).unwrap();

        compress_archive(RotationJob {
            tmp: src.clone(),
            current: current.clone(),
            keep: 2,
        });
        assert_eq!(
            std::fs::read_to_string(numbered(&current, 1)).unwrap(),
            "payload",
            "the archive is kept uncompressed"
        );
        assert!(
            !archive_path(&current, 1).exists(),
            "no partial .1.gz is exposed"
        );
        assert!(!src.exists(), "the temp file was consumed");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// 2.3: a full/disconnected queue takes the same degradation path, so the
    /// pump thread never blocks on compression.
    #[test]
    fn queue_full_degrades_to_an_uncompressed_archive() {
        let tmp = std::env::temp_dir().join(format!("ak-test-rotfull-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let current = tmp.join("t.log");
        let src = tmp.join("full.tmp");
        std::fs::write(&src, "payload").unwrap();
        keep_uncompressed(&src, &current, 2);
        assert_eq!(
            std::fs::read_to_string(numbered(&current, 1)).unwrap(),
            "payload"
        );
        assert!(!src.exists());
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// 3.2/3.3: the daemon's own log rotates through the same writer —
    /// gzip archives, bounded retention, no legacy `.N` left behind.
    #[test]
    fn rotating_writer_rotates_and_bounds_archives() {
        let tmp = std::env::temp_dir().join(format!("ak-test-daemonlog-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let p = tmp.join("daemon.out.log");
        let mut w = RotatingWriter::open(p.clone(), 1024, 2).unwrap();
        let line = format!("daemon-event {}\n", "x".repeat(300));
        for _ in 0..30 {
            w.write_all(line.as_bytes()).unwrap();
        }
        w.flush().unwrap();

        let newest = archive_path(&p, 1);
        let older = archive_path(&p, 2);
        wait_for(|| newest.exists() && older.exists());
        assert!(read_gzip(&newest).contains("daemon-event"));
        assert!(read_gzip(&older).contains("daemon-event"));
        assert!(
            !archive_path(&p, 3).exists(),
            "retention is bounded by `keep`"
        );
        assert!(!numbered(&p, 1).exists(), "no legacy .1 archive");
        assert!(p.exists(), "the live file is recreated after rotation");
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
