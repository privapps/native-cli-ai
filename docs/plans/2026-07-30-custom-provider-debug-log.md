# Custom Provider Debug Log Implementation Plan

## Goal

Persist request-only diagnostics for custom OpenAI Chat Completions and OpenAI Responses in an append-only `./debug.log` when `NCA_DEBUG_REQUEST=1`.

## Implementation

1. Replace custom-provider console emission with an internal file logger that opens `debug.log` in append mode in the process current directory.
2. Format one human-readable block per request with UTC RFC3339 timestamp, protocol, method, URL, redacted headers, and pretty JSON body.
3. Preserve request execution and never log response bodies, streams, parsed events, provider errors, or completion output.
4. Keep custom Anthropic requests silent and make log-write failures warnings rather than provider failures.
5. Use restrictive permissions for newly created logs where supported and update configuration documentation.

## Verification

- Exercise both OpenAI-compatible custom protocols through `Provider::chat`.
- Verify append behavior, formatting, redaction, exact environment activation, Anthropic silence, response omission, and write-failure fallback.
- Run focused tests, workspace typechecking, formatting, and the full test suite.
