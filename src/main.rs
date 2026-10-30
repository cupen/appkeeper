//! appkeeper — a cross-platform, application-layer process keeper with a TOML
//! layered config (global daemon config + per-app deployment files).
//!
//! `ak daemon start` is the daemon entry (the only one); everything else is a
//! client: `start`/`stop`/`restart` control registered apps (control-plane
//! actions over the HTTP/1.1 API carried on a local IPC endpoint), `run`
//! registers and starts an ad-hoc command line, `ps` is the runtime overview,
//! and compose/remove manage the app registry (app_dir links).

mod action;
mod api;
mod assets;
mod client;
mod config;
mod daemonize;
mod health;
mod metrics;
mod platform;
mod program;
mod pump;
mod registry;
mod server;
mod service;
mod shell;
mod shutdown_signal;
mod supervisor;
mod transport;
mod web;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use clap::{CommandFactory, Parser, Subcommand};
use log::info;

use crate::config::DaemonConfig;
use crate::registry::AddOptions;
use crate::supervisor::Supervisor;

const EXIT_OK: i32 = 0;
const EXIT_ERROR: i32 = 1;
const EXIT_CONFIG: i32 = 2;
/// Client contract: 3 = daemon unreachable (mapped by `client::exit_code_of`).
const EXIT_UNREACHABLE: i32 = 3;

#[derive(Parser, Debug)]
#[command(
    name = "ak",
    version,
    about = "Cross-platform application-layer process keeper (daemon + CLI)",
    after_help = "Examples:
  ak daemon start   # start the daemon in the background
  ak start <app>    # start an app (all its programs, in order)
  ak stop <app>     # stop an app"
)]
struct Cli {
    /// Path to the daemon config file (default: platform location)
    #[arg(short = 'c', long, global = true)]
    config: Option<PathBuf>,

    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Run or manage the daemon (start / install / uninstall / shutdown)
    Daemon {
        #[command(subcommand)]
        cmd: Option<DaemonCmd>,
    },
    /// Start an app (all its programs, in order) or one program of it
    Start {
        /// App name
        #[arg(value_name = "APP")]
        app: String,
        /// Only this program of the app (default: the whole app)
        #[arg(value_name = "PROGRAM")]
        program: Option<String>,
        /// Machine-readable JSON output
        #[arg(long = "json")]
        json: bool,
    },
    /// Register and start a new command line (ad-hoc; daemon required)
    Run {
        /// The command line: everything after `--`
        #[arg(
            last = true,
            num_args = 1..,
            allow_hyphen_values = true,
            value_name = "CMD",
            conflicts_with = "cmd_str"
        )]
        cmd: Vec<String>,
        /// The command line as one shell-like string
        #[arg(long = "cmd", value_name = "COMMAND")]
        cmd_str: Option<String>,
        /// Ad-hoc app name (default: the command file stem)
        #[arg(long = "app", value_name = "APP")]
        scaffold_name: Option<String>,
        /// Ad-hoc app description
        #[arg(long = "description")]
        description: Option<String>,
        /// Ad-hoc restart policy: no | on-failure | always | unless-stopped
        #[arg(long = "restart", value_parser = ["no", "on-failure", "always", "unless-stopped"])]
        restart: Option<String>,
        /// Ad-hoc first restart delay in seconds
        #[arg(long = "restart-backoff")]
        restart_backoff: Option<f64>,
        /// Ad-hoc environment variable K=V (repeatable)
        #[arg(long = "env")]
        envs: Vec<String>,
        /// Ad-hoc working directory (default: current directory)
        #[arg(long = "workdir")]
        workdir: Option<PathBuf>,
        /// Machine-readable JSON output
        #[arg(long = "json")]
        json: bool,
    },
    /// Validate the daemon config and all registered apps
    Validate {
        /// Validate only this app config file
        path: Option<PathBuf>,
    },
    /// Manage the daemon config file (--init/--set/--get/--delete/--edit/--reload)
    Config {
        /// Set a daemon config key (repeatable): --set log_level=debug
        #[arg(long = "set", value_name = "KEY=VALUE", conflicts_with_all = ["get", "delete", "edit", "init"])]
        set: Vec<String>,
        /// Print a key's effective value (repeatable)
        #[arg(long = "get", value_name = "KEY", conflicts_with_all = ["delete", "edit", "init", "reload"])]
        get: Vec<String>,
        /// Remove a key or whole section (repeatable), e.g. --delete webui
        #[arg(long = "delete", value_name = "KEY", conflicts_with_all = ["edit", "init"])]
        delete: Vec<String>,
        /// Edit the config in $VISUAL/$EDITOR, then validate it
        #[arg(long = "edit", conflicts_with = "init")]
        edit: bool,
        /// Create a fresh config, app dir and example template
        #[arg(long = "init", conflicts_with = "reload")]
        init: bool,
        /// Ask the daemon to re-read the config and rescan (daemon required)
        #[arg(long = "reload")]
        reload: bool,
    },
    /// Show programs and their state
    Ps {
        /// Include registered apps that are not running (offline: local list)
        #[arg(long = "all")]
        all: bool,
        /// Output format: table (default) or json
        #[arg(long = "out", value_parser = ["json", "table"], default_value = "table")]
        out: String,
    },
    /// Stop an app (all its programs, in reverse order) or one program of it
    Stop {
        /// App name
        #[arg(value_name = "APP")]
        app: String,
        /// Only this program of the app (default: the whole app)
        #[arg(value_name = "PROGRAM")]
        program: Option<String>,
    },
    /// Restart an app (all its programs) or one program of it
    Restart {
        /// App name
        #[arg(value_name = "APP")]
        app: String,
        /// Only this program of the app (default: the whole app)
        #[arg(value_name = "PROGRAM")]
        program: Option<String>,
    },
    /// Run a custom action of one program (exit code passed through)
    Action {
        /// Program name (a single-program app's program has the app's name)
        #[arg(value_name = "PROGRAM")]
        program: String,
        /// Action name
        #[arg(value_name = "ACTION")]
        name: String,
    },
    /// Send a whitelist signal to one program (unix only)
    Signal {
        /// Program name (a single-program app's program has the app's name)
        #[arg(value_name = "PROGRAM")]
        program: String,
        /// Signal: TERM | INT | HUP | QUIT | USR1 | USR2
        #[arg(value_name = "SIGNAL")]
        signal: String,
    },
    /// Print a program's PID
    Pid {
        /// Program name (a single-program app's program has the app's name)
        #[arg(value_name = "PROGRAM")]
        name: String,
    },
    /// Show or follow program logs
    Log {
        /// One or more program names
        #[arg(required = true, value_name = "PROGRAM")]
        names: Vec<String>,
        /// Lines to show (default 20)
        #[arg(long = "tail", default_value = "20")]
        tail: usize,
        /// Follow new output
        #[arg(short = 'f', long = "follow")]
        follow: bool,
        /// Which stream: out (default), err, or both
        #[arg(long = "stream", value_parser = ["out", "err", "both"], default_value = "out")]
        stream: String,
    },
    /// Apply pending config changes (all, or one app/program)
    Apply {
        /// App name (`all` = everything, the default)
        #[arg(default_value = None)]
        app: Option<String>,
        /// Program name within that app
        #[arg(default_value = None)]
        program: Option<String>,
        /// Also restart unchanged programs (user-stopped ones stay stopped)
        #[arg(long = "restart")]
        restart: bool,
    },
    /// Register an app (deploy dir, config file, or executable)
    Compose {
        /// App dir, config file, or executable to scaffold from
        #[arg(default_value = ".")]
        path: PathBuf,
        /// App name (default: directory or executable name; `all` is reserved)
        #[arg(long = "name")]
        name: Option<String>,
        #[arg(long = "description")]
        description: Option<String>,
        /// Restart policy for this app: no | on-failure | always | unless-stopped
        #[arg(long = "restart", value_parser = ["no", "on-failure", "always", "unless-stopped"])]
        restart: Option<String>,
        #[arg(long = "restart-backoff")]
        restart_backoff: Option<f64>,
        #[arg(long = "priority")]
        priority: Option<i32>,
        /// (scaffold) startup args as one string: --args "--port 8080"
        #[arg(long = "args")]
        args: Option<String>,
        /// (scaffold) environment variable K=V (repeatable)
        #[arg(long = "env")]
        envs: Vec<String>,
        /// (scaffold) working directory (default: current directory)
        #[arg(long = "workdir")]
        workdir: Option<PathBuf>,
        /// Apply this app right after registering
        #[arg(long = "apply")]
        apply: bool,
    },
    /// Unregister an app: only when it has no running/starting/stopping/
    /// backoff program; the daemon removes it immediately (no apply needed)
    Remove {
        /// App name
        #[arg(value_name = "APP")]
        name: String,
    },
    /// Interactive shell client (supervisorctl style)
    Shell {
        /// Execute one command and exit
        #[arg(short = 'e', long = "exec")]
        cmd: Option<String>,
    },
}

/// The `daemon` subcommand family (control-plane + background-daemon specs).
/// `start` runs the supervisor; `install`/`uninstall` manage the OS service
/// registration (former `ak service` family, flags unchanged);
/// `shutdown` stops every program and exits the daemon over the control
/// plane (former top-level `shutdown`).
#[derive(Subcommand, Debug)]
enum DaemonCmd {
    /// Start the daemon (background by default)
    Start {
        /// Keep it attached to the terminal (Ctrl+C; SIGTERM/SIGQUIT on unix)
        #[arg(long = "foreground", conflicts_with = "service")]
        foreground: bool,
        /// Internal: run under the Windows SCM with this service name
        /// (set by the installer, never typed by hand)
        #[arg(long, value_name = "NAME", hide = true)]
        service: Option<String>,
    },
    /// Register appkeeper as a system service (root/admin required)
    Install {
        /// Start the service right after installing
        #[arg(long, visible_alias = "now")]
        start: bool,
        /// Overwrite an existing service/unit with different settings
        #[arg(long)]
        force: bool,
        /// Linux only: unit file path (default /etc/systemd/system/ak.service)
        #[arg(long, value_name = "PATH")]
        unit_file: Option<PathBuf>,
        /// Windows only: service name (default "ak")
        #[arg(long, value_name = "NAME")]
        name: Option<String>,
        /// Service description (default "appkeeper process keeper")
        #[arg(long, value_name = "TEXT")]
        desc: Option<String>,
        /// Account to run the service as (Linux: User=; Windows: LocalSystem /
        /// LocalService / NetworkService)
        #[arg(long)]
        user: Option<String>,
        /// systemd Restart= policy
        #[arg(long, value_name = "POLICY", default_value_t = service::RestartPolicy::Always)]
        restart: service::RestartPolicy,
    },
    /// Unregister the appkeeper system service
    Uninstall {
        /// Linux only: unit file path to remove
        #[arg(long, value_name = "PATH")]
        unit_file: Option<PathBuf>,
        /// Windows only: service name (default "ak")
        #[arg(long, value_name = "NAME")]
        name: Option<String>,
    },
    /// Stop all programs and exit the daemon
    Shutdown,
}

fn main() {
    // The old systemd ExecStart / Windows binPath `daemon --config <path>`
    // is a usage error now; intercept it first so the message also names the
    // unit regeneration (service-registration upgrade compatibility).
    reject_removed_daemon_old_form();
    // The daemon form of `run` was removed in favour of the explicit `daemon`
    // subcommand (daemon-subcommand spec). Intercepted before clap for the
    // same reason — the error must carry the per-form migration hint.
    reject_removed_run();
    let cli = Cli::parse();
    let code = match dispatch(&cli) {
        Ok(()) => EXIT_OK,
        Err(e) => {
            eprintln!("ak: error: {e:#}");
            EXIT_ERROR
        }
    };
    std::process::exit(code);
}

/// The removed daemon form of `run` (design D2): the first subcommand token is
/// `run` and every other token is only the `-c/--config` flag (either position,
/// `--config=<path>` included) or `--detach` — no positional, no `--`/`--cmd`,
/// i.e. the old daemon signature. At least one such marker token is required:
/// bare `ak run` has no daemon marker, so it stays legal input and reaches
/// dispatch, which reports the "nothing to run" usage error (design D4).
/// Returns `Some(detach)` when the signature matches, `None` otherwise (client
/// forms and anything unknown are left to clap).
fn daemon_run_form(args: &[String]) -> Option<bool> {
    let mut detach = false;
    // Any daemon marker seen anywhere. Global flags may precede the subcommand
    // (`ak -c x.toml run` is the intercepted signature too — the config-only
    // form is intercepted in either position).
    let mut marker = false;
    // Tokens before the subcommand: global flags. Unknown flags are not ours
    // to judge (skipped without marking, like reject_removed_start does).
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-c" | "--config" => {
                marker = true;
                i += 2; // skip the value (reject_removed_start's approach)
            }
            "--detach" => {
                marker = true;
                detach = true;
                i += 1;
            }
            _ if args[i].starts_with("--config=") => {
                marker = true;
                i += 1;
            }
            _ if args[i].starts_with('-') => i += 1,
            _ => break, // first non-flag token = the subcommand
        }
    }
    if args.get(i).map(String::as_str) != Some("run") {
        return None;
    }
    // Everything after `run` must be config/detach flags only: a positional,
    // `--`, `--cmd` or any other flag means a client form — never intercepted.
    let mut j = i + 1;
    while j < args.len() {
        match args[j].as_str() {
            "-c" | "--config" => {
                marker = true;
                j += 2;
            }
            "--detach" => {
                marker = true;
                detach = true;
                j += 1;
            }
            _ if args[j].starts_with("--config=") => {
                marker = true;
                j += 1;
            }
            _ => return None,
        }
    }
    if !marker {
        // Bare `run`: no daemon marker — not the daemon form (see design D4).
        return None;
    }
    Some(detach)
}

/// Argv snapshot for the pre-clap detectors (both are pure over this form so
/// their signatures are unit-testable).
fn argv() -> Vec<String> {
    std::env::args_os()
        .skip(1)
        .map(|a| a.to_string_lossy().into_owned())
        .collect()
}

/// The old systemd `ExecStart` / Windows SCM binPath signature
/// `daemon --config <path>`: after the `daemon` token every remaining token
/// is the `-c/--config` flag (either position, `--config=<path>` included)
/// or the hidden `--service <name>` marker the old binPath carried, with at
/// least one such marker so a truly bare `ak daemon` does not match.
///
/// This is the upgrade-compatibility form of service-registration: under the
/// new CLI it is a usage error (exit 2), and the message must additionally
/// point at `ak daemon install --force --now`. It is detected before clap
/// because `--service` no longer exists on bare `daemon`, so clap would
/// answer "unexpected argument" with no regeneration hint.
fn bare_daemon_old_unit_form(args: &[String]) -> bool {
    let mut marker = false;
    // A global `-c/--config` before the subcommand is the same old entry
    // (`ak -c <path> daemon` was the foreground daemon before this change).
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-c" | "--config" => {
                marker = true;
                i += 2;
            }
            _ if args[i].starts_with("--config=") => {
                marker = true;
                i += 1;
            }
            _ if args[i].starts_with('-') => i += 1,
            _ => break, // first non-flag token = the subcommand
        }
    }
    if args.get(i).map(String::as_str) != Some("daemon") {
        return false;
    }
    let mut j = i + 1;
    while j < args.len() {
        match args[j].as_str() {
            "-c" | "--config" => {
                marker = true;
                j += 2;
            }
            _ if args[j].starts_with("--config=") => {
                marker = true;
                j += 1;
            }
            // The pre-change Windows binPath: `--service <NAME>`.
            "--service" => {
                marker = true;
                j += 2;
            }
            _ => return false,
        }
    }
    marker
}

/// Exit-2 interception of the old `daemon --config <path>` entry
/// (service-registration upgrade compatibility): prints the bare-`daemon`
/// usage error with the unit-regeneration hint instead of clap's generic
/// "unexpected argument" answer.
fn reject_removed_daemon_old_form() {
    if bare_daemon_old_unit_form(&argv()) {
        eprintln!("ak: error: {}", daemon_usage_error(true));
        std::process::exit(EXIT_CONFIG);
    }
}

/// Exit-2 interception of the removed daemon form of `run`, with per-form
/// hints (control-plane spec + design D7): the detached form points at
/// `ak daemon start`; the bare/config-only form (the systemd old-ExecStart
/// signature) additionally tells the operator to regenerate the unit.
fn reject_removed_run() {
    let args: Vec<String> = std::env::args_os()
        .skip(1)
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    let Some(detach) = daemon_run_form(&args) else {
        return;
    };
    eprintln!("ak: error: {}", removed_run_hint(detach));
    std::process::exit(EXIT_CONFIG);
}

/// The per-form message of the removed daemon `run` (control-plane spec):
/// `--detach` points at the background `ak daemon start`; the config-only
/// form is also the old systemd `ExecStart`, so it names the foreground form
/// that replaces it and the command that regenerates the unit.
fn removed_run_hint(detach: bool) -> String {
    if detach {
        "the daemon form of `run` has been removed — use \
         `ak daemon start` to start the daemon in the background"
            .to_string()
    } else {
        "the daemon form of `run` has been removed — use `ak daemon start` \
         for the background daemon or `ak daemon start --foreground` to run it in the \
         foreground. a systemd unit with the old ExecStart must be regenerated: \
         ak daemon install --force --now"
            .to_string()
    }
}

/// The bare-`daemon` usage error (exit 2): names all four subcommands.
/// `old_unit` marks the `daemon --config <path>` signature — the previous
/// systemd `ExecStart`, which lands here now that bare `daemon` is a usage
/// error (service-registration spec: upgrade compatibility). That form
/// additionally points at `ak daemon install --force --now`.
fn daemon_usage_error(old_unit: bool) -> String {
    let base = "`daemon` needs a subcommand: start | install | uninstall | shutdown \
                (see `ak daemon --help`)";
    if old_unit {
        format!(
            "{base}\n      a systemd unit with the old ExecStart (`daemon --config <path>`) \
             must be regenerated: ak daemon install --force --now"
        )
    } else {
        base.to_string()
    }
}

/// Platform default location for the daemon config.
pub(crate) fn default_config_path() -> PathBuf {
    #[cfg(unix)]
    {
        PathBuf::from("/etc/appkeeper/daemon.toml")
    }
    #[cfg(windows)]
    {
        // %APPDATA%\appkeeper\daemon.toml
        match std::env::var("APPDATA") {
            Ok(appdata) => PathBuf::from(appdata).join("appkeeper").join("daemon.toml"),
            Err(_) => {
                let home = std::env::var("USERPROFILE").unwrap_or_default();
                PathBuf::from(home)
                    .join("AppData")
                    .join("Roaming")
                    .join("appkeeper")
                    .join("daemon.toml")
            }
        }
    }
}

fn config_path_of(cli: &Cli) -> PathBuf {
    cli.config.clone().unwrap_or_else(default_config_path)
}

fn dispatch(cli: &Cli) -> Result<()> {
    // Client subcommands don't reach the daemon's logger init; without
    // this, warnings from the registry/config layers would be dropped
    // silently. `daemon` (and bare `appkeeper`) init their own logger from
    // the config's log_level.
    if !matches!(
        &cli.cmd,
        None | Some(Cmd::Run { .. }) | Some(Cmd::Daemon { .. })
    ) {
        env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
            .try_init()
            .ok();
    }
    let config_path = config_path_of(cli);
    match &cli.cmd {
        Some(Cmd::Daemon { cmd }) => match cmd {
            None => {
                // Bare `ak daemon` is a usage error (exit 2) that lists the
                // four subcommands (background-daemon spec). The old
                // `daemon --config <path>` forms never reach here: they are
                // intercepted before clap with the unit-regeneration hint
                // (`reject_removed_daemon_old_form`).
                eprintln!("ak: error: {}", daemon_usage_error(false));
                std::process::exit(EXIT_CONFIG);
            }
            Some(DaemonCmd::Start {
                foreground,
                service,
            }) => {
                if *foreground {
                    run_daemon(&config_path, service.as_deref())
                } else {
                    // Background is the default (the former `--detach`
                    // semantics); `--service` implies the service-managed
                    // foreground entry.
                    match service {
                        Some(name) => run_daemon(&config_path, Some(name)),
                        None => run_background(&config_path),
                    }
                }
            }
            Some(DaemonCmd::Install {
                start,
                force,
                unit_file,
                name,
                desc,
                user,
                restart,
            }) => {
                let opts = service::ServiceOptions {
                    unit_file: unit_file.clone(),
                    name: name.clone(),
                    desc: desc.clone(),
                    user: user.clone(),
                    force: *force,
                    start: *start,
                    restart: *restart,
                };
                service::install(&config_path, &opts)
            }
            Some(DaemonCmd::Uninstall { unit_file, name }) => {
                let opts = service::ServiceOptions {
                    unit_file: unit_file.clone(),
                    name: name.clone(),
                    desc: None,
                    user: None,
                    force: false,
                    start: false,
                    restart: service::RestartPolicy::Always,
                };
                service::uninstall(&config_path, &opts)
            }
            Some(DaemonCmd::Shutdown) => client_cmd(&config_path, |c| {
                let v = c.shutdown()?;
                println!(
                    "{}",
                    v.get("result")
                        .and_then(|r| r.as_str())
                        .unwrap_or("shutting down")
                );
                Ok(())
            }),
        },
        Some(Cmd::Run {
            cmd,
            cmd_str,
            scaffold_name,
            description,
            restart,
            restart_backoff,
            envs,
            workdir,
            json,
        }) => {
            let adhoc = !cmd.is_empty() || cmd_str.is_some();
            if !adhoc {
                // Design D4 / control-plane spec: `run` is the ad-hoc
                // registration verb only. clap cannot express "one of the two
                // forms", so the bare form is reported here.
                eprintln!(
                    "ak: error: nothing to run — `run` registers a new command line; use one of:\n  {}",
                    run_client_forms_hint()
                );
                std::process::exit(EXIT_CONFIG);
            }
            run_adhoc(
                &config_path,
                cmd,
                cmd_str.as_deref(),
                scaffold_name.as_deref(),
                description.as_deref(),
                restart.as_deref(),
                *restart_backoff,
                envs,
                workdir.as_deref(),
                *json,
            )
        }
        Some(Cmd::Start { app, program, json }) => match program {
            Some(p) => program_scoped_cmd(&config_path, app, p, StartVerb::Start, *json),
            None => fanout_cmd(&config_path, app, "start", *json),
        },
        None => {
            // Bare `ak` is a legal exploratory call (design D1): print the
            // full help to stdout and exit 0. Not clap's
            // arg_required_else_help — that goes to stderr with exit 2.
            let mut cmd = Cli::command();
            let _ = cmd.print_help();
            Ok(())
        }
        Some(Cmd::Validate { path }) => validate(&config_path, path.as_deref()),
        Some(Cmd::Config {
            set,
            get,
            delete,
            edit,
            init,
            reload,
        }) => {
            if set.is_empty() && get.is_empty() && delete.is_empty() && !*edit && !*init && !*reload
            {
                // `ak config` with no action flag prints its own usage
                // and exits 0 (a global --config alone must stay legal, so
                // clap's arg_required_else_help does not apply here).
                let mut cmd = Cli::command();
                let sub = cmd
                    .find_subcommand_mut("config")
                    .expect("config subcommand exists");
                println!("{}", sub.render_help());
                return Ok(());
            }
            let code = config_cmd(&config_path, set, get, delete, *edit, *init, *reload)?;
            if code != EXIT_OK {
                std::process::exit(code);
            }
            Ok(())
        }
        Some(Cmd::Ps { all, out }) => ps_cmd(&config_path, *all, out),
        Some(Cmd::Pid { name }) => client_cmd(&config_path, |c| {
            let v = c.program(name)?;
            match v.get("pid") {
                Some(pid) if !pid.is_null() => println!("{pid}"),
                _ => println!("not running"),
            }
            Ok(())
        }),
        Some(Cmd::Stop { app, program }) => match program {
            Some(p) => program_scoped_cmd(&config_path, app, p, StartVerb::Stop, false),
            None => fanout_cmd(&config_path, app, "stop", false),
        },
        Some(Cmd::Restart { app, program }) => match program {
            Some(p) => program_scoped_cmd(&config_path, app, p, StartVerb::Restart, false),
            None => fanout_cmd(&config_path, app, "restart", false),
        },
        Some(Cmd::Action { program, name }) => custom_action_cmd(&config_path, program, name),
        Some(Cmd::Signal { program, signal }) => client_cmd(&config_path, |c| {
            let v = c.signal(program, signal)?;
            println!(
                "{}",
                v.get("result")
                    .and_then(|r| r.as_str())
                    .unwrap_or("delivered")
            );
            Ok(())
        }),
        Some(Cmd::Apply {
            app,
            program,
            restart,
        }) => client_cmd(&config_path, |c| {
            let v = c.apply(app.as_deref(), program.as_deref(), *restart)?;
            println!(
                "{}",
                v.get("result")
                    .and_then(|r| r.as_str())
                    .unwrap_or("applied")
            );
            Ok(())
        }),
        Some(Cmd::Log {
            names,
            tail,
            follow,
            stream,
        }) => client_cmd(&config_path, |c| log_cmd(c, names, *tail, *follow, stream)),
        Some(Cmd::Compose {
            path,
            name,
            description,
            restart,
            restart_backoff,
            priority,
            args,
            envs,
            workdir,
            apply: apply_now,
        }) => {
            let config = client::load_config(&config_path)?;
            let config_dir = config_dir_of(&config_path);

            // v0.1 single-file configs are recognized and converted first
            // (only real .toml candidates — anything else may be a binary
            // executable destined for the scaffold flow).
            let mut target = path.clone();
            if path.is_file() && path.extension().map(|x| x == "toml").unwrap_or(false) {
                let text = std::fs::read_to_string(&path)
                    .with_context(|| format!("cannot read {}", path.display()))?;
                if config::looks_legacy(&text) {
                    let imp = config::import_legacy(&path)?;
                    println!(
                        "legacy config imported as app {:?}: {} generated (source kept untouched)",
                        imp.app_name,
                        imp.new_file.display()
                    );
                    if !imp.daemon_hint.is_empty() {
                        eprintln!(
                            "note: [daemon] settings ({}) belong in the daemon config; move them manually",
                            imp.daemon_hint.join(", ")
                        );
                    }
                    target = imp.new_file;
                }
            }

            let env = match parse_envs(envs) {
                Ok(e) => e,
                Err(code) => std::process::exit(code),
            };
            let restart_policy = match restart.as_deref().map(config::parse_restart_cli) {
                Some(Ok(v)) => Some(v),
                Some(Err(e)) => {
                    eprintln!("ak: error: {e}");
                    std::process::exit(EXIT_CONFIG);
                }
                None => None,
            };

            let opts = AddOptions {
                name: name.clone(),
                description: description.clone(),
                restart: restart_policy,
                restart_backoff: *restart_backoff,
                priority: *priority,
                args: args.clone(),
                args_argv: None,
                env: if env.is_empty() { None } else { Some(env) },
                workdir: workdir.clone(),
            };
            let result = registry::add(&config, &config_dir, &target, &opts)?;
            print_add_result(&result);
            sync_if_online(&config)?;

            if *apply_now {
                match apply_after_add(&config, &config_path, &result.app) {
                    AddApplyOutcome::Applied(text) => println!("{text}"),
                    AddApplyOutcome::Failed(msg, code) => {
                        eprintln!("ak: error: {msg}");
                        std::process::exit(code);
                    }
                }
            }
            Ok(())
        }
        Some(Cmd::Remove { name }) => remove_cmd(&config_path, name),
        Some(Cmd::Shell { cmd }) => shell::run(&config_path, cmd.as_deref()),
    }
}

/// Parse `--env K=V` pairs (repeatable, last wins). A missing `=` or an empty
/// key is a config-level error; the caller exits with [`EXIT_CONFIG`].
fn parse_envs(
    envs: &[String],
) -> std::result::Result<std::collections::BTreeMap<String, String>, i32> {
    let mut env = std::collections::BTreeMap::new();
    for kv in envs {
        let (k, v) = match kv.split_once('=') {
            Some(pair) => pair,
            None => {
                eprintln!("ak: error: --env expects K=V, got {kv:?}");
                return Err(EXIT_CONFIG);
            }
        };
        if k.trim().is_empty() {
            eprintln!("ak: error: --env key must not be empty: {kv:?}");
            return Err(EXIT_CONFIG);
        }
        env.insert(k.to_string(), v.to_string());
    }
    Ok(env)
}

/// Print the full non-interactive report of one `registry::add` result.
fn print_add_result(result: &registry::AddResult) {
    if result.scaffolded {
        println!("generated {}", result.file.display());
        println!("app[{}] program[{}]", result.app, result.program);
        println!("  command:  {}", result.command);
        if result.args.is_empty() {
            println!("  args:     (none)");
        } else {
            println!("  args:     {}", shell_join(&result.args));
        }
        if result.env.is_empty() {
            println!("  env:      (none)");
        } else {
            let kv: Vec<String> = result.env.iter().map(|(k, v)| format!("{k}={v}")).collect();
            println!("  env:      {}", kv.join(" "));
        }
        println!("  work_dir: {}", result.work_dir.display());
        if result.changed {
            println!(
                "changed: run `ak apply {}` (or bare `ak apply`) to take effect",
                result.app
            );
        } else {
            println!("no change: generated config is already up to date");
        }
    } else {
        println!("app[{}] registered", result.app);
    }
}

/// One output line of `log`: multi-program output carries a `[name] ` prefix,
/// single-program output is byte-identical to the legacy form (no prefix).
fn format_log_line(name: &str, line: &str, multi: bool) -> String {
    if multi {
        format!("[{name}] {line}")
    } else {
        line.to_string()
    }
}

/// `ak log <name...>`: multi-program aggregation. A single program keeps
/// the byte-identical legacy output (no prefix); multiple programs get a
/// `[name] ` line prefix and, in follow mode, concurrent connections.
fn log_cmd(
    c: &client::Client,
    names: &[String],
    tail: usize,
    follow: bool,
    stream: &str,
) -> Result<()> {
    let streams: Vec<&str> = if stream == "both" {
        vec!["out", "err"]
    } else {
        vec![stream]
    };
    let multi = names.len() > 1;
    if follow {
        // Concurrent connections, line-buffered, serialized through one
        // stdout lock so interleaved lines never tear (design D8).
        let out = std::sync::Arc::new(std::sync::Mutex::new(std::io::stdout()));
        let mut handles = Vec::new();
        for name in names {
            for s in &streams {
                let c = c.clone();
                let name = name.clone();
                let s = s.to_string();
                let out = out.clone();
                handles.push(std::thread::spawn(move || -> Result<()> {
                    use std::io::Write;
                    c.follow_lines(&name, &s, |line| {
                        let mut w = out.lock().unwrap();
                        let r = writeln!(&mut *w, "{}", format_log_line(&name, line, multi));
                        let _ = w.flush();
                        r.is_ok()
                    })
                }));
            }
        }
        for h in handles {
            match h.join() {
                Ok(Ok(())) => {}
                Ok(Err(e)) => return Err(e),
                Err(_) => bail!("a log follower thread panicked"),
            }
        }
        return Ok(());
    }
    // Non-follow: per program, in argument order; `both` prints out then err.
    for name in names {
        for s in &streams {
            for line in c.log_tail(name, s, tail)? {
                println!("{}", format_log_line(name, &line, multi));
            }
        }
    }
    Ok(())
}

/// `ak daemon start` (background default): spawn the daemon detached and
/// report once it is serving (background-daemon spec). Non-zero exit on
/// timeout / bind conflict, naming the background log.
///
/// An already-serving instance is a bind conflict for this entry point: the
/// spec's「端口已占用时报错」scenario requires a non-zero exit and a message
/// naming the existing instance (no second process is started). The
/// `Ready { already_online }` outcome stays a success for the implicit
/// pull-up (`compose --apply`, `daemonize::ensure_daemon`), which treats a
/// healthy daemon as "nothing to do" — only this explicit entry maps it to an
/// error.
fn run_background(config_path: &Path) -> Result<()> {
    let config = client::load_config(config_path)?;
    let config_dir = config_dir_of(config_path);
    match daemonize::ensure_daemon(&config, config_path, &config_dir) {
        daemonize::SpawnOutcome::Ready { already_online } => {
            let endpoint = config
                .control_endpoint()
                .map(|e| e.display())
                .unwrap_or_else(|_| config.daemon.socket.to_string_lossy().into_owned());
            if already_online {
                eprintln!(
                    "ak: error: another daemon is already running on control endpoint {endpoint} \
                     (control endpoint already in use); no second instance was started — \
                     use `ak ps` to inspect it or `ak daemon shutdown` to stop it"
                );
                std::process::exit(EXIT_ERROR);
            }
            println!("daemon running in the background on control endpoint {endpoint}");
            println!(
                "logs: {}",
                daemonize::daemon_log_path(&config, &config_dir).display()
            );
            Ok(())
        }
        daemonize::SpawnOutcome::TimedOut { log_path } => {
            eprintln!(
                "ak: error: the daemon did not become ready; see {} \
                 (a control-endpoint conflict is likely — check `ak ps`)",
                log_path.display()
            );
            std::process::exit(EXIT_ERROR);
        }
    }
}

/// Local registration rows for `ps` (name / config object / description).
/// Broken records are filtered out (`ListedApp::good`), the same semantics the
/// former `list` verb had.
fn ps_registry_rows(config: &DaemonConfig, config_dir: &Path) -> Result<Vec<shell::RegistryRow>> {
    Ok(
        registry::ListedApp::good(registry::list(config, config_dir)?)
            .into_iter()
            .map(|a| shell::RegistryRow {
                name: a.name,
                path: a.path.display().to_string(),
                description: a.description,
            })
            .collect(),
    )
}

/// The `/v1` projection's program rows grouped by app name: the set of apps
/// the daemon knows about (the authoritative half of the `--all` join).
fn projection_apps(v: &serde_json::Value) -> std::collections::BTreeSet<String> {
    v.get("programs")
        .and_then(|p| p.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|p| p.get("app").and_then(|a| a.as_str()))
                .map(String::from)
                .collect()
        })
        .unwrap_or_default()
}

/// Whether a failed `/v1/status` call degrades `ps` to the local registry
/// view instead of exiting: only `--all` degrades, and only when the daemon
/// genuinely is not there (control-plane spec: `ps --all` offline exits 0; a
/// plain `ps` stays at exit 3). A permission-denied endpoint or a rejected TCP
/// credential proves a daemon IS serving, so neither may be reported as the
/// offline degradation.
fn ps_degrades_to_registry(all: bool, err: &anyhow::Error) -> bool {
    all && client::is_unreachable(err)
}

/// `ak ps` (control-plane spec): online-only by default (offline exits 3 via
/// the client error class); `--all` joins the local registry with the `/v1`
/// projection (online: the projection is authoritative, the registry supplies
/// only apps it does not cover) or degrades to the registry listing with a
/// daemon-offline note (exit 0). `--out json` prints the `GET /v1/status`
/// projection; `--all` adds a `registry` array for the apps the projection
/// does not cover, and the offline degradation prints that array alone.
fn ps_cmd(config_path: &Path, all: bool, out: &str) -> Result<()> {
    let config = client::load_config(config_path)?;
    let config_dir = config_dir_of(config_path);
    // The local registry is advisory for the default view: it only supplies
    // the DESCRIPTION column (the config path comes from the `/v1` projection).
    // A registry read failure (e.g. an unreadable `app_dir`) MUST NOT turn a
    // connection-class error into exit 1 — `ps` keeps the exit-3 / permission
    // contract by asking the daemon first. `--all` keeps the hard dependency:
    // its view and its offline degradation are registry-sourced by definition.
    let registry_rows = match ps_registry_rows(&config, &config_dir) {
        Ok(rows) => rows,
        Err(e) if !all => {
            eprintln!(
                "note: the local registry could not be read; descriptions are omitted: {e:#}"
            );
            Vec::new()
        }
        Err(e) => return Err(e),
    };
    let descriptions: std::collections::BTreeMap<String, String> = registry_rows
        .iter()
        .filter_map(|r| r.description.clone().map(|d| (r.name.clone(), d)))
        .collect();
    let c = client::Client::from_config(&config);
    let status = match c.status() {
        Ok(v) => Some(v),
        Err(e) => {
            // Plain `ps` keeps the script contract (exit 3); `--all`
            // degrades to the local registry view and exits 0. Only the
            // daemon-unreachable class degrades — a caller fixable error
            // (e.g. a malformed config answer) must not be masked.
            if !ps_degrades_to_registry(all, &e) {
                eprintln!("ak: error: {e:#}");
                std::process::exit(client::exit_code_of(&e));
            }
            eprintln!("note: {e:#}");
            None
        }
    };

    if out == "json" {
        match status {
            Some(mut v) => {
                if all {
                    let known = projection_apps(&v);
                    let registry: Vec<serde_json::Value> = registry_rows
                        .iter()
                        .filter(|r| !known.contains(&r.name))
                        .map(|r| {
                            serde_json::json!({
                                "name": r.name,
                                "config_path": r.path,
                                "description": r.description,
                                "state": "not-applied",
                            })
                        })
                        .collect();
                    v["registry"] = serde_json::Value::Array(registry);
                }
                println!("{}", serde_json::to_string_pretty(&v)?);
            }
            // Offline `--all`: the registry JSON array alone (control-plane
            // spec: `--all --out json` degradation).
            None => {
                let registry: Vec<serde_json::Value> = registry_rows
                    .iter()
                    .map(|r| {
                        serde_json::json!({
                            "name": r.name,
                            "config_path": r.path,
                            "description": r.description,
                        })
                    })
                    .collect();
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::Value::Array(registry))?
                );
            }
        }
        return Ok(());
    }

    match status {
        Some(v) => {
            if all {
                print!(
                    "{}",
                    shell::render_ps_all_table(&v, &descriptions, &registry_rows)
                );
            } else {
                print!("{}", shell::render_ps_table(&v, &descriptions));
            }
        }
        None => print!("{}", shell::render_registry_listing(&registry_rows, true)),
    }
    use std::io::Write;
    let _ = std::io::stdout().flush();
    Ok(())
}

/// The scaffold flags recognized inside the trailing `run --` region (after
/// the command line). A leading `--` split means clap hands the whole region
/// over as one vector, so the runtime splits command-vs-flags at the first
/// recognized flag (design D10 / app-registry spec examples).
#[derive(Debug, Default, PartialEq)]
struct AdhocFlags {
    /// The ad-hoc app name (`--app`; formerly `--name`).
    app: Option<String>,
    description: Option<String>,
    restart: Option<String>,
    restart_backoff: Option<f64>,
    envs: Vec<String>,
    workdir: Option<PathBuf>,
}

/// Split the trailing region into `(argv, flags)`: argv is everything up to
/// the first recognized scaffold flag; the rest is parsed as scaffold flags
/// (supporting both `--flag value` and `--flag=value`).
fn split_adhoc_trailing(
    trailing: &[String],
) -> std::result::Result<(Vec<String>, AdhocFlags), String> {
    const FLAGS: [&str; 6] = [
        "--app",
        "--description",
        "--restart",
        "--restart-backoff",
        "--env",
        "--workdir",
    ];
    let is_flag = |t: &str| {
        FLAGS
            .iter()
            .any(|f| t == *f || t.starts_with(&format!("{f}=")))
    };
    let split = trailing
        .iter()
        .position(|t| is_flag(t))
        .unwrap_or(trailing.len());
    let (argv, rest) = trailing.split_at(split);
    let mut flags = AdhocFlags::default();
    let mut i = 0;
    while i < rest.len() {
        let tok = &rest[i];
        let (flag, inline) = match tok.split_once('=') {
            Some((f, v)) => (f.to_string(), Some(v.to_string())),
            None => (tok.clone(), None),
        };
        let take_value = |i: &mut usize| -> std::result::Result<String, String> {
            if let Some(v) = &inline {
                return Ok(v.clone());
            }
            *i += 1;
            rest.get(*i)
                .cloned()
                .ok_or_else(|| format!("{flag} needs a value"))
        };
        match flag.as_str() {
            "--app" => flags.app = Some(take_value(&mut i)?),
            "--description" => flags.description = Some(take_value(&mut i)?),
            "--restart" => flags.restart = Some(take_value(&mut i)?),
            "--restart-backoff" => {
                let v = take_value(&mut i)?;
                flags.restart_backoff = Some(
                    v.parse()
                        .map_err(|_| format!("--restart-backoff expects a number, got {v:?}"))?,
                );
            }
            "--env" => flags.envs.push(take_value(&mut i)?),
            "--workdir" => flags.workdir = Some(PathBuf::from(take_value(&mut i)?)),
            other => return Err(format!("unknown flag {other:?} in the run command form")),
        }
        i += 1;
    }
    Ok((argv.to_vec(), flags))
}

/// `ak run -- <cmd...>` / `run --cmd "<string>"`: daemon-first ad-hoc
/// registration (run-daemon-first). The CLI resolves the command line and
/// hands it to the daemon, which starts the program, waits for the startup
/// judgement, then lands the record. The CLI itself never writes `app_dir`.
#[allow(clippy::too_many_arguments)]
fn run_adhoc(
    config_path: &Path,
    trailing: &[String],
    cmd_str: Option<&str>,
    name: Option<&str>,
    description: Option<&str>,
    restart: Option<&str>,
    restart_backoff: Option<f64>,
    envs: &[String],
    workdir: Option<&Path>,
    json: bool,
) -> Result<()> {
    // For the `--` form the scaffold flags may ride after the command line
    // (`run -- node server.js --name worker`); split them out here.
    let (argv, flags) = if cmd_str.is_some() {
        (Vec::new(), AdhocFlags::default())
    } else {
        match split_adhoc_trailing(trailing) {
            Ok(pair) => pair,
            Err(e) => {
                eprintln!("ak: error: {e}");
                std::process::exit(EXIT_CONFIG);
            }
        }
    };
    // clap-level flags (before `--`) win when set; otherwise the trailing ones.
    let name = flags.app.as_deref().or(name);
    let description = flags.description.as_deref().or(description);
    let restart = flags.restart.as_deref().or(restart);
    let restart_backoff = flags.restart_backoff.or(restart_backoff);
    let workdir = flags.workdir.as_deref().or(workdir);
    let envs: Vec<String> = if flags.envs.is_empty() {
        envs.to_vec()
    } else {
        flags.envs.clone()
    };

    let argv: Vec<String> = if let Some(s) = cmd_str {
        config::split_command(s)
    } else {
        argv
    };
    if argv.is_empty() {
        eprintln!("ak: error: no command given (use `ak run -- <cmd...>` or `--cmd \"...\"`)");
        std::process::exit(EXIT_CONFIG);
    }
    let env = match parse_envs(&envs) {
        Ok(e) => e,
        Err(code) => std::process::exit(code),
    };
    // Command resolution stays client-side: a resolution failure is exit 2 and
    // must leave `app_dir` untouched (app-registry spec).
    let (command, args) = match registry::resolve_command(&argv[0]) {
        Ok(p) => (p, argv[1..].to_vec()),
        Err(e) => {
            eprintln!("ak: error: {e:#}");
            std::process::exit(EXIT_CONFIG);
        }
    };
    let register_only = matches!(restart, Some("no"));

    let config = client::load_config(config_path)?;
    let c = client::Client::from_config(&config);
    // daemon-first: offline is exit 3 and MUST NOT pull a daemon up.
    if let Err(e) = c.health() {
        eprintln!(
            "ak: error: {e:#} — `ak run` is daemon-first and does not start the daemon \
             for you (control endpoint: {})",
            c.endpoint().display()
        );
        std::process::exit(EXIT_UNREACHABLE);
    }
    let body = serde_json::json!({
        "name": name,
        "command": command.display().to_string(),
        "args": args,
        "env": env,
        "work_dir": workdir.map(|p| p.display().to_string()),
        "description": description,
        "restart": restart,
        "restart_backoff": restart_backoff,
        "register_only": register_only,
    });
    let mut residual: Option<serde_json::Value> = None;
    let frames = c.adhoc_run(&body, |frame| {
        if json {
            return; // --json: suppress human progress, only the final object prints
        }
        if frame.get("residual").and_then(|r| r.as_bool()) == Some(true) {
            residual = Some(frame.clone());
            return;
        }
        print_progress_frame(frame);
    });
    let terminal = match frames {
        Ok(v) => v,
        Err(e) => {
            eprintln!("ak: error: {e:#}");
            std::process::exit(client::exit_code_of(&e));
        }
    };
    // Residual: prompt (or fail when non-interactive) and commit on confirm.
    if terminal.get("residual").and_then(|r| r.as_bool()) == Some(true) || residual.is_some() {
        let frame = residual.unwrap_or_else(|| terminal.clone());
        return handle_residual(&c, &frame, json);
    }
    render_run_terminal(&terminal, json)
}

/// Print one human-readable progress frame (run-daemon-first task 6.6).
fn print_progress_frame(frame: &serde_json::Value) {
    if let Some(line) = progress_line(frame) {
        println!("{line}");
    }
}

/// The human-readable progress line for one frame, or `None` when the frame
/// carries no progress (the terminal frame). Pure so it is unit-testable.
fn progress_line(frame: &serde_json::Value) -> Option<String> {
    match frame.get("phase").and_then(|p| p.as_str()) {
        Some("staged") => {
            let est = frame
                .get("estimate_secs")
                .and_then(|e| e.as_f64())
                .unwrap_or(0.0);
            Some(format!("starting (estimated wait up to {est:.0}s)..."))
        }
        Some("waiting_startsecs") => Some("waiting for startsecs...".to_string()),
        Some("waiting_health") => {
            let est = frame
                .get("estimate_secs")
                .and_then(|e| e.as_f64())
                .unwrap_or(0.0);
            Some(format!(
                "waiting for the first health probe (up to {est:.0}s)..."
            ))
        }
        _ => None,
    }
}

/// What to do about a residual (started-but-unregistered) program.
#[derive(Debug, PartialEq, Eq)]
enum ResidualAction {
    /// Re-land the registration (user confirmed).
    Commit,
    /// Refuse: non-interactive session, or the user declined.
    Refuse(&'static str),
}

/// Decide the residual action from the session kind and the user's answer.
/// Pure (task 6.7: non-TTY defaults to refusal).
fn residual_decision(interactive: bool, answer: Option<&str>) -> ResidualAction {
    if !interactive {
        return ResidualAction::Refuse("non-interactive session defaults to refusing");
    }
    match answer.map(|a| a.trim().to_ascii_lowercase()) {
        Some(a) if a == "y" || a == "yes" => ResidualAction::Commit,
        _ => ResidualAction::Refuse("declined by the user"),
    }
}

/// Handle the residual (already started, not persisted) state (task 6.7):
/// prompt on a TTY, commit on confirm; refuse on decline or non-TTY.
fn handle_residual(c: &client::Client, frame: &serde_json::Value, json: bool) -> Result<()> {
    let program = frame
        .get("program")
        .and_then(|p| p.as_str())
        .unwrap_or("?")
        .to_string();
    let config_path = frame
        .get("config_path")
        .and_then(|p| p.as_str())
        .unwrap_or("")
        .to_string();
    let pid = frame.get("pid").cloned().unwrap_or(serde_json::Value::Null);
    let interactive = std::io::IsTerminal::is_terminal(&std::io::stdin());
    let answer = if interactive {
        print!(
            "program {program} is already running (pid {pid}) but not registered.\n\
             Re-land its registration at {config_path}? [y/N] "
        );
        use std::io::Write;
        let _ = std::io::stdout().flush();
        let mut line = String::new();
        let _ = std::io::stdin().read_line(&mut line);
        Some(line)
    } else {
        None
    };
    match residual_decision(interactive, answer.as_deref()) {
        ResidualAction::Refuse(why) => {
            eprintln!(
                "ak: error: program {program} is already running (pid {pid}) but not \
                 registered (config would be {config_path}); not re-landing ({why})"
            );
            std::process::exit(EXIT_ERROR);
        }
        ResidualAction::Commit => match c.adhoc_commit(&program) {
            Ok(v) => {
                if json {
                    println!("{}", serde_json::to_string_pretty(&v)?);
                } else {
                    println!(
                        "{}",
                        v.get("result")
                            .and_then(|r| r.as_str())
                            .unwrap_or("registered")
                    );
                    println!(
                        "  config: {}",
                        v.get("config_path").and_then(|p| p.as_str()).unwrap_or("")
                    );
                    println!("  pid:    {pid}");
                }
                Ok(())
            }
            Err(e) => {
                eprintln!("ak: error: {e:#}");
                std::process::exit(client::exit_code_of(&e));
            }
        },
    }
}

/// Render the terminal frame of an ad-hoc run (task 6.4/6.8). A partial
/// failure prints the full config for manual landing and exits non-zero.
fn render_run_terminal(frame: &serde_json::Value, json: bool) -> Result<()> {
    let (code, out, err) = run_terminal_render(frame, json);
    for line in &out {
        println!("{line}");
    }
    for line in &err {
        eprintln!("{line}");
    }
    if code != EXIT_OK {
        std::process::exit(code);
    }
    Ok(())
}

/// Pure rendering of the terminal frame into `(exit_code, stdout, stderr)`
/// (task 6.4/6.8) so the branches are unit-testable.
fn run_terminal_render(frame: &serde_json::Value, json: bool) -> (i32, Vec<String>, Vec<String>) {
    let ok = frame.get("ok").and_then(|o| o.as_bool()).unwrap_or(false);
    if json {
        // Machine-readable shape (task 6.4): name / config_path / pid / status.
        // The full frame (error, partial, config_text) rides along so a
        // script can diagnose a partial failure too.
        let program = frame.get("program").and_then(|p| p.as_str()).unwrap_or("");
        let config_path = frame
            .get("config_path")
            .and_then(|p| p.as_str())
            .unwrap_or("");
        let pid = frame.get("pid").cloned().unwrap_or(serde_json::Value::Null);
        let mut obj = serde_json::json!({
            "name": program,
            "config_path": config_path,
            "pid": pid,
            "status": if ok { "started" } else { "failed" },
        });
        if !ok {
            if let Some(e) = frame.get("error") {
                obj["error"] = e.clone();
            }
            if let Some(p) = frame.get("phase") {
                obj["phase"] = p.clone();
            }
            if frame.get("partial").and_then(|p| p.as_bool()) == Some(true) {
                obj["partial"] = serde_json::Value::Bool(true);
                if let Some(t) = frame.get("config_text") {
                    obj["config_text"] = t.clone();
                }
            }
        }
        let text = serde_json::to_string_pretty(&obj).unwrap_or_default();
        let code = if ok { EXIT_OK } else { EXIT_ERROR };
        return (code, vec![text], Vec::new());
    }
    if !ok {
        let phase = frame
            .get("phase")
            .and_then(|p| p.as_str())
            .unwrap_or("startup");
        let err = frame
            .get("error")
            .and_then(|e| e.as_str())
            .unwrap_or("startup failed");
        let mut stderr = vec![format!("ak: error: {phase}: {err}")];
        if frame.get("partial").and_then(|p| p.as_bool()) == Some(true) {
            stderr.extend(partial_config_lines(frame));
        }
        return (EXIT_ERROR, Vec::new(), stderr);
    }
    if frame.get("partial").and_then(|p| p.as_bool()) == Some(true) {
        // Started but not persisted: report and exit non-zero with the config.
        let err = frame
            .get("error")
            .and_then(|e| e.as_str())
            .unwrap_or("registration not written");
        let mut stderr = vec![format!("ak: error: {err}")];
        stderr.extend(partial_config_lines(frame));
        return (EXIT_ERROR, Vec::new(), stderr);
    }
    let program = frame.get("program").and_then(|p| p.as_str()).unwrap_or("?");
    let config_path = frame
        .get("config_path")
        .and_then(|p| p.as_str())
        .unwrap_or("");
    let pid = frame
        .get("pid")
        .and_then(|p| p.as_u64())
        .map(|p| p.to_string())
        .unwrap_or_else(|| "null".to_string());
    (
        EXIT_OK,
        vec![
            format!("program {program} started"),
            format!("  config: {config_path}"),
            format!("  pid:    {pid}"),
        ],
        Vec::new(),
    )
}

/// The full configuration information lines of a partial failure (task 6.8).
fn partial_config_lines(frame: &serde_json::Value) -> Vec<String> {
    let program = frame.get("program").and_then(|p| p.as_str()).unwrap_or("?");
    let config_path = frame
        .get("config_path")
        .and_then(|p| p.as_str())
        .unwrap_or("");
    let mut out = vec![
        "the process is running but its registration was not written.".to_string(),
        format!("  app/program: {program}"),
        format!("  target file: {config_path}"),
    ];
    if let Some(text) = frame.get("config_text").and_then(|t| t.as_str())
        && !text.is_empty()
    {
        out.push("--- write this config by hand ---".to_string());
        out.push(text.trim_end_matches('\n').to_string());
        out.push("--- end of config ---".to_string());
    }
    out
}

/// Render argv as one paste-able line (quoting mirrors the quote-aware
/// `config::split_command` rules, so the output round-trips).
fn shell_join(args: &[String]) -> String {
    args.iter()
        .map(|a| {
            if a.chars().any(|c| c.is_whitespace()) {
                format!("\"{a}\"")
            } else {
                a.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn config_dir_of(config_path: &Path) -> PathBuf {
    config_path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."))
        .to_path_buf()
}

/// After a registry change, poke a running daemon so it picks the change up
/// immediately. Offline is fine — the next `run` reads the registry anyway.
/// A reachable daemon whose reload fails is a partial failure and must not
/// exit 0: the registry changed but the daemon did not pick it up.
fn sync_if_online(config: &DaemonConfig) -> Result<()> {
    let c = client::Client::from_config(config);
    if c.health().is_ok() {
        // reload = rescan + detect only: the registry change enters pending;
        // `ak apply` makes it take effect (app-registry spec).
        c.reload().with_context(|| {
            "registry change persisted, but the running daemon failed to rescan; \
             run `ak config --reload` once the problem is fixed, then `ak apply`"
        })?;
        println!("daemon synced: registration is pending — run `ak apply` to take effect");
    }
    Ok(())
}

fn client_cmd(config_path: &Path, f: impl FnOnce(&client::Client) -> Result<()>) -> Result<()> {
    let config = client::load_config(config_path)?;
    let c = client::Client::from_config(&config);
    match f(&c) {
        Ok(()) => Ok(()),
        Err(e) => {
            // Exit with the class code, but never silently — the message is
            // the only trace of what the control plane answered.
            eprintln!("ak: error: {e:#}");
            std::process::exit(client::exit_code_of(&e));
        }
    }
}

/// The three client forms listed by the bare-`run` usage error (design D4 /
/// control-plane spec). `run` is the ad-hoc registration verb only; starting
/// registered programs/apps is `start`.
fn run_client_forms_hint() -> &'static str {
    "ak run -- <cmd...>        register and start a new command line\n  \
     ak run --cmd \"<string>\"   the same command line as one string"
}

/// The `start|stop|restart <app> <program>` 404 hint: the name is not a
/// registered program, so the way to run an ad-hoc command line is `run --`.
fn eprint_unknown_program_hint(program: &str, e: &anyhow::Error) {
    if format!("{e:#}").contains("404") {
        eprintln!(
            "hint: `{program}` is not a registered program — to register and run \
             a new command line, use `ak run -- <cmd...>`"
        );
    }
}

/// `ak start|stop|restart <app> <program>`: the second positional narrows the
/// scope to one program of that app (design D1). The program must belong to
/// the app — a cross-app name is a scope error, not a silent retarget. `start`
/// blocks until the daemon's startup judgement and renders
/// `{程序名, 配置路径, PID}`; stop/restart print the daemon's result text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StartVerb {
    Start,
    Stop,
    Restart,
}

fn program_scoped_cmd(
    config_path: &Path,
    app: &str,
    program: &str,
    verb: StartVerb,
    json: bool,
) -> Result<()> {
    let config = client::load_config(config_path)?;
    let c = client::Client::from_config(&config);
    // Resolve the program's owning app first: the control-plane program
    // endpoints are name-addressed (program names are globally unique), so
    // the app scope has to be checked here (control-plane spec: 未知 app 报错).
    let info = match c.program(program) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("ak: error: {e:#}");
            eprint_unknown_program_hint(program, &e);
            std::process::exit(client::exit_code_of(&e));
        }
    };
    let owner = info.get("app").and_then(|a| a.as_str()).unwrap_or("");
    if owner != app {
        eprintln!(
            "ak: error: program {program:?} does not belong to app {app:?} \
             (its app is {owner:?})"
        );
        std::process::exit(EXIT_ERROR);
    }
    match verb {
        StartVerb::Start => match c.start_and_wait(program) {
            Ok(v) => {
                render_program_result(&v, program, json);
                Ok(())
            }
            Err(e) => {
                eprintln!("ak: error: {e:#}");
                eprint_unknown_program_hint(program, &e);
                std::process::exit(client::exit_code_of(&e));
            }
        },
        StartVerb::Stop | StartVerb::Restart => {
            let action = if verb == StartVerb::Stop {
                "stop"
            } else {
                "restart"
            };
            match c.action(program, action) {
                Ok(v) => {
                    println!(
                        "{}",
                        v.get("result").and_then(|r| r.as_str()).unwrap_or("done")
                    );
                    Ok(())
                }
                Err(e) => {
                    eprintln!("ak: error: {e:#}");
                    std::process::exit(client::exit_code_of(&e));
                }
            }
        }
    }
}

/// Render one start response as `{程序名, 配置路径, PID}` (task 6.3/6.4).
fn render_program_result(v: &serde_json::Value, name: &str, json: bool) {
    let program = v.get("name").and_then(|n| n.as_str()).unwrap_or(name);
    let config_path = v.get("config_path").and_then(|p| p.as_str()).unwrap_or("");
    let pid = v.get("pid").cloned().unwrap_or(serde_json::Value::Null);
    if json {
        let obj = serde_json::json!({
            "name": program,
            "config_path": config_path,
            "pid": pid,
            "status": "started",
        });
        println!("{obj}");
        return;
    }
    println!(
        "{}",
        program_result_line(program, config_path, &pid, "started")
    );
}

/// One human-readable `run <name>` result line. Pure (task 6.3).
fn program_result_line(
    program: &str,
    config_path: &str,
    pid: &serde_json::Value,
    status: &str,
) -> String {
    let pid_s = match pid {
        serde_json::Value::Null => "null".to_string(),
        other => other.to_string(),
    };
    format!("{program}  config={config_path}  pid={pid_s}  {status}")
}

/// `appkeeper action <program> <action>`: print the output tail and pass the
/// action's own exit code through (control-plane spec: 调用失败 1、守护不可达 3、
/// 超时 1 — anything else is the action's exit status).
fn custom_action_cmd(config_path: &Path, program: &str, action: &str) -> Result<()> {
    let config = client::load_config(config_path)?;
    let c = client::Client::from_config(&config);
    let v = match c.custom_action(program, action) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("ak: error: {e:#}");
            std::process::exit(client::exit_code_of(&e));
        }
    };
    if let Some(out) = v
        .get("output")
        .and_then(|o| o.as_str())
        .filter(|s| !s.is_empty())
    {
        println!("{out}");
    }
    if v.get("timed_out")
        .and_then(|t| t.as_bool())
        .unwrap_or(false)
    {
        eprintln!("ak: error: action {program}.{action} timed out (process tree killed)");
        std::process::exit(EXIT_ERROR);
    }
    match v.get("exit_code").and_then(|e| e.as_i64()) {
        Some(0) | None => Ok(()),
        Some(code) => {
            eprintln!("action {program}.{action} exited with code {code}");
            std::process::exit(code.clamp(i32::MIN as i64, 255) as i32);
        }
    }
}

/// `ak remove <app>`: the daemon-executed atomic removal (guard-remove-until-
/// stopped). The CLI no longer deletes registration records or triggers a
/// reload/apply afterwards — the daemon performs the liveness guard, the
/// record deletion and the state cleanup in one turn and the result already
/// says the removal is in effect.
///
/// Exit codes follow the client contract: an unreachable daemon is 3 (via
/// [`client::exit_code_of`]), a 409 conflict / 404 unknown app is 1.
fn remove_cmd(config_path: &Path, app: &str) -> Result<()> {
    client_cmd(config_path, |c| {
        let v = c.remove_app(app)?;
        println!(
            "{}",
            v.get("result")
                .and_then(|r| r.as_str())
                .unwrap_or("removed")
        );
        Ok(())
    })
}

/// `ak start|stop|restart <app>`: app-level fan-out; the server expands it in
/// the established order and reports per-program results. For `start` the
/// per-program `{name, config_path, pid, status}` is rendered and any failure
/// exits non-zero (run-daemon-first 6.3).
fn fanout_cmd(config_path: &Path, app: &str, verb: &str, json: bool) -> Result<()> {
    let config = client::load_config(config_path)?;
    let c = client::Client::from_config(&config);
    let v = match if verb == "start" {
        c.app_start_and_wait(app)
    } else {
        c.app_action(app, verb)
    } {
        Ok(v) => v,
        Err(e) => {
            eprintln!("ak: error: {e:#}");
            std::process::exit(client::exit_code_of(&e));
        }
    };
    if verb == "start" {
        return render_fanout_start(&v, json);
    }
    println!(
        "{}",
        v.get("result").and_then(|r| r.as_str()).unwrap_or("done")
    );
    Ok(())
}

/// Render a `start <app>` fan-out: one line per program
/// `{程序名, 配置路径, PID, 状态}`; all-success exits 0, any failure lists the
/// failures and exits non-zero (task 6.3/6.4).
fn render_fanout_start(v: &serde_json::Value, json: bool) -> Result<()> {
    let (code, out, err) = fanout_start_render(v, json);
    for line in &out {
        println!("{line}");
    }
    for line in &err {
        eprintln!("{line}");
    }
    if code != EXIT_OK {
        std::process::exit(code);
    }
    Ok(())
}

/// Pure rendering of a `start <app>` fan-out into `(exit_code, stdout, stderr)`
/// (task 6.3/6.4).
fn fanout_start_render(v: &serde_json::Value, json: bool) -> (i32, Vec<String>, Vec<String>) {
    let programs = v
        .get("programs")
        .and_then(|p| p.as_array())
        .cloned()
        .unwrap_or_default();
    let mut failed = Vec::new();
    let mut entries = Vec::new();
    for p in &programs {
        let name = p.get("name").and_then(|n| n.as_str()).unwrap_or("?");
        let config_path = p.get("config_path").and_then(|c| c.as_str()).unwrap_or("");
        let pid = p.get("pid").cloned().unwrap_or(serde_json::Value::Null);
        let result = p.get("result").and_then(|r| r.as_str()).unwrap_or("");
        let ok = !result.starts_with("error");
        if !ok {
            failed.push(format!("{name}: {result}"));
        }
        entries.push(serde_json::json!({
            "name": name,
            "config_path": config_path,
            "pid": pid,
            "status": if ok { "started" } else { "failed" },
            "result": result,
        }));
    }
    let code = if failed.is_empty() {
        EXIT_OK
    } else {
        EXIT_ERROR
    };
    if json {
        let text = serde_json::to_string(&entries).unwrap_or_else(|_| "[]".into());
        return (code, vec![text], Vec::new());
    }
    let mut out = Vec::new();
    for e in &entries {
        let name = e.get("name").and_then(|n| n.as_str()).unwrap_or("?");
        let config_path = e.get("config_path").and_then(|c| c.as_str()).unwrap_or("");
        let pid = e.get("pid").cloned().unwrap_or(serde_json::Value::Null);
        let status = e.get("status").and_then(|s| s.as_str()).unwrap_or("");
        let result = e.get("result").and_then(|x| x.as_str()).unwrap_or("");
        out.push(format!(
            "{} ({result})",
            program_result_line(name, config_path, &pid, status)
        ));
    }
    let mut err = Vec::new();
    if !failed.is_empty() {
        err.push(format!(
            "ak: error: {} program(s) failed to start:",
            failed.len()
        ));
        for f in &failed {
            err.push(format!("  {f}"));
        }
    }
    (code, out, err)
}

// -- add --apply --------------------------------------------------------------

/// Outcome of `add --apply` / the run ad-hoc form: the caller prints
/// `Applied`/the message and terminates with the outcome's exit code (0 for
/// `Applied`). Kept free of `process::exit` so tests can drive the branches.
#[derive(Debug)]
enum AddApplyOutcome {
    /// The control-plane result text (rendered apply result).
    Applied(String),
    /// The daemon answered but the apply failed: message + exit code.
    Failed(String, i32),
}

/// Apply one app right after registering (app scope only — other apps'
/// pending is untouched). When the daemon is offline it is pulled up in the
/// background first (background-daemon spec), so the "take effect now"
/// entries never fail with "daemon offline".
fn apply_after_add(config: &DaemonConfig, config_path: &Path, app: &str) -> AddApplyOutcome {
    let config_dir = config_dir_of(config_path);
    match daemonize::ensure_daemon(config, config_path, &config_dir) {
        daemonize::SpawnOutcome::Ready { .. } => {}
        daemonize::SpawnOutcome::TimedOut { log_path } => {
            return AddApplyOutcome::Failed(
                format!(
                    "the daemon did not become ready, so app[{app}] could not take effect; \
                     see {} (check the config with `ak validate`)",
                    log_path.display()
                ),
                EXIT_ERROR,
            );
        }
    }
    let c = client::Client::from_config(config);
    match c.apply(Some(app), None, false) {
        Ok(v) => AddApplyOutcome::Applied(
            v.get("result")
                .and_then(|r| r.as_str())
                .unwrap_or("applied")
                .to_string(),
        ),
        Err(e) => AddApplyOutcome::Failed(format!("{e:#}"), client::exit_code_of(&e)),
    }
}

// -- validate ---------------------------------------------------------------

/// The warning `validate` prints when the console is on but unauthenticated.
///
/// The console keeps TCP (browsers cannot reach a unix socket) and owns
/// stop / apply authority in-process, so with no `[webui] auth_token` the
/// isolation the control socket provides does not extend to the console: any
/// local user who can reach `webui.listen` can stop or replace programs.
/// That is a deliberate default (existing deployments must keep working, see
/// the `webui-api` capability) — but it must not stay silent.
pub fn webui_open_console_warning(config: &DaemonConfig) -> Option<String> {
    let listen = config.webui_listen()?;
    if config.webui_auth_token().is_some() {
        return None;
    }
    Some(format!(
        "warning: [webui] is enabled without auth_token — any local user able to reach \
         {listen} can stop or replace your programs via the console API; set one with \
         `ak config --set webui.auth_token=<secret>` (clients then exchange it for a \
         session cookie once)"
    ))
}

fn validate(config_path: &Path, single: Option<&Path>) -> Result<()> {
    if let Some(p) = single {
        let config = client::load_config(config_path)?;
        let (raw, _) = config::AppRaw::load(p)?;
        let name = registry::name_from_dir(p);
        let app = config::resolve_app(&name, p, &raw, config.app_default.as_ref())?;
        config::validate_all(&[app.clone()])?;
        println!(
            "OK: app {:?} with {} program(s)",
            app.name,
            app.programs.len()
        );
        for prog in &app.programs {
            println!("  - {} <{}>", prog.name, prog.command);
        }
        return Ok(());
    }
    let (config, existed) = DaemonConfig::load_or_default(config_path)?;
    if !existed {
        println!(
            "OK: no daemon config at {} (built-in defaults in effect)",
            config_path.display()
        );
    }
    let config_dir = config_dir_of(config_path);
    let apps = registry::ListedApp::good(registry::list(&config, &config_dir)?);
    let mut resolved = Vec::new();
    for l in &apps {
        let (raw, _) = config::AppRaw::load(&l.path)?;
        resolved.push(config::resolve_app(
            &l.name,
            &l.path,
            &raw,
            config.app_default.as_ref(),
        )?);
    }
    config::validate_all(&resolved)?;
    println!(
        "OK: daemon config + {} registered app(s), {} program(s)",
        resolved.len(),
        resolved.iter().map(|a| a.programs.len()).sum::<usize>()
    );
    for a in &resolved {
        println!("  app {:?} ({})", a.name, a.path.display());
        for p in &a.programs {
            println!("    - {} <{}>", p.name, p.command);
        }
    }
    if let Some(warning) = webui_open_console_warning(&config) {
        println!("{warning}");
    }
    Ok(())
}

// -- config subcommand ------------------------------------------------------

/// One short health probe after a config write (design D6): online → a reload
/// hint, offline/failure → an offline note. Never retries, never blocks and
/// never fails the action; the probe address comes from the pre-modification
/// config (a running daemon still listens on the old values). `None` when the
/// same command carries `--reload` (the reload follows immediately).
fn config_sync_hint(config: &DaemonConfig, reload_attached: bool) -> Option<&'static str> {
    if reload_attached {
        return None;
    }
    let c = client::Client::from_config(config);
    if c.health().is_ok() {
        Some(
            "note: daemon is online — run `ak config --reload` (then `ak apply` if apps \
             changed) for the new config to take effect",
        )
    } else {
        Some("note: daemon is offline — the change takes effect at next start")
    }
}

/// `ak config [--reload]`: run the local action class first (clap enforces the
/// mutual exclusion and rejects `--reload` with `--get`/`--init`), then, when
/// `--reload` was given, ask the daemon to re-read the config and rescan
/// (design D3). `--reload` alone skips the local action entirely. Any failing
/// step returns its own exit code so tests can drive the branches.
fn config_cmd(
    config_path: &Path,
    sets: &[String],
    gets: &[String],
    deletes: &[String],
    edit: bool,
    init: bool,
    reload: bool,
) -> Result<i32> {
    let local_action = !sets.is_empty() || !gets.is_empty() || !deletes.is_empty() || edit || init;
    if local_action {
        let code = config_local_cmd(config_path, sets, gets, deletes, edit, init, reload)?;
        if code != EXIT_OK {
            return Ok(code);
        }
    }
    if reload {
        return reload_cmd(config_path);
    }
    Ok(EXIT_OK)
}

/// The daemon reload half of `ak config --reload`: `POST /v1/reload`, print
/// the daemon's pending preview, map a failed answer to its exit class
/// (offline = 3, per the control-plane contract).
fn reload_cmd(config_path: &Path) -> Result<i32> {
    let config = client::load_config(config_path)?;
    let c = client::Client::from_config(&config);
    match c.reload() {
        Ok(v) => {
            println!(
                "{}",
                v.get("result")
                    .and_then(|r| r.as_str())
                    .unwrap_or("reloaded")
            );
            Ok(EXIT_OK)
        }
        Err(e) => {
            eprintln!("ak: error: {e:#}");
            Ok(client::exit_code_of(&e))
        }
    }
}

/// `ak config` local half: perform exactly one action class (clap enforces
/// the mutual exclusion). Returns the process exit code so tests can drive
/// the branches; config-level problems print to stderr and return
/// `EXIT_CONFIG` (2), IO errors bubble up (`EXIT_ERROR` via main).
/// `reload_attached` suppresses the "run `ak config --reload`" hint — the
/// reload follows immediately.
fn config_local_cmd(
    config_path: &Path,
    sets: &[String],
    gets: &[String],
    deletes: &[String],
    edit: bool,
    init: bool,
    reload_attached: bool,
) -> Result<i32> {
    // The pre-action config feeds the online probe: the daemon still listens
    // on the values that were in effect before this write. A broken config
    // file must not block the action itself (the probe is advisory, and
    // `--edit` is exactly how a broken file gets repaired) — the probe then
    // falls back to the built-in default address.
    let pre_config = client::load_config(config_path).unwrap_or_else(|_| DaemonConfig::empty());

    if !gets.is_empty() {
        // Validate every key first — an unknown key answers nothing.
        let text = match std::fs::read_to_string(config_path) {
            Ok(t) => Some(t),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => {
                return Err(e).with_context(|| format!("cannot read {}", config_path.display()));
            }
        };
        let mut out = Vec::new();
        for k in gets {
            match config::config_effective_value(text.as_deref(), k) {
                Ok(v) => out.push((k.clone(), v)),
                Err(e) => {
                    eprintln!("ak: error: {e:#}");
                    return Ok(EXIT_CONFIG);
                }
            }
        }
        if out.len() == 1 {
            println!("{}", out[0].1);
        } else {
            for (k, v) in &out {
                println!("{k}={v}");
            }
        }
        return Ok(EXIT_OK);
    }

    if !sets.is_empty() {
        let parsed = match config::parse_sets(sets) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("ak: error: {e:#}");
                return Ok(EXIT_CONFIG);
            }
        };
        if !config_path.exists() {
            eprintln!(
                "ak: error: no daemon config at {} — run `ak config --init` first",
                config_path.display()
            );
            return Ok(EXIT_CONFIG);
        }
        let text = std::fs::read_to_string(config_path)
            .with_context(|| format!("cannot read {}", config_path.display()))?;
        let rendered = match config::config_apply_sets(&text, &parsed) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("ak: error: {e:#}");
                return Ok(EXIT_CONFIG);
            }
        };
        if let Err(e) = config::atomic_write_daemon_config(config_path, &rendered) {
            eprintln!("ak: error: {e:#}");
            return Ok(EXIT_CONFIG);
        }
        for (k, kv) in &parsed {
            println!("set {k} = {kv}");
        }
        if let Some(hint) = config_sync_hint(&pre_config, reload_attached) {
            println!("{hint}");
        }
        return Ok(EXIT_OK);
    }

    if !deletes.is_empty() {
        if !config_path.exists() {
            eprintln!(
                "ak: error: no daemon config at {} — run `ak config --init` first",
                config_path.display()
            );
            return Ok(EXIT_CONFIG);
        }
        let text = std::fs::read_to_string(config_path)
            .with_context(|| format!("cannot read {}", config_path.display()))?;
        let rendered = match config::config_apply_deletes(&text, deletes) {
            Ok(config::DeleteOutcome::Changed(r)) => Some(r),
            Ok(config::DeleteOutcome::NoChange) => None,
            Err(e) => {
                eprintln!("ak: error: {e:#}");
                return Ok(EXIT_CONFIG);
            }
        };
        match rendered {
            Some(r) => {
                if let Err(e) = config::atomic_write_daemon_config(config_path, &r) {
                    eprintln!("ak: error: {e:#}");
                    return Ok(EXIT_CONFIG);
                }
                for k in deletes {
                    println!("deleted {k}");
                }
                if let Some(hint) = config_sync_hint(&pre_config, reload_attached) {
                    println!("{hint}");
                }
            }
            None => {
                println!("no change: {} was not set", deletes.join(", "));
            }
        }
        return Ok(EXIT_OK);
    }

    if edit {
        let code = edit_config(config_path)?;
        if code == EXIT_OK
            && let Some(hint) = config_sync_hint(&pre_config, reload_attached)
        {
            println!("{hint}");
        }
        return Ok(code);
    }

    if init {
        if config_path.exists() {
            eprintln!(
                "ak: error: {} already exists — refusing to initialize over an existing \
                 config (delete it first or use `ak config --edit`)",
                config_path.display()
            );
            return Ok(EXIT_CONFIG);
        }
        let rep = config::config_init(config_path)?;
        println!("created {}", rep.config_file.display());
        println!("created app dir {}", rep.app_dir.display());
        if rep.sample_created {
            println!("created {}", rep.sample_file.display());
        } else {
            println!("kept existing {}", rep.sample_file.display());
        }
        println!("next: `ak config --edit` to customize, `ak compose <dir>` to register apps");
        if let Some(hint) = config_sync_hint(&pre_config, reload_attached) {
            println!("{hint}");
        }
        return Ok(EXIT_OK);
    }

    unreachable!("an action was selected (dispatch checks the flags)")
}

/// Content written when `edit` creates a missing config file.
const EDIT_SEED: &str = "\
# appkeeper daemon config — every field is optional; built-in defaults apply
# for anything omitted. Annotated templates live in the repository:
#   conf/daemon.toml  (this file)    conf/app.toml  (per-app deployment)
";

/// Launch the user's editor on the daemon config, then validate the result.
///
/// `$VISUAL` wins over `$EDITOR` (standard precedence); both fall back to a
/// platform default. A missing config file is created (with parents) seeded
/// with `EDIT_SEED`, so first-time setup via `ak config --edit` works.
/// Returns the process exit code: `EXIT_OK` when the config is valid (or
/// absent, meaning defaults), `EXIT_CONFIG` when the editor left an invalid
/// file behind; launching the editor itself fails with `Err`.
fn edit_config(config_path: &Path) -> Result<i32> {
    let from_env = |key: &str| std::env::var(key).ok().filter(|v| !v.trim().is_empty());
    let editor = from_env("VISUAL")
        .or_else(|| from_env("EDITOR"))
        .unwrap_or_else(|| {
            if cfg!(windows) {
                "notepad".to_string()
            } else {
                "vi".to_string()
            }
        });
    edit_config_with(&editor, config_path)
}

fn edit_config_with(editor: &str, config_path: &Path) -> Result<i32> {
    if !config_path.exists() {
        if let Some(dir) = config_path.parent() {
            std::fs::create_dir_all(dir)
                .with_context(|| format!("cannot create config dir {}", dir.display()))?;
        }
        std::fs::write(config_path, EDIT_SEED)
            .with_context(|| format!("cannot create {}", config_path.display()))?;
        println!("created {}", config_path.display());
    }

    let mut argv = config::split_command(editor);
    let prog = argv.drain(..).next().context("editor command is empty")?;
    let status = std::process::Command::new(prog)
        .args(argv)
        .arg(config_path)
        .status()
        .with_context(|| format!("cannot launch editor {editor:?}"))?;
    if !status.success() {
        bail!("editor {editor:?} exited with {status}");
    }

    // Report what the edit produced, mirroring `validate` wording.
    let (cfg, existed) = match DaemonConfig::load_or_default(config_path) {
        Ok(pair) => pair,
        Err(e) => {
            eprintln!("ak: daemon config is invalid:\n{e:#}");
            return Ok(EXIT_CONFIG);
        }
    };
    if let Err(e) = cfg.validate() {
        eprintln!("ak: daemon config is invalid:\n{e:#}");
        return Ok(EXIT_CONFIG);
    }
    if existed {
        println!("OK: daemon config at {} is valid", config_path.display());
    } else {
        println!(
            "OK: no daemon config at {} (built-in defaults in effect)",
            config_path.display()
        );
    }
    Ok(EXIT_OK)
}

// -- daemon -----------------------------------------------------------------
/// `daemon start` is the only daemon run entry (`--foreground` for the
/// attached form, `--service <name>` for the SCM-managed form). The web
/// console is config-driven: its initial state comes from the `[webui]`
/// section of the daemon config (absent = off), and `ak config --reload`
/// converges it later at runtime.
///
/// `service_name` is set when the Windows Service Control Manager starts the
/// daemon (`ak daemon start --service <name>`); `None` is plain console mode.
fn run_daemon(config_path: &Path, service_name: Option<&str>) -> Result<()> {
    #[cfg(windows)]
    if let Some(service_name) = service_name {
        return service::run_windows_service(config_path, service_name);
    }
    #[cfg(not(windows))]
    if service_name.is_some() {
        // Usage-class error (control-plane spec exit-code contract): 2, not
        // the generic 1 an `anyhow::bail!` would map to.
        eprintln!("ak: error: `--service` is only supported on Windows (Service Control Manager)");
        std::process::exit(EXIT_CONFIG);
    }
    run_daemon_console(config_path)
}

/// Foreground/console daemon: `ak daemon start --foreground` without
/// `--service`.
fn run_daemon_console(config_path: &Path) -> Result<()> {
    let sup = run_daemon_core(config_path)?;
    install_stop_signals(&sup)?;
    sup.run();
    Ok(())
}

/// Wire the console daemon's stop signals to the existing shutdown intent.
///
/// Unix (handle-daemon-quit-signal D1/D2): SIGINT, SIGTERM and SIGQUIT all
/// feed one self-pipe notifier thread, which only raises
/// [`Supervisor::request_shutdown`]. The signal actions stay installed for the
/// rest of the process, so a repeated delivery during a graceful stop cannot
/// fall back to the default (terminating) action; the intent itself is
/// idempotent (design D3). An installation failure aborts startup rather than
/// entering the supervised loop unprotected.
///
/// Windows keeps the `ctrlc` Ctrl+C path here; the SCM service shell
/// (`service::run_windows_service`) is a separate entry and is untouched.
fn install_stop_signals(sup: &Arc<Supervisor>) -> Result<()> {
    #[cfg(unix)]
    {
        let sup = sup.clone();
        shutdown_signal::install(move || sup.request_shutdown())?;
        info!(
            "ak {} running (Ctrl+C / SIGTERM / SIGQUIT to stop)",
            env!("CARGO_PKG_VERSION")
        );
    }
    #[cfg(not(unix))]
    {
        let sup = sup.clone();
        ctrlc::set_handler(move || {
            info!("shutdown signal received");
            sup.request_shutdown();
        })
        .context("failed to install Ctrl+C handler")?;
        info!("ak {} running (Ctrl+C to stop)", env!("CARGO_PKG_VERSION"));
    }
    Ok(())
}

/// Load config, wire the supervisor and its sidecar threads, bind the API and
/// bootstrap the registry. Shared by console mode and Windows service mode.
fn run_daemon_core(config_path: &Path) -> Result<Arc<Supervisor>> {
    let (config, existed) = DaemonConfig::load_or_default(config_path)
        .with_context(|| format!("failed to load daemon config {}", config_path.display()))?;
    if !existed {
        info!(
            "no daemon config at {} — starting empty with built-in defaults",
            config_path.display()
        );
    }

    // Honor RUST_LOG over the config level.
    env_logger::Builder::from_env(
        env_logger::Env::default().default_filter_or(&config.daemon.log_level),
    )
    .try_init()
    .ok();
    info!(
        "daemon config: {}{}",
        config_path.display(),
        if existed { "" } else { " (absent)" }
    );

    // Capture the console intent before `config` is moved into the supervisor.
    let webui_intent = supervisor::WebuiIntent::from_config(&config);

    let config_dir = config_dir_of(config_path);
    let sup = Supervisor::new(config, &config_dir)?;
    sup.set_config_path(config_path);

    // Web console manager: converges the running console to the latest
    // intent (published here for startup and by `reload` later). Bind
    // failures degrade non-fatally; the manager exits with the daemon.
    sup.webui.set_desired(webui_intent);
    {
        let sup2 = sup.clone();
        std::thread::Builder::new()
            .name("webui-manager".into())
            .spawn(move || supervisor::webui_manager_loop(sup2))
            .context("failed to spawn webui manager thread")?;
    }

    // Metrics sampler: 1 Hz read-only sideband (CPU/RSS + log rates). Runs
    // for the daemon; exits with it. Its failure only degrades metric
    // fields to null.
    metrics::spawn_sampler(sup.clone());

    // Health checker thread: probes due tasks, reports back into the state
    // and enqueues restarts for restart_on_unhealthy programs.
    {
        let sup = sup.clone();
        let tasks = sup.health_tasks.clone();
        std::thread::Builder::new()
            .name("health-checker".into())
            .spawn(move || {
                health::checker_loop(tasks, move |name, ok| {
                    let restart = {
                        let mut st = sup.state.lock().unwrap();
                        st.programs
                            .get_mut(&name)
                            .map(|p| p.record_health(ok, std::time::Instant::now()))
                            .unwrap_or(false)
                    };
                    if restart {
                        sup.enqueue(supervisor::Command::HealthRestart { name });
                    }
                });
            })
            .context("failed to spawn health checker")?;
    }

    // Bind the control endpoint first so a conflict (a live instance, or a
    // non-socket file at the path) fails fast and clearly, then serve
    // connections on a background thread while the main thread supervises.
    let listener = server::bind(&sup)?;
    {
        let sup2 = sup.clone();
        std::thread::Builder::new()
            .name("api-accept".into())
            .spawn(move || server::serve(sup2, listener))
            .context("failed to spawn api accept loop")?;
    }

    // Bootstrap the registry; the caller decides how to wait for shutdown
    // (Ctrl+C handler in console mode, SCM status reporting in service mode).
    sup.bootstrap()?;
    Ok(sup)
}

#[cfg(test)]
#[cfg(unix)] // every test in here needs a real editor process (/bin/sh)
mod edit_tests {
    static EDIT_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    use super::*;

    #[cfg(unix)]
    fn temp_root(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ak-test-edit-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    #[cfg(unix)]
    fn edit_creates_missing_config_and_passes() {
        let _guard = EDIT_TEST_LOCK.lock().unwrap();
        let dir = temp_root("create");
        let path = dir.join("nested").join("daemon.toml");
        // `true` exits 0 without touching the file.
        let code = edit_config_with("true", &path).unwrap();
        assert_eq!(code, EXIT_OK);
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.contains("# appkeeper daemon config"),
            "seed missing: {text:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    #[cfg(unix)]
    fn edit_reports_invalid_result_with_config_exit_code() {
        // Spawning a real editor process here is flaky under full-parallel
        // test load (observed sporadic failures only in `cargo test` runs);
        // serialize the editor-spawning tests.
        let _guard = EDIT_TEST_LOCK.lock().unwrap();
        let dir = temp_root("invalid");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("daemon.toml");
        std::fs::write(&path, "").unwrap();
        let script = dir.join("fake-editor.sh");
        std::fs::write(&script, "#!/bin/sh\nprintf 'bogus_field = 1\\n' > \"$1\"\n").unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let code = edit_config_with(script.to_str().unwrap(), &path).unwrap();
        assert_eq!(code, EXIT_CONFIG);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    #[cfg(unix)]
    fn editor_must_exist() {
        let dir = temp_root("noeditor");
        let path = dir.join("daemon.toml");
        let r = edit_config_with("ak-definitely-not-an-editor", &path);
        assert!(r.is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    #[cfg(unix)]
    fn editor_abnormal_exit_is_an_error() {
        // Spec: 编辑器异常退出以非零码退出（Err bubbles up to EXIT_ERROR）.
        let _guard = EDIT_TEST_LOCK.lock().unwrap();
        let dir = temp_root("abnormal");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("daemon.toml");
        std::fs::write(&path, "[daemon]\nsocket = \"/tmp/ak.sock\"\n").unwrap();
        let script = dir.join("fake-editor.sh");
        std::fs::write(&script, "#!/bin/sh\nexit 3\n").unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let r = edit_config_with(script.to_str().unwrap(), &path);
        assert!(
            r.is_err(),
            "an editor that exits non-zero must fail the command"
        );
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "[daemon]\nsocket = \"/tmp/ak.sock\"\n",
            "the file keeps its pre-edit content"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod sync_tests {
    use super::*;

    /// A private control endpoint for tests: the mock control plane binds it
    /// before the client is allowed to connect, so there is no startup race.
    fn mock_socket(tag: &str) -> (PathBuf, crate::transport::Listener) {
        let dir = std::env::temp_dir().join(format!("ak-test-mock-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let socket = dir.join("ak.sock");
        let ep = crate::transport::Endpoint::parse(&socket.display().to_string()).unwrap();
        let listener = ep.bind().unwrap();
        (socket, listener)
    }

    fn config_with_socket(socket: &Path) -> DaemonConfig {
        toml::from_str(&format!(
            "[daemon]\nsocket = {:?}\n",
            socket.to_string_lossy()
        ))
        .unwrap()
    }

    /// A reachable daemon whose reload fails must surface as an error (main
    /// maps it to a non-zero exit), never a swallowed warning — the registry
    /// changed but the daemon did not pick it up.
    #[test]
    fn reload_failure_while_online_is_an_error() {
        let (socket, mut listener) = mock_socket("reload-fail");
        let server = std::thread::spawn(move || {
            for _ in 0..2 {
                let mut sock = listener.accept().unwrap();
                let mut buf = [0u8; 4096];
                let n = std::io::Read::read(&mut sock, &mut buf).unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]).to_string();
                let (status, body) = if req.starts_with("GET /v1/health") {
                    ("200 OK", "{}")
                } else {
                    ("500 Internal Server Error", "{\"error\":\"reload boom\"}")
                };
                let resp = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\n\
                     Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                std::io::Write::write_all(&mut sock, resp.as_bytes()).unwrap();
            }
        });

        let config = config_with_socket(&socket);
        let r = sync_if_online(&config);
        let err = r.err().expect("reload failure while online must error");
        assert!(
            err.to_string().contains("failed to rescan"),
            "error must explain the sync failure: {err:#}"
        );
        server.join().unwrap();
    }

    /// guard-remove-until-stopped 2.2: `ak remove` is a single online call to
    /// the daemon-owned endpoint. It never probes/pulls up a daemon first, and
    /// it never deletes the registration record or asks for a reload/apply
    /// itself — the daemon owns all of that.
    #[test]
    fn remove_cli_is_one_online_call_and_touches_no_files() {
        let (socket, mut listener) = mock_socket("remove-cli");
        let server = std::thread::spawn(move || {
            let mut sock = listener.accept().unwrap();
            let mut raw = Vec::new();
            let mut byte = [0u8; 1];
            while !raw.ends_with(b"\r\n\r\n") {
                if std::io::Read::read(&mut sock, &mut byte).unwrap_or(0) == 0 {
                    break;
                }
                raw.push(byte[0]);
            }
            let req = String::from_utf8_lossy(&raw).to_string();
            let body = concat!(
                r#"{"app":"web","config_path":"/tmp/apps/web.toml","deleted_file":true,"#,
                r#""kept_config":null,"programs":["api"],"applied":true,"#,
                r#""result":"app[web] removed; no apply needed"}"#
            );
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            std::io::Write::write_all(&mut sock, resp.as_bytes()).unwrap();
            req
        });

        let dir = std::env::temp_dir().join(format!("ak-test-remove-cli-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let config_path = dir.join("daemon.toml");
        // The record exists on disk; the client must leave it alone.
        let record = dir.join("web.toml");
        std::fs::write(&record, "[program.api]\ncommand = \"sleep\"\n").unwrap();
        std::fs::write(
            &config_path,
            format!(
                "[daemon]\nsocket = {:?}\napp_dir = {:?}\n",
                socket.to_string_lossy(),
                dir.to_string_lossy()
            ),
        )
        .unwrap();

        remove_cmd(&config_path, "web").expect("the daemon answered 200");
        let req = server.join().unwrap();
        assert!(
            req.starts_with("POST /v1/apps/web/remove HTTP/1.1\r\n"),
            "one targeted call, no preflight: {req}"
        );
        assert!(
            !req.contains("/v1/health")
                && !req.contains("/v1/apply")
                && !req.contains("/v1/reload"),
            "no implicit pull-up / follow-up sync: {req}"
        );
        assert!(
            record.exists(),
            "the CLI never deletes registration files itself"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Offline: registry ops are offline-first; unreachable daemon means
    /// nothing to sync — silent success.
    #[test]
    fn offline_sync_is_a_silent_success() {
        // Nothing is bound at this path, so the probe cannot succeed.
        let config = config_with_socket(Path::new("/tmp/ak-test-mock-absent/ak.sock"));
        sync_if_online(&config).expect("offline sync must be a silent success");
    }

    /// add --apply online: the daemon is already healthy, so no spawn happens
    /// (online short-circuit) and one app-scoped apply is sent; the rendered
    /// control-plane result comes back (fake daemon, sync_tests style).
    #[test]
    fn add_apply_online_runs_app_scoped_apply() {
        let (socket, mut listener) = mock_socket("apply-online");
        let server = std::thread::spawn(move || {
            for _ in 0..2 {
                let mut sock = listener.accept().unwrap();
                // Read the full request: headers to the blank line, then the
                // Content-Length body (it may arrive in a separate segment).
                let mut raw = Vec::new();
                let mut byte = [0u8; 1];
                while !raw.ends_with(b"\r\n\r\n") {
                    if std::io::Read::read(&mut sock, &mut byte).unwrap_or(0) == 0 {
                        break;
                    }
                    raw.push(byte[0]);
                }
                let head = String::from_utf8_lossy(&raw).to_string();
                let len: usize = head
                    .to_ascii_lowercase()
                    .lines()
                    .find_map(|l| l.strip_prefix("content-length:"))
                    .and_then(|v| v.trim().parse().ok())
                    .unwrap_or(0);
                let mut body_bytes = vec![0u8; len];
                if len > 0 {
                    std::io::Read::read_exact(&mut sock, &mut body_bytes).unwrap();
                }
                let req = format!(
                    "{}{}",
                    head.lines().next().unwrap_or(""),
                    String::from_utf8_lossy(&body_bytes)
                );
                let (status, body) = if req.starts_with("GET /v1/health") {
                    ("200 OK", "{}".to_string())
                } else {
                    assert!(req.starts_with("POST /v1/apply"), "apply is POSTed: {req}");
                    assert!(
                        req.contains("\"app\": \"abc\"") || req.contains("\"app\":\"abc\""),
                        "app scope rides in the body: {req}"
                    );
                    (
                        "200 OK",
                        "{\"result\":\"changed:\\n  abc.abc -> start\"}".to_string(),
                    )
                };
                let resp = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\n\
                     Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                std::io::Write::write_all(&mut sock, resp.as_bytes()).unwrap();
            }
        });

        let dir = std::env::temp_dir().join(format!("ak-test-apply-online-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let config_path = dir.join("daemon.toml");
        std::fs::write(&config_path, "").unwrap();
        let config = config_with_socket(&socket);
        match apply_after_add(&config, &config_path, "abc") {
            AddApplyOutcome::Applied(text) => {
                assert!(text.contains("abc.abc -> start"), "result text: {text}");
            }
            other => panic!("online add --apply must be Applied, got {other:?}"),
        }
        server.join().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod config_cmd_tests {
    use super::*;

    fn temp_root(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ak-test-cmd-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    // -- clap: parsing and action mutual exclusion (task 1.1) ----------------

    #[test]
    fn clap_config_repeated_set_parses() {
        let cli =
            Cli::try_parse_from(["appkeeper", "config", "--set", "port=1", "--set", "host=x"])
                .unwrap();
        match cli.cmd {
            Some(Cmd::Config {
                set,
                get,
                delete,
                edit,
                init,
                reload,
            }) => {
                assert_eq!(set, vec!["port=1", "host=x"]);
                assert!(get.is_empty() && delete.is_empty() && !edit && !init && !reload);
            }
            other => panic!("expected Config, got {other:?}"),
        }
    }

    #[test]
    fn clap_config_bare_action_flags_parse() {
        let cli = Cli::try_parse_from(["appkeeper", "config", "--edit"]).unwrap();
        assert!(matches!(cli.cmd, Some(Cmd::Config { edit: true, .. })));
        let cli = Cli::try_parse_from(["appkeeper", "config", "--init"]).unwrap();
        assert!(matches!(cli.cmd, Some(Cmd::Config { init: true, .. })));
        // Bare `config` parses too — dispatch then prints help with exit 0.
        let cli = Cli::try_parse_from(["appkeeper", "config"]).unwrap();
        match cli.cmd {
            Some(Cmd::Config {
                set,
                get,
                delete,
                edit,
                init,
                reload,
            }) => {
                assert!(set.is_empty() && get.is_empty() && delete.is_empty());
                assert!(!edit && !init && !reload);
            }
            other => panic!("expected Config, got {other:?}"),
        }
    }

    #[test]
    fn clap_config_actions_are_mutually_exclusive() {
        // One class of action per call (spec: 动作互斥).
        let combos: &[&[&str]] = &[
            &["--init", "--get", "port"],
            &["--set", "port=1", "--delete", "port"],
            &["--set", "port=1", "--get", "port"],
            &["--get", "port", "--delete", "port"],
            &["--edit", "--init"],
            &["--edit", "--set", "port=1"],
            &["--edit", "--get", "port"],
            &["--edit", "--delete", "port"],
            &["--init", "--set", "port=1"],
            &["--init", "--delete", "port"],
        ];
        for combo in combos {
            let mut argv = vec!["appkeeper", "config"];
            argv.extend_from_slice(combo);
            let r = Cli::try_parse_from(&argv);
            assert!(r.is_err(), "config {combo:?} must be rejected");
        }
    }

    #[test]
    fn config_no_action_prints_help_and_exits_zero() {
        // Spec: 无任何动作 flag 时打印五个动作 flag 的用法说明并以 0 退出。
        // dispatch renders the subcommand help and returns Ok (exit 0); the
        // usage content itself is asserted by the e2e config pass (K13).
        let cli = Cli::try_parse_from(["appkeeper", "config"]).unwrap();
        dispatch(&cli).expect("bare `ak config` must exit 0");
    }

    // -- orchestration: exit codes over real temp files -----------------------

    #[test]
    fn set_missing_file_is_rejected_with_init_hint() {
        let dir = temp_root("set-missing");
        let path = dir.join("daemon.toml");
        let code = config_cmd(
            &path,
            &["socket=/tmp/ak.sock".to_string()],
            &[],
            &[],
            false,
            false,
            false,
        )
        .unwrap();
        assert_eq!(code, EXIT_CONFIG, "missing file set must exit 2");
        assert!(!path.exists(), "a rejected set must not create the file");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn delete_missing_file_is_rejected() {
        let dir = temp_root("del-missing");
        let path = dir.join("daemon.toml");
        let code = config_cmd(
            &path,
            &[],
            &[],
            &["socket".to_string()],
            false,
            false,
            false,
        )
        .unwrap();
        assert_eq!(code, EXIT_CONFIG);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn get_missing_file_answers_defaults_and_exits_zero() {
        let dir = temp_root("get-missing");
        let path = dir.join("nested").join("daemon.toml");
        let code = config_cmd(
            &path,
            &[],
            &["socket".to_string()],
            &[],
            false,
            false,
            false,
        )
        .unwrap();
        assert_eq!(code, EXIT_OK, "--get on a missing file is all defaults");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn set_get_delete_roundtrip_over_a_real_file() {
        let dir = temp_root("roundtrip");
        let path = dir.join("daemon.toml");
        std::fs::write(&path, "[daemon]\nsocket = \"/tmp/old.sock\"\n").unwrap();

        // set: exit 0, value lands on disk.
        let code = config_cmd(
            &path,
            &[
                "socket=/tmp/new.sock".to_string(),
                "log_level=debug".to_string(),
            ],
            &[],
            &[],
            false,
            false,
            false,
        )
        .unwrap();
        assert_eq!(code, EXIT_OK);
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.contains("socket = \"/tmp/new.sock\"") && text.contains("log_level = \"debug\""),
            "{text}"
        );
        // A rejected set leaves the file byte-identical.
        let before = text.clone();
        let code = config_cmd(
            &path,
            &["socket=".to_string()],
            &[],
            &[],
            false,
            false,
            false,
        )
        .unwrap();
        assert_eq!(code, EXIT_CONFIG, "an empty socket exits 2");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
        // A removed key is refused with an explanation and writes nothing.
        let code = config_cmd(
            &path,
            &["port=8080".to_string()],
            &[],
            &[],
            false,
            false,
            false,
        )
        .unwrap();
        assert_eq!(code, EXIT_CONFIG, "a removed key exits 2");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), before);

        // delete: exit 0, key gone; second delete of an absent key is still 0.
        let code = config_cmd(
            &path,
            &[],
            &[],
            &["socket".to_string()],
            false,
            false,
            false,
        )
        .unwrap();
        assert_eq!(code, EXIT_OK);
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains("/tmp/new.sock"), "{text}");
        assert!(
            text.contains("log_level = \"debug\""),
            "untouched key kept: {text}"
        );
        let before = text.clone();
        let code = config_cmd(
            &path,
            &[],
            &[],
            &["socket".to_string()],
            false,
            false,
            false,
        )
        .unwrap();
        assert_eq!(code, EXIT_OK, "absent-key delete is idempotent success");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn get_unknown_key_is_rejected_with_config_exit_code() {
        let dir = temp_root("get-unknown");
        let path = dir.join("daemon.toml");
        std::fs::write(&path, "[daemon]\nsocket = \"/tmp/ak.sock\"\n").unwrap();
        let code = config_cmd(&path, &[], &["foo".to_string()], &[], false, false, false).unwrap();
        assert_eq!(code, EXIT_CONFIG, "an unknown --get key must exit 2");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn delete_multiple_targets_in_one_call() {
        // --delete is repeatable (spec): several targets go in one call.
        let dir = temp_root("del-multi");
        let path = dir.join("daemon.toml");
        std::fs::write(
            &path,
            "[daemon]\n# socket note\nsocket = \"/tmp/old.sock\"\nlog_level = \"debug\"\n",
        )
        .unwrap();
        let code = config_cmd(
            &path,
            &[],
            &[],
            &["socket".to_string(), "log_buffer_lines".to_string()],
            false,
            false,
            false,
        )
        .unwrap();
        assert_eq!(code, EXIT_OK);
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            !text.contains("/tmp/old.sock") && !text.contains("log_buffer_lines"),
            "{text}"
        );
        assert!(
            text.contains("log_level = \"debug\""),
            "the untouched key survives: {text}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn init_creates_workspace_and_second_init_is_rejected() {
        let dir = temp_root("init");
        let path = dir.join("conf").join("daemon.toml");
        let code = config_cmd(&path, &[], &[], &[], false, true, false).unwrap();
        assert_eq!(code, EXIT_OK);
        assert!(path.is_file());
        assert!(dir.join("conf/app.d/example.toml.sample").is_file());
        // Second init on the existing config: exit 2, file untouched.
        let before = std::fs::read_to_string(&path).unwrap();
        let code = config_cmd(&path, &[], &[], &[], false, true, false).unwrap();
        assert_eq!(code, EXIT_CONFIG);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn init_then_set_then_get_roundtrip() {
        let dir = temp_root("init-flow");
        let path = dir.join("daemon.toml");
        assert_eq!(
            config_cmd(&path, &[], &[], &[], false, true, false).unwrap(),
            EXIT_OK
        );
        // set on the init-produced file works (8-key template is legal).
        assert_eq!(
            config_cmd(
                &path,
                &["monitor_interval=2.5".to_string()],
                &[],
                &[],
                false,
                false,
                false
            )
            .unwrap(),
            EXIT_OK
        );
        assert_eq!(
            config_cmd(
                &path,
                &[],
                &["monitor_interval".to_string()],
                &[],
                false,
                false,
                false
            )
            .unwrap(),
            EXIT_OK
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    // -- online probe (task 4.1): mock daemon, both branches ------------------

    /// A /v1/health answerer on a private control socket, sync_tests style.

    /// A private control endpoint for tests: the mock control plane binds it
    /// before the client is allowed to connect, so there is no startup race.
    fn mock_socket(tag: &str) -> (PathBuf, crate::transport::Listener) {
        let dir = std::env::temp_dir().join(format!("ak-test-mock-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let socket = dir.join("ak.sock");
        let ep = crate::transport::Endpoint::parse(&socket.display().to_string()).unwrap();
        let listener = ep.bind().unwrap();
        (socket, listener)
    }

    fn config_with_socket(socket: &Path) -> DaemonConfig {
        toml::from_str(&format!(
            "[daemon]\nsocket = {:?}\n",
            socket.to_string_lossy()
        ))
        .unwrap()
    }

    #[test]
    fn sync_hint_online_suggests_reload() {
        let (socket, mut listener) = mock_socket("sync-online");
        let server = std::thread::spawn(move || {
            let mut sock = listener.accept().unwrap();
            let mut buf = [0u8; 4096];
            let n = std::io::Read::read(&mut sock, &mut buf).unwrap_or(0);
            let req = String::from_utf8_lossy(&buf[..n]).to_string();
            assert!(
                req.starts_with("GET /v1/health"),
                "probe hits /v1/health: {req}"
            );
            let resp = "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
                        Content-Length: 2\r\nConnection: close\r\n\r\n{}";
            std::io::Write::write_all(&mut sock, resp.as_bytes()).unwrap();
        });
        let config = config_with_socket(&socket);
        let msg = config_sync_hint(&config, false).expect("online hint");
        assert!(msg.contains("online") && msg.contains("reload"), "{msg}");
        assert!(
            msg.contains("ak config --reload"),
            "the hint names the migrated entry point: {msg}"
        );
        server.join().unwrap();
    }

    #[test]
    fn sync_hint_offline_names_next_start() {
        let config = config_with_socket(Path::new("/tmp/ak-test-mock-absent/ak.sock"));
        let msg = config_sync_hint(&config, false).expect("offline hint");
        assert!(
            msg.contains("offline") && msg.contains("next start"),
            "{msg}"
        );
    }

    /// A write combined with `--reload` prints no "run the reload" hint — the
    /// reload follows in the same command (configuration spec).
    #[test]
    fn sync_hint_suppressed_when_reload_is_attached() {
        let config = config_with_socket(Path::new("/tmp/ak-test-mock-absent/ak.sock"));
        assert!(config_sync_hint(&config, true).is_none());
    }
}

// ---------------------------------------------------------------------------
// tests: the `validate` console-auth warning (unix-socket-control-plane 6.4)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod validate_warning_tests {
    use super::*;

    fn daemon_config(toml_text: &str) -> DaemonConfig {
        toml::from_str(toml_text).unwrap()
    }

    #[test]
    fn console_without_token_warns_with_listen_address() {
        let config = daemon_config("[webui]\nlisten = \"127.0.0.1:8527\"\n");
        let warning = webui_open_console_warning(&config).expect("an open console must warn");
        assert!(warning.contains("127.0.0.1:8527"), "{warning}");
        assert!(warning.contains("without auth_token"), "{warning}");
        // The warning must name what is actually at risk, not just "insecure".
        assert!(warning.contains("stop or replace"), "{warning}");
        // And it must name the fix.
        assert!(
            warning.contains("ak config --set webui.auth_token="),
            "{warning}"
        );
    }

    #[test]
    fn console_with_token_stays_silent() {
        let config =
            daemon_config("[webui]\nlisten = \"127.0.0.1:8527\"\nauth_token = \"s3cret\"\n");
        assert!(webui_open_console_warning(&config).is_none());
    }

    #[test]
    fn console_absent_stays_silent() {
        let config = daemon_config("[daemon]\nlog_level = \"warn\"\n");
        assert!(webui_open_console_warning(&config).is_none());
    }
}

// ---------------------------------------------------------------------------
// tests: the client `run` verb, the `daemon` entry, the removed daemon-form
// interception, `log` multi-name and `status --json`
// (cli-revamp tasks 2.1 / 2.2 / 2.3; daemon-subcommand tasks 1.x / 2.x / 4.1)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod cli_verb_tests {
    use super::*;

    fn parse(argv: &[&str]) -> Cli {
        let mut full = vec!["appkeeper"];
        full.extend_from_slice(argv);
        Cli::try_parse_from(&full).unwrap_or_else(|e| panic!("{argv:?} must parse: {e}"))
    }

    fn parse_err(argv: &[&str]) -> String {
        let mut full = vec!["appkeeper"];
        full.extend_from_slice(argv);
        Cli::try_parse_from(&full)
            .err()
            .unwrap_or_else(|| panic!("{argv:?} must be rejected"))
            .to_string()
    }

    /// `daemon` is a subcommand family (control-plane / background-daemon
    /// specs): `start` carries `--foreground` and the hidden `--service`;
    /// `install`/`uninstall` keep the former `service` flags verbatim;
    /// `shutdown` is the client stop; bare `daemon` parses to no subcommand
    /// (dispatch turns that into the exit-2 usage error).
    #[test]
    fn daemon_subcommand_parses() {
        match parse(&["daemon"]).cmd {
            Some(Cmd::Daemon { cmd: None }) => {}
            other => panic!("expected bare Daemon, got {other:?}"),
        }
        let cli = parse(&["daemon", "start"]);
        assert!(matches!(
            cli.cmd,
            Some(Cmd::Daemon {
                cmd: Some(DaemonCmd::Start {
                    foreground: false,
                    service: None
                })
            })
        ));
        let cli = parse(&["daemon", "start", "--foreground"]);
        assert!(matches!(
            cli.cmd,
            Some(Cmd::Daemon {
                cmd: Some(DaemonCmd::Start {
                    foreground: true,
                    ..
                })
            })
        ));
        // The hidden service marker conflicts with --foreground.
        let cli = parse(&["daemon", "start", "--service", "ak"]);
        assert!(matches!(
            cli.cmd,
            Some(Cmd::Daemon {
                cmd: Some(DaemonCmd::Start {
                    service: Some(ref s),
                    ..
                })
            }) if s == "ak"
        ));
        assert!(
            parse_err(&["daemon", "start", "--foreground", "--service", "ak"])
                .contains("cannot be used with")
        );
        // The global `-c/--config` keeps working in both positions.
        let cli = parse(&["-c", "/tmp/x.toml", "daemon", "start"]);
        assert!(matches!(
            cli.cmd,
            Some(Cmd::Daemon {
                cmd: Some(DaemonCmd::Start { .. })
            })
        ));
        assert_eq!(cli.config.as_deref(), Some(Path::new("/tmp/x.toml")));
        // `install` keeps the whole former service flag set.
        let cli = parse(&[
            "daemon",
            "install",
            "--now",
            "--force",
            "--name",
            "ak",
            "--desc",
            "d",
            "--user",
            "u",
            "--restart",
            "on-failure",
        ]);
        match cli.cmd {
            Some(Cmd::Daemon {
                cmd:
                    Some(DaemonCmd::Install {
                        start,
                        force,
                        name,
                        desc,
                        user,
                        restart,
                        ..
                    }),
            }) => {
                assert!(start && force);
                assert_eq!(name.as_deref(), Some("ak"));
                assert_eq!(desc.as_deref(), Some("d"));
                assert_eq!(user.as_deref(), Some("u"));
                assert_eq!(restart, service::RestartPolicy::OnFailure);
            }
            other => panic!("expected daemon install, got {other:?}"),
        }
        assert!(matches!(
            parse(&["daemon", "uninstall", "--unit-file", "/etc/x.service"]).cmd,
            Some(Cmd::Daemon {
                cmd: Some(DaemonCmd::Uninstall { .. })
            })
        ));
        assert!(matches!(
            parse(&["daemon", "shutdown"]).cmd,
            Some(Cmd::Daemon {
                cmd: Some(DaemonCmd::Shutdown)
            })
        ));
        // The removed `--detach` flag no longer exists.
        assert!(parse_err(&["daemon", "--detach"]).contains("unexpected argument"));
    }

    /// Bare `ak` (cmd = None) prints the full help to stdout and exits 0 —
    /// it must never start the daemon (design D1 / control-plane spec).
    #[test]
    fn bare_ak_is_help_exit_zero() {
        let cli = Cli::try_parse_from(["appkeeper"]).unwrap();
        assert!(cli.cmd.is_none(), "no subcommand parses to cmd = None");
        // dispatch prints `render_help` to stdout and returns Ok (exit 0).
        dispatch(&cli).expect("bare `ak` must be a successful no-op");
        // The help the branch renders is the full `ak --help` text, which
        // lists both the daemon entry and the client verb.
        let help = Cli::command().render_help().to_string();
        assert!(help.contains("daemon"), "help lists the daemon entry");
        assert!(help.contains("run"), "help lists the run verb");
    }

    /// The removed daemon form of `run` is recognized before clap (design D2):
    /// signature = first subcommand `run` + only `-c/--config` (both positions,
    /// `--config=<path>` included) and `--detach`. Client forms never match.
    #[test]
    fn daemon_form_run_signature_detection() {
        let s = |xs: &[&str]| -> Option<bool> {
            daemon_run_form(&xs.iter().map(|x| x.to_string()).collect::<Vec<_>>())
        };
        // Daemon-form signatures: detached, config-only, mixed. Bare `run`
        // has no daemon marker — it must reach dispatch for the
        // "nothing to run" usage error (design D4), not this interceptor.
        assert_eq!(s(&["run"]), None);
        assert_eq!(s(&["run", "--detach"]), Some(true));
        assert_eq!(s(&["run", "-c", "/tmp/x.toml"]), Some(false));
        assert_eq!(s(&["run", "--config", "/tmp/x.toml"]), Some(false));
        assert_eq!(s(&["run", "--config=/tmp/x.toml"]), Some(false));
        assert_eq!(s(&["-c", "/tmp/x.toml", "run"]), Some(false));
        assert_eq!(s(&["--config=/tmp/x.toml", "run"]), Some(false));
        assert_eq!(s(&["--detach", "run"]), Some(true));
        assert_eq!(s(&["run", "--detach", "-c", "/tmp/x.toml"]), Some(true));
        // A client operand after `run` wins even when a global --config
        // precedes it: client forms are never intercepted.
        assert_eq!(s(&["-c", "/tmp/x.toml", "run", "web"]), None);
        assert_eq!(s(&["-c", "/tmp/x.toml", "run", "--", "node"]), None);
        // Client forms are never intercepted.
        assert_eq!(s(&["run", "web"]), None);
        assert_eq!(s(&["run", "--app", "demo"]), None);
        assert_eq!(s(&["run", "--", "node", "x"]), None);
        assert_eq!(s(&["run", "--cmd", "node x"]), None);
        assert_eq!(s(&["run", "--app", "web"]), None);
        assert_eq!(s(&["run", "start"]), None);
        // Other subcommands and empty argv are untouched.
        assert_eq!(s(&["ps"]), None);
        assert_eq!(s(&["daemon", "start"]), None);
        assert_eq!(s(&[]), None);
    }

    /// The bare-`run` usage error (design D4) lists the two ad-hoc forms.
    /// The bare-`run` usage error lists the two ad-hoc forms only (the start
    /// forms moved to `start`, design D2/D4).
    #[test]
    fn nothing_to_run_hint_lists_the_two_adhoc_forms() {
        let hint = run_client_forms_hint();
        assert!(hint.contains("ak run -- <cmd...>"), "{hint}");
        assert!(hint.contains("ak run --cmd"), "{hint}");
        assert!(!hint.contains("run <name>"), "{hint}");
        assert!(!hint.contains("run --app"), "{hint}");
    }

    /// `run` carries no positional start form and no fan-out flag: a bare
    /// token or `--app <app>` is no longer accepted (the ad-hoc app name is
    /// `--app`, so a *value* parses but a positional does not).
    #[test]
    fn run_start_forms_are_gone() {
        assert!(parse_err(&["run", "web"]).contains("unexpected argument"));
        // `--app` now names the ad-hoc app (it takes a value, so it parses);
        // the removed program-level fan-out semantics are gone with it.
        let cli = parse(&["run", "--", "python", "-c", "x", "--app", "demo"]);
        let cmd = match cli.cmd {
            Some(Cmd::Run { cmd, .. }) => cmd,
            other => panic!("expected Run, got {other:?}"),
        };
        let (argv, flags) = split_adhoc_trailing(&cmd).unwrap();
        assert_eq!(argv, vec!["python", "-c", "x"]);
        assert_eq!(flags.app.as_deref(), Some("demo"));
    }

    /// The ad-hoc `--` form captures everything after `--` as the command
    /// region (including hyphen-leading tokens); scaffold flags riding after
    /// the command are split out at runtime.
    #[test]
    fn run_adhoc_trailing_and_cmd_str_parse() {
        let cli = parse(&["run", "--", "python", "-m", "http.server", "8000"]);
        match cli.cmd {
            Some(Cmd::Run {
                cmd,
                cmd_str,
                scaffold_name,
                ..
            }) => {
                assert_eq!(cmd, vec!["python", "-m", "http.server", "8000"]);
                assert!(cmd_str.is_none() && scaffold_name.is_none());
            }
            other => panic!("expected Run, got {other:?}"),
        }
        // Scaffold flags after the command line are split out.
        let cli = parse(&[
            "run",
            "--",
            "node",
            "server.js",
            "--app",
            "worker",
            "--restart",
            "unless-stopped",
        ]);
        let cmd = match cli.cmd {
            Some(Cmd::Run { cmd, .. }) => cmd,
            other => panic!("expected Run, got {other:?}"),
        };
        let (argv, flags) = split_adhoc_trailing(&cmd).unwrap();
        assert_eq!(argv, vec!["node", "server.js"]);
        assert_eq!(flags.app.as_deref(), Some("worker"));
        assert_eq!(flags.restart.as_deref(), Some("unless-stopped"));
        // `--flag=value` form too.
        let (argv, flags) =
            split_adhoc_trailing(&["/bin/echo".into(), "hi".into(), "--app=w2".into()]).unwrap();
        assert_eq!(argv, vec!["/bin/echo", "hi"]);
        assert_eq!(flags.app.as_deref(), Some("w2"));
        // A missing value is a diagnostic error.
        assert!(split_adhoc_trailing(&["x".into(), "--app".into()]).is_err());
        // `--cmd "<string>"` is the equivalent single-string entry.
        let cli = parse(&["run", "--cmd", "python -m http.server 8000", "--app", "web"]);
        match cli.cmd {
            Some(Cmd::Run {
                cmd,
                cmd_str,
                scaffold_name,
                ..
            }) => {
                assert!(cmd.is_empty());
                assert_eq!(cmd_str.as_deref(), Some("python -m http.server 8000"));
                assert_eq!(scaffold_name.as_deref(), Some("web"));
            }
            other => panic!("expected Run, got {other:?}"),
        }
        // `--name` is no longer an accepted ad-hoc naming flag.
        assert!(parse_err(&["run", "--cmd", "x", "--name", "web"]).contains("unexpected argument"));
        // The two entry forms are mutually exclusive.
        assert!(parse_err(&["run", "--cmd", "x", "--", "y"]).contains("cannot be used with"));
    }

    /// An invalid `--restart` literal is rejected — at clap parse time for the
    /// flags before `--`, and at runtime for the trailing form.
    #[test]
    fn run_adhoc_restart_literal_validated() {
        for ok in ["no", "on-failure", "always", "unless-stopped"] {
            parse(&["run", "--restart", ok, "--", "x"]);
        }
        assert!(parse_err(&["run", "--restart", "never", "--", "x"]).contains("invalid value"));
        // The trailing form validates at runtime via parse_restart_literal.
        let (_, flags) =
            split_adhoc_trailing(&["x".into(), "--restart".into(), "never".into()]).unwrap();
        assert!(config::parse_restart_cli(flags.restart.as_deref().unwrap()).is_err());
    }

    /// `reload` is gone from the top level (it moved into `config --reload`).
    #[test]
    fn reload_subcommand_is_gone() {
        let e = parse_err(&["reload"]);
        assert!(
            e.contains("unrecognized subcommand") || e.contains("invalid subcommand"),
            "{e}"
        );
    }

    /// `log` takes one or more program names; `--stream` accepts both.
    #[test]
    fn log_multi_name_and_stream_both_parse() {
        let cli = parse(&["log", "web"]);
        match cli.cmd {
            Some(Cmd::Log {
                names,
                tail,
                follow,
                stream,
            }) => {
                assert_eq!(names, vec!["web"]);
                assert_eq!(tail, 20);
                assert!(!follow);
                assert_eq!(stream, "out");
            }
            other => panic!("expected Log, got {other:?}"),
        }
        let cli = parse(&[
            "log", "web", "api", "-f", "--tail", "50", "--stream", "both",
        ]);
        match cli.cmd {
            Some(Cmd::Log {
                names,
                tail,
                follow,
                stream,
            }) => {
                assert_eq!(names, vec!["web", "api"]);
                assert_eq!(tail, 50);
                assert!(follow);
                assert_eq!(stream, "both");
            }
            other => panic!("expected Log, got {other:?}"),
        }
        // At least one name is required.
        assert!(parse_err(&["log"]).contains("required"));
        // An out-of-range stream value is rejected.
        assert!(parse_err(&["log", "web", "--stream", "side"]).contains("invalid value"));
    }

    /// `ps` defaults to the table and `--out` selects json/table; any other
    /// value is a clap error.
    #[test]
    fn ps_out_flag_parses() {
        assert!(matches!(
            parse(&["ps"]).cmd,
            Some(Cmd::Ps {
                all: false,
                ref out
            }) if out == "table"
        ));
        assert!(matches!(
            parse(&["ps", "--out", "json"]).cmd,
            Some(Cmd::Ps { ref out, .. }) if out == "json"
        ));
        assert!(matches!(
            parse(&["ps", "--out", "table"]).cmd,
            Some(Cmd::Ps { ref out, .. }) if out == "table"
        ));
        assert!(matches!(
            parse(&["ps", "--all"]).cmd,
            Some(Cmd::Ps { all: true, .. })
        ));
        assert!(parse_err(&["ps", "--out", "yaml"]).contains("invalid value"));
    }

    /// The five removed verbs are gone: clap answers with an unknown
    /// subcommand (no alias, no migration interception — design D7).
    #[test]
    fn removed_verbs_are_gone() {
        for verb in ["status", "list", "shutdown", "add", "service"] {
            let e = parse_err(&[verb]);
            assert!(
                e.contains("unrecognized subcommand") || e.contains("invalid subcommand"),
                "{verb}: {e}"
            );
        }
        // `compose` parses with the former `add` flag set.
        let cli = parse(&["compose", ".", "--restart", "on-failure", "--priority", "2"]);
        match cli.cmd {
            Some(Cmd::Compose {
                restart, priority, ..
            }) => {
                assert_eq!(restart.as_deref(), Some("on-failure"));
                assert_eq!(priority, Some(2));
            }
            other => panic!("expected Compose, got {other:?}"),
        }
    }

    /// The bare-`daemon` usage error names all four subcommands (exit 2 at
    /// dispatch time).
    #[test]
    fn daemon_usage_error_lists_four_subcommands() {
        let msg = daemon_usage_error(false);
        for sub in ["start", "install", "uninstall", "shutdown"] {
            assert!(msg.contains(sub), "missing {sub}: {msg}");
        }
        assert!(
            !msg.contains("--force --now"),
            "plain usage error carries no unit hint: {msg}"
        );
        // 3.2 upgrade compatibility: the old `daemon --config <path>`
        // ExecStart gets the regeneration hint.
        let old = daemon_usage_error(true);
        for sub in ["start", "install", "uninstall", "shutdown"] {
            assert!(old.contains(sub), "missing {sub}: {old}");
        }
        assert!(
            old.contains("ak daemon install --force --now"),
            "old ExecStart hint must name the regeneration command: {old}"
        );
    }

    /// The old `daemon --config <path>` signature (systemd ExecStart and the
    /// Windows `--service` binPath) is recognized so the usage error can add
    /// the unit-regeneration hint; client subcommands and bare `daemon` never
    /// match.
    #[test]
    fn old_daemon_signature_detection() {
        let s = |xs: &[&str]| -> bool {
            bare_daemon_old_unit_form(&xs.iter().map(|x| x.to_string()).collect::<Vec<_>>())
        };
        // The two legacy entry forms (either flag position, `--config=` too).
        assert!(s(&["daemon", "--config", "/etc/appkeeper/daemon.toml"]));
        assert!(s(&["daemon", "-c", "/etc/appkeeper/daemon.toml"]));
        assert!(s(&["daemon", "--config=/etc/appkeeper/daemon.toml"]));
        assert!(s(&["-c", "/etc/x.toml", "daemon"]));
        assert!(s(&[
            "daemon",
            "--config",
            "/etc/appkeeper/daemon.toml",
            "--service",
            "ak"
        ]));
        // Bare `daemon` (the real usage error) and every subcommand form are
        // not the old signature.
        assert!(!s(&["daemon"]));
        for sub in ["start", "install", "uninstall", "shutdown"] {
            assert!(!s(&["daemon", sub]), "daemon {sub} is not the old form");
        }
        assert!(!s(&["daemon", "start", "--foreground"]));
        assert!(!s(&["ps"]));
        assert!(!s(&[]));
    }

    /// 2.3: the removed daemon `run` hints per form — `--detach` points at
    /// the background entry, the config-only form also at the foreground one
    /// and at the unit regeneration (control-plane spec).
    #[test]
    fn removed_run_hints_point_at_the_daemon_subcommands() {
        let detach = removed_run_hint(true);
        assert!(detach.contains("ak daemon start"), "{detach}");
        assert!(
            !detach.contains("--foreground"),
            "the detach form keeps its hint short: {detach}"
        );
        let config_only = removed_run_hint(false);
        assert!(config_only.contains("ak daemon start"), "{config_only}");
        assert!(
            config_only.contains("ak daemon start --foreground"),
            "{config_only}"
        );
        assert!(
            config_only.contains("ak daemon install --force --now"),
            "the old ExecStart form names the regeneration command: {config_only}"
        );
        // The five removed verbs the new error must NOT mention (design D7).
        for stale in ["ak service install", "ak daemon --detach"] {
            assert!(!config_only.contains(stale), "{stale}: {config_only}");
            assert!(!detach.contains(stale), "{stale}: {detach}");
        }
    }

    /// `compose` no longer carries the deprecated flags; `--restart` is the
    /// single policy flag.
    #[test]
    fn compose_flags_converged_to_restart() {
        let cli = parse(&["compose", ".", "--restart", "on-failure", "--priority", "2"]);
        match cli.cmd {
            Some(Cmd::Compose {
                restart, priority, ..
            }) => {
                assert_eq!(restart.as_deref(), Some("on-failure"));
                assert_eq!(priority, Some(2));
            }
            other => panic!("expected Compose, got {other:?}"),
        }
        for bad in [
            vec!["compose", ".", "--autostart"],
            vec!["compose", ".", "--no-autostart"],
            vec!["compose", ".", "--autorestart", "always"],
        ] {
            assert!(
                parse_err(&bad).contains("unexpected argument"),
                "{bad:?} must be rejected"
            );
        }
        assert!(parse_err(&["compose", ".", "--restart", "never"]).contains("invalid value"));
    }

    /// `start` is a first-class verb again (parallel to stop/restart). The
    /// three take `<app> [<program>]`; the old `--app` flag is rejected.
    #[test]
    fn start_stop_restart_take_app_and_program() {
        match parse(&["start", "demo"]).cmd {
            Some(Cmd::Start { app, program, json }) => {
                assert_eq!(app, "demo");
                assert!(program.is_none() && !json);
            }
            other => panic!("expected Start, got {other:?}"),
        }
        match parse(&["start", "demo", "web", "--json"]).cmd {
            Some(Cmd::Start { app, program, json }) => {
                assert_eq!(app, "demo");
                assert_eq!(program.as_deref(), Some("web"));
                assert!(json);
            }
            other => panic!("expected Start, got {other:?}"),
        }
        match parse(&["stop", "demo", "web"]).cmd {
            Some(Cmd::Stop { app, program }) => {
                assert_eq!((app.as_str(), program.as_deref()), ("demo", Some("web")));
            }
            other => panic!("expected Stop, got {other:?}"),
        }
        match parse(&["restart", "demo"]).cmd {
            Some(Cmd::Restart { app, program }) => {
                assert_eq!(app, "demo");
                assert!(program.is_none());
            }
            other => panic!("expected Restart, got {other:?}"),
        }
        // `--app` is gone from all three (control-plane spec).
        for verb in ["start", "stop", "restart"] {
            let e = parse_err(&[verb, "--app", "demo"]);
            assert!(e.contains("unexpected argument"), "{verb}: {e}");
        }
        // A missing app name is a clap usage error.
        for verb in ["start", "stop", "restart"] {
            assert!(parse_err(&[verb]).contains("required"), "{verb}");
        }
    }

    /// Multi-program log lines carry the `[name] ` prefix; a single program
    /// stays byte-identical (no prefix) — the legacy output contract.
    #[test]
    fn log_line_prefix_only_for_multiple_programs() {
        assert_eq!(format_log_line("web", "hello", false), "hello");
        assert_eq!(format_log_line("web", "hello", true), "[web] hello");
        // Empty lines keep the prefix too (a blank line still belongs to a stream).
        assert_eq!(format_log_line("api", "", true), "[api] ");
    }

    // -- run daemon-first (run-daemon-first section 6) ----------------------

    /// 6.2: the ad-hoc CLI no longer writes app_dir itself — the code path
    /// goes through the daemon endpoint. (The unit-visible proof is that the
    /// old local registration call is gone from `run_adhoc`; this asserts the
    /// source has no `registry::add` in the run path.)
    #[test]
    fn run_adhoc_no_longer_calls_local_registry_add() {
        let src = include_str!("main.rs");
        let body = src
            .split("fn run_adhoc(")
            .nth(1)
            .expect("run_adhoc exists")
            .split("\n/// Print one human-readable progress")
            .next()
            .unwrap();
        assert!(
            !body.contains("registry::add("),
            "run_adhoc must not write the registry locally"
        );
        assert!(
            !body.contains("apply_after_add("),
            "run_adhoc must not pull a daemon up"
        );
        assert!(
            body.contains("adhoc_run("),
            "run_adhoc goes through the daemon endpoint"
        );
        assert!(
            body.contains("EXIT_UNREACHABLE"),
            "run_adhoc exits 3 when the daemon is offline"
        );
        assert!(
            !body.contains("ensure_daemon"),
            "run_adhoc MUST NOT pull a daemon up (background-daemon spec)"
        );
    }

    /// 7.1: `compose --apply` keeps the implicit pull-up (`apply_after_add`),
    /// which is the ONLY remaining caller of `daemonize::ensure_daemon`
    /// outside the explicit `daemon start` entry.
    #[test]
    fn compose_apply_keeps_implicit_pull_up() {
        let src = include_str!("main.rs");
        // The compose branch still routes through apply_after_add.
        let add_branch = src
            .split("if *apply_now {")
            .nth(1)
            .expect("compose --apply branch exists")
            .split("\n        }\n")
            .next()
            .unwrap();
        assert!(
            add_branch.contains("apply_after_add("),
            "compose --apply keeps the implicit pull-up"
        );
        // apply_after_add still calls ensure_daemon (offline pull-up intact).
        let apply_fn = src
            .split("fn apply_after_add(")
            .nth(1)
            .expect("apply_after_add exists")
            .split("\n// -- validate")
            .next()
            .unwrap();
        assert!(
            apply_fn.contains("ensure_daemon("),
            "apply_after_add pulls the daemon up when offline"
        );
    }

    /// 6.3/6.4: the single-program result renders `{name, config_path, pid}`
    /// and the JSON form carries status.
    #[test]
    fn program_result_line_renders_fields() {
        let line = program_result_line(
            "web",
            "/etc/appkeeper/app.d/web.toml",
            &serde_json::json!(1234),
            "started",
        );
        assert!(line.contains("web"), "{line}");
        assert!(line.contains("/etc/appkeeper/app.d/web.toml"), "{line}");
        assert!(line.contains("1234"), "{line}");
        // A null pid renders as `null`, never a panic.
        let line = program_result_line("web", "", &serde_json::Value::Null, "started");
        assert!(line.contains("null"), "{line}");
    }

    /// 6.3/6.4: the fan-out renderer emits one line per program, all-success
    /// exits 0, and the JSON form is an array with the four fields.
    #[test]
    fn fanout_start_render_success_and_json() {
        let v = serde_json::json!({
            "programs": [
                {"name": "web", "config_path": "/d/web.toml", "pid": 11, "result": "program web is starting"},
                {"name": "api", "config_path": "/d/api.toml", "pid": 22, "result": "program api is running"},
            ]
        });
        let (code, out, err) = fanout_start_render(&v, false);
        assert_eq!(code, EXIT_OK);
        assert_eq!(out.len(), 2, "{out:?}");
        assert!(out[0].contains("web") && out[0].contains("/d/web.toml") && out[0].contains("11"));
        assert!(err.is_empty());

        let (code, out, _) = fanout_start_render(&v, true);
        assert_eq!(code, EXIT_OK);
        let arr: serde_json::Value = serde_json::from_str(&out[0]).unwrap();
        assert_eq!(arr.as_array().unwrap().len(), 2);
        for p in arr.as_array().unwrap() {
            for f in ["name", "config_path", "pid", "status"] {
                assert!(p.get(f).is_some(), "field {f}: {p}");
            }
        }
        assert_eq!(arr[0]["status"], "started");
    }

    /// 6.3: a per-program failure exits non-zero and lists the failures.
    #[test]
    fn fanout_start_render_failure_is_nonzero() {
        let v = serde_json::json!({
            "programs": [
                {"name": "web", "config_path": "/d/web.toml", "pid": 11, "result": "program web is running"},
                {"name": "api", "config_path": null, "pid": null, "result": "error: program api is already fatal"},
            ]
        });
        let (code, out, err) = fanout_start_render(&v, false);
        assert_eq!(code, EXIT_ERROR);
        assert_eq!(out.len(), 2);
        assert!(err.iter().any(|l| l.contains("api")), "{err:?}");
        assert!(out[1].contains("failed"), "{out:?}");
    }

    /// 6.6: progress frames render human-readable lines; the terminal frame
    /// has no phase and yields nothing.
    #[test]
    fn progress_line_covers_phases() {
        assert!(
            progress_line(&serde_json::json!({"phase": "staged", "estimate_secs": 3}))
                .unwrap()
                .contains('3')
        );
        assert!(
            progress_line(&serde_json::json!({"phase": "waiting_startsecs"}))
                .unwrap()
                .contains("startsecs")
        );
        let health =
            progress_line(&serde_json::json!({"phase": "waiting_health", "estimate_secs": 35}))
                .unwrap();
        assert!(health.contains("35"), "{health}");
        assert!(progress_line(&serde_json::json!({"ok": true, "pid": 1})).is_none());
    }

    /// 6.7: the residual decision defaults to refusal when non-interactive,
    /// commits only on an explicit yes, and refuses on anything else.
    #[test]
    fn residual_decision_branches() {
        assert_eq!(
            residual_decision(false, Some("y")),
            ResidualAction::Refuse("non-interactive session defaults to refusing")
        );
        assert_eq!(residual_decision(true, Some("y")), ResidualAction::Commit);
        assert_eq!(
            residual_decision(true, Some("yes\n")),
            ResidualAction::Commit
        );
        assert_eq!(
            residual_decision(true, Some("n")),
            ResidualAction::Refuse("declined by the user")
        );
        assert_eq!(
            residual_decision(true, None),
            ResidualAction::Refuse("declined by the user")
        );
    }

    /// 6.8: a partial failure renders the full config info and exits non-zero.
    #[test]
    fn run_terminal_partial_failure_prints_config() {
        let frame = serde_json::json!({
            "ok": false,
            "partial": true,
            "program": "web",
            "config_path": "/d/web.toml",
            "pid": 9,
            "error": "process started but the registration could not be written",
            "config_text": "[program.web]\ncommand = \"/srv/web\"\n",
        });
        let (code, out, err) = run_terminal_render(&frame, false);
        assert_eq!(code, EXIT_ERROR);
        assert!(out.is_empty());
        let joined = err.join("\n");
        assert!(joined.contains("web"), "{joined}");
        assert!(joined.contains("/d/web.toml"), "{joined}");
        assert!(joined.contains("[program.web]"), "{joined}");
        assert!(joined.contains("command"), "{joined}");
    }

    /// 6.4: a successful terminal frame prints program/config/pid and exits 0;
    /// the JSON form is the frame itself.
    #[test]
    fn run_terminal_success_and_json() {
        let frame = serde_json::json!({
            "ok": true, "program": "web", "config_path": "/d/web.toml", "pid": 42,
        });
        let (code, out, err) = run_terminal_render(&frame, false);
        assert_eq!(code, EXIT_OK);
        assert!(err.is_empty());
        let joined = out.join("\n");
        assert!(joined.contains("web") && joined.contains("42"), "{out:?}");
        let (code, out, _) = run_terminal_render(&frame, true);
        assert_eq!(code, EXIT_OK);
        let v: serde_json::Value = serde_json::from_str(&out[0]).unwrap();
        assert_eq!(v["pid"], 42);
        assert_eq!(v["name"], "web");
        assert_eq!(v["config_path"], "/d/web.toml");
        assert_eq!(v["status"], "started");
        // A failed JSON frame exits non-zero and carries the reason.
        let (code, out, _) = run_terminal_render(
            &serde_json::json!({"ok": false, "phase": "startup", "error": "exited"}),
            true,
        );
        assert_eq!(code, EXIT_ERROR);
        let v: serde_json::Value = serde_json::from_str(&out[0]).unwrap();
        assert_eq!(v["status"], "failed");
        assert_eq!(v["error"], "exited");
    }

    // -- ps (control-plane spec, tasks 4.x) ---------------------------------

    /// 4.3: only the "no daemon there" case degrades `ps --all` to the
    /// registry view. The other two exit-3 classes (permission-denied
    /// endpoint, rejected TCP credential) prove a daemon IS serving, so the
    /// control-plane spec forbids reporting them as the offline degradation.
    #[test]
    fn ps_degradation_is_limited_to_missing_daemon() {
        // The client's marker literals (client.rs keeps them private, and the
        // contract is what the operator actually reads).
        let unreachable =
            anyhow::anyhow!("daemon unreachable (is `ak daemon` active? /tmp/x.sock): not found");
        assert!(ps_degrades_to_registry(true, &unreachable));
        assert!(
            !ps_degrades_to_registry(false, &unreachable),
            "plain `ps` keeps the exit-3 contract"
        );
        assert_eq!(client::exit_code_of(&unreachable), EXIT_UNREACHABLE);

        // Exit 3 but a live daemon: never the offline listing.
        for live in [
            "control endpoint access denied: endpoint /run/appkeeper/0/ak.sock is owned by uid 0",
            "control endpoint rejected the credential (api error 401): a tcp:// control \
             endpoint requires the daemon's `[daemon] auth_token`",
        ] {
            let e = anyhow::anyhow!("{live}");
            assert_eq!(client::exit_code_of(&e), EXIT_UNREACHABLE, "{live}");
            assert!(
                !ps_degrades_to_registry(true, &e),
                "must not degrade: {live}"
            );
        }

        // A caller-fixable control-plane answer is not an offline listing.
        let other = anyhow::anyhow!("api error 500: internal failure");
        assert!(
            !ps_degrades_to_registry(true, &other),
            "a control-plane answer must not become a registry listing"
        );
    }

    /// 4.3: the online `--all` JSON join takes the projection as
    /// authoritative and lists only the apps it does not cover.
    #[test]
    fn projection_apps_collects_distinct_app_names() {
        let v = serde_json::json!({
            "programs": [
                {"app": "demo", "name": "web"},
                {"app": "demo", "name": "worker"},
                {"app": "gateway", "name": "api"},
                {"name": "no-app-field"},
            ]
        });
        let apps = projection_apps(&v);
        assert_eq!(
            apps.iter().cloned().collect::<Vec<_>>(),
            vec!["demo".to_string(), "gateway".to_string()]
        );
        assert!(projection_apps(&serde_json::json!({})).is_empty());
        assert!(projection_apps(&serde_json::json!({"programs": []})).is_empty());
    }
}
