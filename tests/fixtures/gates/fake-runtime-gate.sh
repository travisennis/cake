#!/bin/sh
# Deterministic cake-gate/1 gate for runtime configuration tests.
#
# Usage: fake-runtime-gate.sh MODE_FILE [CAPTURE_DIR] <describe|serve>
#
# MODE_FILE selects behavior:
#   allow, oneshot-allow   allow every check (oneshot-allow declares oneshot)
#   block                  block every check with code "runtime-block"
#   init-error             answer init with a config error frame
#   check-error            answer a check with a config error frame
#   wrong-id               answer a check with a different request id
#   no-models              omit the resolved-models capability
#   no-shadow              omit the shadow capability
#
# CAPTURE_DIR, when provided, receives:
#   starts      one line per serve process
#   init.jsonl  every init frame received
#   env.<pid>   the process environment, one NAME=VALUE per line
#
# The script reads one NDJSON frame per line and never reads the workspace.

set -eu

MODE=$(cat "$1")
shift

CAPTURE_DIR=""
if [ "$#" -gt 0 ] && [ -d "$1" ]; then
    CAPTURE_DIR=$1
    shift
fi

capture_start() {
    if [ -n "$CAPTURE_DIR" ]; then
        printf 'start\n' >>"$CAPTURE_DIR/starts"
        env | sort >"$CAPTURE_DIR/env.$$"
    fi
}

case "${1:-}" in
    describe)
        transports='["stdio"]'
        case "$MODE" in
            oneshot-allow) transports='["stdio","oneshot"]' ;;
        esac
        capabilities='["resolved-models","shadow","diagnostic"]'
        case "$MODE" in
            no-models) capabilities='["shadow","diagnostic"]' ;;
            no-shadow) capabilities='["resolved-models","diagnostic"]' ;;
        esac
        printf '%s\n' "{\"protocol\":\"cake-gate/1\",\"name\":\"runtime-fake\",\"version\":\"0.1.0\",\"phases\":[\"pre_tool\"],\"actions\":[\"bash.exec\"],\"transports\":$transports,\"capabilities\":$capabilities}"
        exit 0
        ;;
    serve)
        capture_start
        ;;
    *)
        printf 'fake-runtime-gate: unknown subcommand %s\n' "${1:-}" >&2
        exit 64
        ;;
esac

while IFS= read -r line; do
    case "$line" in
        *'"type":"init"'*)
            if [ -n "$CAPTURE_DIR" ]; then
                printf '%s\n' "$line" >>"$CAPTURE_DIR/init.jsonl"
            fi
            if [ "$MODE" = "init-error" ]; then
                printf '%s\n' '{"protocol":"cake-gate/1","type":"error","id":"init","class":"config","message":"fake init failure"}'
            else
                printf '%s\n' '{"protocol":"cake-gate/1","type":"ready","id":"init","name":"runtime-fake","version":"0.1.0","warnings":[]}'
            fi
            ;;
        *'"type":"check"'*)
            id=$(printf '%s' "$line" | sed -n 's/.*"id":"\([^"]*\)".*/\1/p')
            case "$MODE" in
                block)
                    printf '{"protocol":"cake-gate/1","type":"decision","id":"%s","decision":"block","code":"runtime-block","message":"runtime fake blocked","confidence":0.99,"annotations":[],"short_circuit":false,"evidence":[]}\n' "$id"
                    ;;
                check-error)
                    printf '{"protocol":"cake-gate/1","type":"error","id":"%s","class":"config","message":"fake check failure"}\n' "$id"
                    ;;
                wrong-id)
                    printf '{"protocol":"cake-gate/1","type":"decision","id":"other-%s","decision":"allow","message":"","annotations":[],"short_circuit":false,"evidence":[]}\n' "$id"
                    ;;
                *)
                    printf '{"protocol":"cake-gate/1","type":"decision","id":"%s","decision":"allow","message":"","annotations":[],"short_circuit":false,"evidence":[]}\n' "$id"
                    ;;
            esac
            ;;
        *'"type":"shutdown"'*)
            exit 0
            ;;
    esac
done
exit 0
