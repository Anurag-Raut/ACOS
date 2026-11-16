#!/bin/sh
# SERVELEAK2/F2: reap only the daemons **this run** started.
#
# `acos-bench.sh` used to reap with `pgrep -f "acos serve .*$SCRATCH_ROOT/r[0-9]"`.
# The default scratch root is shared (`<acos>/.cache/h6`, `--scratch-root` is
# optional) and two runs both create `<scratch>/r1-<group>`, so the first run to
# finish matched, and SIGKILLed, the *other* run's live session daemons and their
# ART guests.
#
# Ownership is now proved, not guessed:
#
#   * `acos-bench.sh` exports `ACOS_BENCH_RUN_ID` (unique per run) and every
#     daemon this run spawns inherits it (`d2_warm.Serve` passes the environment
#     through), so `/proc/<pid>/environ` names the run that started it;
#   * `d2_warm.Serve` records `pid starttime` in `ACOS_BENCH_PIDFILE` at spawn,
#     and the reap reads only those pids;
#   * every pid is re-verified immediately before it is signalled: it must still
#     exist, still carry our run id, still be an `acos serve`, and its
#     `/proc/<pid>/stat` start time must equal the recorded one (a recycled pid
#     fails this, so a stranger can never be signalled through it).
#
# Sourced by `tools/bench/acos-bench.sh`; `tools/bench/test-reap-scope.sh` is the
# dry test (two runs, one shared scratch root, only the caller's pids reaped).

# Field 22 (starttime) of /proc/<pid>/stat, or empty when the pid is gone.
# The command name can hold spaces and parentheses, so parse after the last ')'.
bench_starttime() {
    _st_pid=$1
    [ -r "/proc/$_st_pid/stat" ] || return 0
    sed 's/.*) //' "/proc/$_st_pid/stat" 2>/dev/null | awk '{print $20}'
}

# Does /proc/<pid>/environ carry exactly NAME=value?
bench_environ_has() {
    _env_pid=$1
    _env_needle=$2
    [ -r "/proc/$_env_pid/environ" ] || return 1
    tr '\0' '\n' < "/proc/$_env_pid/environ" 2>/dev/null | grep -qxF -- "$_env_needle"
}

# Is <pid> a daemon this run owns, still the same process?
bench_is_our_daemon() {
    _own_pid=$1
    _own_starttime=$2
    [ -n "$_own_pid" ] || return 1
    case "$_own_pid" in
        *[!0-9]*) return 1 ;;
    esac
    [ "$_own_pid" = "$$" ] && return 1
    [ -d "/proc/$_own_pid" ] || return 1
    case "$(tr '\0' ' ' < "/proc/$_own_pid/cmdline" 2>/dev/null || true)" in
        *"acos serve"*) ;;
        *) return 1 ;;
    esac
    [ -n "${ACOS_BENCH_RUN_ID:-}" ] || return 1
    bench_environ_has "$_own_pid" "ACOS_BENCH_RUN_ID=$ACOS_BENCH_RUN_ID" || return 1
    if [ -n "$_own_starttime" ]; then
        _now=$(bench_starttime "$_own_pid")
        [ "$_now" = "$_own_starttime" ] || return 1
    fi
    return 0
}

bench_reap_daemons() {
    [ -n "${ACOS_BENCH_RUN_ID:-}" ] || return 0
    _pidfile="${ACOS_BENCH_PIDFILE:-}"
    _pids=""
    if [ -n "$_pidfile" ] && [ -r "$_pidfile" ]; then
        _pids=$(awk '{print $1":"$2}' "$_pidfile" 2>/dev/null)
    fi
    for _entry in $_pids; do
        _pid=${_entry%%:*}
        _recorded_start=${_entry#*:}
        bench_is_our_daemon "$_pid" "$_recorded_start" || continue
        echo "acos-bench: reaping leftover session daemon $_pid (run $ACOS_BENCH_RUN_ID)" >&2
        kill -TERM "$_pid" 2>/dev/null || true
        for _ in $(seq 1 50); do
            [ -d "/proc/$_pid" ] || break
            sleep 0.1
        done
        if [ -d "/proc/$_pid" ] && bench_is_our_daemon "$_pid" "$_recorded_start"; then
            kill -KILL "$_pid" 2>/dev/null || true
        fi
    done
}
