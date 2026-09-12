# Integrating Herdr with Cake Hooks

Use this runbook to report Cake's lifecycle to [Herdr](https://herdr.dev) so a Cake pane shows `working` while a turn runs and settles when the process finishes. The integration is configuration only: a reporter script plus `hooks.json` entries. No Cake code changes are required. The same pattern works for any host that reports agent state from lifecycle hooks.

## When to Use It

- You run Cake inside a Herdr pane and want the pane's agent state to reflect Cake activity.
- You are building a lifecycle integration for another host and want a worked example of the Cake hook protocol.

Herdr already detects agents from the screen. This runbook only adds semantic state that Herdr cannot read from the screen; if you do not need that, do nothing.

## What Herdr Expects

The [Herdr custom integration guide](https://herdr.dev/docs/integrations/#integrate-your-own-agent) defines the contract:

- A process in a Herdr pane inherits `HERDR_ENV`, `HERDR_PANE_ID`, `HERDR_BIN_PATH`, and `HERDR_SOCKET_PATH`.
- The agent reports state with `herdr pane report-agent "$HERDR_PANE_ID" --source custom:<name> --agent <label> --state idle|working|blocked`.
- Session identity can ride along with `--agent-session-id`/`--agent-session-path`, or be reported separately with `herdr pane report-agent-session`.
- The integration guards on `HERDR_ENV=1` so it is inert outside Herdr.
- If reports can arrive out of order, pass a strictly increasing `--seq`; Herdr ignores stale sequence numbers from the same source. A release must also carry a newer `--seq` or it is discarded.

Cake supplies what the report needs. Hook commands inherit the pane environment, and Cake's hook payload already carries `session_id`, `transcript_path`, `hook_event_name`, and the `SessionStart` `source` (`startup`, `resume`, or `fork`).

## Lifecycle Mapping

  | Cake hook event    | Herdr action                                                           | Why                                                                        |
  | ------------------ | ---------------------------------------------------------------------- | -------------------------------------------------------------------------- |
  | `SessionStart`     | `report-agent-session` (identity), then `report-agent --state working` | The turn is starting; report the resumable session reference alongside it. |
  | `UserPromptSubmit` | `report-agent --state working`                                         | The agent is processing the prompt.                                        |
  | `Stop`             | `report-agent --state idle`                                            | The turn finished and the process is about to exit.                        |
  | `ErrorOccurred`    | `report-agent --state idle`                                            | The turn failed; the process is ending.                                    |

`PreToolUse`/`PostToolUse` are not needed because `working` already covers the whole turn. Cake never reports `blocked`: it has no interactive permission or ask flow ([Integration contracts](../integrations.md)), so it never pauses mid-turn for a user decision.

## Install

### Global

1. Save the reporter script below to `~/.config/cake/hooks/herdr-cake-report.sh` and make it executable.
2. Add the hooks.json entries below to `~/.config/cake/hooks.json`, merging with an existing file if one is present.

Cake loads hooks from `<config>/cake/hooks.json`, then `.cake/hooks.json`, then `.cake/hooks.local.json`; the global file applies to every project.

### Per Project

Put the same two files under `.cake/` in the project. Commit them only when every project user is expected to trust them: hook commands run outside the model sandbox.

## Reporter Script

Save as `herdr-cake-report.sh` (or `.cake/hooks/herdr-cake-report.sh` for a project install).

```sh
#!/bin/sh
# herdr-cake-report.sh - report Cake lifecycle state to Herdr.
#
# Wire from Cake hooks (see hooks.json):
#   SessionStart     -> session  (report session identity, then working)
#   UserPromptSubmit -> working
#   Stop             -> idle
#   ErrorOccurred    -> idle
#
# Outside a Herdr pane every action is a silent no-op, so the hook never
# changes Cake behavior on a normal terminal.
set -u

action="${1:-}"
case "$action" in
  session|working|idle|blocked|release) ;;
  *) exit 0 ;;
esac

[ "${HERDR_ENV:-}" = "1" ] || exit 0
[ -n "${HERDR_BIN_PATH:-}" ] || exit 0
[ -n "${HERDR_PANE_ID:-}" ] || exit 0

payload="$(cat 2>/dev/null || true)"
if command -v jq >/dev/null 2>&1; then
  sid="$(printf '%s' "$payload" | jq -r '.session_id // empty' 2>/dev/null || true)"
  tpath="$(printf '%s' "$payload" | jq -r '.transcript_path // empty' 2>/dev/null || true)"
  ssource="$(printf '%s' "$payload" | jq -r '.source // empty' 2>/dev/null || true)"
else
  sid=""; tpath=""; ssource=""
fi
case "$ssource" in startup|resume|fork) ;; *) ssource="" ;; esac

SOURCE="custom:cake"
AGENT="cake"
seq="$(python3 -c 'import time; print(time.time_ns())' 2>/dev/null || echo 0)"

if [ -n "${HERDR_CAKE_LOG:-}" ]; then
  printf '%s action=%s pane=%s sid=%s path=%s src=%s seq=%s\n' \
    "$(date -u +%FT%TZ)" "$action" "${HERDR_PANE_ID:-}" "$sid" "$tpath" "$ssource" "$seq" \
    >>"$HERDR_CAKE_LOG" 2>/dev/null || true
fi

run() { "$HERDR_BIN_PATH" "$@" >/dev/null 2>&1 || true; }

report_state() {
  run pane report-agent "$HERDR_PANE_ID" --source "$SOURCE" --agent "$AGENT" \
    --state "$1" --seq "$seq"
}

report_identity() {
  if [ -n "$tpath" ]; then
    if [ -n "$ssource" ]; then
      run pane report-agent-session "$HERDR_PANE_ID" --source "$SOURCE" --agent "$AGENT" \
        --agent-session-id "$sid" --agent-session-path "$tpath" \
        --session-start-source "$ssource" --seq "$seq"
    else
      run pane report-agent-session "$HERDR_PANE_ID" --source "$SOURCE" --agent "$AGENT" \
        --agent-session-id "$sid" --agent-session-path "$tpath" --seq "$seq"
    fi
  fi
}

case "$action" in
  session)
    [ -n "$sid" ] && report_identity
    report_state working
    ;;
  working) report_state working ;;
  idle)    report_state idle ;;
  blocked) report_state blocked ;;
  release) run pane release-agent "$HERDR_PANE_ID" --source "$SOURCE" --agent "$AGENT" --seq "$seq" ;;
esac

exit 0
```

The script writes nothing and always exits `0`, which matters because Cake parses `SessionStart`/`UserPromptSubmit` stdout as a hook decision and treats exit code `2` as a block. Set `HERDR_CAKE_LOG=/path/to/log` to have it append each action it received.

## hooks.json

```json
{
  "version": 1,
  "hooks": {
    "SessionStart": [
      {
        "matcher": "*",
        "hooks": [
          { "type": "command", "command": "\"${XDG_CONFIG_HOME:-$HOME/.config}/cake/hooks/herdr-cake-report.sh\" session", "timeout": 10 }
        ]
      }
    ],
    "UserPromptSubmit": [
      {
        "hooks": [
          { "type": "command", "command": "\"${XDG_CONFIG_HOME:-$HOME/.config}/cake/hooks/herdr-cake-report.sh\" working", "timeout": 10 }
        ]
      }
    ],
    "Stop": [
      {
        "hooks": [
          { "type": "command", "command": "\"${XDG_CONFIG_HOME:-$HOME/.config}/cake/hooks/herdr-cake-report.sh\" idle", "timeout": 10 }
        ]
      }
    ],
    "ErrorOccurred": [
      {
        "hooks": [
          { "type": "command", "command": "\"${XDG_CONFIG_HOME:-$HOME/.config}/cake/hooks/herdr-cake-report.sh\" idle", "timeout": 10 }
        ]
      }
    ]
  }
}
```

For a project install, replace the command with the path to the project copy, for example `"./.cake/hooks/herdr-cake-report.sh" session`. Command strings run through `sh -c` with the project root as the working directory.

## Verify

1. Open a Herdr pane and run a Cake turn, such as `cake "run sleep 5"`.
2. In another shell, `herdr agent list` should show the pane with `agent: cake`, moving `working` to `idle` and then `done` once the process exits.
3. `herdr agent explain <pane>` reports screen-manifest detection only; a custom source does not appear there.

If no state appears, re-run with `HERDR_CAKE_LOG` set and confirm the pane exports `HERDR_ENV=1` and `HERDR_PANE_ID`.

## Limitations

- **No exit event.** Cake has no `SessionEnd` hook, so the reporter cannot call `herdr pane release-agent` on process exit (tracked in [#542](https://github.com/travisennis/cake/issues/542)). Herdr marks the pane `done` once the one-shot Cake process exits, so stale `working` state is only possible on an abnormal exit that skips both `Stop` and `ErrorOccurred`.
- **No automatic session restore.** Herdr accepts `report-agent-session` for an unrecognized agent but does not surface or resume the reference; adding a new agent needs a Herdr binary update. The reported identity is still readable through Herdr's pane and agent APIs.
- **One-shot agent.** Cake runs one agent turn per process, so `idle` means the process is about to exit, not that it is waiting for the next prompt.
- **`blocked` is unreachable.** Cake has no mid-turn user decision, so only `working` and `idle` are ever reported.

## References

- [Integration contracts](../integrations.md) owns Cake's hook protocol and payload fields.
- [Configuration](../configuration.md) owns hook precedence and the `[limits]` hook caps.
- [Herdr custom integration guide](https://herdr.dev/docs/integrations/#integrate-your-own-agent).
