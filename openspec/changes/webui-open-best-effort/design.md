## Context

`src/webui_open.rs` is the single decision point for `ak webui`: it decides the
exit code and the lines for each stream, and `main.rs::webui_cmd` only prints
them and exits. The probe and launch steps are injected, so the whole contract
is unit-testable without a socket or a browser.

The current order is: unavailable check → print nothing → probe `/api/health`
(5 s budget) → headless check → launch → print. Three consequences shape this
design: (a) a failed probe returns before any URL is printed, even though
`GET /v1/webui` already handed us the address; (b) the headless path is checked
*after* the probe, so a machine that will never launch a browser still waits the
full budget and can fail without printing the URL; (c) even where the URL is
printed, it is printed **last**, so a probe timeout (up to 5 s) or a launcher
that blocks on `cmd.status()` delays it exactly on the paths where the operator
most wants it. See proposal.md — Why.

## Goals / Non-Goals

**Goals:**

- Deliver the URL as early and unconditionally as possible: any state in which
  the daemon reports `enabled && serving` prints the URL before anything else can
  fail, and before anything else can take time.
- Make every browser-step failure (no GUI, probe timeout, missing/failing
  launcher) a single stderr note with exit 0; keep exit 1 exclusively for the
  daemon-reported configuration states (`enabled == false` / `serving == false`).
- Keep the decision pure (`plan(status, mode, gui) -> Plan`) so the stream
  layout and the exit code are unit-testable without a socket or a browser, and
  keep `main.rs` a thin consume-and-exit.

**Non-Goals:**

- Changing the probe itself (URL, timeout, `/api/health` being public).
- Trying other launchers or `$BROWSER` when the platform opener is missing.
- Bounding a launcher that blocks until the browser exits (`cmd.status()` stays);
  pre-existing behavior, unrelated to the exit-code contract.
- Changing the interactive shell's `open` (`src/shell.rs`): it keeps
  `opening <url>` and its single-shot probe. Its probe/hint contract lives in
  `shell-client` and has no process exit code, so `ak webui`'s stream layout
  does not apply — the two share URL construction, not output format.

## Decisions

### D1: Print the URL before the side effects; the probe only gates the launch

Split the decision at the side-effect boundary instead of at "what to print":

```rust
enum Plan {
    Unavailable { reason: String }, // no URL field => "no URL when unusable" is structural
    Print { line: String },         // --print-url / --json
    Open { url: String, gui: bool },
}
```

- `plan(status, mode, gui)` is pure: it decides everything that depends on
  `enabled` / `serving` / `url` / `auth` / `token` / `gui` and nothing else.
- `main` consumes the plan. `Print` prints its line. `Open` prints `url` and
  flushes **before** calling the executor; Rust's `Stdout` is line-buffered, so
  the newline is visible immediately even when piped.
- `execute(gui, probe, launch) -> (code, notes)` only produces stderr notes and
  an exit code. It cannot suppress or reorder the URL, because it never sees it.

The URL is the artifact, so it is emitted while the only facts needed for it are
in hand. Nothing after that point can suppress it — and, unlike a `WebuiReport`
that `main` prints *after* `run` returns, nothing can delay it either: the
probe-failure path would otherwise show the URL 5 s late, and a blocking
launcher would hold it indefinitely.

*Alternatives considered:*

- **Keep probe-first and print the URL inside the probe-failure branch.**
  Rejected: two print sites that must stay word-for-word identical, the
  headless path waits 5 s for a browser it will never open, and the ordering
  trap returns for any future failure added between probe and launch.
- **Print only on success/headless (closest to today).** Rejected: that is the
  contradiction this change removes. (Today's launch-failure branch already
  prints the URL; the probe-failure branch is the one that does not.)
- **Keep the single `run -> WebuiReport` and print in `main` after it
  returns.** Rejected: the report is *decided* before the side effects but
  *printed* after them, so "immediately" would be true only of the data
  structure, not of the process — the probe-failure path still shows the URL
  5 s late.
- **Pass an `emit` sink into `run`.** Works, but pushes a side effect back into
  the pure decision. The plan/execute split keeps the decision pure and makes
  the ordering a property of the caller.

### D2: Probe failure is a degradation, not a command failure (exit 0)

Exit 1 stays reserved for the daemon's own verdict (`enabled == false` /
`serving == false`) — the only states the operator can fix with a config command.
A probe timeout is weaker, client-side evidence that can be produced by a proxy,
a firewall, an IPv4/IPv6 mismatch, or a console that is still binding; treating it
as "not serving" pretends to the same certainty while throwing away the URL.

*Alternatives considered:*

- **Print the URL first but keep exit 1 on probe failure.** Rejected: it keeps
  "convenience step failed ⇒ command failed", the exact semantic this change
  exists to remove, and would make the exit code contradict `--json` (which never
  probes and exits 0 in the same daemon state).
- **Keep exit 1 and add a `--strict` flag.** Rejected: extra surface for a
  convenience path; a caller that needs reachability can probe `/api/health`.

*What this gives up (capability removal, not just a code change):* after this
change no exit code reports reachability — `ak webui` can exit 0 against a
console that answers nothing. A script that wants "is the console up?" must make
the HTTP call itself. Accepted because the daemon's `serving` self-report already
covers the common case, and the probe only catches a stale self-report or an
intermediary (proxy, firewall, address-family mismatch); a convenience command
should not double as the monitoring interface.

### D3: One degradation path for "no launcher" and "launcher failed", one line each

`launch_browser` keeps returning `Err` with context in both cases (spawn failure
carries `could not start the system browser`; non-zero exit carries the status).
`execute` treats them identically and never falls back to another launcher, so
one scenario and one test branch cover both.

Each degradation is **one** stderr line carrying both the reason and the manual
fallback, prefixed `ak: warning:` (not `ak: error:` — the exit code is 0, and
`error:` would trip CI log matchers and editor problem panes). The old two-line
`ak: error: could not launch …` + `hint: …` pair is gone.

### D4: `opened` is a constant `false` in the only mode that emits JSON

`--json` is an output mode: it never probes and never launches, so `opened` can
never be true in a JSON document. The spec now says so instead of leaving the
field undefined. No `opened` state is carried in `Plan` — only `Open` mode could
observe a launch, and `Open` mode prints no JSON, so a carried boolean would have
no reader.

*Alternative considered:* drop `opened` from the JSON entirely (breaking the shape
and e2e W6b) to avoid a field that is always false. Rejected: removal buys
nothing for callers and costs a compatibility break; documenting the constant is
cheaper and honest.

### D5: stdout is exactly one bare URL in every `Open` outcome

The launch-success branch used to print `opening <url>`. With the URL already on
stdout, that line would duplicate it and break the "stdout is the URL" rule that
lets terminals linkify it. Success, headless, probe failure and launch failure all
print exactly one bare URL on stdout; the only differences are the optional
single `ak: warning:` line on stderr and the exit code (now 0 in every state
where the daemon serves the console).

The two output modes are side-effect free and silent: `--print-url` prints the
URL alone and `--json` the JSON document, and neither writes stderr.

## Risks / Trade-offs

- **Probe failure is no longer visible in the exit code** → the stderr note
  carries the reason and the URL still lets the operator try; scripts that need
  reachability should call `/api/health`, not infer it from `ak webui`.
- **Breaking: exit 1 → 0, the `opening ` prefix disappears, and the headless
  hint moves from stdout to stderr** → called out in the proposal;
  machine-readable callers use `--print-url` / `--json`.
- **Silent degradation** (browser never opens) → the URL is on stdout and the
  reason on stderr, so terminal output always tells the whole story.
- **Probe failure is not e2e-reproducible** (it needs a daemon that reports
  `serving` while its console does not answer) → unit tests at
  `plan`/`execute` are its gate. Headless and launcher-missing *are* e2e-covered
  (env overrides only), and e2e W6b's `opened == false` assertion stays valid.
- **Two comments describe the old contract** (`main.rs::webui_cmd` —
  "failed launch are exit 1", "token never by the regular success line";
  `shell.rs::open_webui` — "Same contract as `ak webui`"). They are not
  behavior, but they are the only place the old promise is written down, so the
  change must update them.

## Migration Plan

No data or config migration. Deploy = ship the binary; rollback = revert the
commit. Users relying on exit 1 (or on the `opening ` prefix) should switch to
`--print-url` / `--json` or to a direct `/api/health` check.
