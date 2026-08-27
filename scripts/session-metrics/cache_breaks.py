#!/usr/bin/env python3
"""Detect prompt-cache breaks from telemetry api_attempt usage.

The scan is intentionally diagnostic. It uses provider-reported token counts,
never reads prompts, and never writes to session or telemetry files.
"""

from __future__ import annotations

from collections import defaultdict
from dataclasses import dataclass, field
from datetime import datetime, timezone

import cakelib
from cakelib import fmt_int, fmt_ms, print_header, print_table


NOISE_FLOOR_TOKENS = 1_024
CACHE_TTL_MS = 5 * 60 * 1_000

CAUSE_MODEL_SWITCH = "model switch"
CAUSE_IDLE_TTL = "idle >= 5m"
CAUSE_GENERIC = "generic"


@dataclass(frozen=True)
class _Turn:
    session_id: str
    invocation_id: str
    turn_index: int
    model: str
    timestamp: datetime | None
    usage: dict
    sequence: int


@dataclass(frozen=True)
class CacheMiss:
    """One cache miss that exceeded the scanner's noise floor."""

    session_id: str
    invocation_id: str
    turn_index: int
    model: str
    timestamp: str | None
    missed_tokens: int
    idle_ms: int
    cause: str


@dataclass
class CacheBreakSummary:
    """Aggregate cache-break evidence for the selected telemetry window."""

    misses: list[CacheMiss] = field(default_factory=list)

    @property
    def missed_tokens(self) -> int:
        return sum(miss.missed_tokens for miss in self.misses)

    @property
    def miss_count(self) -> int:
        return len(self.misses)

    @property
    def missed_cost(self) -> None:
        """Dollar cost is unknown until a provider pricing source exists."""
        return None


def _token_count(value: object) -> int:
    return value if isinstance(value, int) and value >= 0 else 0


def _input_details(usage: dict) -> dict:
    details = usage.get("input_tokens_details")
    return details if isinstance(details, dict) else {}


def _terminal_turns(invocation: cakelib.Invocation, sequence_start: int) -> list[_Turn]:
    """Return one usage-bearing API attempt per invocation-local turn.

    Failed retries normally have no usage. Selecting the highest attempt that
    does have usage also handles a provider that reports usage for more than
    one attempt without counting a retry twice.
    """
    by_turn: dict[int, tuple[int, int, dict]] = {}
    sequence = sequence_start
    for attempt in invocation.attempts:
        usage = attempt.get("usage")
        turn_index = attempt.get("turn_index")
        if not isinstance(usage, dict) or not isinstance(turn_index, int):
            continue
        attempt_number = _token_count(attempt.get("attempt"))
        current = (attempt_number, sequence, attempt)
        previous = by_turn.get(turn_index)
        if previous is None or current[:2] >= previous[:2]:
            by_turn[turn_index] = current
        sequence += 1

    turns = []
    for turn_index, (_, sequence, attempt) in by_turn.items():
        turns.append(_Turn(
            session_id=invocation.session_id,
            invocation_id=invocation.invocation_id,
            turn_index=turn_index,
            model=invocation.model,
            timestamp=cakelib.parse_ts(attempt.get("timestamp")),
            usage=attempt["usage"],
            sequence=sequence,
        ))
    return turns


def _idle_ms(previous: _Turn, current: _Turn) -> int:
    if previous.timestamp is None or current.timestamp is None:
        return 0
    return max(0, int((current.timestamp - previous.timestamp).total_seconds() * 1_000))


def _cache_miss(previous: _Turn, current: _Turn, reported_cache: bool) -> CacheMiss | None:
    usage = current.usage
    prompt_tokens = _token_count(usage.get("input_tokens"))
    details = _input_details(usage)
    cached_tokens = _token_count(details.get("cached_tokens"))
    has_cache_activity = cached_tokens > 0 or _token_count(
        details.get("cache_write_tokens")
    ) > 0

    if prompt_tokens == 0 or (not has_cache_activity and not reported_cache):
        return None

    previous_prompt_tokens = _token_count(previous.usage.get("input_tokens"))
    missed_tokens = max(0, min(previous_prompt_tokens, prompt_tokens) - cached_tokens)
    if missed_tokens <= NOISE_FLOOR_TOKENS:
        return None

    idle_ms = _idle_ms(previous, current)
    if current.model != previous.model:
        cause = CAUSE_MODEL_SWITCH
    elif idle_ms >= CACHE_TTL_MS:
        cause = CAUSE_IDLE_TTL
    else:
        cause = CAUSE_GENERIC

    timestamp = current.timestamp.isoformat() if current.timestamp is not None else None
    return CacheMiss(
        session_id=current.session_id,
        invocation_id=current.invocation_id,
        turn_index=current.turn_index,
        model=current.model,
        timestamp=timestamp,
        missed_tokens=missed_tokens,
        idle_ms=idle_ms,
        cause=cause,
    )


def scan(invocations: list[cakelib.Invocation]) -> CacheBreakSummary:
    """Scan usage-bearing provider turns in chronological session order.

    The cache-reporting latch is per session. It prevents a provider that never
    reports cache activity from producing false misses, while still allowing a
    later full miss after an earlier cache read or write. Invocation boundaries
    do not reset the state because continue/resume can expose an idle-TTL break.
    """
    by_session: dict[str, list[_Turn]] = defaultdict(list)
    sequence = 0
    for invocation in invocations:
        turns = _terminal_turns(invocation, sequence)
        by_session[invocation.session_id].extend(turns)
        sequence += len(invocation.attempts)

    misses: list[CacheMiss] = []
    for turns in by_session.values():
        turns.sort(key=lambda turn: (
            turn.timestamp is None,
            turn.timestamp or datetime.max.replace(tzinfo=timezone.utc),
            turn.sequence,
        ))
        previous: _Turn | None = None
        reported_cache = False
        for current in turns:
            prompt_tokens = _token_count(current.usage.get("input_tokens"))
            if prompt_tokens == 0:
                continue

            details = _input_details(current.usage)
            cached_tokens = _token_count(details.get("cached_tokens"))
            cache_write_tokens = _token_count(details.get("cache_write_tokens"))
            has_cache_activity = cached_tokens > 0 or cache_write_tokens > 0

            if previous is not None:
                miss = _cache_miss(previous, current, reported_cache)
                if miss is not None:
                    misses.append(miss)

            previous = current
            reported_cache = reported_cache or has_cache_activity

    return CacheBreakSummary(misses=misses)


def _cause_rows(summary: CacheBreakSummary) -> list[list[str]]:
    grouped: dict[str, list[CacheMiss]] = defaultdict(list)
    for miss in summary.misses:
        grouped[miss.cause].append(miss)
    return [
        [cause, fmt_int(len(misses)), fmt_int(sum(m.missed_tokens for m in misses))]
        for cause, misses in sorted(grouped.items(), key=lambda item: -len(item[1]))
    ]


def run(data: cakelib.Dataset) -> None:
    print_header("PROMPT CACHE BREAKS")
    print(cakelib.describe_window(data))

    if not any(inv.attempts for inv in data.invocations):
        print("\nNo telemetry api_attempt records in window.")
        return

    summary = scan(data.invocations)
    print(f"\nDetected misses: {fmt_int(summary.miss_count)}")
    print(f"Missed prompt tokens: {fmt_int(summary.missed_tokens)}")
    if summary.misses:
        print("\nMisses by likely cause:")
        print_table(["cause", "misses", "missed tokens"], _cause_rows(summary))
    else:
        print("No cache breaks exceeded the 1,024-token noise floor.")

    if summary.missed_cost is None:
        print("\nWasted cost: unavailable (no provider pricing source)")
    else:
        print(f"\nWasted cost: ${summary.missed_cost:.2f}")
    if summary.misses:
        print("\nMiss details:")
        print_table(
            ["session", "model", "cause", "missed tokens", "idle"],
            [
                [
                    miss.session_id,
                    miss.model,
                    miss.cause,
                    fmt_int(miss.missed_tokens),
                    fmt_ms(miss.idle_ms),
                ]
                for miss in summary.misses
            ],
        )


def main() -> None:
    ns = cakelib.build_arg_parser(__doc__).parse_args()
    run(cakelib.load(ns))


if __name__ == "__main__":
    main()
