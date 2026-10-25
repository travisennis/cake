#!/bin/sh
# Fake cake-gate/1 gate for slice-1 seam tests.
#
# FAKE_GATE_MODE selects behavior:
#   allow, warn, block, abstain   one decision mode
#   malformed                     stdout noise instead of a frame
#   delayed                       sleep FAKE_GATE_DELAY seconds, then allow
#   crash                         exit 7 while serving a check
#   crash-once                    crash on the first check, then allow
#   oversized                     one frame over the 1 MiB limit
#   bad-describe                  describe speaks the wrong protocol
#   init-error                    reply to init with an error frame
#
# The script reads one NDJSON frame per line and answers on stdout. It never
# reads the workspace or the command text.

MODE=${FAKE_GATE_MODE:-allow}
DELAY=${FAKE_GATE_DELAY:-5}

describe() {
    if [ "$MODE" = "bad-describe" ]; then
        printf '%s\n' '{"protocol":"cake-gate/2","name":"fake-safety","version":"0.1.0","phases":["pre_tool"],"actions":["bash.exec"],"transports":["stdio"],"capabilities":[]}'
        return 0
    fi
    printf '%s\n' '{"protocol":"cake-gate/1","name":"fake-safety","version":"0.1.0","phases":["pre_tool"],"actions":["bash.exec"],"transports":["stdio"],"capabilities":["shadow","diagnostic"]}'
}

decision() {
    id=$1
    case "$MODE" in
        warn)
            printf '{"protocol":"cake-gate/1","type":"decision","id":"%s","decision":"warn","code":"fake-warn","message":"Fake gate says to inspect this command.","confidence":0.75,"annotations":[],"short_circuit":false,"evidence":[]}\n' "$id"
            ;;
        block)
            printf '{"protocol":"cake-gate/1","type":"decision","id":"%s","decision":"block","code":"fake-block","message":"Fake gate blocked this command.","confidence":0.99,"annotations":[],"short_circuit":false,"evidence":[]}\n' "$id"
            ;;
        abstain)
            printf '{"protocol":"cake-gate/1","type":"decision","id":"%s","decision":"abstain","message":"","annotations":[],"short_circuit":false,"evidence":[]}\n' "$id"
            ;;
        *)
            printf '{"protocol":"cake-gate/1","type":"decision","id":"%s","decision":"allow","message":"","annotations":[],"short_circuit":false,"evidence":[]}\n' "$id"
            ;;
    esac
}

case "${1:-}" in
    describe)
        describe
        exit 0
        ;;
    serve)
        ;;
    *)
        printf 'fake-gate: unknown mode %s\n' "${1:-}" >&2
        exit 64
        ;;
esac

while IFS= read -r line; do
    case "$line" in
        *'"type":"init"'*)
            if [ "$MODE" = "init-error" ]; then
                printf '%s\n' '{"protocol":"cake-gate/1","type":"error","id":"init","class":"config","message":"fake init failure"}'
            else
                printf '%s\n' '{"protocol":"cake-gate/1","type":"ready","id":"init","name":"fake-safety","version":"0.1.0","warnings":[]}'
            fi
            ;;
        *'"type":"check"'*)
            id=$(printf '%s' "$line" | sed -n 's/.*"id":"\([^"]*\)".*/\1/p')
            case "$MODE" in
                malformed)
                    printf '%s\n' 'this is not json'
                    ;;
                delayed)
                    sleep "$DELAY"
                    decision "$id"
                    ;;
                crash)
                    exit 7
                    ;;
                crash-once)
                    state=${FAKE_GATE_STATE:?FAKE_GATE_STATE is required for crash-once}
                    count=0
                    if [ -f "$state" ]; then
                        count=$(cat "$state")
                    fi
                    count=$((count + 1))
                    printf '%s' "$count" > "$state"
                    if [ "$count" -eq 1 ]; then
                        exit 7
                    fi
                    decision "$id"
                    ;;
                oversized)
                    printf '%s' '{"protocol":"cake-gate/1","type":"decision","id":"'
                    printf '%s' "$id"
                    printf '%s' '","decision":"allow","message":"'
                    head -c 1100000 /dev/zero | tr '\0' x
                    printf '%s\n' '"}'
                    ;;
                *)
                    decision "$id"
                    ;;
            esac
            ;;
        *'"type":"shutdown"'*)
            exit 0
            ;;
    esac
done
exit 0
