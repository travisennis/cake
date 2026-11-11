#!/usr/bin/env python3
"""Minimal MCP stdio server used by Cake's client tests.

Set FAKE_MCP_ERA to "legacy" or "modern". The server answers discovery,
initialization, two pages of tools/list, and a small set of tool calls. Set
FAKE_MCP_DYNAMIC to "1" to advertise tools.listChanged, accept a
subscriptions/listen stream in the modern era, and expose a mutate tool that
changes the list and notifies the client. It is deliberately dependency-free
so the test suite needs only python3.
"""

import json
import os
import sys
import time

ERA = os.environ.get("FAKE_MCP_ERA", "legacy")
DYNAMIC = os.environ.get("FAKE_MCP_DYNAMIC") == "1"
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

MUTATE_TOOL = {
    "name": "mutate",
    "description": "Change the tool list and notify the client",
    "inputSchema": {"type": "object", "properties": {}},
}

ADDED_TOOL = {
    "name": "added",
    "description": "A tool that appears after a list change",
    "inputSchema": {"type": "object", "properties": {}},
}

ECHO_AFTER_CHANGE = dict(
    TOOLS_PAGE_1[0],
    description="Echo a message after the change. First line.\nSecond line.",
)

STATE = {"mutated": False, "subscription_id": None}


def capabilities():
    return {"tools": {"listChanged": DYNAMIC}}


def tools_for_page(page):
    if page == 1:
        echo = ECHO_AFTER_CHANGE if DYNAMIC and STATE["mutated"] else TOOLS_PAGE_1[0]
        return [echo, TOOLS_PAGE_1[1]]
    tools = list(TOOLS_PAGE_2)
    if DYNAMIC:
        if STATE["mutated"]:
            tools = [tool for tool in tools if tool["name"] != "fail"]
            tools.append(ADDED_TOOL)
        else:
            tools.append(MUTATE_TOOL)
    return tools


def send(message):
    sys.stdout.write(json.dumps(message, separators=(",", ":")) + "\n")
    sys.stdout.flush()


def notify(method, params=None):
    message = {"jsonrpc": "2.0", "method": method}
    if params is not None:
        message["params"] = params
    send(message)


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
                    "capabilities": capabilities(),
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
                "capabilities": capabilities(),
                "serverInfo": {"name": "fake-legacy", "version": "1.0.0"},
            },
        )
        return

    if method == "notifications/initialized":
        return

    if method == "subscriptions/listen":
        if ERA != "modern":
            error(request_id, -32601, "Method not found")
            return
        if modern_meta(params) != "2026-07-28":
            error(request_id, -32602, "missing or wrong protocol metadata")
            return
        STATE["subscription_id"] = request_id
        notify(
            "notifications/subscriptions/acknowledged",
            {
                "_meta": {"io.modelcontextprotocol/subscriptionId": request_id},
                "notifications": {"toolsListChanged": True},
            },
        )
        # The subscription has no response until it closes; keep the stream
        # open without sending a result.
        return

    if method == "tools/list":
        if ERA == "modern" and modern_meta(params) != "2026-07-28":
            error(request_id, -32602, "missing or wrong protocol metadata")
            return
        if ERA == "legacy" and modern_meta(params) is not None:
            error(request_id, -32602, "legacy server received modern metadata")
            return
        if params.get("cursor") == "page-2":
            result(request_id, {"resultType": "complete", "tools": tools_for_page(2)})
        else:
            result(
                request_id,
                {
                    "resultType": "complete",
                    "tools": tools_for_page(1),
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
        elif name == "mutate" and DYNAMIC:
            STATE["mutated"] = True
            notification = None
            if ERA == "modern" and STATE["subscription_id"] is not None:
                notification = {
                    "_meta": {
                        "io.modelcontextprotocol/subscriptionId": STATE["subscription_id"],
                    }
                }
            notify("notifications/tools/list_changed", notification)
            result(request_id, {"content": [{"type": "text", "text": "tools changed"}]})
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
        if params.get("requestId") == STATE["subscription_id"]:
            STATE["subscription_id"] = None
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
