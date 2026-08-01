# Configurable Reasoning Effort for OpenAI-Compatible Chat Completions and Responses

**Type:** Feature specification
**Status:** Implemented locally
**Date:** 2026-07-29
**Related:** [Reasoning effort implementation plan](../plans/2026-07-29-reasoning-effort.md)
**Last verified:** 2026-07-31 (documentation review)
## Problem Statement

Users connect `nca` to OpenAI-compatible gateways and models that support the Chat Completions `reasoning_effort` request parameter. Today, `nca` always sends the shared OpenAI-compatible request shape without that parameter, so users cannot select values such as `none`, `low`, `medium`, or `high` for supported models.

This is especially limiting for the Custom provider, where the endpoint and model are user-selected and may expose provider-specific reasoning-effort values. The existing `enable_thinking` and `thinking_budget` settings do not provide this capability: they are independent settings and are not translated into an OpenAI-compatible `reasoning_effort` field.

The current Custom provider supports OpenAI-compatible Chat Completions, OpenAI Responses, and Anthropic-compatible Messages. Chat Completions shares the request-body path used by OpenAI and OpenRouter; Responses uses native input and nested reasoning fields; Anthropic-compatible Custom endpoints use the Anthropic Messages shape and must not receive OpenAI-only fields.

## Solution

Add a global model-level `reasoning_effort` setting and expose it through configuration, the CLI, and the interactive TUI. The setting is a string so custom gateways can support values beyond a fixed built-in list.

The default value is the literal string `"nil"`. After trimming surrounding whitespace:

- `"nil"` omits `reasoning_effort` from the JSON request.
- An empty value also omits the property.
- Every other value is sent unchanged, including `"none"`, `"low"`, `"medium"`, `"high"`, `"xhigh"`, and provider-specific values.

The field is added to OpenAI-compatible Chat Completions requests for OpenAI, OpenRouter, and Custom providers configured with `openai`, and to Custom Responses requests as `reasoning.effort`. Anthropic-compatible requests do not receive it. Configured values are sent without model-capability detection or automatic fallback; provider rejection is surfaced as the normal request error.

## User Stories

1. As an `nca` user, I want to configure a reasoning-effort value, so that supported models can use the amount of reasoning I choose.
2. As an `nca` user, I want the default reasoning-effort value to be `"nil"`, so that existing request payloads remain unchanged unless I opt in.
3. As an `nca` user, I want `"nil"` to omit the JSON property entirely, so that providers that do not understand the option receive the legacy request shape.
4. As an `nca` user, I want `"none"` to be sent as a real provider value, so that I can explicitly request no reasoning when the model supports that level.
5. As an `nca` user, I want to use standard values such as `low`, `medium`, and `high`, so that I can select common reasoning policies.
6. As a Custom-provider user, I want to pass provider-specific values through unchanged, so that gateways with values outside the standard list remain usable.
7. As an `nca` user, I want whitespace around the configured value to be ignored, so that formatting in config files or command input does not change the resulting request.
8. As an `nca` user, I want an empty or whitespace-only value to omit the property, so that `nca` never sends an empty reasoning-effort string.
9. As an OpenAI user, I want the configured value in Chat Completions requests, so that native OpenAI models can honor my reasoning preference.
10. As an OpenRouter user, I want the configured value in Chat Completions requests, so that routed reasoning models can honor my preference.
11. As a Custom-provider user using OpenAI-compatible Chat Completions, I want the configured value in the request, so that self-hosted and third-party gateways can honor it.
12. As a Custom-provider user using an Anthropic-compatible endpoint, I want OpenAI-only fields omitted, so that the endpoint receives a valid Anthropic Messages request.
13. As an `nca` user, I want one global model-level setting to apply across OpenAI-compatible providers, so that switching providers does not require duplicate configuration.
14. As an `nca` user, I want `--reasoning-effort VALUE` to override the configured value for one invocation, so that I can experiment without changing persistent settings.
15. As an `nca` user, I want `--reasoning-effort nil` to disable the request property for one invocation, so that I can explicitly restore legacy behavior.
16. As an `nca` user, I want omitting the CLI flag to preserve the configured value, so that unrelated CLI invocations do not reset my preference.
17. As an interactive TUI user, I want `/reasoning-effort VALUE` to change the setting, so that I can adjust it without leaving a session.
18. As an interactive TUI user, I want `/reasoning-effort` without a value to display the current setting, so that I can inspect the effective configuration before sending a turn.
19. As an interactive TUI user, I want `/reasoning-effort nil` to disable the property persistently, so that I can return to provider defaults from the session.
20. As an interactive TUI user, I want TUI changes persisted to workspace configuration, so that my chosen setting is reused in later workspace sessions.
21. As an `nca` user, I want the setting visible in model and configuration status output, so that I can distinguish an explicit `nil` setting from a missing or accidentally ignored setting.
22. As an `nca` user, I want status output to identify that the setting is OpenAI-compatible-only, so that seeing a configured value while using an Anthropic-compatible endpoint is not misleading.
23. As an `nca` user, I want `reasoning_effort` to remain independent from `enable_thinking` and `thinking_budget`, so that existing thinking configuration is not silently reinterpreted.
24. As an `nca` user, I want temperature behavior unchanged on Chat Completions and Anthropic-compatible paths, while Responses omits unsupported `temperature`, so that each protocol receives a valid request.
25. As an `nca` user, I want an unsupported value to produce the provider's normal error, so that invalid or incompatible configuration is visible instead of being silently downgraded.
26. As an `nca` maintainer, I want protocol adapters to own this behavior, so that OpenAI, OpenRouter, Custom Chat Completions, and Custom Responses requests stay consistent.

## Implementation Decisions

- Add `reasoning_effort` to the global model configuration as a string with a default of `"nil"`. Existing configurations that lack the field must acquire that default during deserialization and merging.
- Treat the setting as a pass-through value rather than an enum. This preserves compatibility with custom gateways and future provider values.
- Normalize only at the boundary: trim surrounding whitespace; omit for `nil` after trimming, and omit for an empty result. Preserve the remaining value exactly, including values such as `none` and vendor-specific strings.
- Extend the shared OpenAI-compatible Chat Completions request-body contract with the optional root-level `reasoning_effort` JSON property. The property is present only when the normalized value is non-empty and not `nil`.
- Route the global setting into OpenAI, OpenRouter, and the Custom Chat Completions adapter through the existing shared request-body seam.
- Route the global setting into the Custom Responses adapter as nested `reasoning.effort`.
- Do not add the property to the Anthropic-compatible request-body contract, including Anthropic-compatible Custom endpoints and the MiniMax Anthropic-compatible path.
- Always send a configured non-`nil` value for the selected Chat Completions or Responses provider in that protocol's request shape, without inspecting model names or attempting capability detection.
- Do not retry a rejected request without the property. Provider errors remain visible to the user.
- Keep `temperature` unchanged on Chat Completions and Anthropic-compatible paths. Custom Responses omits `temperature` because model support varies; this is a fixed protocol rule, not capability detection or retry behavior.
- Keep `enable_thinking` and `thinking_budget` independent. This feature does not translate those settings into `reasoning_effort` and does not implement Anthropic `thinking` blocks.
- Add a CLI `--reasoning-effort` option whose omission leaves the loaded configuration unchanged and whose explicit value, including `nil`, overrides it for the current invocation only.
- Add the `/reasoning-effort` TUI command. With a value, it trims and persists the new setting to workspace configuration; without a value, it displays the current setting. The Custom-provider setup wizard remains focused on endpoint and credential configuration.
- Include the setting in existing model/configuration status surfaces, with an OpenAI-compatible-only indication when appropriate. The displayed value remains `nil` when that is the configured default.
- Update user-facing configuration, provider, command, and interactive-mode documentation to describe the sentinel, pass-through values, protocol scope, CLI override, and TUI command.

## Testing Decisions

- Tests must assert observable request payloads and user-facing configuration behavior, not the internal representation used to conditionally construct the JSON object.
- The primary seam is the shared OpenAI-compatible request-body builder. Tests at this seam must verify that `nil` and empty values omit the property and that `none`, standard levels, trimmed values, and custom strings are emitted unchanged.
- Existing provider request-capture tests should verify the behavior through the Custom OpenAI-compatible path, because that is the motivating path, and through the native OpenAI and OpenRouter adapters to prove shared behavior.
- Existing Anthropic-compatible request-capture tests should verify that the property is absent even when the global setting is non-`nil`.
- Configuration tests should verify the default, layered configuration merge, whitespace/sentinel normalization contract, and backward compatibility for configurations written before the new field existed.
- CLI tests should verify that an omitted flag preserves configuration, a non-`nil` value overrides it for the invocation, and an explicit `nil` disables the wire property without persisting the override.
- TUI command-boundary tests should verify display with no argument, setting and trimming a value, explicit `nil`, workspace persistence, and status visibility.
- Regression coverage should retain the existing request payload assertions for the default configuration, proving that the default does not add a new JSON property.
- Full workspace tests should be run after focused provider, configuration, CLI, and TUI tests pass.

## Out of Scope

- Implementing Anthropic `thinking` request blocks or mapping `thinking_budget` to an Anthropic budget.
- Removing, renaming, or redefining `enable_thinking` or `thinking_budget`.
- Automatic model capability discovery or model-name heuristics.
- Automatic capability detection or retry-based removal of `temperature`; Custom Responses has a documented fixed omission rule.
- Retrying requests without `reasoning_effort` after provider rejection.
- A fixed allowlist of reasoning-effort values.
- A reasoning-effort field in the Custom-provider setup wizard.
- Per-provider or per-model reasoning-effort profiles.
- Sending `reasoning_effort` to Anthropic-compatible or other non-Chat-Completions/Responses request paths.
- Adding a separate provider-specific JSON extension mechanism beyond this setting.

## Further Notes

- The literal string `"nil"` is an application-level sentinel, not a TOML null value. Documentation must make clear that it means “do not include the request property.”
- The setting is intentionally permissive because Custom endpoints may implement values that are not part of the standard OpenAI vocabulary.
- Status output can show a configured value even when the active protocol does not use it, but should label the scope so users understand why the outgoing request does not contain it.
- This specification is intentionally kept as a local repository document and is not being published to the external issue tracker.
