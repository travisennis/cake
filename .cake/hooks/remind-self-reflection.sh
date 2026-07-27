#!/bin/sh
#
# Remind the agent to record mistakes, learnings, and tool/context gaps
# at the end of each task.  This hook fires on SessionStart so the
# reminder appears as developer context once per session (including
# resume and fork).
#
# Input:  JSON payload on stdin (HookRecord + event-specific fields)
# Output: JSON object with an "additional_context" string (exit 0)

echo '{"additional_context":"When finishing a task, remember to record mistakes in ~/.local/share/cake/MISTAKES.md, learnings about the environment or tooling in ~/.local/share/cake/LEARNINGS.md, and tool or context gaps in ~/.local/share/cake/DESIRES.md. Each entry must include the working directory and date."}'
