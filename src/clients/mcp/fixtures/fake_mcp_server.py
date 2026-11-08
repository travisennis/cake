#!/usr/bin/env python3
"""Minimal MCP stdio server used by Cake's client tests.

Set FAKE_MCP_ERA to "legacy" or "modern". The server answers discovery,
initialization, two pages of tools/list, and a small set of tool calls. It is
deliberately dependency-free so the test suite needs only python3.
"""

import json
import os
import sys
import time

ERA = os.environ.get("FAKE_MCP_ERA", "legacy")
PNG = (
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8"
    "/x8AAusB9Y9ZQmcAAAAASUVORK5CYII="
)

TOOLS_PAGE_1 = [
    {
        "name": "echo",
        "description": "Echo a message. First line.\nSecond line.",
        "inputSchema": {
            "type": "object",
            "properties": {"message": {"type": "string"}},
            "required": ["message"],
        },
        "outputSchema": {"type": "object"},
    },
    {
        "name": "get.page",
        "description": "Return a page",
        "inputSchema": {"type": "object", "properties": {}},
    },
]

TOOLS_PAGE_2 = [
    {
        "name": "slow",
        "description": "Wait longer than the configured timeout",
        "inputSchema": {"type": "object", "properties": {}},
    },
    {
        "name": "fail",
        "description": "Return an isError result",
        "inputSchema": {"type": "object", "properties": {}},
    },
]


def send(message):
    sys.stdout.write(json.dumps(message, separators=(",", ":")) + "\n")
    sys.stdout.flush()


def result(request_id, value):
    send({"jsonrpc": "2.0", "id": request_id, "result": value})


def error(request_id, code, message):
    send({"jsonrpc": "2.0", "id": request_id, "error": {"code": code, "message": message}})


def modern_meta(params):
    return (params or {}).get("_meta", {}).get("io.modelcontextprotocol/protocolVersion")


def handle(message):
    method = message.get("method")
    request_id = message.get("id")
    params = message.get("params") or {}

    if method == "server/discover":
        if ERA == "modern":
            result(
                request_id,
                {
                    "resultType": "complete",
                    "supportedVersions": ["2026-07-28"],
                    "capabilities": {"tools": {}},
                    "_meta": {
                        "io.modelcontextprotocol/serverInfo": {
                            "name": "fake-modern",
                            "version": "1.0.0",
                        }
                    },
                },
            )
        else:
            error(request_id, -32601, "Method not found")
        return

    if method == "initialize":
        result(
            request_id,
            {
                "protocolVersion": "2025-11-25",
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "fake-legacy", "version": "1.0.0"},
            },
        )
        return

    if method == "notifications/initialized":
        return

    if method == "tools/list":
        if ERA == "modern" and modern_meta(params) != "2026-07-28":
            error(request_id, -32602, "missing or wrong protocol metadata")
            return
        if ERA == "legacy" and modern_meta(params) is not None:
            error(request_id, -32602, "legacy server received modern metadata")
            return
        if params.get("cursor") == "page-2":
            result(request_id, {"resultType": "complete", "tools": TOOLS_PAGE_2})
        else:
            result(
                request_id,
                {
                    "resultType": "complete",
                    "tools": TOOLS_PAGE_1,
                    "nextCursor": "page-2",
                },
            )
        return

    if method == "tools/call":
        if ERA == "modern" and modern_meta(params) != "2026-07-28":
            error(request_id, -32602, "missing or wrong protocol metadata")
            return
        name = params.get("name")
        arguments = params.get("arguments") or {}
        if name == "slow":
            time.sleep(5)
            result(request_id, {"content": [{"type": "text", "text": "finally"}]})
        elif name == "fail":
            result(
                request_id,
                {
                    "content": [{"type": "text", "text": "deliberate failure"}],
                    "isError": True,
                },
            )
        elif name == "get.page":
            result(
                request_id,
                {
                    "content": [
                        {
                            "type": "resource",
                            "resource": {
                                "uri": "file:///tmp/page.txt",
                                "mimeType": "text/plain",
                                "text": "resource body",
                            },
                        },
                        {
                            "type": "resource_link",
                            "uri": "file:///tmp/other.txt",
                            "name": "other",
                        },
                    ]
                },
            )
        else:
            message_text = arguments.get("message", "")
            result(
                request_id,
                {
                    "content": [
                        {"type": "text", "text": "echo: " + message_text},
                        {"type": "image", "data": PNG, "mimeType": "image/png"},
                        {"type": "audio", "data": "AAAA", "mimeType": "audio/wav"},
                    ],
                    "structuredContent": {"message": message_text},
                    "isError": False,
                },
            )
        return

    if method == "notifications/cancelled":
        return

    if request_id is not None:
        error(request_id, -32601, "Method not found")


def main():
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        try:
            message = json.loads(line)
        except json.JSONDecodeError:
            continue
        handle(message)


if __name__ == "__main__":
    main()
