# Testing Follow-up: Error Path Coverage

## Overview

Issue #12 from the code review identified a gap in test coverage: the lack of tests for error paths in the API client code. This document outlines what needs to be done to add comprehensive error handling tests.

## Implementation Status

### Completed Tests

#### 1. HTTP Error Response Tests (✅ Complete)

**File:** `src/clients/agent.rs` (in `error_tests` module)

| Test | Status |
|------|--------|
| 400 Bad Request response returns meaningful error | ✅ `test_400_bad_request_returns_error` |
| 401 Unauthorized response returns meaningful error | ✅ `test_401_unauthorized_returns_error` |
| 403 Forbidden response returns meaningful error | ✅ `test_403_forbidden_returns_error` |
| 404 Not Found response returns meaningful error | ✅ `test_404_not_found_returns_error` |
| 429 Too Many Requests triggers retry and succeeds | ✅ `test_429_too_many_requests_retries_and_succeeds` |
| 500 Internal Server Error triggers retry and succeeds | ✅ `test_500_internal_server_error_retries_and_succeeds` |
| 503 Service Unavailable triggers retry and succeeds | ✅ `test_503_service_unavailable_retries_and_succeeds` |
| Max retries exceeded returns error | ✅ `test_max_retries_exceeded_returns_error` |
| Chat Completions 400 error | ✅ `test_chat_completions_400_bad_request_returns_error` |
| Chat Completions 429 retry | ✅ `test_chat_completions_429_retries_and_succeeds` |
| Successful Responses API call | ✅ `test_successful_responses_api_call` |
| Successful Chat Completions API call | ✅ `test_successful_chat_completions_api_call` |

#### 2. Malformed Response Tests (✅ Complete)

**File:** `src/clients/responses.rs` (in `response_parsing_tests` module)

| Test | Status |
|------|--------|
| Invalid JSON in response body | ✅ `parse_response_invalid_json` |
| Empty response body | ✅ `parse_response_empty_body` |
| Missing required field (output) | ✅ `parse_response_missing_output_field_fails` |
| Valid JSON response | ✅ `parse_response_valid_json` |
| Response with usage | ✅ `parse_response_with_usage` |
| Partial usage fields | ✅ `parse_response_partial_usage` |
| Empty output array | ✅ `parse_output_items_empty_output_array` |
| Missing id for reasoning | ✅ `parse_output_items_missing_id_for_reasoning` |
| Function call with missing fields | ✅ `parse_output_items_function_call_missing_fields` |
| Message with empty content array | ✅ `parse_output_items_message_with_empty_content_array` |
| Message with non-text content | ✅ `parse_output_items_message_with_non_text_content` |
| Reasoning with summary fallback | ✅ `parse_output_items_reasoning_with_summary_fallback` |
| Reasoning content fallback | ✅ `parse_output_items_reasoning_content_fallback_to_summary` |

**File:** `src/clients/chat_completions.rs` (in `response_parsing_tests` module)

| Test | Status |
|------|--------|
| Invalid JSON in response body | ✅ `parse_response_invalid_json` |
| Empty response body | ✅ `parse_response_empty_body` |
| Missing required field (choices) | ✅ `parse_response_missing_choices_fails` |
| Valid JSON response | ✅ `parse_response_valid_json` |
| Response with usage | ✅ `parse_response_with_usage` |
| Partial usage fields | ✅ `parse_response_partial_usage` |
| Response with tool calls | ✅ `parse_response_with_tool_calls` |
| Empty message content | ✅ `parse_choices_empty_message_content` |
| None content creates empty message | ✅ `parse_choices_none_content_creates_empty_message` |
| Multiple tool calls | ✅ `parse_choices_multiple_tool_calls` |
| Tool calls with text content | ✅ `parse_choices_tool_calls_with_text_content` |
| Missing id defaults to none | ✅ `parse_choices_missing_id_defaults_to_none` |
| Missing role defaults to none | ✅ `parse_choices_missing_role_defaults_to_none` |

#### 3. Retry Logic Tests (✅ Complete)

**File:** `src/clients/agent.rs` (in `error_tests` module)

| Test | Status |
|------|--------|
| Retry stops after MAX_RETRIES | ✅ `test_max_retries_exceeded_returns_error` |
| Successful retry after 429 | ✅ `test_429_too_many_requests_retries_and_succeeds` |
| Successful retry after 500 | ✅ `test_500_internal_server_error_retries_and_succeeds` |
| Successful retry after 503 | ✅ `test_503_service_unavailable_retries_and_succeeds` |
| Non-retryable 4xx errors fail immediately | ✅ `test_400/401/403/404_*_returns_error` |

### Deferred Tests

#### Network Error Tests (⏸️ Deferred)

**Reason for deferral:** Wiremock is designed for mocking HTTP responses, not simulating low-level network failures. Testing connection refused, DNS failures, and SSL/TLS errors would require either:
1. A trait-based HTTP client abstraction (significant refactoring)
2. Integration tests with real network conditions (slow, flaky)

The retry logic for network errors (`is_connect()` and `is_timeout()` checks in `complete_turn()`) follows the same code path as HTTP error retries, which is already tested.

| Test | Status | Notes |
|------|--------|-------|
| Connection refused error triggers retry | ⏸️ Deferred | Requires trait-based HTTP client |
| Connection timeout triggers retry | ⏸️ Deferred | Requires trait-based HTTP client |
| DNS resolution failure | ⏸️ Deferred | Requires trait-based HTTP client |
| SSL/TLS error | ⏸️ Deferred | Requires trait-based HTTP client |
| Exponential backoff timing | ⏸️ Deferred | Time-dependent tests are flaky |

## Implementation Details

### Dependencies Added

```toml
[dev-dependencies]
wiremock = "0.6"
```

### Test Organization

Tests are organized in dedicated test modules within each source file:

- `src/clients/agent.rs` - `error_tests` module for HTTP error and retry tests
- `src/clients/responses.rs` - `response_parsing_tests` module for malformed response tests
- `src/clients/chat_completions.rs` - `response_parsing_tests` module for malformed response tests

### Code Changes

1. Added `#[derive(Debug)]` to `TurnResult` struct for error testing
2. Added `wiremock` dependency for HTTP mocking
3. Created helper functions for test agent configuration

## Running Tests

```bash
# Run all tests
cargo test

# Run only error tests
cargo test error_tests

# Run only response parsing tests
cargo test response_parsing_tests
```

## Success Criteria

- [x] All HTTP status codes have corresponding tests
- [x] Retry logic is verified for transient failures
- [ ] Network errors are properly handled (deferred)
- [x] Error messages are meaningful and actionable
- [x] No regression in existing tests
- [x] Code coverage for error paths improved

## Related Files

- `src/clients/agent.rs` - Main agent loop, retry logic, HTTP error tests
- `src/clients/responses.rs` - Responses API backend, malformed response tests
- `src/clients/chat_completions.rs` - Chat Completions API backend, malformed response tests
- `src/config/model.rs` - Model configuration
- `Cargo.toml` - Dependencies (wiremock added)