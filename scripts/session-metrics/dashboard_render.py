"""Server-side HTML rendering for the session-metrics dashboard.

The page is a static file opened from disk. It contains no script, no external
asset, and no network request. Tables sit inside scrollable containers; the
report applies its configurable detail limit before rendering.
"""

from __future__ import annotations

import re
from html import escape
from pathlib import Path


_PLACEHOLDER = re.compile(r"__(TITLE|SUBTITLE|NAV|TOC|BODY|FOOTER)__")


def _cell(value) -> str:
    if value is None:
        return ""
    text = str(value)
    css = ' class="wrap"' if "\n" in text else ""
    return f"<td{css}>{escape(text)}</td>"


def _header(value: str) -> str:
    return f"<th>{escape(str(value))}</th>"


def render_table(table: dict) -> str:
    headers = table.get("headers") or []
    rows = table.get("rows") or []
    numeric = set(table.get("numeric") or [])
    title = table.get("title")
    note = table.get("note")
    parts = ['<div class="table-block">']
    if title:
        parts.append(f"<h3>{escape(title)}</h3>")
    if not rows:
        rows = [["(no data)"] + [""] * max(0, len(headers) - 1)]
    parts.append('<div class="scroll"><table>')
    if title:
        parts.append(f"<caption>{escape(title)}</caption>")
    parts.append("<thead><tr>")
    parts.extend(_header(header) for header in headers)
    parts.append("</tr></thead><tbody>")
    for row in rows:
        parts.append("<tr>")
        for index, value in enumerate(row):
            if index in numeric:
                text = escape("" if value is None else str(value))
                parts.append(f'<td class="num">{text}</td>')
            else:
                parts.append(_cell(value))
        parts.append("</tr>")
    parts.append("</tbody></table></div>")
    if note:
        parts.append(f'<p class="note">{escape(note)}</p>')
    parts.append("</div>")
    return "".join(parts)


def render_cards(cards: list[dict]) -> str:
    if not cards:
        return ""
    parts = ['<div class="cards">']
    for card in cards:
        parts.append(
            '<div class="card">'
            f'<div class="card-label">{escape(str(card.get("label", "")))}</div>'
            f'<div class="card-value">{escape(str(card.get("value", "")))}</div>'
            f'<div class="card-detail">{escape(str(card.get("detail", "")))}</div>'
            "</div>"
        )
    parts.append("</div>")
    return "".join(parts)


def render_section(section: dict) -> str:
    parts = [f'<section id="{escape(section["id"])}">',
             f'<h2>{escape(section["title"])}</h2>']
    if section.get("description"):
        parts.append(f'<p class="description">{escape(section["description"])}</p>')
    parts.append(render_cards(section.get("cards") or []))
    for table in section.get("tables") or []:
        parts.append(render_table(table))
    for note in section.get("notes") or []:
        parts.append(f'<p class="note">{escape(note)}</p>')
    parts.append("</section>")
    return "".join(parts)


def render_toc(sections: list[dict]) -> str:
    links = "".join(
        f'<a href="#{escape(section["id"])}">{escape(section["title"])}</a>'
        for section in sections
    )
    return f'<nav class="toc" aria-label="Sections">{links}</nav>'


def _render_filter_links(group: dict, current: str) -> str:
    links = []
    for link in group.get("links") or []:
        active = ' class="active"' if link.get("href") == current else ""
        links.append(f'<a href="{escape(link["href"])}"{active}>{escape(link["label"])}</a>')
    return " ".join(links)


def render_filter_nav(groups: list[dict], current: str) -> str:
    parts = ['<nav class="filters" aria-label="Filters">']
    for group in groups:
        links = _render_filter_links(group, current)
        if group.get("collapsible"):
            parts.append(
                '<details class="filter-group">'
                f'<summary class="filter-label">{escape(group["label"])} '
                f'({len(group.get("links") or [])})</summary>'
                f'<div class="filter-links">{links}</div>'
                '</details>'
            )
        else:
            parts.append(
                f'<div class="filter-group"><span class="filter-label">{escape(group["label"])}</span>'
                f'{links}</div>'
            )
    parts.append("</nav>")
    return "".join(parts)


def render_page(*, title: str, subtitle: str, nav: str, sections: list[dict],
                footer: str) -> str:
    template = (Path(__file__).with_name("dashboard.html")).read_text(encoding="utf-8")
    body = "".join(render_section(section) for section in sections)
    toc = render_toc(sections)
    values = {
        "TITLE": escape(title),
        "SUBTITLE": escape(subtitle),
        "NAV": nav,
        "TOC": toc,
        "BODY": body,
        "FOOTER": escape(footer),
    }
    return _PLACEHOLDER.sub(lambda match: values[match.group(1)], template)
