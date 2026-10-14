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
    // The old `--connect` flag is gone rather than silently accepted
    // (benchmark spec: 旧的 --connect 不再生效 → 非零码退出并说明替代).
    // `--token` returned with tcp-control-endpoint: `resolve` accepts it for
    // a tcp:// `--socket` and refuses it otherwise.
    if std::env::args()
        .any(|a| a == "--connect" || a.starts_with("--connect="))
    {
        eprintln!(
            "appkeeper-bench: error: --connect has been removed: use --socket <endpoint> \
             (a local IPC path or tcp://host:port)"
        );
        std::process::exit(2);
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
