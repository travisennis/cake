# GLM-5.1 Model Error Investigation

## Summary

The default `glm-5.1` model fails when used through acai but works correctly when the same request is sent via curl. The server returns a 500 Internal Server Error with a JavaScript error message.

## Error from Acai

```
Error: glm-5.1

{
  "error": {
    "message": "Cannot read properties of undefined (reading 'prompt_tokens')",
    "type": "error"
  },
  "type": "error"
}
```

The server returns HTTP 500 with this JSON body. The error message indicates a server-side JavaScript error where code is trying to access `prompt_tokens` on an undefined object.

## Acai Configuration

**Default settings (`src/config/defaults.rs`):**
- Model: `glm-5.1`
- Base URL: `https://opencode.ai/zen/go/v1/`
- API Key Env: `OPENCODE_ZEN_API_TOKEN`
- Temperature: `0.8`
- Max Output Tokens: `8000`

## Working Curl Commands

### Simple Request (Works)

```bash
curl -s -X POST "https://opencode.ai/zen/go/v1/chat/completions" \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer $OPENCODE_ZEN_API_TOKEN" \
  -H "HTTP-Referer: https://github.com/travisennis/acai" \
  -H "X-Title: acai" \
  -d '{
    "model": "glm-5.1",
    "messages": [{"role": "user", "content": "What is 2+2?"}],
    "temperature": 0.8,
    "max_completion_tokens": 8000
  }'
```

**Response:**
```json
{"choices":[{"finish_reason":"stop","index":0,"message":{"content":"2 + 2 = 4","reasoning_content":"1.  **Identify the core question:** The user is asking for the sum of 2 and 2.\n2.  **Perform the calculation:** 2 + 2 = 4.\n3.  **Formulate the response:** State the answer clearly and concisely. (e.g., \"4\", \"2 + 2 = 4\", \"The answer is 4.\")\n4.  **Select the best response:** \"4\" or \"2 + 2 = 4\" are both perfectly acceptable and direct. I'll go with a simple, direct answer.","role":"assistant"}}],"created":1775612809,"id":"20260408094646672fd5dd85b2447f","model":"GLM-5.1","object":"chat.completion","request_id":"20260408094646672fd5dd85b2447f","usage":{"completion_tokens":130,"completion_tokens_details":{"reasoning_tokens":121},"prompt_tokens":12,"prompt_tokens_details":{"cached_tokens":0},"total_tokens":142},"cost":"0"}
```

### With Tools (Works)

```bash
curl -s -X POST "https://opencode.ai/zen/go/v1/chat/completions" \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer $OPENCODE_ZEN_API_TOKEN" \
  -H "HTTP-Referer: https://github.com/travisennis/acai" \
  -H "X-Title: acai" \
  -d '{
    "model": "glm-5.1",
    "messages": [
      {"role": "system", "content": "You are a helpful assistant."},
      {"role": "user", "content": "What is 2+2?"}
    ],
    "temperature": 0.8,
    "max_completion_tokens": 8000,
    "tools": [{"type": "function", "function": {"name": "test", "description": "A test function", "parameters": {"type": "object", "properties": {}}}}],
    "tool_choice": "auto"
  }'
```

**Response:**
```json
{"choices":[{"finish_reason":"stop","index":0,"message":{"content":"2 + 2 = **4**","reasoning_content":"The user is asking a simple math question. No tools needed.","role":"assistant"}}],"created":1775613013,"id":"20260408095011da5c088352ff4118","model":"GLM-5.1","object":"chat.completion","request_id":"20260408095011da5c088352ff4118","usage":{"completion_tokens":4,"completion_tokens_details":{"reasoning_tokens":1},"prompt_tokens":481,"prompt_tokens_details":{"cached_tokens":0},"total_tokens":485},"cost":"0"}
```

### Full Acai Request Body (Works via Curl)

```bash
curl -s -X POST "https://opencode.ai/zen/go/v1/chat/completions" \
  -H "Content-Type: application/json" \
  -H "Authorization: Bearer $OPENCODE_ZEN_API_TOKEN" \
  -H "HTTP-Referer: https://github.com/travisennis/acai" \
  -H "X-Title: acai" \
  -d @/tmp/request.json
```

**Response:**
```json
{"choices":[{"finish_reason":"stop","index":0,"message":{"content":"4","reasoning_content":"4","role":"assistant"}}],"created":1775613130,"id":"202604080952084c2579e0f74b4286","model":"GLM-5.1","object":"chat.completion","request_id":"202604080952084c2579e0f74b4286","usage":{"completion_tokens":7,"completion_tokens_details":{"reasoning_tokens":4},"prompt_tokens":1910,"prompt_tokens_details":{"cached_tokens":0},"total_tokens":1917},"cost":"0"}
```

## Request JSON from Acai

The exact JSON body sent by acai (captured from trace logs):

```json
{
  "model": "glm-5.1",
  "messages": [
    {
      "role": "system",
      "content": "You are acai. You are running as a coding agent in a CLI on the user's computer.\n\n## Project Context:..."
    },
    {
      "role": "user",
      "content": "What is 2+2?"
    }
  ],
  "temperature": 0.8,
  "max_completion_tokens": 8000,
  "tools": [
    {
      "type": "function",
      "function": {
        "name": "Bash",
        "description": "Execute a shell command...",
        "parameters": {...}
      }
    },
    ...
  ],
  "tool_choice": "auto"
}
```

## Investigation Findings

### What Works
- All curl variations work consistently
- HTTP/2 via curl works
- HTTP/1.1 via curl works
- Large system prompts work
- Tools array works
- Same exact JSON body works via curl

### What Fails
- Acai using reqwest HTTP client fails consistently
- Fails with HTTP/2 (default)
- Fails with HTTP/1.1 (forced via `.http1_only()`)
- Fails regardless of User-Agent header
- Fails regardless of Accept header

### HTTP Headers Comparison

**Curl sends:**
```
User-Agent: curl/8.7.1
Accept: */*
Content-Type: application/json
Authorization: Bearer sk-...
HTTP-Referer: https://github.com/travisennis/acai
X-Title: acai
```

**Acai (reqwest) sends:**
```
Content-Type: application/json
Authorization: Bearer sk-...
HTTP-Referer: https://github.com/travisennis/acai
X-Title: acai
User-Agent: acai
Accept: */*
```

### Server Behavior

The server returns:
- HTTP 500 Internal Server Error
- Response body contains JavaScript error JSON

The error `"Cannot read properties of undefined (reading 'prompt_tokens')"` is a JavaScript TypeError indicating the server-side code is trying to access `prompt_tokens` on an undefined object.

### Log Evidence

From `~/.cache/acai/acai.2026-04-08.log`:

```
API request failed with status 500 Internal Server Error, retrying in 1s (attempt 1/3)
API request failed with status 500 Internal Server Error, retrying in 2s (attempt 2/3)
API request failed with status 500 Internal Server Error, retrying in 4s (attempt 3/3)
```

Final error logged:
```
{"type":"error","error":{"type":"error","message":"Cannot read properties of undefined (reading 'prompt_tokens')"}}
```

## Possible Causes

1. **Server-side bug**: The API server may have a bug that's triggered by specific request patterns from reqwest but not curl
2. **HTTP/2 framing difference**: Subtle differences in how reqwest frames HTTP/2 requests vs curl
3. **Connection state**: Reqwest may be reusing connections in a way that causes server issues
4. **Timing/race condition**: Server may have intermittent issues

## Code References

- Default model: `src/config/defaults.rs:2`
- Request building: `src/clients/chat_completions.rs:send_request()`
- HTTP client: `src/clients/agent.rs:Agent::new()` (creates reqwest::Client)
- Error handling: `src/clients/agent.rs:complete_turn()`

## Next Steps

1. Consider trying a different model as the default (one that works reliably)
2. Report issue to the API provider (opencode.ai)
3. Add more detailed error logging to capture the full HTTP response
4. Consider adding retry logic with different connection settings