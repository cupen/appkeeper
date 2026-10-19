#!/usr/bin/env bash
# appkeeper one-shot acceptance entrypoint (docker-test-env).
#
# Runs the full suite unattended and stops at the first failing stage:
#   01 cargo test --workspace
#   02 webui: pnpm install, tsc --noEmit, vitest, vite build
#   03 xtask e2e (CLI + webui lifecycle + playwright browser + bench smoke)
#   04 appkeeper-bench firehose report (JSON into /out)
#
# AK_TEST_STAGES selects a comma-separated subset (test,webui,e2e,bench).
set -uo pipefail

WORK="${AK_WORK_DIR:-/work}"
OUT="${AK_OUT_DIR:-/out}"
STAGES="${AK_TEST_STAGES:-all}"

log() { printf '[ak-test] %s\n' "$*"; }
die() { printf '[ak-test] ERROR: %s\n' "$*" >&2; exit 2; }

# Any argument other than the default `all` replaces the suite, so
# `docker run -it <img> bash` opens a shell instead of running acceptance.
if [ "$#" -gt 0 ] && [ "$1" != "all" ]; then
    exec "$@"
fi

[ -f "$WORK/Cargo.toml" ] || die "$WORK is not an appkeeper checkout (mount it at $WORK)"
mkdir -p "$OUT"

case "$STAGES" in
    all|ALL) WANT="test webui e2e bench" ;;
    *)       WANT="${STAGES//,/ }" ;;
esac

want() {
    case " $WANT " in *" $1 "*) return 0 ;; *) return 1 ;; esac
}

stage_test()  { cd "$WORK"        && cargo test --workspace; }
stage_webui() {
    cd "$WORK/webui" \
        && pnpm install --frozen-lockfile --prefer-offline \
        && pnpm exec tsc --noEmit \
        && pnpm test \
        && pnpm build
}
stage_e2e()   { cd "$WORK"        && cargo run -p xtask -- e2e; }
stage_bench() {
    # bench discovers a binary named `appkeeper`, which the workspace no longer
    # produces (the daemon is `ak`), so point it at the path explicitly.
    cd "$WORK" \
        && cargo build --quiet --bin ak \
        && cargo run -q -p appkeeper-bench -- \
             --case firehose --duration 10 \
             --daemon "$WORK/target/debug/ak" \
             --json "$OUT/bench-firehose.json"
}

run_stage() {
    local num="$1" name="$2" fn="$3"
    local logfile="$OUT/$num-$name.log"
    log "stage $num ($name) -> $logfile"
    ( "$fn" ) 2>&1 | tee "$logfile"
    local rc=${PIPESTATUS[0]}
    if [ "$rc" -ne 0 ]; then
        log "stage $num ($name) FAILED with exit $rc"
        return "$rc"
    fi
    log "stage $num ($name) ok"
    return 0
}

restore_ownership() {
    [ -n "${HOST_UID:-}" ] && [ -n "${HOST_GID:-}" ] || return 0
    local paths=()
    [ -d "$WORK/target" ] && paths+=("$WORK/target")
    [ -d "$WORK/webui/node_modules" ] && paths+=("$WORK/webui/node_modules")
    [ -d "$WORK/webui/dist" ] && paths+=("$WORK/webui/dist")
    [ -d "$OUT" ] && paths+=("$OUT")
    [ "${#paths[@]}" -gt 0 ] || return 0
    log "restoring ownership of ${#paths[@]} path(s) to $HOST_UID:$HOST_GID"
    if ! chown -R "$HOST_UID:$HOST_GID" "${paths[@]}"; then
        log "WARNING: ownership restore failed (acceptance result is unaffected)"
    fi
}

rc=0
if want test;  then run_stage 01 cargo-test stage_test  || rc=$?; fi
if [ "$rc" -eq 0 ] && want webui; then run_stage 02 webui     stage_webui || rc=$?; fi
if [ "$rc" -eq 0 ] && want e2e;   then run_stage 03 e2e       stage_e2e   || rc=$?; fi
if [ "$rc" -eq 0 ] && want bench; then run_stage 04 bench     stage_bench || rc=$?; fi

restore_ownership
log "done with exit $rc"
exit "$rc"
