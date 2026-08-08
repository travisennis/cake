"""Sales report."""

from lib import format_price


def total_line(items):
    return format_price(sum(items))
