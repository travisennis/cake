#!/usr/bin/env python3
"""Combined report: runs every metric module against a single load of the data.

Usage:
  python3 report.py                 # last 30 days
  python3 report.py --days 0        # all time
  python3 report.py --model deepseek --project cake
"""

import api
import cache_breaks
import cakelib
import compensations
import hooks
import judge
import outcomes
import overview
import time_breakdown
import tokens
import tools


def main() -> None:
    ns = cakelib.build_arg_parser(__doc__).parse_args()
    data = cakelib.load(ns)
    for module in (
        overview,
        tokens,
        cache_breaks,
        tools,
        api,
        judge,
        time_breakdown,
        outcomes,
        hooks,
        compensations,
    ):
        module.run(data)
    print()


if __name__ == "__main__":
    main()
