"""Storefront app."""

from lib import format_price


def render(price_cents):
    return format_price(price_cents)
