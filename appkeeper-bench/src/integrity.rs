//! On-disk log reconciliation (design D6): walk the live file plus all
//! rotation files in rotation order and verify the embedded per-stream seqs
//! are contiguous across the retained window — no gaps, no duplicates, no
//! disorder. Rows the rotation policy deliberately deleted are not expected:
//! the expectation is derived from the retention window (benchmark spec: 实际
//! 读到的行数 SHALL 与按轮转保留策略推算的期望一致).

use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq)]
pub struct StreamCheck {
    pub expected: u64,
    pub found: u64,
    pub pass: bool,
    pub problems: Vec<String>,
}

/// Rotation archives for `live`, newest-first, with their slot number. Both
/// the compressed `live.N.gz` (normal rotation) and the pre-compression
/// `live.N` (degraded rotation, or left by an older release) forms count; if
/// both exist for one slot the compressed file wins so lines are read once.
fn rotated_archives(live: &Path) -> Vec<(u32, PathBuf)> {
    let suffix = live
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    // (slot, path, compressed)
    let mut rotated: Vec<(u32, PathBuf, bool)> = Vec::new();
    if let Some(parent) = live.parent()
        && let Ok(entries) = std::fs::read_dir(parent)
    {
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if let Some(num) = name.strip_prefix(&suffix)
                && let Some(n) = num.strip_prefix('.')
            {
                let (digits, compressed) = match n.strip_suffix(".gz") {
                    Some(d) => (d, true),
                    None => (n, false),
                };
                if let Ok(n) = digits.parse::<u32>() {
                    rotated.push((n, e.path(), compressed));
                }
            }
        }
    }
    // One entry per slot, compressed first, then newest slot first.
    rotated.sort_by(|a, b| a.0.cmp(&b.0).then(b.2.cmp(&a.2)));
    rotated.dedup_by_key(|(n, _, _)| *n);
    rotated.sort_by_key(|(n, _, _)| std::cmp::Reverse(*n));
    rotated
        .into_iter()
        .map(|(n, p, _)| (n, p))
        .collect::<Vec<_>>()
}

/// `live` is `<name>.<stream>.log`; rotation archives are `live.1.gz`,
/// `live.2.gz`, ... (pump shifts live -> .1.gz -> ... -> .keep.gz), or the
/// pre-compression `live.N` form left by a degraded rotation. Rotation order
/// is oldest first, i.e. the HIGHEST suffix first, then down to the live file.
#[cfg(test)]
pub fn rotation_chain(live: &Path) -> Vec<PathBuf> {
    let mut chain = rotated_archives(live)
        .into_iter()
        .map(|(_, p)| p)
        .collect::<Vec<_>>();
    chain.push(live.to_path_buf());
    chain
}

/// Read one log file, transparently decompressing a gzip archive.
fn read_log(path: &Path) -> std::io::Result<Vec<u8>> {
    if path.extension().and_then(|e| e.to_str()) != Some("gz") {
        return std::fs::read(path);
    }
    use std::io::Read;
    let mut bytes = Vec::new();
    flate2::read::GzDecoder::new(std::fs::File::open(path)?).read_to_end(&mut bytes)?;
    Ok(bytes)
}

/// Number of rotation files that exist for this stream (the report's
/// rotation_count sums this over all programs and streams).
pub fn count_rotated(live: &Path) -> u64 {
    rotated_archives(live).len() as u64
}

/// True while the daemon still has a rotation temp file for either stream of
/// `name` — i.e. a compression job is queued or in flight, so the archive set
/// is about to change. Reconciliation MUST NOT start before this settles.
pub fn rotations_pending(log_dir: &Path, name: &str) -> bool {
    let Ok(entries) = std::fs::read_dir(log_dir) else {
        return false;
    };
    for e in entries.flatten() {
        let n = e.file_name().to_string_lossy().to_string();
        let Some(rest) = n.strip_prefix(name) else {
            continue;
        };
        if (rest.starts_with(".out.log.") || rest.starts_with(".err.log."))
            && rest.ends_with(".tmp")
        {
            return true;
        }
    }
    false
}

/// Archive slot numbers of both streams, sorted. Two consecutive snapshots
/// being equal means the async shift is not in progress right now.
pub fn rotation_slots(log_dir: &Path, name: &str) -> (Vec<u32>, Vec<u32>) {
    let slots = |stream: &str| {
        let mut v: Vec<u32> = rotated_archives(&log_dir.join(format!("{name}.{stream}.log")))
            .into_iter()
            .map(|(n, _)| n)
            .collect();
        v.sort_unstable();
        v
    };
    (slots("out"), slots("err"))
}

/// Reconcile one stream against `expected` produced rows and `keep` archive
/// slots. When the archive set is full the pump deleted the oldest rows by
/// design, so only the retained window is reconciled; a short archive set
/// means nothing may be missing.
fn check_stream(live: &Path, tag: &str, expected: u64, keep: u32) -> StreamCheck {
    let archives = rotated_archives(live);
    // The shift reached the retention boundary once an archive sits in slot
    // `keep`; from then on the oldest rows are deleted deliberately.
    let max_slot = archives.first().map(|(n, _)| *n).unwrap_or(0);
    let retained_full = keep > 0 && max_slot >= keep;
    let mut problems = Vec::new();
    if retained_full {
        // A boundary-reaching set must be the contiguous run 1..=keep; a hole
        // means an archive was removed or lost (not a retention deletion).
        let mut present: Vec<u32> = archives.iter().map(|(n, _)| *n).collect();
        present.sort_unstable();
        if present != (1..=keep).collect::<Vec<u32>>() {
            problems.push(format!(
                "rotation archive slots are incomplete (present {present:?}, expected 1..={keep}): \
                 archives were removed or lost"
            ));
        }
    }

    let mut found = 0u64;
    // First seq seen on disk; with a full archive set this is the retention
    // boundary, so the rows before it were deleted deliberately.
    let mut first_seq: Option<u64> = None;
    let mut next: Option<u64> = None;
    let chain: Vec<PathBuf> = archives
        .into_iter()
        .map(|(_, p)| p)
        .chain(std::iter::once(live.to_path_buf()))
        .collect();
    for path in chain {
        let Ok(bytes) = read_log(&path) else {
            // A missing live file with expected rows is a real problem; with
            // zero expected rows it is fine (nothing was ever written).
            if expected > 0 {
                problems.push(format!(
                    "cannot read {}: expected rows exist",
                    path.display()
                ));
            }
            continue;
        };
        let mut lines = bytes.split(|&b| b == b'\n').peekable();
        while let Some(raw) = lines.next() {
            // A trailing newline yields one empty tail element.
            if raw.is_empty() && lines.peek().is_none() {
                continue;
            }
            found += 1;
            let line = String::from_utf8_lossy(raw);
            match crate::generate::parse_seq(&line, tag) {
                Some(seq) => {
                    first_seq.get_or_insert(seq);
                    // The oldest surviving file may legitimately start at the
                    // retention boundary; anything else must start at 0.
                    let want = next.unwrap_or(if retained_full { seq } else { 0 });
                    if seq != want {
                        problems.push(format!(
                            "{} line {} (seq {seq}): expected {want} (gap/duplicate/disorder)",
                            path.display(),
                            found
                        ));
                    }
                    next = Some(seq + 1);
                }
                None => problems.push(format!(
                    "{} line {}: not a [{tag}] seq line: {:?}",
                    path.display(),
                    found,
                    line.chars().take(60).collect::<String>()
                )),
            }
        }
    }
    let dropped = if retained_full {
        first_seq.unwrap_or(0)
    } else {
        0
    };
    let kept_expected = expected.saturating_sub(dropped);
    if kept_expected != found {
        problems.push(format!(
            "row count mismatch: expected {kept_expected} rows on disk, found {found}"
        ));
    }
    StreamCheck {
        expected: kept_expected,
        found,
        pass: problems.is_empty(),
        problems,
    }
}

/// Verify both streams of one program. `keep` is the configured archive slot
/// count (`log.keep`); `0` when rotation is disabled.
/// Returns (out_check, err_check).
pub fn verify_program(
    log_dir: &Path,
    name: &str,
    expected_out: u64,
    expected_err: u64,
    keep: u32,
) -> (StreamCheck, StreamCheck) {
    (
        check_stream(
            &log_dir.join(format!("{name}.out.log")),
            "out",
            expected_out,
            keep,
        ),
        check_stream(
            &log_dir.join(format!("{name}.err.log")),
            "err",
            expected_err,
            keep,
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn cleanup(d: &Path) {
        let _ = fs::remove_dir_all(d);
    }

    fn tmpdir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "ak-test-int-{}-{tag}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .subsec_nanos()
        ));
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn line(tag: &str, seq: u64) -> String {
        crate::generate::render_line(tag, seq, 1000, 64).unwrap()
    }

    #[test]
    fn contiguous_single_file_passes() {
        let d = tmpdir("ok");
        let live = d.join("p.out.log");
        let text: String = (0..10).map(|i| format!("{}\n", line("out", i))).collect();
        fs::write(&live, text).unwrap();
        let c = check_stream(&live, "out", 10, 0);
        assert!(c.pass, "{:?}", c.problems);
        assert_eq!(c.found, 10);
        cleanup(&d);
    }

    /// Write `text` as a gzip archive (the normal rotation form).
    fn write_gz(path: &Path, text: &str) {
        use std::io::Write;
        let out = fs::File::create(path).unwrap();
        let mut enc = flate2::write::GzEncoder::new(out, flate2::Compression::fast());
        enc.write_all(text.as_bytes()).unwrap();
        enc.finish().unwrap();
    }

    #[test]
    fn rotation_chain_is_read_oldest_first() {
        let d = tmpdir("rot");
        let live = d.join("p.out.log");
        // pump shifts live -> .1.gz -> .2.gz: .2 holds the oldest rows
        let text = |from: u64, to: u64| {
            let mut s = String::new();
            for i in from..to {
                s.push_str(&format!("{}\n", line("out", i)));
            }
            s
        };
        write_gz(&d.join("p.out.log.2.gz"), &text(0, 5));
        write_gz(&d.join("p.out.log.1.gz"), &text(5, 10));
        fs::write(&live, text(10, 12)).unwrap();
        let chain = rotation_chain(&live);
        assert_eq!(chain.len(), 3);
        assert!(chain[0].to_string_lossy().ends_with(".2.gz"));
        assert!(chain[1].to_string_lossy().ends_with(".1.gz"));
        let c = check_stream(&live, "out", 12, 0);
        assert!(c.pass, "{:?}", c.problems);
        assert_eq!(c.found, 12);
        assert_eq!(count_rotated(&live), 2);
        cleanup(&d);
    }

    /// Pre-upgrade uncompressed archives (`.N`) are still reconciled.
    #[test]
    fn legacy_uncompressed_archives_are_still_read() {
        let d = tmpdir("legacy");
        let live = d.join("p.out.log");
        let text = |from: u64, to: u64| {
            let mut s = String::new();
            for i in from..to {
                s.push_str(&format!("{}\n", line("out", i)));
            }
            s
        };
        fs::write(d.join("p.out.log.1"), text(0, 4)).unwrap();
        fs::write(&live, text(4, 6)).unwrap();
        let c = check_stream(&live, "out", 6, 0);
        assert!(c.pass, "{:?}", c.problems);
        assert_eq!(count_rotated(&live), 1);
        cleanup(&d);
    }

    #[test]
    fn gap_dup_and_disorder_fail() {
        let d = tmpdir("gap");
        let live = d.join("p.out.log");
        fs::write(
            &live,
            format!("{}\n{}\n", line("out", 0), line("out", 2)), // gap at 1
        )
        .unwrap();
        let c = check_stream(&live, "out", 2, 0);
        assert!(!c.pass);
        cleanup(&d);

        let d = tmpdir("dup");
        let live = d.join("p.out.log");
        fs::write(
            &live,
            format!("{}\n{}\n", line("out", 0), line("out", 0)), // duplicate
        )
        .unwrap();
        assert!(!check_stream(&live, "out", 2, 0).pass);
        cleanup(&d);

        let d = tmpdir("disorder");
        let live = d.join("p.out.log");
        fs::write(
            &live,
            format!("{}\n{}\n", line("out", 1), line("out", 0)), // disorder
        )
        .unwrap();
        assert!(!check_stream(&live, "out", 2, 0).pass);
        cleanup(&d);
    }

    #[test]
    fn wrong_tag_or_foreign_lines_fail() {
        let d = tmpdir("tag");
        let live = d.join("p.out.log");
        fs::write(&live, format!("{}\n", line("err", 0))).unwrap(); // err line in out file
        let c = check_stream(&live, "out", 1, 0);
        assert!(!c.pass);
        cleanup(&d);
    }

    #[test]
    fn count_mismatch_fails_even_when_seqs_look_fine() {
        let d = tmpdir("count");
        let live = d.join("p.out.log");
        let text: String = (0..5).map(|i| format!("{}\n", line("out", i))).collect();
        fs::write(&live, text).unwrap();
        let c = check_stream(&live, "out", 7, 0);
        assert!(!c.pass);
        assert_eq!(c.found, 5);
        assert_eq!(c.expected, 7);
        cleanup(&d);
    }

    #[test]
    fn missing_file_with_zero_expected_is_a_pass() {
        let d = tmpdir("empty");
        let c = check_stream(&d.join("nothing.out.log"), "out", 0, 0);
        assert!(c.pass);
        cleanup(&d);
    }

    #[test]
    fn non_numeric_suffixes_are_not_chain_members() {
        // foreign suffixes next to the live file (e.g. editor backups) must
        // not be pulled into the rotation order or break reconciliation
        let d = tmpdir("sfx");
        let live = d.join("p.out.log");
        fs::write(d.join("p.out.log.bak"), "not a bench line\n").unwrap();
        fs::write(&live, format!("{}\n", line("out", 0))).unwrap();
        assert_eq!(count_rotated(&live), 0);
        let c = check_stream(&live, "out", 1, 0);
        assert!(c.pass, "{:?}", c.problems);
        cleanup(&d);
    }

    /// With a full archive set the oldest rows were deleted by the retention
    /// policy, so the expectation shrinks to the retained window — and the
    /// window itself must still be contiguous.
    #[test]
    fn full_archive_set_reconciles_the_retained_window() {
        let d = tmpdir("retain");
        let live = d.join("p.out.log");
        let text = |from: u64, to: u64| {
            let mut s = String::new();
            for i in from..to {
                s.push_str(&format!("{}\n", line("out", i)));
            }
            s
        };
        // keep = 2: `.2.gz` holds the oldest surviving rows (seqs 100..150),
        // everything before seq 100 was rotated away.
        write_gz(&d.join("p.out.log.2.gz"), &text(100, 150));
        write_gz(&d.join("p.out.log.1.gz"), &text(150, 200));
        fs::write(&live, text(200, 210)).unwrap();
        let c = check_stream(&live, "out", 210, 2);
        assert!(c.pass, "{:?}", c.problems);
        assert_eq!(
            c.expected, 110,
            "expectation starts at the retention boundary"
        );
        assert_eq!(c.found, 110);
        cleanup(&d);
    }

    /// A hole inside a full archive set is not a retention deletion: it means
    /// an archive was removed, and reconciliation must fail.
    #[test]
    fn a_missing_archive_slot_fails_even_when_full() {
        let d = tmpdir("hole");
        let live = d.join("p.out.log");
        let text = |from: u64, to: u64| {
            let mut s = String::new();
            for i in from..to {
                s.push_str(&format!("{}\n", line("out", i)));
            }
            s
        };
        // keep = 2 but `.1.gz` is gone: seqs 100..150 and 200..210 are on disk.
        write_gz(&d.join("p.out.log.2.gz"), &text(100, 150));
        fs::write(&live, text(200, 210)).unwrap();
        let c = check_stream(&live, "out", 210, 2);
        assert!(!c.pass, "a removed archive must be reported: {c:?}");
        assert!(
            c.problems.iter().any(|p| p.contains("incomplete")),
            "{:?}",
            c.problems
        );
        cleanup(&d);
    }
}
