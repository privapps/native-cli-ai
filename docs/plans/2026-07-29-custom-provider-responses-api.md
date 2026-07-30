# Custom Provider OpenAI Responses API Implementation Plan

## Goal

Add OpenAI Responses API support to the existing Custom provider without changing the built-in OpenAI provider or the existing OpenAI-compatible Chat Completions and Anthropic-compatible Messages paths.

## Boundaries

- The existing Custom provider protocol-adapter seam is the primary implementation and test seam.
- The Responses adapter owns endpoint construction, native input/tool mapping, request settings, SSE parsing, and protocol errors.
- Existing provider, configuration, TUI, model-discovery, and documentation boundaries remain responsible for their current user-facing concerns.
- All implementation remains Rust-native and preserves the empty-completion failure invariant.

## Bounded steps

1. [x] Add the `OpenAI Responses` compatibility value, dispatch it through Custom provider construction, and make a basic streamed text request work with full history, bearer auth, `stream: true`, and `store: false`.
2. [x] Add native Responses function tools, function-call argument streaming, multiple calls, function-call outputs, and tool-loop coverage.
3. [x] Add image input mapping and generation-setting support for `max_output_tokens` and nested reasoning effort; omit model-dependent `temperature` from Responses requests.
4. [x] Complete best-effort model discovery, save-anyway/setup surface coverage, diagnostics, documentation, and legacy-provider regression verification.
5. [x] Run formatting, Clippy, focused tests, the full workspace suite, and a final requirement-by-requirement audit against the approved spec and tickets.
6. [x] Follow up on model-specific Responses compatibility: omit the unsupported `temperature` field, add a regression fixture for the reported API error, and document the behavior.

## Invariants

- Existing `openai` Custom configuration continues to mean Chat Completions.
- Built-in OpenAI remains on its current Chat Completions path.
- Responses requests do not use `previous_response_id` and set `store` to false.
- Responses requests omit `temperature`; model support for that field varies and sending the configured default can make otherwise valid requests fail.
- Unsupported hosted Responses tools are not emitted or silently enabled.
- Unknown SSE event types may be ignored, but malformed known events, provider failures, transport failures, invalid tool arguments, and empty completions fail explicitly.
- Provider credentials and endpoint secrets are never exposed in diagnostics.
