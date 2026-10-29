//! Interactive shell client (`appkeeper shell`) — a pure client over the
//! control-plane API, mirroring the supervisorctl workflow. The daemon is
//! untouched: every command reuses `client::Client` and the shared exit-code
//! contract. The `open` verb probes the web console (`GET /api/health`) and
//! launches the system browser; enabling the console itself is a daemon
//! config concern (`[webui]` section + `ak config --reload`).

use std::io::Write;

use anyhow::{Context, Result, bail};

use crate::client::{self, Client};
use crate::config::DEFAULT_WEBUI_LISTEN;

/// Default webui listen address, matching the built-in default of the
/// daemon config's `[webui]` section (`webui.listen`, 127.0.0.1:9877).
const DEFAULT_WEBUI_URL: &str = "http://127.0.0.1:9877";

/// What a start/stop/restart targets: a whole app (bare `<app>`, fan-out
/// expanded server-side) or one program of that app (the second positional).
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Target {
    /// The whole app.
    App(String),
    /// One program inside an app (validated against its owning app).
    Program { app: String, program: String },
}

/// One parsed shell line.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum ShellCmd {
    Ps,
    Start(Target),
    Stop(Target),
    Restart(Target),
    Action {
        program: String,
        name: String,
    },
    Signal {
        program: String,
        signal: String,
    },
    Pid(String),
    Log {
        name: String,
        follow: bool,
        tail: usize,
        stream: String,
    },
    Pending,
    Apply {
        app: Option<String>,
        program: Option<String>,
        restart: bool,
    },
    Shutdown,
    Open,
    Help,
    Exit,
}

/// Parse one shell line. `None` = blank line (ignore in the REPL).
pub(crate) fn parse_line(line: &str) -> Result<Option<ShellCmd>> {
    let words = shell_words(line);
    let Some((head, args)) = words.split_first() else {
        return Ok(None);
    };
    let cmd = match head.as_str() {
        "ps" => ShellCmd::Ps,
        "start" => parse_target("start", args).map(ShellCmd::Start)?,
        "stop" => parse_target("stop", args).map(ShellCmd::Stop)?,
        "restart" => parse_target("restart", args).map(ShellCmd::Restart)?,
        "action" => parse_two_args("action", args, "action <program> <action>")
            .map(|(program, name)| ShellCmd::Action { program, name })?,
        "signal" => parse_two_args("signal", args, "signal <program> <SIGNAL>")
            .map(|(program, signal)| ShellCmd::Signal { program, signal })?,
        "pid" => ShellCmd::Pid(one_arg(head, args)?),
        "log" => parse_log(args)?,
        "pending" => ShellCmd::Pending,
        "apply" => parse_apply(args)?,
        "shutdown" => ShellCmd::Shutdown,
        "open" => ShellCmd::Open,
        "help" | "?" => ShellCmd::Help,
        "exit" | "quit" | "q" => ShellCmd::Exit,
        other => bail!("unknown command {other:?}{}", similar_hint(other)),
    };
    Ok(Some(cmd))
}

/// Parse `<app> [<program>]` for start/stop/restart (the same range syntax as
/// the single-shot CLI verbs).
fn parse_target(cmd: &str, args: &[String]) -> Result<Target> {
    let mut positional: Vec<String> = Vec::new();
    for a in args {
        if a.starts_with('-') {
            bail!("unknown {cmd} flag {a:?}");
        }
        positional.push(a.clone());
    }
    match positional.as_slice() {
        [app] => Ok(Target::App(app.clone())),
        [app, program] => Ok(Target::Program {
            app: app.clone(),
            program: program.clone(),
        }),
        [] => bail!("{cmd} needs an app name: {cmd} <app> [<program>]"),
        _ => bail!("{cmd} takes at most two positional args: <app> [<program>]"),
    }
}

/// Parse exactly two positional arguments (`action`/`signal` verbs).
fn parse_two_args(cmd: &str, args: &[String], usage: &str) -> Result<(String, String)> {
    match args {
        [a, b] => Ok((a.clone(), b.clone())),
        _ => bail!("{cmd} takes exactly two arguments: {usage}"),
    }
}

/// Parse `apply [<app> [<program>]] [--restart]`.
fn parse_apply(args: &[String]) -> Result<ShellCmd> {
    let mut app = None;
    let mut program = None;
    let mut restart = false;
    let mut positional = 0;
    for a in args {
        if a == "--restart" {
            restart = true;
        } else if a.starts_with("--") {
            bail!("unknown flag {a:?} (only --restart is supported)");
        } else if positional == 0 {
            app = Some(a.clone());
            positional = 1;
        } else if positional == 1 {
            program = Some(a.clone());
            positional = 2;
        } else {
            bail!("apply takes at most two positional args: <app> <program>");
        }
    }
    if app.is_none() && program.is_some() {
        bail!("apply <program> needs an app first: apply <app> <program>");
    }
    Ok(ShellCmd::Apply {
        app,
        program,
        restart,
    })
}

/// Split on whitespace, honoring double/single quotes (enough for
/// `shell -e "log web --tail 5"`).
fn shell_words(line: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let mut started = false;
    for ch in line.chars() {
        match (quote, ch) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), c) => cur.push(c),
            (None, c) if c == '\'' || c == '"' => {
                quote = Some(c);
                started = true;
            }
            (None, c) if c.is_whitespace() => {
                if started {
                    words.push(std::mem::take(&mut cur));
                    started = false;
                }
            }
            (None, c) => {
                cur.push(c);
                started = true;
            }
        }
    }
    if started || !cur.is_empty() {
        words.push(cur);
    }
    words
}

fn one_arg(cmd: &str, args: &[String]) -> Result<String> {
    match args {
        [name] => Ok(name.clone()),
        [] => bail!("{cmd} needs a program name"),
        _ => bail!("{cmd} takes exactly one program name"),
    }
}

fn parse_log(args: &[String]) -> Result<ShellCmd> {
    let mut name: Option<String> = None;
    let mut follow = false;
    let mut tail: Option<usize> = None;
    let mut stream = "out".to_string();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-f" | "--follow" => follow = true,
            "--tail" => {
                i += 1;
                tail = Some(
                    args.get(i)
                        .and_then(|v| v.parse().ok())
                        .ok_or_else(|| anyhow::anyhow!("--tail needs a number"))?,
                );
            }
            "--stream" => {
                i += 1;
                stream = args
                    .get(i)
                    .cloned()
                    .ok_or_else(|| anyhow::anyhow!("--stream needs out|err"))?;
                if stream != "out" && stream != "err" {
                    bail!("--stream must be out|err");
                }
            }
            other if other.starts_with('-') => bail!("unknown log flag {other:?}"),
            other => {
                if name.is_some() {
                    bail!("log takes exactly one program name");
                }
                name = Some(other.to_string());
            }
        }
        i += 1;
    }
    Ok(ShellCmd::Log {
        name: name.ok_or_else(|| anyhow::anyhow!("log needs a program name"))?,
        follow,
        tail: tail.unwrap_or(20),
        stream,
    })
}

/// Naive "did you mean" for the unknown-command hint. The word list mirrors
/// the CLI vocabulary (`start|stop|restart|ps`, no `reload`, no `status`).
fn similar_hint(input: &str) -> String {
    const COMMANDS: [&str; 13] = [
        "ps", "start", "stop", "restart", "action", "signal", "pid", "log", "pending", "apply",
        "shutdown", "open", "help",
    ];
    let input = input.to_lowercase();
    let mut words: Vec<&'static str> = Vec::new();
    // The retired words keep a route to their successor, including near-miss
    // typos: `status`/`st` → `ps`, `reload` → `pending`/`apply`
    // (shell-client spec).
    if input == "st" || levenshtein(&input, "status") <= 2 {
        words.push("ps");
    }
    if levenshtein(&input, "reload") <= 2 {
        words.extend(["pending", "apply"]);
    }
    if levenshtein(&input, "run") <= 1 {
        words.push("start");
    }
    for c in COMMANDS {
        // cheap similarity: shared prefix or edit distance <= 2 on short words
        if !words.contains(&c)
            && c.starts_with(input.chars().next().unwrap_or_default())
            && levenshtein(&input, c) <= 2
        {
            words.push(c);
        }
    }
    match words.as_slice() {
        [] => String::new(),
        [one] => format!(" (did you mean {one}?)"),
        many => format!(" (did you mean one of: {}?)", many.join(", ")),
    }
}

fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0; b.len() + 1];
    for i in 1..=a.len() {
        cur[0] = i;
        for j in 1..=b.len() {
            cur[j] = (prev[j] + 1)
                .min(cur[j - 1] + 1)
                .min(prev[j - 1] + usize::from(a[i - 1] != b[j - 1]));
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

pub(crate) const HELP_TEXT: &str = "\
Built-in commands:
  ps                                  list all programs (app, state, pid, restarts, pending)
  start|stop|restart <app> [<program>]  control an app, or one program of it
  action <program> <action>           run a custom action (prints its output tail;
                                      -e mode exits with the action's exit code)
  signal <program> <SIGNAL>           send a whitelist signal to the program's
                                      child (TERM INT HUP QUIT USR1 USR2; unix only)
  pid <program>                       print a program's pid
  log <program> [-f] [--tail N] [--stream out|err]
                                      view program logs
  pending                             show pending (detected but not applied) changes
  apply [<app> [<program>]] [--restart]
                                      apply pending changes; the literal `all`
                                      (= no argument) applies everything and is
                                      reserved — no app can be named `all`;
                                      --restart restarts unchanged programs too
                                      (user-stopped ones stay stopped)
  shutdown                            stop all programs and exit the daemon
  open                                open the web console in the system browser
  help (or ?)                         show this help
  exit (or quit)                      leave the shell";

/// Exit-code carrier for `-e` single-command mode: an action's own exit code
/// passes through the shell exactly like `appkeeper action` would. Implements
/// StdError so it can ride in an `anyhow::Error` and be downcast later.
#[derive(Debug)]
struct ActionExit(i32);

impl std::fmt::Display for ActionExit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "action exit code {}", self.0)
    }
}

impl std::error::Error for ActionExit {}

/// Outcome of one executed command: keep looping or leave the shell.
enum Outcome {
    Continue,
    Exit,
}

/// Entry: REPL, or a single command via `-e`.
pub(crate) fn run(config_path: &std::path::Path, single: Option<&str>) -> Result<()> {
    let config = client::load_config(config_path)?;
    let c = Client::from_config(&config);

    if let Some(line) = single {
        let cmd = parse_line(line).context("invalid command")?;
        match cmd {
            None => Ok(()),
            Some(c2) => {
                let webui_url = webui_url_of(&config);
                match execute(&c, &c2, &webui_url) {
                    Ok(Outcome::Continue) => Ok(()),
                    Ok(Outcome::Exit) => Ok(()),
                    Err(e) => {
                        // The message is the only trace of what the control
                        // plane answered — never exit silently.
                        eprintln!("ak: error: {e:#}");
                        let code = e
                            .downcast_ref::<ActionExit>()
                            .map(|a| a.0)
                            .unwrap_or_else(|| client::exit_code_of(&e));
                        std::process::exit(code);
                    }
                }
            }
        }
    } else {
        repl(&c, &webui_url_of(&config))
    }
}

/// Console base URL for the `open` verb: the configured `[webui]` listen
/// address when the section is present, else the built-in default.
fn webui_url_of(config: &crate::config::DaemonConfig) -> String {
    match &config.webui {
        Some(w) => format!("http://{}", w.listen),
        None => DEFAULT_WEBUI_URL.to_string(),
    }
}

/// Read-eval-print loop. rustyline provides line editing/history on a TTY and
/// degrades to plain line reads when stdin is not one.
fn repl(c: &Client, webui_url: &str) -> Result<()> {
    let mut rl = rustyline::DefaultEditor::new().context("cannot init line editor")?;
    let interactive = std::io::IsTerminal::is_terminal(&std::io::stdin());
    if interactive {
        println!("appkeeper shell — type `help` for commands, `exit` to leave.");
    }
    loop {
        let readline = rl.readline("appkeeper> ");
        let line = match readline {
            Ok(line) => line,
            Err(rustyline::error::ReadlineError::Interrupted) => continue,
            Err(rustyline::error::ReadlineError::Eof) => break,
            Err(e) => return Err(e.into()),
        };
        match parse_line(&line) {
            Ok(None) => continue,
            Ok(Some(cmd)) => {
                if interactive {
                    rl.add_history_entry(line.trim())?;
                }
                match execute(c, &cmd, webui_url) {
                    Ok(Outcome::Continue) => {}
                    Ok(Outcome::Exit) => break,
                    // Per the CLI contract the shell stays alive on command
                    // errors; only the `-e` mode maps them to exit codes.
                    Err(e) => eprintln!("ak: error: {e:#}"),
                }
            }
            Err(e) => eprintln!("ak: {e:#}"),
        }
    }
    Ok(())
}

/// Execute one parsed command against the control-plane client.
fn execute(c: &Client, cmd: &ShellCmd, webui_url: &str) -> Result<Outcome> {
    match cmd {
        ShellCmd::Ps => print_status(c).map(|_| Outcome::Continue),
        ShellCmd::Start(t) => run_target(c, t, "start"),
        ShellCmd::Stop(t) => run_target(c, t, "stop"),
        ShellCmd::Restart(t) => run_target(c, t, "restart"),
        ShellCmd::Action { program, name } => run_custom_action(c, program, name),
        ShellCmd::Signal { program, signal } => {
            let v = c.signal(program, signal)?;
            println!(
                "{}",
                v.get("result")
                    .and_then(|r| r.as_str())
                    .unwrap_or("delivered")
            );
            Ok(Outcome::Continue)
        }
        ShellCmd::Pid(name) => {
            let v = c.program(name)?;
            match v.get("pid") {
                Some(pid) if !pid.is_null() => println!("{pid}"),
                _ => println!("not running"),
            }
            Ok(Outcome::Continue)
        }
        ShellCmd::Log {
            name,
            follow,
            tail,
            stream,
        } => {
            if *follow {
                c.log_follow(name, stream)?;
            } else {
                for line in c.log_tail(name, stream, *tail)? {
                    println!("{line}");
                }
            }
            Ok(Outcome::Continue)
        }
        ShellCmd::Pending => {
            let v = c.pending()?;
            let programs = v
                .get("programs")
                .and_then(|p| p.as_array())
                .cloned()
                .unwrap_or_default();
            if programs.is_empty() {
                println!("no pending changes");
            } else {
                println!("pending changes ({}):", programs.len());
                for p in programs {
                    let app = p.get("app").and_then(|x| x.as_str()).unwrap_or("?");
                    let prog = p.get("program").and_then(|x| x.as_str()).unwrap_or("?");
                    let state = if p.get("running").and_then(|x| x.as_bool()).unwrap_or(false) {
                        "running"
                    } else {
                        "stopped"
                    };
                    println!("  {app}.{prog} [{state}] config changed");
                }
            }
            for key in ["apps_added", "apps_removed", "daemon_hints", "errors"] {
                if let Some(list) = v.get(key).and_then(|x| x.as_array()) {
                    for item in list {
                        println!("  {}", item.as_str().unwrap_or_default());
                    }
                }
            }
            Ok(Outcome::Continue)
        }
        ShellCmd::Apply {
            app,
            program,
            restart,
        } => {
            let v = c.apply(app.as_deref(), program.as_deref(), *restart)?;
            println!(
                "{}",
                v.get("result")
                    .and_then(|r| r.as_str())
                    .unwrap_or("applied")
            );
            Ok(Outcome::Continue)
        }
        ShellCmd::Shutdown => {
            let v = c.shutdown()?;
            println!(
                "{}",
                v.get("result")
                    .and_then(|r| r.as_str())
                    .unwrap_or("shutting down")
            );
            Ok(Outcome::Continue)
        }
        ShellCmd::Open => open_webui(webui_url).map(|_| Outcome::Continue),
        ShellCmd::Help => {
            println!("{HELP_TEXT}");
            Ok(Outcome::Continue)
        }
        ShellCmd::Exit => Ok(Outcome::Exit),
    }
}

fn print_action(c: &Client, name: &str, action: &str) -> Result<Outcome> {
    let v = c.action(name, action)?;
    println!(
        "{}",
        v.get("result").and_then(|r| r.as_str()).unwrap_or("done")
    );
    Ok(Outcome::Continue)
}

/// start/stop/restart against a whole app (bare `<app>`, server-side fan-out)
/// or one program of it (the second positional, ownership-checked here since
/// the program endpoints are name-addressed).
fn run_target(c: &Client, t: &Target, verb: &str) -> Result<Outcome> {
    match t {
        Target::App(app) => {
            let v = c.app_action(app, verb)?;
            println!(
                "{}",
                v.get("result").and_then(|r| r.as_str()).unwrap_or("done")
            );
            Ok(Outcome::Continue)
        }
        Target::Program { app, program } => {
            let info = c.program(program)?;
            let owner = info.get("app").and_then(|a| a.as_str()).unwrap_or("");
            if owner != app {
                bail!("program {program:?} does not belong to app {app:?} (its app is {owner:?})");
            }
            print_action(c, program, verb)
        }
    }
}

/// `action <program> <action>`: print the output tail, keep the same
/// success/failure semantics as `appkeeper action` (non-zero exit → error;
/// in `-e` mode the action's own exit code leaves the shell via ActionExit).
fn run_custom_action(c: &Client, program: &str, name: &str) -> Result<Outcome> {
    let v = c.custom_action(program, name)?;
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
        return Err(anyhow::Error::new(ActionExit(1)).context(format!(
            "action {program}.{name} timed out (process tree killed)"
        )));
    }
    match v.get("exit_code").and_then(|e| e.as_i64()) {
        Some(0) | None => Ok(Outcome::Continue),
        Some(code) => Err(anyhow::Error::new(ActionExit(code.clamp(1, 255) as i32))
            .context(format!("action {program}.{name} exited with code {code}"))),
    }
}

/// Aligned status table: name / app / state / pid / restarts / unhealthy /
/// pending. Shared by the shell's `ps` built-in (shell-client spec) and by
/// `ak ps`, which adds the registry columns (see [`render_ps_table`]).
pub(crate) fn print_status(c: &Client) -> Result<()> {
    let v = c.status()?;
    print!("{}", render_status_table(&v));
    let _ = std::io::stdout().flush();
    Ok(())
}

/// One registered app as the CLI sees it locally (`registry::list`): the
/// name, the config object the registration record points at, and the
/// description. Used by `ps --all` (join with the projection / offline
/// degradation listing).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RegistryRow {
    pub name: String,
    /// The config object the registration record points at (link target).
    pub path: String,
    pub description: Option<String>,
}

/// One rendered status row. The projection feeds the first six fields; the
/// pending marker comes from `StatusDoc.pending`, the last two from the local
/// registry (`ak ps` only).
#[derive(Debug, Clone, PartialEq, Eq)]
struct StatusRow {
    name: String,
    app: String,
    state: String,
    pid: String,
    restarts: String,
    unhealthy: String,
    pending: String,
    config: String,
    description: String,
}

const PS_HEADER: [&str; 9] = [
    "NAME",
    "APP",
    "STATE",
    "PID",
    "RESTARTS",
    "UNHEALTHY",
    "PENDING",
    "CONFIG",
    "DESCRIPTION",
];

/// The shell table columns (the shared prefix of [`PS_HEADER`]).
const STATUS_WIDTH: usize = 7;

/// Pending-change view lifted from the `StatusDoc.pending` projection: the
/// per-program markers plus the tail summary lines.
#[derive(Debug, Default, Clone)]
struct PendingView {
    /// `(app, program)` pairs with an unapplied change.
    programs: std::collections::BTreeSet<(String, String)>,
    apps_added: std::collections::BTreeSet<String>,
    summary: Vec<String>,
}

impl PendingView {
    fn from_status(v: &serde_json::Value) -> Self {
        let Some(p) = v.get("pending") else {
            return Self::default();
        };
        let programs: std::collections::BTreeSet<(String, String)> = p
            .get("programs")
            .and_then(|x| x.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|e| {
                        let app = e.get("app").and_then(|x| x.as_str())?;
                        let program = e.get("program").and_then(|x| x.as_str())?;
                        Some((app.to_string(), program.to_string()))
                    })
                    .collect()
            })
            .unwrap_or_default();
        let apps_added: std::collections::BTreeSet<String> = p
            .get("apps_added")
            .and_then(|x| x.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|e| e.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        let mut summary = Vec::new();
        for a in &apps_added {
            summary.push(format!("pending: app[{a}] registered (new)"));
        }
        for key in ["apps_removed", "daemon_hints", "errors"] {
            if let Some(list) = p.get(key).and_then(|x| x.as_array()) {
                for item in list {
                    if let Some(text) = item.as_str() {
                        summary.push(format!("pending: {text}"));
                    }
                }
            }
        }
        if !programs.is_empty() || !summary.is_empty() {
            summary.push("pending: run `ak apply` to apply".to_string());
        }
        Self {
            programs,
            apps_added,
            summary,
        }
    }

    /// The PENDING cell of one program row: a short marker (`changed`) when
    /// the disk definition differs, empty otherwise.
    fn marker(&self, app: &str, program: &str) -> String {
        if self
            .programs
            .contains(&(app.to_string(), program.to_string()))
        {
            "changed".to_string()
        } else {
            String::new()
        }
    }
}

/// Projection rows expanded to the full `ps` column set (no PENDING yet).
fn program_rows(
    v: &serde_json::Value,
    descriptions: &std::collections::BTreeMap<String, String>,
) -> Vec<StatusRow> {
    v.get("programs")
        .and_then(|p| p.as_array())
        .map(|arr| {
            arr.iter()
                .map(|p| {
                    let app = str_field(p, "app");
                    StatusRow {
                        name: str_field(p, "name"),
                        app: app.clone(),
                        state: str_field(p, "state"),
                        pid: match p.get("pid") {
                            Some(x) if !x.is_null() => x.to_string(),
                            _ => "-".into(),
                        },
                        restarts: p
                            .get("total_exits")
                            .and_then(|x| x.as_u64())
                            .map(|x| x.to_string())
                            .unwrap_or_else(|| "0".into()),
                        unhealthy: if p
                            .get("unhealthy")
                            .and_then(|x| x.as_bool())
                            .unwrap_or(false)
                        {
                            "yes".into()
                        } else {
                            "-".into()
                        },
                        pending: String::new(),
                        config: p
                            .get("config_path")
                            .and_then(|x| x.as_str())
                            .map(String::from)
                            .unwrap_or_else(|| "-".into()),
                        description: descriptions
                            .get(&app)
                            .filter(|d| !d.is_empty())
                            .cloned()
                            .unwrap_or_else(|| "-".into()),
                    }
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Fill each row's PENDING marker from the projection's pending set.
fn mark_pending(rows: &mut [StatusRow], pending: &PendingView) {
    for row in rows.iter_mut() {
        row.pending = pending.marker(&row.app, &row.name);
    }
}

/// Pure renderer for `ak ps`'s online table: the shared status columns plus
/// the config path (from the projection's per-program `config_path`), the
/// description (supplied by the CLI from the local registry, keyed by app
/// name) and the PENDING marker. An empty program list renders the
/// registration hint.
pub(crate) fn render_ps_table(
    v: &serde_json::Value,
    descriptions: &std::collections::BTreeMap<String, String>,
) -> String {
    let pending = PendingView::from_status(v);
    let mut rows = program_rows(v, descriptions);
    mark_pending(&mut rows, &pending);
    if rows.is_empty() {
        return ps_empty_hint();
    }
    sort_rows(&mut rows);
    render_ps_rows(&rows, &pending.summary)
}

/// Pure renderer for `ak ps --all` (online): the projection is authoritative;
/// registry apps it does not cover are appended and labelled not applied. A
/// registry-only app that is also pending-added carries the `new` marker.
pub(crate) fn render_ps_all_table(
    v: &serde_json::Value,
    descriptions: &std::collections::BTreeMap<String, String>,
    registry: &[RegistryRow],
) -> String {
    let pending = PendingView::from_status(v);
    let mut rows = program_rows(v, descriptions);
    mark_pending(&mut rows, &pending);
    let known: std::collections::BTreeSet<String> = rows.iter().map(|r| r.app.clone()).collect();
    for app in registry.iter().filter(|a| !known.contains(&a.name)) {
        rows.push(StatusRow {
            name: app.name.clone(),
            app: app.name.clone(),
            state: "not-applied".to_string(),
            pid: "-".to_string(),
            restarts: "-".to_string(),
            unhealthy: "-".to_string(),
            pending: if pending.apps_added.contains(&app.name) {
                "new".to_string()
            } else {
                String::new()
            },
            config: app.path.clone(),
            description: app.description.clone().unwrap_or_else(|| "-".to_string()),
        });
    }
    if rows.is_empty() {
        return ps_empty_hint();
    }
    sort_rows(&mut rows);
    render_ps_rows(&rows, &pending.summary)
}

/// Pure renderer for the offline `ps --all` degradation: a daemon-offline
/// note followed by the local registry listing (name / config path /
/// description). Exit code is 0 by contract.
pub(crate) fn render_registry_listing(registry: &[RegistryRow], offline: bool) -> String {
    let mut flat: Vec<RegistryRow> = registry.to_vec();
    flat.sort_by(|a, b| a.name.cmp(&b.name));
    let mut out = String::new();
    if offline {
        out.push_str("daemon offline — showing the local registry only\n");
    }
    if flat.is_empty() {
        out.push_str(&ps_empty_hint());
        return out;
    }
    let header = ["NAME", "CONFIG", "DESCRIPTION"];
    let table: Vec<[String; 3]> = flat
        .iter()
        .map(|a| {
            [
                a.name.clone(),
                a.path.clone(),
                a.description.clone().unwrap_or_else(|| "-".to_string()),
            ]
        })
        .collect();
    let mut width = [0usize; 3];
    for (i, h) in header.iter().enumerate() {
        width[i] = h.len();
    }
    for row in &table {
        for (i, cell) in row.iter().enumerate() {
            width[i] = width[i].max(cell.chars().count());
        }
    }
    out.push_str(&emit_cells(
        &header.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
        &width,
    ));
    out.push_str(&separator(&width));
    for row in table {
        out.push_str(&emit_cells(&row, &width));
    }
    out
}

fn ps_empty_hint() -> String {
    "no programs (register an app with `ak compose <dir>`)\n".to_string()
}

fn sort_rows(rows: &mut [StatusRow]) {
    rows.sort_by(|a, b| (&a.app, &a.name).cmp(&(&b.app, &b.name)));
}

/// Render the `ps` column set, then the pending summary lines (if any).
fn render_ps_rows(rows: &[StatusRow], summary: &[String]) -> String {
    render_table(rows, &PS_HEADER, summary, true)
}

/// Render a table over the shared column set. `full` selects the `ps`
/// columns (all nine); otherwise the shell's status columns (the first seven)
/// are used.
fn render_table(rows: &[StatusRow], header: &[&str], summary: &[String], full: bool) -> String {
    let cells: Vec<Vec<String>> = rows
        .iter()
        .map(|r| {
            let all = vec![
                r.name.clone(),
                r.app.clone(),
                r.state.clone(),
                r.pid.clone(),
                r.restarts.clone(),
                r.unhealthy.clone(),
                r.pending.clone(),
                r.config.clone(),
                r.description.clone(),
            ];
            if full {
                all
            } else {
                all[..STATUS_WIDTH].to_vec()
            }
        })
        .collect();
    let mut width = vec![0usize; header.len()];
    for (i, h) in header.iter().enumerate() {
        width[i] = h.len();
    }
    for row in &cells {
        for (i, cell) in row.iter().enumerate() {
            width[i] = width[i].max(cell.chars().count());
        }
    }
    let mut out = String::new();
    out.push_str(&emit_cells(
        &header.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
        &width,
    ));
    out.push_str(&separator(&width));
    for row in &cells {
        out.push_str(&emit_cells(row, &width));
    }
    for line in summary {
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// One table line: left-aligned cells, two-space gutters, no trailing spaces
/// on the last column.
fn emit_cells(cells: &[String], width: &[usize]) -> String {
    let mut line = String::new();
    for (i, cell) in cells.iter().enumerate() {
        if i + 1 == cells.len() {
            line.push_str(cell);
        } else {
            line.push_str(&format!("{cell:<width$}  ", width = width[i]));
        }
    }
    line.push('\n');
    line
}

fn separator(width: &[usize]) -> String {
    let mut s = width
        .iter()
        .map(|w| "-".repeat(*w))
        .collect::<Vec<_>>()
        .join("  ");
    s.push('\n');
    s
}

/// Pure renderer for the status table (the `GET /v1/status` projection is its
/// input), so both callers share one format and it is unit-testable without a
/// live daemon. An empty program list renders the registration hint.
pub(crate) fn render_status_table(v: &serde_json::Value) -> String {
    let pending = PendingView::from_status(v);
    let empty = std::collections::BTreeMap::new();
    let mut rows = program_rows(v, &empty);
    mark_pending(&mut rows, &pending);
    if rows.is_empty() {
        return "no programs (register an app with `ak compose <dir>`)\n".to_string();
    }
    sort_rows(&mut rows);
    let header = [
        "NAME",
        "APP",
        "STATE",
        "PID",
        "RESTARTS",
        "UNHEALTHY",
        "PENDING",
    ];
    render_table(&rows, &header, &pending.summary, false)
}

fn str_field(v: &serde_json::Value, key: &str) -> String {
    v.get(key)
        .and_then(|x| x.as_str())
        .unwrap_or("-")
        .to_string()
}

// -- webui probing + browser launch -----------------------------------------

/// `GET /api/health` on the webui port, short timeout. Public endpoint, no
/// token involved (webui auth is independent of /v1).
pub(crate) fn webui_health(base: &str) -> Result<()> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_connect(Some(std::time::Duration::from_secs(2)))
        .timeout_per_call(Some(std::time::Duration::from_secs(2)))
        .build()
        .into();
    let resp = agent
        .get(&format!("{base}/api/health"))
        .call()
        .map_err(|e| anyhow::anyhow!("webui unreachable: {e}"))?;
    if (200..300).contains(&resp.status().as_u16()) {
        Ok(())
    } else {
        bail!("webui health returned {}", resp.status().as_u16())
    }
}

/// Platform browser launcher. Returns the command name for diagnostics.
fn browser_command(url: &str) -> std::process::Command {
    #[cfg(target_os = "macos")]
    {
        let mut cmd = std::process::Command::new("open");
        cmd.arg(url);
        cmd
    }
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        let mut cmd = std::process::Command::new("cmd");
        cmd.args(["/C", "start"]);
        // raw arg: `start` mangles extra quotes otherwise
        cmd.raw_arg(format!("\"{url}\""));
        cmd
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let mut cmd = std::process::Command::new("xdg-open");
        cmd.arg(url);
        cmd
    }
}

/// Probe webui; when reachable open the browser, otherwise print a hint
/// (shell-client spec: `open` never launches the browser when the console
/// is down).
fn open_webui(url: &str) -> Result<()> {
    if let Err(e) = webui_health(url) {
        eprintln!(
            "webui not reachable at {url} — enable it in the daemon config \
             (`ak config --set webui.listen={DEFAULT_WEBUI_LISTEN}`) and run \
             `ak config --reload`, then retry"
        );
        bail!("{e:#}");
    }
    let mut cmd = browser_command(url);
    match cmd.status() {
        Ok(st) if st.success() => {
            println!("opening {url}");
            Ok(())
        }
        _ => bail!("could not launch the system browser for {url}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(line: &str) -> Option<ShellCmd> {
        parse_line(line).unwrap()
    }

    #[test]
    fn blank_and_comments() {
        assert_eq!(parse(""), None);
        assert_eq!(parse("   "), None);
    }

    #[test]
    fn aliases_and_basic_commands() {
        assert_eq!(parse("?"), Some(ShellCmd::Help));
        assert_eq!(parse("quit"), Some(ShellCmd::Exit));
        assert_eq!(parse("exit"), Some(ShellCmd::Exit));
        assert_eq!(parse("ps"), Some(ShellCmd::Ps));
        assert_eq!(parse("pending"), Some(ShellCmd::Pending));
        assert_eq!(
            parse("apply"),
            Some(ShellCmd::Apply {
                app: None,
                program: None,
                restart: false
            })
        );
        assert_eq!(
            parse("apply myapp --restart"),
            Some(ShellCmd::Apply {
                app: Some("myapp".into()),
                program: None,
                restart: true
            })
        );
        assert_eq!(
            parse("apply myapp web"),
            Some(ShellCmd::Apply {
                app: Some("myapp".into()),
                program: Some("web".into()),
                restart: false
            })
        );
        assert!(parse_line("apply --bogus").is_err());
        assert!(
            parse_line("apply a b c").is_err(),
            "too many positional args"
        );
        assert_eq!(parse("open"), Some(ShellCmd::Open));
        assert_eq!(parse("shutdown"), Some(ShellCmd::Shutdown));
    }

    #[test]
    fn unknown_and_missing_args() {
        assert!(parse_line("sttaus").is_err());
        assert!(parse_line("start").is_err(), "`start` needs an app name");
        assert!(parse_line("log").is_err());
        assert!(parse_line("log web --tail").is_err());
        assert!(parse_line("log web --stream both").is_err());
    }

    /// actions: start/stop/restart take `<app> [<program>]` (the CLI range
    /// syntax); `--app` is gone.
    #[test]
    fn target_parsing_app_and_program() {
        assert_eq!(
            parse("start web"),
            Some(ShellCmd::Start(Target::App("web".into())))
        );
        assert_eq!(
            parse("stop web"),
            Some(ShellCmd::Stop(Target::App("web".into())))
        );
        assert_eq!(
            parse("restart gateway"),
            Some(ShellCmd::Restart(Target::App("gateway".into())))
        );
        assert_eq!(
            parse("start web api"),
            Some(ShellCmd::Start(Target::Program {
                app: "web".into(),
                program: "api".into()
            }))
        );
        assert_eq!(
            parse("stop demo worker"),
            Some(ShellCmd::Stop(Target::Program {
                app: "demo".into(),
                program: "worker".into()
            }))
        );
        // A missing app, too many positionals, or the removed --app flag: errors.
        assert!(parse_line("start").is_err());
        assert!(parse_line("start --app web").is_err());
        assert!(parse_line("start a b c").is_err());
        assert!(parse_line("stop --bogus x").is_err());
    }

    /// The retired words are unknown and get the successor hint (shell-client
    /// spec): `status`/`st` → `ps`, `reload` → `pending`/`apply`, `run` → gone.
    #[test]
    fn retired_words_are_unknown_with_successors() {
        for (word, want) in [
            ("status", "ps"),
            ("st", "ps"),
            ("sttaus", "ps"),
            ("reload", "pending"),
            ("run web", "start"),
        ] {
            let e = parse_line(word).unwrap_err().to_string();
            assert!(e.contains("unknown command"), "{word}: {e}");
            assert!(e.contains(want), "{word} should hint {want}: {e}");
        }
    }

    /// actions: `action`/`signal` verbs parse two positional arguments.
    #[test]
    fn action_and_signal_parsing() {
        assert_eq!(
            parse("action api upgrade"),
            Some(ShellCmd::Action {
                program: "api".into(),
                name: "upgrade".into()
            })
        );
        assert_eq!(
            parse("signal web SIGUSR1"),
            Some(ShellCmd::Signal {
                program: "web".into(),
                signal: "SIGUSR1".into()
            })
        );
        assert!(parse_line("action api").is_err(), "needs two args");
        assert!(parse_line("action api upgrade extra").is_err());
        assert!(parse_line("signal web").is_err());
        // The new words are reachable via the typo hint.
        assert!(similar_hint("actoin").contains("action"));
        assert!(similar_hint("signl").contains("signal"));
    }

    /// actions: `-e` single-command mode passes an action's exit code through
    /// via the ActionExit carrier (checked here without a daemon).
    #[test]
    fn action_exit_code_carrier_downcasts() {
        let e: anyhow::Error =
            anyhow::Error::new(ActionExit(3)).context("action p.a exited with code 3");
        assert_eq!(e.downcast_ref::<ActionExit>().map(|a| a.0), Some(3));
        let e: anyhow::Error = anyhow::anyhow!("daemon unreachable");
        assert_eq!(e.downcast_ref::<ActionExit>().map(|a| a.0), None);
    }

    #[test]
    fn log_flags() {
        assert_eq!(
            parse("log web"),
            Some(ShellCmd::Log {
                name: "web".into(),
                follow: false,
                tail: 20,
                stream: "out".into()
            })
        );
        assert_eq!(
            parse("log web -f --tail 100 --stream err"),
            Some(ShellCmd::Log {
                name: "web".into(),
                follow: true,
                tail: 100,
                stream: "err".into()
            })
        );
        // quoted program name
        assert_eq!(
            parse("log \"my app\" -f"),
            Some(ShellCmd::Log {
                name: "my app".into(),
                follow: true,
                tail: 20,
                stream: "out".into()
            })
        );
    }

    #[test]
    fn similar_hint_suggests() {
        assert!(similar_hint("pss").contains("ps"));
        assert!(similar_hint("resart").contains("restart"));
        assert!(similar_hint("startt").contains("start"));
        assert!(!similar_hint("zzzzz").contains("did you mean"));
    }
    #[test]
    fn status_table_alignment() {
        // Pure formatting check: the shared renderer lines columns up and
        // flags unhealthy programs. Same input the shell `ps` built-in
        // consumes (the top-level `ps` adds two registry columns on top).
        let v = serde_json::json!({
            "programs": [
                {"name": "web", "app": "demo", "state": "running", "pid": 42,
                 "total_exits": 1, "unhealthy": false},
                {"name": "worker-a", "app": "demo", "state": "backoff", "pid": null,
                 "total_exits": 0, "unhealthy": true}
            ]
        });
        let out = render_status_table(&v);
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 4, "header + separator + 2 rows: {out}");
        assert!(lines[0].starts_with("NAME"), "{out}");
        assert!(lines[0].contains("APP") && lines[0].contains("STATE"));
        assert!(lines[0].contains("PID") && lines[0].contains("RESTARTS"));
        assert!(lines[0].contains("UNHEALTHY"));
        assert!(
            lines[0].contains("PENDING"),
            "the shared renderer has the column: {out}"
        );
        assert!(lines[1].starts_with("----"), "{out}");
        assert!(lines[2].contains("web") && lines[2].contains("42"));
        assert!(lines[3].contains("worker-a"));
        assert!(lines[3].contains("yes"), "unhealthy marked: {out}");
        // No JSON braces in the table form.
        assert!(!out.contains('{') && !out.contains('}'), "{out}");
    }

    /// An empty program list renders the registration hint (both callers).
    #[test]
    fn status_table_empty_hint() {
        let v = serde_json::json!({ "programs": [] });
        let out = render_status_table(&v);
        assert!(out.contains("no programs"), "{out}");
    }

    fn projection() -> serde_json::Value {
        serde_json::json!({
            "programs": [
                {"name": "web", "app": "demo", "state": "running", "pid": 42,
                 "total_exits": 1, "unhealthy": false,
                 "config_path": "/opt/demo/appkeeper.toml"},
                {"name": "worker", "app": "demo", "state": "backoff", "pid": null,
                 "total_exits": 0, "unhealthy": true,
                 "config_path": "/opt/demo/appkeeper.toml"},
                {"name": "api", "app": "gateway", "state": "running", "pid": 7,
                 "total_exits": 0, "unhealthy": false, "config_path": null}
            ]
        })
    }

    /// 4.1: `ps` adds the config path (per-program `config_path`) and the
    /// description (CLI-side, keyed by app name) to the shell status columns.
    #[test]
    fn ps_table_adds_config_path_and_description() {
        let mut desc = std::collections::BTreeMap::new();
        desc.insert("demo".to_string(), "the demo app".to_string());
        let out = render_ps_table(&projection(), &desc);
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 5, "header + separator + 3 rows: {out}");
        let header = lines[0];
        for col in ["NAME", "APP", "STATE", "PID", "RESTARTS", "UNHEALTHY"] {
            assert!(header.contains(col), "missing {col}: {header}");
        }
        assert!(
            header.contains("CONFIG") && header.contains("DESCRIPTION"),
            "{header}"
        );
        assert!(lines[1].starts_with("----"), "{out}");
        // Rows are ordered by (app, name): demo/web, demo/worker, gateway/api.
        assert!(lines[2].contains("web") && lines[2].contains("/opt/demo/appkeeper.toml"));
        assert!(
            lines[2].contains("the demo app"),
            "description comes from the registry: {out}"
        );
        assert!(
            lines[3].contains("worker") && lines[3].contains("yes"),
            "{out}"
        );
        // An app without a registry description shows a placeholder.
        assert!(lines[4].contains("api") && lines[4].contains("-"));
        assert!(!out.contains('{') && !out.contains('}'), "{out}");
    }

    /// 4.1: the empty projection renders the registration hint.
    #[test]
    fn ps_table_empty_hint() {
        let desc = std::collections::BTreeMap::new();
        let out = render_ps_table(&serde_json::json!({ "programs": [] }), &desc);
        assert!(out.contains("no programs"), "{out}");
        // A projection without a `programs` array is empty too.
        let out = render_ps_table(&serde_json::json!({}), &desc);
        assert!(out.contains("no programs"), "{out}");
    }

    /// 4.3: `ps --all` online — the projection is authoritative and the
    /// registry only fills in apps it does not cover, labelled not applied.
    #[test]
    fn ps_all_joins_registry_only_for_missing_apps() {
        let desc = std::collections::BTreeMap::new();
        let registry = vec![
            RegistryRow {
                name: "demo".to_string(),
                path: "/opt/demo/appkeeper.toml".to_string(),
                description: Some("the demo app".to_string()),
            },
            RegistryRow {
                name: "pending-app".to_string(),
                path: "/opt/pending/appkeeper.toml".to_string(),
                description: Some("registered but not applied".to_string()),
            },
        ];
        let out = render_ps_all_table(&projection(), &desc, &registry);
        let lines: Vec<&str> = out.lines().collect();
        // header + separator + 3 programs + 1 registry-only app.
        assert_eq!(lines.len(), 6, "{out}");
        assert_eq!(
            lines.iter().filter(|l| l.contains("pending-app")).count(),
            1,
            "the registry-only app is listed exactly once: {out}"
        );
        let added = lines.iter().find(|l| l.contains("pending-app")).unwrap();
        assert!(added.contains("not-applied"), "{added}");
        assert!(
            added.contains("/opt/pending/appkeeper.toml"),
            "the config object path is shown: {added}"
        );
        // The already-loaded app is not duplicated from the registry.
        assert_eq!(
            lines.iter().filter(|l| l.contains("web")).count(),
            1,
            "no duplicate row for the applied app: {out}"
        );
    }

    /// 4.3: `ps --all` online with an empty registry is just the projection.
    #[test]
    fn ps_all_without_registry_matches_plain_ps() {
        let desc = std::collections::BTreeMap::new();
        let v = projection();
        assert_eq!(
            render_ps_all_table(&v, &desc, &[]),
            render_ps_table(&v, &desc)
        );
    }

    /// `ps` PENDING column: a program whose disk definition differs carries a
    /// short marker; the rest stay empty, and the app-level registration /
    /// error entries render as tail summary lines (control-plane spec).
    #[test]
    fn ps_table_marks_pending_and_renders_summary() {
        let mut v = projection();
        v["pending"] = serde_json::json!({
            "programs": [{"app": "demo", "program": "worker", "running": true}],
            "apps_added": ["fresh"],
            "apps_removed": [],
            "daemon_hints": [],
            "errors": [],
        });
        let desc = std::collections::BTreeMap::new();
        let out = render_ps_table(&v, &desc);
        let lines: Vec<&str> = out.lines().collect();
        let header = lines[0];
        assert!(header.contains("PENDING"), "{header}");
        let worker = lines.iter().find(|l| l.contains("worker")).unwrap();
        assert!(worker.contains("changed"), "worker is marked: {worker}");
        let web = lines.iter().find(|l| l.contains("web")).unwrap();
        assert!(!web.contains("changed"), "web carries no marker: {web}");
        // The tail summary names the new app and points at apply.
        assert!(
            out.contains("pending: app[fresh] registered (new)"),
            "{out}"
        );
        assert!(out.contains("pending: run `ak apply` to apply"), "{out}");
    }

    /// A `ps --all` registry-only app that is also pending-added carries the
    /// `new` marker; no pending section means no marker and no summary.
    #[test]
    fn ps_all_marks_pending_added_registry_rows() {
        let mut v = projection();
        v["pending"] = serde_json::json!({
            "programs": [],
            "apps_added": ["pending-app"],
            "apps_removed": [],
            "daemon_hints": [],
            "errors": [],
        });
        let desc = std::collections::BTreeMap::new();
        let registry = vec![RegistryRow {
            name: "pending-app".to_string(),
            path: "/opt/pending/appkeeper.toml".to_string(),
            description: None,
        }];
        let out = render_ps_all_table(&v, &desc, &registry);
        let row = out
            .lines()
            .find(|l| l.contains("pending-app"))
            .expect("registry row");
        assert!(row.contains("new"), "pending-added app marked new: {row}");

        // Without pending data the column is empty and no summary follows.
        let plain = render_ps_all_table(&projection(), &desc, &[]);
        assert!(!plain.contains("pending:"), "{plain}");
        assert!(!plain.contains("changed"), "{plain}");
    }

    /// The shell `ps` built-in shares the renderer: the same table (including
    /// the PENDING marker) minus the two registry columns.
    #[test]
    fn status_table_shares_the_pending_column() {
        let mut v = projection();
        v["pending"] = serde_json::json!({
            "programs": [{"app": "gateway", "program": "api", "running": false}],
        });
        let out = render_status_table(&v);
        assert!(out.contains("PENDING"), "{out}");
        assert!(
            !out.contains("CONFIG") && !out.contains("DESCRIPTION"),
            "{out}"
        );
        let api = out.lines().find(|l| l.contains("api")).unwrap();
        assert!(api.contains("changed"), "{api}");
    }

    /// 4.3: the offline degradation is a daemon-offline note plus the local
    /// registry listing (name / config path / description), exit code 0.
    #[test]
    fn registry_listing_offline_note_and_columns() {
        let registry = vec![
            RegistryRow {
                name: "zeta".to_string(),
                path: "/opt/zeta/appkeeper.toml".to_string(),
                description: None,
            },
            RegistryRow {
                name: "alpha".to_string(),
                path: "/opt/alpha/appkeeper.toml".to_string(),
                description: Some("alpha app".to_string()),
            },
        ];
        let out = render_registry_listing(&registry, true);
        let lines: Vec<&str> = out.lines().collect();
        assert!(lines[0].contains("daemon offline"), "{out}");
        assert!(lines[1].starts_with("NAME"), "{out}");
        assert!(lines[1].contains("CONFIG") && lines[1].contains("DESCRIPTION"));
        assert!(lines[2].starts_with("----"), "{out}");
        // Sorted by name.
        assert!(
            lines[3].contains("alpha") && lines[3].contains("alpha app"),
            "{out}"
        );
        assert!(lines[4].contains("zeta") && lines[4].contains("-"), "{out}");
        assert_eq!(lines.len(), 5, "{out}");

        // Without the offline flag there is no note (the online empty case).
        let plain = render_registry_listing(&registry, false);
        assert!(!plain.contains("daemon offline"), "{plain}");
        assert!(plain.starts_with("NAME"), "{plain}");
    }

    /// An empty registry still renders a hint rather than a bare header.
    #[test]
    fn registry_listing_empty_hint() {
        let out = render_registry_listing(&[], true);
        assert!(out.contains("daemon offline"), "{out}");
        assert!(out.contains("no programs"), "{out}");
    }

    #[test]
    fn webui_health_offline_is_error() {
        assert!(webui_health("http://127.0.0.1:1").is_err());
    }

    /// The `open` verb addresses the console from the `[webui]` config when
    /// the section is present and falls back to the built-in default listen
    /// otherwise (shell-client migration: `system webui` is gone, the URL
    /// follows `webui.listen`).
    #[test]
    fn webui_url_of_uses_configured_listen_or_default() {
        let cfg: crate::config::DaemonConfig =
            toml::from_str("[webui]\nlisten = '0.0.0.0:9999'\n").unwrap();
        assert_eq!(webui_url_of(&cfg), "http://0.0.0.0:9999");
        let off: crate::config::DaemonConfig =
            toml::from_str("[daemon]\nsocket = \"/tmp/ak.sock\"\n").unwrap();
        assert_eq!(webui_url_of(&off), "http://127.0.0.1:9877");
        assert_eq!(
            webui_url_of(&off),
            format!("http://{DEFAULT_WEBUI_LISTEN}"),
            "the fallback matches the built-in [webui] default"
        );
    }
}
