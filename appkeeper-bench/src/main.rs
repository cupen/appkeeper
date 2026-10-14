//! appkeeper-bench — load benchmark harness for a real appkeeper daemon.
//!
//! Bench is a pure external consumer of the daemon: it drives the existing
//! /v1 control plane (no daemon changes) and reads the load programs' disk
//! logs for integrity reconciliation. Four cases: firehose (single-program
//! full-speed throughput), rotation (small threshold, per-row integrity
//! under high rotation), fanout (N programs × stdout/stderr), drip (slow
//! steady state, daemon RSS boundedness).
//!
//! The load generator is this same binary re-entered via the hidden
//! `__generate` mode, running as an ordinary daemon child (design D2).

mod api;
mod cases;
mod cli;
mod dcfg;
mod generate;
mod integrity;
mod measure;
mod report;
mod rss;
mod topology;

use clap::Parser;

fn main() {
    // The control plane moved from TCP to a local IPC endpoint; `--connect`
    // and `--token` are gone rather than silently accepted (benchmark spec:
    // 旧的 --connect 与 --token 不再生效 → 非零码退出并说明替代).
    for (flag, replacement) in [
        ("--connect", "--socket <path>"),
        ("--token", "nothing (the control plane has no token)"),
    ] {
        if std::env::args().any(|a| a == flag || a.starts_with(&format!("{flag}="))) {
            eprintln!(
                "appkeeper-bench: error: {flag} has been removed: the daemon serves a local IPC \
                 endpoint now — use {replacement}"
            );
            std::process::exit(2);
        }
    }
    let cli = cli::Cli::parse();
    let result = match &cli.cmd {
        Some(cli::Cmd::Generate(g)) => generate::run_generate(g),
        None => measure::run_bench(&cli.bench),
    };
    if let Err(e) = result {
        eprintln!("appkeeper-bench: error: {e:#}");
        std::process::exit(1);
    }
}
