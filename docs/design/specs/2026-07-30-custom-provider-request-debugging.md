# Custom Provider Request Debugging

**Type:** Feature specification
**Status:** Implemented locally
**Date:** 2026-07-30
**Related:** [Custom Provider Request Debugging implementation plan](../plans/2026-07-30-custom-provider-request-debugging.md)
**Last verified:** 2026-07-31 (documentation review)
## Problem Statement

Users configuring a custom provider cannot inspect the exact request that nca sends when diagnosing compatibility problems. This is especially difficult because custom OpenAI-compatible providers may implement either Chat Completions or OpenAI Responses, and request-shape differences can cause failures before the provider returns a useful completion.

The repository already has an `NCA_DEBUG_REQUEST` convention, but its current behavior is limited and its documentation describes only MiniMax request logging. Custom-provider users need the same diagnostic capability for both supported OpenAI-compatible request protocols.

## Solution

When `NCA_DEBUG_REQUEST=1` is set, nca appends the complete outgoing custom-provider request to `./debug.log` for both:

- OpenAI-compatible Chat Completions requests.
- OpenAI Responses requests.

The diagnostic output is a human-readable UTC-timestamped block containing the HTTP method, URL, headers with credentials redacted, and pretty-printed JSON request body. It is appended immediately before transmission. If the log cannot be written, nca warns on stderr and continues the request.

No response data is logged. This includes successful responses, non-success response bodies, streamed SSE chunks, parsed response events, tool calls, usage events, and empty-completion errors.

## User Stories

1. As a custom-provider user, I want to inspect the exact Chat Completions request nca sends, so that I can diagnose endpoint compatibility issues.
2. As a custom-provider user, I want to inspect the exact OpenAI Responses request nca sends, so that I can diagnose Responses API compatibility issues.
3. As a custom-provider user, I want to see the request method and URL, so that I can verify the selected endpoint and preserved base-URL path.
4. As a custom-provider user, I want to see request headers without exposed credentials, so that I can verify protocol metadata safely.
5. As a custom-provider user, I want to see the serialized request body, so that I can inspect model selection, messages, tools, streaming options, token limits, temperature, and reasoning settings.
6. As a custom-provider user, I want multimodal request content to appear in the diagnostic payload, so that I can diagnose image and content-shape problems.
7. As a custom-provider user, I want tool definitions and prior tool-call messages to appear in the diagnostic payload, so that I can diagnose tool-use protocol mismatches.
8. As a custom-provider user, I want Chat Completions and Responses requests to use the same debug switch, so that I do not need protocol-specific configuration.
9. As a custom-provider user, I want diagnostics enabled only when I explicitly set `NCA_DEBUG_REQUEST=1`, so that ordinary runs remain quiet.
10. As a custom-provider user, I want unset, empty, or other values of `NCA_DEBUG_REQUEST` to leave diagnostics disabled, so that accidental environment values do not produce verbose output.
11. As a custom-provider user, I want authorization credentials redacted from diagnostic output, so that copying terminal logs does not disclose my API key.
12. As a custom-provider user, I want configured API-key values redacted wherever they occur in diagnostic output, so that provider payloads or headers cannot leak the credential.
13. As a custom-provider user, I want request diagnostics written to `./debug.log`, so that normal CLI output and machine-readable streams remain usable.
14. As a custom-provider user, I want enabling diagnostics to leave provider behavior unchanged, so that debugging does not alter request construction, streaming, tool handling, or error handling.
15. As a custom-provider user, I want no response data logged, so that prompts and provider output are not duplicated into diagnostic logs after the request is sent.
16. As a custom-provider user, I want non-success responses to remain handled by the existing provider error mapping, so that request logging does not change error classification.
17. As a custom-provider user, I want empty completions to remain explicit errors, so that diagnostics do not weaken the existing provider-completion invariant.
18. As a maintainer, I want the existing OpenAI-compatible request builders and stream parsers reused, so that diagnostics do not create a second protocol implementation.
19. As a maintainer, I want both custom protocol modes covered through the public provider seam, so that tests verify observable behavior rather than private logging mechanics.
20. As a maintainer, I want the configuration documentation to accurately describe the debug switch, so that users understand its scope, output destination, and sensitive-payload implications.

## Implementation Decisions

- Extend custom-provider diagnostics to cover the OpenAI-compatible Chat Completions adapter and the OpenAI Responses adapter.
- Activate diagnostics only when the process environment contains the exact value `NCA_DEBUG_REQUEST=1`.
- Log immediately before sending the request, after the adapter has constructed the final method, URL, headers, and JSON body.
- Include the HTTP method, URL, headers, and serialized request body in the diagnostic record.
- Redact `Authorization`, `x-api-key`, and configured API-key values before writing diagnostics.
- Append diagnostics to `./debug.log` so stdout, stderr, NDJSON, and CLI rendering contracts remain unaffected; use stderr only for log-write warnings.
- Do not log any response body, response stream bytes, parsed response events, provider error body, or completion result.
- Reuse the existing custom protocol adapters, request-body builders, request execution, and stream parsers. Any shared helper or optional diagnostics seam remains internal and does not change the public provider abstraction.
- Preserve existing request behavior for both `/chat/completions` and `/responses`, including path prefixes, authentication, streaming flags, tools, reasoning settings, and model overrides.
- Update runtime configuration documentation to describe custom-provider request logging, exact activation semantics, file output, credential redaction, and the fact that request payloads can contain conversation data.
- Create an implementation plan document before source changes, as required by repository guidance.

## Testing Decisions

- Test through the highest existing seam: construct a `CustomProvider`, call the public `Provider::chat` method, and use the existing local SSE fixtures to verify that request logging does not change provider behavior.
- Cover both custom OpenAI-compatible modes:
  - Chat Completions at the custom `/chat/completions` endpoint.
  - OpenAI Responses at the custom `/responses` endpoint.
- Assert request logging behavior externally: method, URL, relevant headers, request JSON, append behavior, log-write fallback, and absence of credential values.
- Cover enabled and disabled environment values, including unset, empty, `0`, and other non-`1` values.
- Cover redaction of bearer credentials, protocol-specific API-key headers where applicable, and configured key text appearing in serialized data.
- Verify that successful streamed text, tool calls, usage, and completion events remain unchanged for both protocols.
- Verify that non-success responses retain their existing error mapping and that no response body is emitted by the debug facility.
- Verify that empty completions still produce explicit errors.
- Use existing provider tests as prior art: custom-provider request-capture tests, OpenAI-compatible streaming tests, Responses streaming tests, and credential-redaction tests.
- Run focused provider tests, formatting, and the full workspace test suite before completion.

## Out of Scope

- Logging response bodies, raw response streams, parsed response events, or provider output.
- Adding request debugging for non-custom providers beyond the existing behavior.
- Adding a persistent configuration-file setting for request debugging.
- Changing provider request schemas, endpoint normalization, authentication behavior, streaming behavior, or error mapping.
- Redacting arbitrary user content beyond credentials; request bodies may contain prompts, file contents, images, and tool schemas.
- Adding a new CLI command or structured diagnostics format.

## Further Notes

The current custom provider supports multiple compatibility modes, but this feature specifically targets the two OpenAI-compatible modes: Chat Completions and OpenAI Responses. Anthropic-compatible custom requests are not part of this request.

The existing nca invariant that successful HTTP responses with empty provider completions fail loudly must remain intact.
