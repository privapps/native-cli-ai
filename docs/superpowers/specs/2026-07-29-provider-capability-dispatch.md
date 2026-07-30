# Provider Capability Dispatch and Model Catalog Deepening

**Date:** 2026-07-29
**Status:** Implemented locally

## Problem Statement

As nca gains providers and provider protocols, provider-specific behavior is selected repeatedly in configuration and runtime model-capability code. Model accessors, credential checks, context-window lookups, model discovery, and Custom compatibility each enumerate provider variants independently.

This makes the provider abstraction difficult to extend safely. Adding or changing a provider can require edits in several unrelated dispatch sites, and those sites can disagree about endpoints, credentials, compatibility, fallback behavior, or supported capabilities. The implementation also risks turning simple provider selection into a collection of shallow forwarding functions.

The goal is to improve locality and leverage without introducing a speculative plugin framework or changing the existing provider abstraction.

## Solution

Create a focused provider-selection interface that centralizes access to common provider settings and delegates remote model capabilities to protocol-aware adapters. Configuration code selects a provider settings view once; runtime model discovery and context-window resolution use a provider capability adapter instead of repeating provider and compatibility switches.

The interface must preserve current behavior: static fallback limits, best-effort model discovery, environment-backed credentials, Custom URL prefixes, protocol-specific authentication, caching, and secret redaction. Custom OpenAI-compatible and Anthropic-compatible behavior should reuse the protocol adapters defined by the financial-research hardening work.

## User Stories

1. As an nca user, I want changing the active provider to keep selecting that provider’s configured model, so that provider switching remains predictable.
2. As an nca user, I want model overrides to update only the selected provider’s model, so that other provider settings remain unchanged.
3. As an nca user, I want provider base URLs and API-key environment names to remain unchanged when unrelated settings are edited, so that configuration layering stays safe.
4. As an nca user, I want provider readiness checks to use the same credential source as provider construction, so that status does not disagree with actual activation.
5. As an nca user, I want context-window detection to use the active provider’s correct endpoint and authentication, so that token budgeting remains accurate.
6. As an nca user, I want model discovery to use the active provider’s correct catalog protocol, so that available models are not silently missing or attributed to another provider.
7. As an nca user, I want Custom OpenAI-compatible and Anthropic-compatible endpoints to select the correct catalog behavior, so that compatibility configuration controls all remote model lookups consistently.
8. As an nca user, I want a provider API failure to retain the existing static model limits, so that temporary network failures do not prevent a session from starting.
9. As an nca user, I want a provider with no configured credential to skip remote discovery cleanly, so that diagnostics do not expose secrets or produce confusing panics.
10. As an nca user, I want Custom path prefixes preserved during catalog and context lookups, so that gateways mounted below an origin continue to work.
11. As an nca maintainer, I want common provider settings accessed through one interface, so that adding a provider does not require editing many unrelated match expressions.
12. As an nca maintainer, I want model catalog and context-window behavior represented as provider capabilities, so that each capability has one implementation seam.
13. As an nca maintainer, I want provider-specific adapters to own endpoint construction, authentication, response parsing, and fallback mapping, so that runtime orchestration remains small.
14. As an nca maintainer, I want Custom compatibility to delegate to protocol adapters, so that OpenAI and Anthropic behavior is not repeated across configuration and runtime code.
15. As an nca maintainer, I want cache keys to distinguish provider, endpoint, compatibility, model, and credential identity without storing credentials, so that cached results remain correct and safe.
16. As an nca maintainer, I want adding a provider to have a clear compile-time and test-time checklist, so that provider support cannot be partially wired.
17. As an nca maintainer, I want existing public provider construction and chat behavior unchanged, so that this refactor improves locality without changing request semantics.
18. As an nca maintainer, I want the provider abstraction to remain finite and explicit, so that this work does not create a dynamic plugin or generic provider-management framework.

## Implementation Decisions

- Introduce a focused provider settings view at the configuration seam. The view exposes the common settings needed by callers—selected model, base URL, API-key environment name, credential presence, and mutable model or credential updates—without exposing unrelated provider configuration or materializing secrets.
- Keep `ProviderKind` as the stable provider identity and preserve its existing parsing, ordering, serialization, and user-facing names.
- Centralize common provider field access in the provider configuration module. Callers should not independently enumerate every provider merely to read or update a common setting.
- Define provider capability adapters for remote model catalog lookup and context-window lookup. Each adapter owns its endpoint construction, authentication, response parsing, cache identity, and mapping of remote failures to best-effort fallback results.
- Use one runtime dispatch point to select the capability adapter for the active provider. Custom compatibility selects the OpenAI-compatible or Anthropic-compatible adapter at that point.
- Keep MiniMax’s static model list and static model limits behind the same capability interface, even though it does not use a remote catalog endpoint.
- Preserve the existing best-effort contract: remote discovery failures return an empty model list or static model limits, emit only safe diagnostics, and never block normal provider construction.
- Preserve the existing configuration contract for environment-backed credentials, inline-key precedence, layered configuration, targeted persistence, and secret redaction.
- Preserve Custom URL normalization and path prefixes for every catalog and context lookup. Endpoint construction must be owned by the selected protocol adapter rather than reconstructed by each caller.
- Reuse the Custom OpenAI-compatible and Anthropic-compatible protocol adapters from the provider hardening work. The provider capability interface may call their catalog/probe helpers, but it must not duplicate authentication or compatibility branches.
- Keep caching behind the capability implementation. Cache identities include provider kind, normalized base URL, compatibility, model where relevant, and a non-reversible credential tag; raw secrets must never be cached or logged.
- Return capability results with enough information for callers to distinguish a remote result from a static fallback internally, while preserving existing public fallback behavior unless a caller explicitly requests diagnostics.
- Keep the main runtime orchestration shallow: it asks the selected capability for model IDs or context limits and applies the existing fallback policy; it does not know provider endpoint details.
- Do not make every provider configuration type implement one oversized trait. Use narrow interfaces for settings access and model capabilities so each interface remains deep and each adapter has one coherent reason to change.
- Do not introduce dynamic provider loading, user-defined provider plugins, a provider database, or changes to the provider chat trait.
- Add a bounded implementation plan before source changes. Implement common settings access first, then capability adapters and one dispatch point, then Custom compatibility reuse, then cache/fallback verification and documentation.
- Update architecture and provider documentation to describe the provider settings interface, capability adapters, Custom compatibility dispatch, and best-effort fallback behavior.

## Testing Decisions

- Test the configuration interface through public provider selection, model override, base-URL update, credential-presence, and layered-merge behavior. Assert observable configuration results rather than the internal match structure.
- Test a capability matrix covering MiniMax, OpenAI, Anthropic, OpenRouter, Custom OpenAI-compatible, and Custom Anthropic-compatible configurations.
- Use deterministic local HTTP fixtures to assert catalog endpoint paths, path prefixes, authentication headers, response parsing, pagination where supported, and model/context results.
- Test missing credentials, malformed URLs, HTTP failures, malformed successful responses, empty catalogs, and unsupported model entries. Assert static-limit or empty-list fallback and safe diagnostics.
- Test cache reuse and invalidation when provider kind, normalized endpoint, compatibility, model, or credential identity changes. Assert that raw credentials never appear in cache keys exposed to diagnostics or logs.
- Test that Custom compatibility selects exactly one protocol adapter and never sends OpenAI fields or headers through the Anthropic path.
- Retain existing provider streaming, tool-call, empty-completion, reasoning-effort, and custom-provider path-prefix tests to prove that capability dispatch does not alter chat behavior.
- Add a provider-support checklist test or table-driven coverage that fails when a new `ProviderKind` is added without settings and capability coverage.
- Run formatting, Clippy, focused common/runtime/provider tests, and the full workspace suite.

## Out of Scope

- Adding a new provider or changing provider defaults.
- Changing provider request payloads, streaming semantics, tool support, or reasoning-effort behavior.
- Replacing the existing provider chat abstraction.
- Dynamic provider plugins, arbitrary provider profiles, or a provider database.
- Financial research domain rules; those are specified in the companion financial-research hardening spec.
- Reworking every simple enum match that is unrelated to provider settings or model capabilities.
- Performance optimization beyond avoiding duplicate network lookups and preserving the existing cache behavior.

## Further Notes

This spec addresses the review finding that provider selection is repeated across configuration and runtime model-capability code. It intentionally chooses a narrow settings interface and capability-adapter seam rather than a broad provider framework. The companion financial-research spec covers the related Custom protocol probe validation and protocol-adapter hardening.
