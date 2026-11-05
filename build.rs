//! Build script: build the embedded web console frontend automatically.
//!
//! `webui/` is a Vite + Lit SPA whose `dist/` output is compiled into the
//! binary via rust-embed (release builds only — debug builds read from disk
//! at runtime). On every `cargo build` this script decides what to do:
//!
//! 1. Debug builds (`PROFILE=debug`) skip the frontend toolchain by default:
//!    no pnpm invocation, so Rust iteration stays fast. Whatever `dist/` is
//!    on disk gets used (rust-embed serves it live from disk in debug); if
//!    dist is missing a placeholder entry page is written so the server has
//!    something to serve. `AK_WEBUI_BUILD=force` opts back into the
//!    automatic build.
//! 2. Release builds (or debug + `force`) keep the bundle current: when any
//!    webui input (`src/`, configs, lockfile) is newer than the bundle — or
//!    the bundle is missing, or a previous run left the placeholder marker —
//!    run the frontend build: `pnpm install` when dependencies are missing
//!    or the lockfile changed, then `pnpm build`.
//! 3. When the Node toolchain is unavailable (or the build fails), fall back
//!    to a placeholder entry page so `cargo test` and docs builds stay green
//!    on machines without Node; a `cargo:warning` says so the first time.
//! 4. A stamp over the dist contents is emitted as a `rustc-env`, so a
//!    changed bundle recompiles the crate (and is re-embedded) even when no
//!    Rust source changed.
//!
//! Set `AK_WEBUI_BUILD=skip` to never invoke the frontend toolchain and
//! use whatever `dist/` is already on disk (used by CI jobs without a Node
//! setup that don't serve the real UI). The legacy `AK_WEBUI_BUILD`
//! variable is still honored with lower precedence.
//!
//! Independently of the frontend mode, every build emits this build's
//! identity as `rustc-env` variables (show-build-version D1): git commit,
//! build time, target triple and rustc version. Each one degrades to
//! `unknown` instead of failing the build.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::SystemTime;

fn main() {
    // Build identity comes first: the frontend branch below may return early
    // (BuildMode::Skip) and the binary must still carry its metadata
    // (show-build-version D1).
    emit_build_metadata();

    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let webui = manifest.join("webui");
    let dist = webui.join("dist");

    // Watch exactly the webui inputs plus the output directory. Writing dist
    // does re-trigger this script (that is how a manually-run `pnpm build`
    // gets picked up), but the freshness check and the stamp keep those
    // reruns cheap and make the rebuild settle after one pass.
    for watched in [
        "webui/src",
        "webui/index.html",
        "webui/package.json",
        "webui/pnpm-lock.yaml",
        "webui/vite.config.ts",
        "webui/tsconfig.json",
        "webui/dist",
    ] {
        println!("cargo:rerun-if-changed={watched}");
    }

    let index = dist.join("index.html");
    let marker = dist.join(".ak-placeholder");

    match build_mode() {
        BuildMode::Skip => {
            if !index.exists() {
                write_placeholder(&dist);
            }
            println!("cargo:rustc-env=AK_WEBUI_STAMP=skip");
            return;
        }
        BuildMode::Auto | BuildMode::Force => {}
    }

    let inputs = [
        webui.join("src"),
        webui.join("index.html"),
        webui.join("package.json"),
        webui.join("pnpm-lock.yaml"),
        webui.join("vite.config.ts"),
        webui.join("tsconfig.json"),
    ];
    let newest_input = newest_mtime(&inputs);
    let stale = marker.exists()
        || newest_input
            .is_none_or(|newest| !index.exists() || mtime(&index).is_some_and(|t| t < newest));

    if stale && !build_webui(&webui, &dist) && !index.exists() {
        // No usable bundle and no toolchain (or the build failed): embed a
        // placeholder page rather than failing the Rust build.
        write_placeholder(&dist);
        println!(
            "cargo:warning=webui bundle missing and Node toolchain unavailable \
             (pnpm/corepack not found) — embedded a placeholder page; install \
             Node >= 22 + pnpm and rebuild for the real UI"
        );
    }

    println!("cargo:rustc-env=AK_WEBUI_STAMP={}", stamp(&dist));
}

/// Collect this build's identity and export it to the crate as `rustc-env`
/// variables (show-build-version D1). Every probe degrades to `unknown` on
/// failure — a source tarball without git, or a rustc that cannot be spawned,
/// must never break the build.
fn emit_build_metadata() {
    // The pinned timestamp is an input like any other: changing it must
    // re-run this script rather than reuse the previous build's value.
    println!("cargo:rerun-if-env-changed=SOURCE_DATE_EPOCH");

    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    watch_git(&manifest);

    let git = git_commit(&manifest).unwrap_or_else(|| {
        warn_metadata("git metadata unavailable — AK_BUILD_GIT=unknown");
        "unknown".to_string()
    });
    let built_at = build_time();
    let target = std::env::var("TARGET").unwrap_or_else(|_| "unknown".to_string());
    let rustc = rustc_version(&manifest).unwrap_or_else(|| {
        warn_metadata("rustc --version failed — AK_BUILD_RUSTC=unknown");
        "unknown".to_string()
    });

    println!("cargo:rustc-env=AK_BUILD_GIT={git}");
    println!("cargo:rustc-env=AK_BUILD_TIME={built_at}");
    println!("cargo:rustc-env=AK_BUILD_TARGET={target}");
    println!("cargo:rustc-env=AK_BUILD_RUSTC={rustc}");
}

/// Emit a `cargo:warning` for a degraded build-identity probe. Both probes
/// can only fail in unusual environments, so this does not spam normal builds.
fn warn_metadata(msg: &str) {
    println!("cargo:warning={msg}");
}

/// Re-run this script when the checked-out commit changes. Without these
/// watches only a Rust source edit would refresh the embedded git hash.
///
/// Only a real git directory has `HEAD`; in a linked worktree `.git` is a
/// file pointing at the common gitdir, so these paths do not exist — and
/// cargo treats a missing rerun-if-changed path as dirty, which would re-run
/// this script (and recompile the crate) on every single build. Probing
/// `.git/HEAD` skips the watch there instead.
fn watch_git(repo: &Path) {
    let git = repo.join(".git");
    if !git.join("HEAD").exists() {
        return;
    }
    for watched in [".git/HEAD", ".git/refs"] {
        println!("cargo:rerun-if-changed={watched}");
    }
}

/// `<short-hash>` from `git rev-parse`, with a `-dirty` suffix when tracked
/// files have uncommitted changes (untracked files do not count — they are
/// not part of the build). `None` when the probe fails outright.
fn git_commit(repo: &Path) -> Option<String> {
    let hash = command_output(repo, "git", &["rev-parse", "--short=12", "HEAD"])?;
    let dirty = command_output(
        repo,
        "git",
        &["status", "--porcelain", "--untracked-files=no"],
    )
    .map(|out| !out.is_empty())
    .unwrap_or(false);
    Some(if dirty { format!("{hash}-dirty") } else { hash })
}

/// RFC 3339 UTC build timestamp. `SOURCE_DATE_EPOCH` wins when set so release
/// pipelines can reproduce a build; otherwise the system clock is used. The
/// conversion is a small dependency-free epoch→civil calculation — pulling
/// `chrono` into a build script is not worth it.
fn build_time() -> String {
    let secs = std::env::var("SOURCE_DATE_EPOCH")
        .ok()
        .and_then(|raw| raw.trim().parse::<i64>().ok())
        .unwrap_or_else(|| {
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0)
        });
    format_rfc3339_utc(secs)
}

/// Seconds since the Unix epoch → `YYYY-MM-DDTHH:MM:SSZ` (UTC, no leap
/// seconds). Handles pre-1970 timestamps too (negative input).
fn format_rfc3339_utc(epoch_secs: i64) -> String {
    let days = epoch_secs.div_euclid(86_400);
    let rem = epoch_secs.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    let hour = rem / 3600;
    let minute = (rem % 3600) / 60;
    let second = rem % 60;
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

/// Days since 1970-01-01 → proleptic Gregorian (year, month, day). Howard
/// Hinnant's `civil_from_days`: exact for the whole `i64` range we can reach.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// `rustc --version` output, or `None` when the compiler cannot be probed.
fn rustc_version(dir: &Path) -> Option<String> {
    let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".to_string());
    command_output(dir, &rustc, &["--version"])
}

/// Run a command and return its trimmed stdout when it exits successfully.
fn command_output(dir: &Path, program: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(program)
        .args(args)
        .current_dir(dir)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!text.is_empty()).then_some(text)
}

enum BuildMode {
    /// Never invoke the frontend toolchain; use disk as-is.
    Skip,
    /// Release behavior: build when inputs are newer than the bundle.
    Auto,
    /// Like Auto, but honored in debug builds too.
    Force,
}

/// Debug builds skip the frontend toolchain by default; `skip` forces that
/// off-switch everywhere and `force` opts debug builds into the automatic
/// build. There is no legacy compatibility fallback — only `AK_WEBUI_BUILD`
/// is read (design D2: no shim for the pre-rename env var).
fn build_mode() -> BuildMode {
    let raw = std::env::var("AK_WEBUI_BUILD").unwrap_or_default();
    match raw.as_str() {
        "skip" | "0" | "false" => BuildMode::Skip,
        "force" => BuildMode::Force,
        _ => {
            let debug = matches!(std::env::var("PROFILE").as_deref(), Ok("debug") | Err(_));
            if debug {
                BuildMode::Skip
            } else {
                BuildMode::Auto
            }
        }
    }
}

/// Run the frontend toolchain: install deps when needed, then build. The
/// output directory is wiped first so a failed build cannot leave a stale
/// bundle that looks fresh. Returns false on any failure (dist may be gone).
fn build_webui(webui: &Path, dist: &Path) -> bool {
    let Some(pnpm) = discover_pnpm() else {
        return false;
    };

    let lock = webui.join("pnpm-lock.yaml");
    let modules = webui.join("node_modules/.pnpm");
    let need_install = !modules.exists()
        || mtime(&lock).is_some_and(|lock_t| mtime(&modules).is_none_or(|m_t| lock_t > m_t));

    let _ = fs::remove_dir_all(dist);
    if need_install
        && !run(
            &pnpm,
            webui,
            &["install", "--frozen-lockfile"],
            "pnpm install",
        )
    {
        return false;
    }
    run(&pnpm, webui, &["build"], "pnpm build")
}

/// Locate pnpm: a `pnpm` on PATH first, then `corepack pnpm` (Node >= 22
/// ships corepack and package.json pins the version via `packageManager`).
fn discover_pnpm() -> Option<Vec<String>> {
    for prefix in [
        vec!["pnpm".to_string()],
        vec!["corepack".to_string(), "pnpm".to_string()],
    ] {
        let ok = Command::new(&prefix[0])
            .args(&prefix[1..])
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if ok {
            return Some(prefix);
        }
    }
    None
}

fn run(cmd: &[String], dir: &Path, args: &[&str], what: &str) -> bool {
    let output = Command::new(&cmd[0])
        .args(&cmd[1..])
        .args(args)
        .current_dir(dir)
        .output();
    match output {
        Ok(out) if out.status.success() => true,
        Ok(out) => {
            eprintln!(
                "{what} failed:\n{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
            println!("cargo:warning={what} failed — see output above");
            false
        }
        Err(err) => {
            eprintln!("{what} could not start: {err}");
            println!("cargo:warning={what} failed — see output above");
            false
        }
    }
}

fn write_placeholder(dist: &Path) {
    fs::create_dir_all(dist).expect("create webui dist directory");
    fs::write(
        dist.join("index.html"),
        "<!doctype html><html><body><h1>appkeeper webui</h1><p>Frontend bundle not built \
         yet. Install Node >= 22 + pnpm and run <code>cargo build</code> — the \
         frontend is built automatically for release builds (debug: \
         <code>AK_WEBUI_BUILD=force</code> or <code>pnpm build</code>).</p>\
         </body></html>\n",
    )
    .expect("write placeholder index.html");
    fs::write(dist.join(".ak-placeholder"), "placeholder\n").expect("write placeholder marker");
}

/// Newest mtime across the given paths, recursing into directories.
fn newest_mtime(paths: &[PathBuf]) -> Option<SystemTime> {
    paths.iter().filter_map(|p| walk_mtime(p)).max()
}

fn walk_mtime(path: &Path) -> Option<SystemTime> {
    let meta = fs::metadata(path).ok()?;
    if !meta.is_dir() {
        return meta.modified().ok();
    }
    let mut best = meta.modified().ok();
    for entry in fs::read_dir(path).ok()?.flatten() {
        best = best.max(walk_mtime(&entry.path()));
    }
    best
}

fn mtime(path: &Path) -> Option<SystemTime> {
    fs::metadata(path).and_then(|m| m.modified()).ok()
}

/// A stable hash over the dist contents (path, size, mtime). Emitted as a
/// `rustc-env`: when the bundle changes, the build output changes, which
/// makes cargo recompile the crate so rust-embed re-reads it.
fn stamp(dist: &Path) -> String {
    let mut files: Vec<(String, u64, u128)> = Vec::new();
    collect(dist, dist, &mut files);
    files.sort();
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for (path, len, mtime) in files {
        for byte in path.as_bytes() {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x100_0000_01b3);
        }
        for value in [u128::from(len), mtime] {
            for byte in value.to_le_bytes() {
                hash ^= u64::from(byte);
                hash = hash.wrapping_mul(0x100_0000_01b3);
            }
        }
    }
    format!("{hash:016x}")
}

fn collect(root: &Path, dir: &Path, out: &mut Vec<(String, u64, u128)>) {
    for entry in fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect(root, &path, out);
        } else {
            let rel = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .to_string_lossy()
                .into_owned();
            let meta = entry.metadata();
            out.push((
                rel,
                meta.as_ref().map(|m| m.len()).unwrap_or(0),
                meta.ok()
                    .and_then(|m| m.modified().ok())
                    .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
                    .map(|d| d.as_nanos())
                    .unwrap_or(0),
            ));
        }
    }
}
