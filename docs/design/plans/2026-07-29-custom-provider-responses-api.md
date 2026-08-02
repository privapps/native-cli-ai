# Custom Provider OpenAI Responses API Implementation Plan

**Type:** Implementation plan
**Status:** Implemented locally
**Date:** 2026-07-29
**Related:** [Custom Provider OpenAI Responses specification](../specs/2026-07-29-custom-provider-responses-api.md)
**Last verified:** 2026-07-31 (documentation review)

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
3. [x] Add image input mapping and generation-setting support for `max_output_tokens`, omitted Responses `temperature`, and nested reasoning effort.
4. [x] Complete best-effort model discovery, save-anyway/setup surface coverage, diagnostics, documentation, and legacy-provider regression verification.
5. [x] Run formatting, Clippy, focused tests, the full workspace suite, and a final requirement-by-requirement audit against the approved spec and tickets.
6. [x] Follow up on Responses generation-setting behavior: omit configured `temperature` from Responses requests, add a regression fixture for rejecting gateways, and document the behavior.

## Invariants

- Existing `openai` Custom configuration continues to mean Chat Completions.
- Built-in OpenAI remains on its current Chat Completions path.
- Responses requests do not use `previous_response_id` and set `store` to false.
- Responses requests omit `temperature`; model support remains provider-defined, and no retry or fallback is needed for this field.
- Unsupported hosted Responses tools are not emitted or silently enabled.
- Unknown SSE event types may be ignored, but malformed known events, provider failures, transport failures, invalid tool arguments, and empty completions fail explicitly.
- Provider credentials and endpoint secrets are never exposed in diagnostics.
