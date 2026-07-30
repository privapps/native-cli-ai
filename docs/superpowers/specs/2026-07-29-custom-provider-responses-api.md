# Custom Provider OpenAI Responses API Support

## Problem Statement

Customers who use gateways or hosted services that expose the OpenAI Responses API cannot currently connect them to nca through the Custom provider. The Custom provider supports OpenAI-compatible Chat Completions and Anthropic-compatible Messages, but Responses uses a different request item model and streaming event protocol. Treating Responses as Chat Completions would lose native function-call semantics and make otherwise compatible customer endpoints unreliable.

## Solution

Add `OpenAI Responses` as a third Custom-provider compatibility mode. The mode uses a dedicated protocol adapter that sends native Responses API requests to the configured `/responses` endpoint, preserves the complete nca conversation history, supports streaming text and function calls, and converts Responses events into the existing provider stream contract.

Existing built-in OpenAI behavior and existing Custom OpenAI-compatible Chat Completions behavior remain unchanged. Customers configure the existing Custom provider slot, credentials, model, and base URL, then select the new compatibility mode.

## User Stories

1. As a customer-provider user, I want to select OpenAI Responses as the Custom provider protocol, so that I can connect an endpoint that exposes `/responses`.
2. As an existing nca user, I want the current OpenAI-compatible Custom mode to retain its meaning, so that existing configurations do not change behavior.
3. As an nca user, I want the built-in OpenAI provider to remain unchanged initially, so that Responses support can be adopted through Custom without changing native OpenAI defaults.
4. As a customer-provider user, I want to configure an origin or `/v1` base URL, so that nca can construct the protocol endpoint consistently.
5. As a customer-provider user, I want nca to append `/responses` automatically, so that I do not need to enter a full request path.
6. As a customer-provider user, I want the configured API key to be sent as a bearer credential, so that existing credential management continues to work.
7. As a customer-provider user, I want inline and environment-backed credentials to work in Responses mode, so that I can use the same deployment and secret-management patterns.
8. As a customer-provider user, I want the complete canonical conversation history sent on each request, so that sessions remain portable and replayable.
9. As a customer-provider user, I want Responses mode not to depend on `previous_response_id`, so that gateways do not need server-side conversation state.
10. As a privacy-conscious user, I want requests to set `store` to false, so that nca does not ask the provider to retain response data unnecessarily.
11. As a terminal user, I want Responses mode to stream output, so that text appears progressively in the live CLI experience.
12. As a terminal user, I want cancellation to stop an in-flight streamed request, so that a stalled or unwanted provider response does not hold the session.
13. As an agent user, I want Responses text deltas to appear through the existing stream contract, so that the TUI and CLI do not need a Responses-specific renderer.
14. As an agent user, I want function calls represented using native Responses items, so that strict Responses endpoints receive the protocol they expect.
15. As an agent user, I want function-call outputs returned using their original call IDs, so that the provider can associate each result with the correct invocation.
16. As an agent user, I want multiple function calls from one response preserved in order, so that no requested tool operation is silently dropped.
17. As an agent user, I want existing nca tools exposed as Responses function tools, so that the normal agent loop can continue to inspect files, run commands, and perform other registered work.
18. As a security-conscious user, I want built-in provider tools such as web search, file search, and computer use rejected as unsupported, so that nca does not enable provider-side capabilities without its approval and execution model.
19. As a multimodal user, I want existing Custom image attachments mapped to Responses input-image blocks, so that image-enabled workflows remain available.
20. As a user with an unreadable or unsupported attachment, I want a clear request error, so that nca never silently drops an image.
21. As a user configuring a model, I want `max_tokens` mapped to `max_output_tokens`, so that the existing output budget remains meaningful.
22. As a user connecting a Responses model, I want nca to omit unsupported generation fields, so that a model that rejects `temperature` can still receive a valid request.
23. As a user configuring reasoning effort, I want the value mapped to the Responses `reasoning.effort` field, so that supported reasoning models can use the existing setting.
24. As a user with an unsupported reasoning or generation setting, I want the provider rejection surfaced normally, so that nca does not conceal incompatibilities through silent retries.
25. As a user with a provider that supports model listing, I want `/models` discovery to continue working, so that I can inspect available model IDs.
26. As a user of a Responses-only gateway without `/models`, I want to save a manually entered model anyway, so that model discovery is not a hard requirement for runtime use.
27. As a user configuring the provider in onboarding, I want to choose Responses mode, so that the feature is available on first setup.
28. As a user switching providers in an existing session, I want Responses mode available through the existing Custom-provider flow, so that I can change protocols without editing files manually.
29. As a user inspecting configuration or diagnostics, I want the active mode identified as OpenAI Responses, so that protocol behavior is visible and understandable.
30. As a maintainer, I want existing Chat Completions and Anthropic behavior covered by regression tests, so that adding Responses does not alter other provider protocols.
31. As a maintainer, I want malformed Responses events and empty completions to fail loudly, so that a successful HTTP status cannot be mistaken for a usable agent response.
32. As a maintainer, I want unknown future SSE event types ignored safely, so that providers can add events without immediately breaking nca.
33. As a maintainer, I want provider errors and probe diagnostics sanitized, so that credentials and sensitive endpoint details are not exposed.
34. As a maintainer, I want Responses behavior isolated behind one protocol adapter seam, so that provider orchestration and the shared agent loop remain stable.

## Implementation Decisions

- Extend the finite `ProviderCompatibility` model with a distinct OpenAI Responses variant. Its display name is `OpenAI Responses`; parsing accepts `responses`, `openai-responses`, and `openai_responses`. The existing `openai` value continues to select Chat Completions.
- Keep the existing Custom provider slot, setup fields, credential sources, model field, and base-URL validation. Do not add multiple Custom profiles or a general provider plugin system.
- Add a dedicated protocol adapter behind the existing Custom provider abstraction. The adapter owns Responses endpoint construction, request-body construction, native input-item mapping, function-tool mapping, SSE parsing, and protocol-specific error handling.
- Preserve the existing common provider lifecycle, HTTP client, credential resolution, stream-channel contract, cancellation behavior, and error taxonomy wherever they already express the required behavior.
- Normalize the configured base URL as an HTTP(S) origin or an origin path ending in `/v1`. Append `responses` for inference requests and retain `models` for discovery.
- Use `Authorization: Bearer <api-key>`. Do not add OpenAI organization headers, project headers, or arbitrary custom headers in this feature.
- Build each request from the complete canonical message history. Preserve the history’s role and content semantics as native Responses input items rather than using `previous_response_id` or server-side conversation state.
- Represent assistant tool requests as native `function_call` items and tool results as native `function_call_output` items. Preserve call IDs, function names, arguments, and ordering. Support multiple calls in one response.
- Expose only existing nca function tools. Do not support OpenAI-hosted tools or other Responses tool types in this feature.
- Map text and existing image attachments to the corresponding Responses input content blocks. Reuse current attachment loading, workspace-root resolution, and vision-capability checks.
- Send `stream: true` and `store: false` on every Responses inference request. There is no non-streaming fallback.
- Map the existing output-token budget to `max_output_tokens`. Omit `temperature` from Responses requests because model support varies; keep the configured value for the existing Chat Completions and Anthropic-compatible paths. Do not infer model capabilities or automatically retry after provider rejection.
- When reasoning effort is configured and is not empty or `nil`, send it as `reasoning: { effort: <value> }`; omit the field otherwise. Preserve configured values unchanged and surface provider rejection normally.
- Parse the official Responses streaming events needed by nca, including text deltas, function-call argument deltas/completion, usage, response completion, and response failure. Ignore unknown event types for forward compatibility.
- A completed response must yield text or at least one valid function call. Malformed known events, explicit provider failure events, transport failures, invalid function arguments, and empty completions become explicit provider errors.
- Keep model discovery best-effort. Use the existing `/models` path and authentication where available; a failed or unsupported discovery request may be saved past with a manually configured model. Runtime readiness is established by the actual Responses request.
- Expose the new compatibility value in TUI onboarding, in-session Custom-provider setup, provider switching, CLI/config parsing, persisted TOML, status output, and doctor/provider diagnostics. Preserve legacy configuration round-trips.
- Update provider, configuration, command, interactive-mode, and feature documentation to distinguish OpenAI Responses from OpenAI-compatible Chat Completions and Anthropic-compatible Messages.

## Testing Decisions

- Prefer the highest existing seam: exercise the public Custom provider chat contract against deterministic local HTTP fixtures and assert externally observable requests, headers, streamed events, and provider outcomes. Add only narrow unit coverage for pure request/event transformations where the public seam cannot isolate malformed inputs.
- Extend the existing Custom-provider request-capture tests with Responses endpoint construction, `/v1` path prefixes, bearer authentication, `stream`, `store`, model, token-budget, omitted-temperature, and reasoning fields.
- Include a regression fixture where the gateway returns `Unsupported parameter: 'temperature' is not supported with this model.` when that field is present, and assert the request succeeds without it.
- Verify native input mapping for system, user, assistant, tool, text, image, function-call, and function-call-output items, including full-history replay and multiple tool calls.
- Verify function-tool declarations use the Responses shape and that unsupported built-in tool types are rejected rather than emitted.
- Feed deterministic SSE fixtures containing text deltas, split function arguments, multiple function calls, usage, completion, unknown events, malformed known events, provider failures, transport failures, and empty responses. Assert the existing `StreamChunk` behavior and explicit errors.
- Verify cancellation through the existing provider/runtime cancellation path and ensure streamed requests do not silently complete after cancellation.
- Verify model discovery success, empty catalogs, unavailable `/models`, malformed catalogs, authentication errors, sanitized diagnostics, and save-anyway behavior through the existing Custom setup flows.
- Verify compatibility parsing, display names, persisted configuration, CLI aliases, TUI selection, status/doctor output, and legacy `openai`/`anthropic` round-trips.
- Retain existing OpenAI-compatible Chat Completions, Anthropic-compatible Messages, Custom path-prefix, reasoning-effort, empty-completion, and provider regression tests unchanged as a compatibility gate.
- Follow prior art from the existing Custom protocol fixture tests, provider stream-parser tests, configuration round-trip tests, and TUI Custom-provider setup-flow tests. Tests should assert behavior, not adapter names or internal match expressions.
- Finish with formatting, Clippy, focused common/core/TUI tests, the full workspace test suite, and the normal release/build verification appropriate to the repository.

## Out of Scope

- Switching the built-in OpenAI provider from Chat Completions to Responses.
- Replacing the existing provider trait or introducing a dynamic provider/plugin framework.
- Multiple named Custom-provider profiles or per-provider account management.
- OpenAI-hosted Responses tools such as web search, file search, computer use, or code interpreter.
- Dependence on `previous_response_id`, server-side conversation persistence, or provider-managed session state.
- Non-streaming Responses requests or silent retry/fallback to Chat Completions.
- Arbitrary provider-specific headers, request extensions, or a generic JSON escape hatch.
- Automatic model-capability detection, parameter rewriting, or retrying without rejected fields.
- Live-service tests against OpenAI or customer endpoints.
- Changes to the default MiniMax provider or its primary integration path.

## Further Notes

- The existing Custom provider already has protocol-specific adapter behavior for endpoint paths, authentication, probing, request construction, model discovery, and stream handling. Responses support should extend that seam rather than distribute protocol branches through the agent loop or terminal presentation.
- The feature must preserve the nca invariant that an empty provider completion fails loudly; a successful HTTP response without usable text or a function call is not a successful turn.
- The local HTTP fixture approach is important because customer gateways may vary in model discovery support while still implementing the Responses inference contract.
