//! Case planning: turn the effective parameters into one app config per load
//! program (design D9's parameter matrix). Each app is a single-program
//! registration whose program is the bench binary itself in `__generate`
//! mode (design D2).

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::cli::{Case, Effective};

/// The registry name prefix shared by every bench-created app (design D7).
pub const BENCH_PREFIX: &str = "appkeeper-bench-";

/// fanout's built-in stderr split (design D2: fixed, not a parameter).
pub const STDERR_EVERY: u32 = 10;

#[derive(Debug)]
pub struct ProgramPlan {
    /// App name == program name (also the log file stem).
    pub name: String,
    /// Full content of `app_dir/<name>.toml`.
    pub toml: String,
    /// Where the generator writes its row/byte counters on exit.
    pub count_file: PathBuf,
    /// Archive slots configured for both streams (`log.keep`); `0` when
    /// rotation is disabled. Integrity uses it to tell deliberate retention
    /// deletion from lost rows.
    pub keep: u32,
}

/// rotation sizing (design D9): a threshold well below the produced volume
/// so many rotations happen, plus keep large enough that a row-bounded run
/// deletes nothing. A duration-only run has no upper bound on the produced
/// volume, so the fallback `keep` may still be exceeded; integrity reconciles
/// the retained window in that case (see `integrity::check_stream`).
fn rotation_sizing(eff: &Effective) -> (u64, u32) {
    let floor = 64 * 1024u64;
    let max_size = floor.max(eff.row_size.saturating_mul(4)); // max(64KB, 4*row_size)
    let per_stream_bytes = eff
        .rows_per_program
        .saturating_mul(eff.row_size.saturating_add(1));
    let keep = if per_stream_bytes > 0 {
        (per_stream_bytes + max_size - 1) / max_size + 2
    } else {
        64
    };
    let keep = keep.min(u32::MAX as u64) as u32;
    (max_size, keep)
}

fn toml_literal(s: &str) -> Result<String> {
    if s.contains('\'') {
        bail!("path {s:?} contains a single quote and cannot be embedded in the app config");
    }
    Ok(format!("'{s}'"))
}

fn render_args(args: &[&str]) -> String {
    args.iter()
        .map(|a| format!("'{a}'"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Build one app config per load program.
pub fn plan(eff: &Effective, exe: &Path, count_dir: &Path) -> Result<Vec<ProgramPlan>> {
    std::fs::create_dir_all(count_dir)
        .with_context(|| format!("create count dir {}", count_dir.display()))?;
    let exe_lit = toml_literal(&exe.display().to_string())?;

    let names: Vec<String> = match eff.case {
        Case::Fanout => (0..eff.programs)
            .map(|i| format!("{BENCH_PREFIX}fanout-{i}"))
            .collect(),
        other => vec![format!("{BENCH_PREFIX}{}", other.as_str())],
    };

    let stderr_every = match eff.case {
        Case::Fanout => STDERR_EVERY,
        _ => 0,
    };
    let (max_size, keep) = match eff.case {
        Case::Rotation => rotation_sizing(eff),
        // Rotation off for the other cases: the disk stays monotonic and the
        // full row set always reconciles (rotation is the rotation case's job).
        _ => (0, 0),
    };

    let mut plans = Vec::new();
    for name in &names {
        let count_file = count_dir.join(format!("{name}.json"));
        let mut gen_args: Vec<String> = vec![
            "__generate".into(),
            "--row-size".into(),
            eff.row_size.to_string(),
            "--rows".into(),
            eff.rows_per_program.to_string(),
            "--total-size".into(),
            eff.total_per_program().to_string(),
            "--duration".into(),
            eff.duration.to_string(),
            "--stderr-every".into(),
            stderr_every.to_string(),
            "--count-file".into(),
            count_file.display().to_string(),
        ];
        if let Some(r) = eff.rate {
            gen_args.push("--rate".into());
            gen_args.push(r.to_string());
        }
        let gen_args: Vec<&str> = gen_args.iter().map(String::as_str).collect();

        let log_table = if max_size == 0 {
            // Explicit rotation opt-out.
            "log = { max_size = \"0\" }".to_string()
        } else {
            format!("log = {{ max_size = \"{max_size}\", keep = {keep} }}")
        };

        let toml = format!(
            "[app]\nrestart = \"on-failure\"\n\n[program.{name}]\ncommand = {exe_lit}\nargs = [{args}]\nstartsecs = 0\n{log_table}\n",
            args = render_args(&gen_args),
        );
        plans.push(ProgramPlan {
            name: name.clone(),
            toml,
            count_file,
            keep: if max_size == 0 { 0 } else { keep },
        });
    }
    Ok(plans)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn eff(case: Case) -> Effective {
        Effective {
            case,
            rows_per_program: 0,
            row_size: 128,
            total_all: 0,
            rate: None,
            duration: 30,
            programs: 1,
        }
    }

    fn fake_exe() -> PathBuf {
        if cfg!(windows) {
            PathBuf::from("C:\\fake\\appkeeper-bench.exe")
        } else {
            PathBuf::from("/fake/appkeeper-bench")
        }
    }

    #[test]
    fn firehose_is_one_program_with_rotation_off() {
        let dir = std::env::temp_dir().join(format!("ak-test-plan-{}-f", std::process::id()));
        let plans = plan(&eff(Case::Firehose), &fake_exe(), &dir).unwrap();
        assert_eq!(plans.len(), 1);
        assert_eq!(plans[0].name, "appkeeper-bench-firehose");
        assert!(plans[0].toml.contains("log = { max_size = \"0\" }"));
        assert!(plans[0].toml.contains("restart = \"on-failure\""));
        assert!(plans[0].toml.contains("__generate"));
        assert!(plans[0].toml.contains("--stderr-every', '0"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn fanout_spawns_named_programs_with_split() {
        let mut e = eff(Case::Fanout);
        e.programs = 3;
        let dir = std::env::temp_dir().join(format!("ak-test-plan-{}-n", std::process::id()));
        let plans = plan(&e, &fake_exe(), &dir).unwrap();
        assert_eq!(
            plans.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(),
            vec![
                "appkeeper-bench-fanout-0",
                "appkeeper-bench-fanout-1",
                "appkeeper-bench-fanout-2",
            ]
        );
        for p in &plans {
            assert!(
                p.toml.contains("--stderr-every', '10"),
                "fanout splits 10:1"
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rotation_uses_design_d9_sizing() {
        let mut e = eff(Case::Rotation);
        e.rows_per_program = 200_000;
        let dir = std::env::temp_dir().join(format!("ak-test-plan-{}-r", std::process::id()));
        let plans = plan(&e, &fake_exe(), &dir).unwrap();
        // 200000 * 129 bytes = 25.8MB; threshold 65536 → 394 rotations + 2
        assert!(
            plans[0].toml.contains("max_size = \"65536\""),
            "{}",
            plans[0].toml
        );
        assert!(plans[0].toml.contains("keep = 396"), "{}", plans[0].toml);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rotation_without_row_bound_falls_back_to_a_generous_keep() {
        let dir = std::env::temp_dir().join(format!("ak-test-plan-{}-u", std::process::id()));
        let plans = plan(&eff(Case::Rotation), &fake_exe(), &dir).unwrap();
        assert!(plans[0].toml.contains("keep = 64"));
        assert!(plans[0].toml.contains("max_size = \"65536\""));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn total_size_is_split_across_fanout_programs() {
        let mut e = eff(Case::Fanout);
        e.programs = 4;
        e.total_all = 1000;
        let dir = std::env::temp_dir().join(format!("ak-test-plan-{}-s", std::process::id()));
        let plans = plan(&e, &fake_exe(), &dir).unwrap();
        for p in &plans {
            assert!(p.toml.contains("'--total-size', '250'"), "{}", p.toml);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn drip_keeps_explicit_rate_in_generator_args() {
        let mut e = eff(Case::Drip);
        e.rate = Some(100);
        let dir = std::env::temp_dir().join(format!("ak-test-plan-{}-d", std::process::id()));
        let plans = plan(&e, &fake_exe(), &dir).unwrap();
        assert!(
            plans[0].toml.contains("'--rate', '100'"),
            "{}",
            plans[0].toml
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn paths_with_single_quotes_are_refused() {
        let dir = std::env::temp_dir().join(format!("ak-test-plan-{}-q", std::process::id()));
        let err = plan(&eff(Case::Firehose), &PathBuf::from("/fake'quote"), &dir).unwrap_err();
        assert!(format!("{err:#}").contains("single quote"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
