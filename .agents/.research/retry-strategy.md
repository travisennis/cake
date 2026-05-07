# API Retry Strategy Specification

This document describes the comprehensive retry strategy for API operations. Each strategy addresses specific failure modes observed in production environments. Implementations should adapt these patterns to their target language and framework while preserving the semantic behavior.

## Overview

The retry system is built around these principles:
- **Error classification**: Different HTTP status codes and error types trigger different recovery paths
- **Context-aware decisions**: Retry behavior varies based on request source (foreground vs background)
- **State machine approach**: The system tracks state (consecutive errors, fast mode status, backoff timing) across attempts
- **Graceful degradation**: Fallback options (alternate models, reduced features) when primary path fails

## Core Configuration Constants

| Constant | Value | Description |
|----------|-------|-------------|
| DEFAULT_MAX_RETRIES | 10 | Base retry limit for standard operations |
| FLOOR_OUTPUT_TOKENS | 3000 | Minimum token budget for context overflow recovery |
| MAX_529_RETRIES | 3 | Consecutive 529 errors before triggering fallback |
| BASE_DELAY_MS | 500 | Starting delay for exponential backoff |
| PERSISTENT_MAX_BACKOFF_MS | 300000 (5 min) | Maximum backoff in persistent/unattended mode |
| PERSISTENT_RESET_CAP_MS | 21600000 (6 hours) | Absolute upper bound on wait time |
| HEARTBEAT_INTERVAL_MS | 30000 (30 sec) | Interval for keep-alive emissions during long waits |
| SHORT_RETRY_THRESHOLD_MS | 20000 (20 sec) | Threshold for fast mode short vs long retry decision |
| MIN_COOLDOWN_MS | 600000 (10 min) | Minimum cooldown duration for fast mode fallback |
| DEFAULT_FAST_MODE_FALLBACK_HOLD_MS | 1800000 (30 min) | Default cooldown when retry-after is unknown |

## Error Classification and Handling

### 1. Rate Limited (429)

#### Standard Mode
- Parse `Retry-After` header for wait duration
- Calculate delay using backoff formula
- Skip retry for subscription users (Max/Pro) unless enterprise

#### Fast Mode Integration
Fast mode is a high-throughput feature that can be temporarily or permanently disabled based on rate limit responses.

**Short Retry Path** (Retry-After < 20 seconds):
- Wait for specified duration
- Retry with fast mode still active
- Preserves prompt cache (same model name)

**Long Retry Path** (Retry-After >= 20 seconds or unknown):
- Enter cooldown period
- Cooldown duration = max(Retry-After, 10 minutes minimum, 30 minutes default)
- Disable fast mode for subsequent requests
- Cooldown reason tracked as 'rate_limit' or 'overloaded'

**Permanent Disable Path**:
- Check for `anthropic-ratelimit-unified-overage-disabled-reason` header
- If present, permanently disable fast mode
- Display explanation to user about why overage is unavailable

#### Persistent/Unattended Mode
When `PERSISTENT_RETRY_ENABLED` (environment-based flag):
- Retry 429 errors indefinitely
- Wait until `anthropic-ratelimit-unified-reset` timestamp if available (window-based limits)
- Otherwise use exponential backoff capped at 5 minutes
- Emit heartbeat messages every 30 seconds during long waits
- Clamp total wait at 6 hours

### 2. Server Overloaded (529)

#### Foreground vs Background Distinction
The system maintains a set of foreground query sources where users block on results. Background operations bail immediately to prevent cascade amplification during capacity crises.

**Foreground Sources** (retry allowed):
- Main thread operations
- SDK requests
- Agent operations (custom, default, builtin)
- Compact operations
- Hook agents and prompts
- Verification agents
- Side questions
- Security classifiers (auto_mode, bash_classifier)

**Background Sources** (bail immediately):
- Summaries
- Titles
- Suggestions
- Classifiers (unless security-related)
- Any untagged/undefined source defaults to retry (conservative)

#### Consecutive Error Tracking
Track `consecutive529Errors` counter across attempts:
- Increment on each 529 for models eligible for fallback
- Reset on successful request
- When counter >= 3:
  - If fallback model specified: trigger model fallback
  - If external user without fallback: throw overloaded error with user-facing message

#### Model Fallback Logic
Fallback eligibility:
- Primary model is non-custom Opus, OR
- `FALLBACK_FOR_ALL_PRIMARY_MODELS` environment flag is set
- User is not a ClaudeAI subscriber (subscribers get priority capacity)

On fallback trigger:
- Log fallback event with original and fallback model names
- Throw `FallbackTriggeredError` to signal upstream to switch models
- Preserve `consecutive529Errors` count in fallback context to prevent double-counting

#### Persistent Mode Behavior
- Retry 529 errors indefinitely
- Use exponential backoff with 5-minute cap
- Emit heartbeat messages during waits
- Continue retrying even after max retries exceeded

### 3. Context Overflow (400)

This handles the error: "input length and `max_tokens` exceed context limit: {inputTokens} + {maxTokens} > {contextLimit}"

#### Recovery Algorithm
1. Parse error message to extract:
   - `inputTokens`: Actual input token count
   - `maxTokens`: Requested max_tokens value  
   - `contextLimit`: Model's context window limit

2. Calculate adjusted budget:
   ```
   safetyBuffer = 1000
   availableContext = max(0, contextLimit - inputTokens - safetyBuffer)
   ```

3. Validate floor constraint:
   - If `availableContext < FLOOR_OUTPUT_TOKENS` (3000): throw error (cannot satisfy minimum)

4. Calculate minimum required tokens:
   ```
   minRequired = (thinkingEnabled ? thinkingBudgetTokens : 0) + 1
   ```

5. Determine new max_tokens:
   ```
   adjustedMaxTokens = max(FLOOR_OUTPUT_TOKENS, availableContext, minRequired)
   ```

6. Store `maxTokensOverride` in retry context and retry operation

7. Log adjustment event with input tokens, limit, adjusted max, and attempt number

### 4. Authentication Errors (401/403)

#### 401 Unauthorized
- Clear API key cache
- Force-refresh OAuth tokens if using OAuth
- Retry with fresh credentials
- New client instance created for retry attempt

#### 403 Forbidden - OAuth Token Revoked
- Detect via message: "OAuth token has been revoked"
- Force token refresh via `handleOAuth401Error`
- Retry with new credentials

#### Cloud Provider Auth (AWS/GCP)
- AWS: Detect `CredentialsProviderError` or 403 "security token invalid"
- GCP: Detect "Could not load default credentials", "Could not refresh access token", "invalid_grant", or 401
- Clear respective credential caches
- Allow retry (fresh credentials fetched on next attempt)

#### Claude Code Remote Mode (CCR)
- In remote/containerized environments, 401/403 treated as transient
- Auth via infrastructure JWTs, so errors are infrastructure blips not credential issues
- Bypass `x-should-retry: false` header for 401/403

### 5. Network Errors

#### Stale Connection Detection
Detect connection errors via:
- Error type: `APIConnectionError`
- Error codes: `ECONNRESET`, `EPIPE`

#### Recovery
- Disable keep-alive socket pooling via `disableKeepAlive()`
- Create fresh client instance without connection reuse
- Retry with new connection

## Backoff Strategy

### Standard Exponential Backoff
```
delay = min(BASE_DELAY_MS * 2^(attempt - 1), maxDelayMs) + jitter

where:
- BASE_DELAY_MS = 500
- maxDelayMs defaults to 32000 (32 seconds)
- jitter = random(0, 0.25 * baseDelay)
```

### Retry-After Header Override
If `Retry-After` header present:
- Parse as integer seconds
- Use `seconds * 1000` as delay (ignores exponential formula)
- No jitter applied to header-based delays

### Persistent Mode Backoff
```
delay = min(BASE_DELAY_MS * 2^(persistentAttempt - 1), PERSISTENT_MAX_BACKOFF_MS)

with reset timestamp optimization:
- Check `anthropic-ratelimit-unified-reset` header
- If present and valid: delay = resetTimestamp - now
- Clamp delay at PERSISTENT_RESET_CAP_MS (6 hours)
```

### Heartbeat Emission (Persistent Mode)
For delays > 60 seconds in persistent mode:
- Chunk sleep into 30-second intervals
- Yield status message after each chunk: `{type: 'system', subtype: 'api_retry', remainingMs, attempt, maxRetries}`
- Check abort signal between chunks
- Prevents host environment from marking session idle

## Retry Decision Matrix

The `shouldRetry` function implements the following logic:

| Error Type | Condition | Retry? | Notes |
|------------|-----------|--------|-------|
| Mock errors | Always | No | Testing harness errors |
| 429/529 | Persistent mode enabled | Yes | Bypasses all other checks |
| 401/403 | CCR mode enabled | Yes | Infrastructure auth, not user |
| Overloaded | Message contains `"type":"overloaded_error"` | Yes | SDK sometimes loses 529 status |
| Context overflow | Parseable 400 error | Yes | Will adjust tokens and retry |
| x-should-retry: true | Enterprise subscriber | Yes | Max/Pro users check timing |
| x-should-retry: false | Non-5xx or non-ant user | No | Ant users ignore for 5xx |
| Connection errors | APIConnectionError | Yes | Network layer issues |
| Timeout | Status 408 | Yes | Request timeout |
| Lock timeout | Status 409 | Yes | Concurrent access conflict |
| Rate limit | Status 429, non-subscriber | Yes | Subscriber check applies |
| Auth | Status 401 | Yes | After cache clear |
| Token revoked | Status 403, specific message | Yes | OAuth refresh |
| Server error | Status >= 500 | Yes | Internal server errors |

## Streaming Reliability Layer

The streaming implementation has additional reliability mechanisms:

### Idle Timeout Watchdog
- **Warning threshold**: 45 seconds without chunks
- **Abort threshold**: 90 seconds without chunks
- Abort streaming request if threshold exceeded

### Stall Detection
- Track time-to-first-byte (TTFB)
- Log gaps of 30+ seconds between consecutive chunks after TTFB
- Helps diagnose intermittent streaming issues

### Streaming Fallback
If streaming request fails:
- Switch to non-streaming request
- Preserve `consecutive529Errors` count in fallback options
- Prevents double-counting 529 errors across mode switch

## State Management

### Retry Context
Maintain context object across attempts:
```typescript
interface RetryContext {
  maxTokensOverride?: number  // Adjusted after context overflow
  model: string               // Current model (may change on fallback)
  thinkingConfig: object      // Thinking configuration
  fastMode?: boolean          // Fast mode state (may be disabled)
}
```

### Error Tracking
- `consecutive529Errors`: Count of consecutive 529 errors
- `persistentAttempt`: Separate counter for persistent mode (independent of loop counter)
- `lastError`: Most recent error for diagnostic purposes

### Fast Mode State
- Track whether fast mode was active before each attempt
- Handle mid-loop state changes (fallback may disable fast mode)
- Cooldown tracked via timestamp + reason

## Integration Points

### Model Fallback Protocol
When `FallbackTriggeredError` thrown:
- Upstream catches special error type
- Switches to fallback model
- Retries entire operation with new model
- Preserves all other context (thinking, max tokens, etc.)

### Token Refresh Integration
- OAuth token refresh triggered on 401/revoked errors
- AWS/GCP credential cache clearing on cloud auth errors
- New client instance created after auth recovery

### Analytics/Logging
Log events for:
- Retry attempts (attempt number, delay, error message, status)
- Persistent retry long waits (>60s)
- Model fallback triggers
- 529 background drops
- Max token context overflow adjustments
- Fast mode state changes

## Implementation Guidelines

### For Other Languages/Frameworks

1. **Preserve error classification**: Map your HTTP client's error types to the categories above (429, 529, 401, connection errors, etc.)

2. **Maintain state across retries**: Use a context object or closure to track:
   - Attempt counters (both regular and persistent)
   - Consecutive error counters  
   - Modified parameters (max tokens, model)
   - Fast mode state

3. **Header inspection**: Access response headers for:
   - `Retry-After`: Delay directive
   - `x-should-retry`: Server retry recommendation
   - `anthropic-ratelimit-unified-reset`: Rate limit window reset time
   - `anthropic-ratelimit-unified-overage-disabled-reason`: Fast mode disable reason

4. **Async generator pattern**: The original uses async generators to yield status messages. In other languages, consider:
   - Callbacks for status updates
   - Event emitters
   - Stream-based status channels
   - Return status objects with partial results

5. **Signal/abort handling**: Respect abort signals/cancellation tokens:
   - Check before each retry attempt
   - Check between heartbeat chunks in persistent mode
   - Throw appropriate abort error type

6. **Jitter implementation**: Use random jitter to prevent thundering herd:
   - 0-25% of base delay
   - Applied only to calculated delays, not header-based delays

7. **Logging hooks**: Provide hooks for:
   - Retry attempts
   - Error classification
   - State transitions
   - Fallback triggers

8. **Environment-based features**: Support feature flags via environment:
   - Persistent retry mode
   - Fallback model eligibility
   - Cloud provider toggles
   - User type (ant, external, enterprise)

## Error Messages

### User-Facing Messages
- Repeated 529 error: Custom message for external users when capacity unavailable

### Debug Logging
Include in debug output:
- Attempt number and max retries
- Error status and message
- Retry delay
- Fast mode state
- Model being used

## Testing Considerations

1. **Mock rate limits**: Support injecting mock errors for testing (ant-only feature)
2. **Error message parsing**: Unit tests for regex-based error parsing (context overflow)
3. **Backoff calculation**: Verify exponential growth and capping
4. **State transitions**: Test fast mode enable/disable paths
5. **Fallback triggering**: Mock consecutive 529s to test fallback logic
