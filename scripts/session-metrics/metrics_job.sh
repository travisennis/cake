#!/bin/sh
# Run or report on cake's local metrics dashboard from launchd.
#
# launchd starts this script with /bin/sh, so a missing or upgraded Python
# interpreter cannot prevent the job from spawning. The installer passes the
# interpreter it used plus Homebrew and system fallbacks; refresh picks the
# first candidate that exists and reports Python 3.10 or newer.
set -u

usage()
{
    printf 'usage: %s refresh ROOT SESSIONS TELEMETRY PYTHON...\n' "$0" >&2
    printf '       %s remind ROOT\n' "$0" >&2
    exit 64
}

log()
{
    printf '%s %s\n' "$(date '+%Y-%m-%d %H:%M:%S')" "$*" >> "$LOG"
}

notify()
{
    [ "${CAKE_METRICS_NO_NOTIFY:-}" = 1 ] && return 0
    [ "$(uname -s)" = Darwin ] || return 0
    /usr/bin/osascript -e "display notification \"$1\" with title \"Cake metrics\"" \
        >/dev/null 2>&1 || true
}

[ $# -ge 2 ] || usage

MODE=$1
ROOT=$2
shift 2

case "$MODE" in
    refresh)
        [ $# -ge 3 ] || usage
        SESSIONS=$1
        TELEMETRY=$2
        shift 2
        LOG=$ROOT/refresh.log

        PYTHON=
        for candidate in "$@"; do
            [ -n "$candidate" ] && [ -x "$candidate" ] || continue
            if "$candidate" -c 'import sys; raise SystemExit(sys.version_info < (3, 10))' \
                >/dev/null 2>&1; then
                PYTHON=$candidate
                break
            fi
        done

        if [ -z "$PYTHON" ]; then
            log 'refresh failed: no Python 3.10+ interpreter found'
            notify 'Metrics refresh failed: no Python 3.10+ found. See refresh.log.'
            exit 78
        fi

        "$PYTHON" "$ROOT/runtime/dashboard.py" --output "$ROOT" \
            --sessions-dir "$SESSIONS" --telemetry-dir "$TELEMETRY" \
            >> "$ROOT/refresh.log" 2>&1
        status=$?
        if [ "$status" -ne 0 ]; then
            log "refresh failed: dashboard.py exited $status via $PYTHON"
            notify "Metrics refresh failed (exit $status). See refresh.log."
            exit "$status"
        fi
        log "refresh succeeded via $PYTHON"
        ;;
    remind)
        LOG=$ROOT/reminder.log
        METRICS=$ROOT/metrics.json
        if [ ! -f "$METRICS" ]; then
            log 'reminder: metrics.json is missing'
            notify 'Cake metrics are missing. Reinstall the dashboard schedule.'
            exit 0
        fi
        if [ "$(uname -s)" = Darwin ]; then
            MODIFIED=$(stat -f %m "$METRICS")
        else
            MODIFIED=$(stat -c %Y "$METRICS")
        fi
        [ -n "$MODIFIED" ] || MODIFIED=0
        HOURS=$(( ($(date +%s) - MODIFIED) / 3600 ))
        if [ "$HOURS" -ge 36 ]; then
            log "reminder: report is stale (${HOURS}h old)"
            notify "Cake metrics are stale: last refreshed ${HOURS}h ago."
        else
            log "reminder: report is fresh (${HOURS}h old)"
            notify 'Your local dashboard is ready. Open your Cake metrics bookmark.'
        fi
        ;;
    *)
        usage
        ;;
esac
