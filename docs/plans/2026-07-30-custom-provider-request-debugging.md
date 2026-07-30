# Custom Provider Request Debugging Implementation Plan

## Goal

Add request-only diagnostics for custom OpenAI-compatible Chat Completions and OpenAI Responses requests behind the exact environment setting `NCA_DEBUG_REQUEST=1`.

## Implementation

1. Add an internal request-debug formatter that can inspect the final custom request without changing the public `Provider` trait. It will include a UTC timestamp, protocol, method, URL, redacted headers, and pretty JSON body, redact credentials, and append to `./debug.log`.
2. Add the formatter at the custom provider request seam immediately before `.send()` for both protocol adapters.
3. Keep the response untouched: no response body, stream, parsed event, provider error body, or completion output may be logged.
4. Update the runtime configuration documentation.

## Verification

- Exercise both custom protocol modes through the public `Provider::chat` seam and existing local SSE fixtures.
- Cover exact environment activation and disabled values.
- Cover credential redaction, append behavior, log-write fallback, and absence of response logging.
- Run focused provider tests, formatting, and the full workspace test suite.
