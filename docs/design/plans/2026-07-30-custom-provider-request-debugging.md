# Custom Provider Request Debugging Implementation Plan

**Type:** Implementation plan  
**Status:** Implemented locally  
**Date:** 2026-07-30  
**Related:** [Custom Provider Request Debugging specification](../specs/2026-07-30-custom-provider-request-debugging.md)  
**Last verified:** 2026-07-31 (documentation review)

## Goal

Add request-only diagnostics for custom OpenAI-compatible Chat Completions and OpenAI Responses requests behind the exact environment setting `NCA_DEBUG_REQUEST=1`.

## Implementation

1. Add an internal request-debug formatter that can inspect the final custom request without changing the public `Provider` trait. It will include a UTC timestamp, protocol, method, URL, redacted headers, and pretty JSON body, redact credentials, emit the record to stderr, and append it to `./debug.log`.
2. Add the formatter at the custom provider request seam immediately before `.send()` for both protocol adapters.
3. Keep the response untouched: no response body, stream, parsed event, provider error body, or completion output may be logged.
4. Keep custom Anthropic requests silent and make log-write failures warnings rather than provider failures.
5. Use restrictive permissions for newly created logs where supported.
6. Update the runtime configuration documentation.

## Verification

- Exercise both custom protocol modes through the public `Provider::chat` seam and existing local SSE fixtures.
- Cover exact environment activation and disabled values.
- Cover credential redaction, append behavior, log-write fallback, and absence of response logging.
- Run focused provider tests, formatting, and the full workspace test suite.

This plan consolidates the earlier custom-provider debug-log plan, which is retained as a superseded historical note.
