# Custom Provider TUI Configuration

**Date:** 2026-07-27
**Status:** Implemented locally; workspace tests, benchmark compilation, protocol assertions, and production app-event-loop acceptance coverage have passed.

## Problem Statement

The `nca` runtime already has a singular `Custom` provider configuration and adapters for endpoints compatible with OpenAI Chat Completions, OpenAI Responses, or Anthropic Messages. The interactive TUI does not expose that capability consistently.

During first-run onboarding, `Custom` is filtered out of the provider list. In the in-session `/connect` flow, selecting `Custom` follows the generic API-key path instead of opening the custom endpoint setup. Other provider and model-picker routes can also fail when the custom endpoint has not yet been configured. Users are therefore pushed toward an indirect setup path and can become stuck on an existing provider.

Provider persistence also serializes the fully merged configuration. A workspace or environment-derived configuration can consequently copy unrelated provider credentials and settings into global or workspace-local TOML when the user changes only one provider.

## Solution

Make `Custom` a first-class TUI provider configuration flow while retaining one custom provider slot. The flow will support OpenAI-compatible Chat Completions, OpenAI Responses, and Anthropic-compatible endpoints, validate the endpoint with a cheap protocol-specific probe, and then activate it in the current runtime.

The same pure setup state machine will serve standalone first-run onboarding and in-session setup, while each surface keeps its own renderer and persistence scope. First-run setup writes global configuration; in-session setup writes workspace-local configuration. Existing `/custom` syntax and `CUSTOM_PROVIDER_*` environment variables remain supported through the same normalization and persistence behavior.

Provider changes will be applied to the runtime before persistence. A provider-construction failure will leave the old runtime active. A successful activation followed by a persistence failure will remain active and show an explicit warning.

## User Stories

1. As a first-time `nca` user, I want to see `Custom` in the onboarding provider list, so that I can choose a compatible endpoint without leaving onboarding.
2. As a first-time user, I want selecting `Custom` to open endpoint configuration rather than a generic API-key modal, so that I can provide the fields custom endpoints actually require.
3. As an in-session user, I want `/connect` and `Custom` to open the custom setup flow, so that configuration is discoverable from the normal provider entry point.
4. As an in-session user, I want `/provider custom` to activate an already configured endpoint, so that switching providers remains quick and predictable.
5. As a user selecting an unconfigured custom provider from a provider or model picker, I want setup to open instead of producing an opaque failure, so that the picker provides a path to recovery.
6. As a user with an existing custom configuration, I want compatibility, base URL, and model values prefilled, so that editing does not require re-entering public settings.
7. As a user, I want the secret field to remain empty when editing, so that stored credentials are never displayed or accidentally exposed in the TUI.
8. As a user, I want to choose OpenAI-compatible Chat Completions, OpenAI Responses, or Anthropic-compatible protocol behavior, so that the endpoint receives the request format and authentication headers it expects.
9. As a user, I want to enter an endpoint origin or a base path ending in `/v1`, so that common gateway URL formats work without requiring knowledge of the final request path.
10. As a user, I want invalid URLs, URLs containing credentials, query strings, fragments, or final request paths to be rejected before activation, so that malformed configuration cannot create confusing runtime failures.
11. As a user, I want to edit the API-key environment-variable name in the credential step, so that existing and provider-specific environment conventions remain usable.
12. As a user, I want environment-variable names validated as portable shell names, so that a configured variable can be exported consistently across platforms.
13. As a user, I want a blank secret field to preserve the existing credential source, so that environment-backed credentials stay environment-backed and existing inline overrides are not unexpectedly removed.
14. As a user, I want explicitly pasted secrets to become inline overrides, so that the endpoint can be used when no suitable environment variable is available.
15. As a user, I want missing credentials to block setup clearly, so that I do not save a provider that cannot authenticate.
16. As a user, I want the OpenAI-compatible endpoint to receive a cheap model-list probe, so that basic reachability and authentication are checked before activation.
17. As a user, I want the Anthropic-compatible endpoint to receive a minimal Messages probe, so that its protocol and authentication are checked before activation.
18. As a user, I want a failed probe to offer Retry, Save anyway, or Cancel, so that temporary network failures do not force me to abandon setup while malformed input still fails immediately.
19. As a user, I want a successful probe to activate the provider immediately, so that I can start chatting without restarting `nca`.
20. As a user, I want model discovery to be best-effort, so that an endpoint without a usable catalog can still be configured with a manually entered model ID.
21. As a user, I want provider status to identify the custom provider and sanitized host, so that I can tell which endpoint is active without seeing secrets.
22. As a user, I want streaming responses to continue working after switching to a custom provider, so that the TUI retains its normal live-response behavior.
23. As a user, I want tool calls to work through a custom provider, so that custom endpoints remain useful for agentic tasks rather than text-only chat.
24. As a user, I want empty provider completions to be reported as errors, so that a broken endpoint cannot appear to have completed successfully.
25. As a user, I want global onboarding persistence to change only the selected provider and onboarding flag, so that workspace settings and unrelated credentials are not copied into my global configuration.
26. As a user, I want in-session provider setup to change only the fields involved in that setup, so that unrelated workspace configuration remains intact.
27. As a user, I want existing TOML comments and unknown keys preserved, so that configuring a provider does not rewrite or discard settings maintained outside the provider flow.
28. As a user, I want a malformed configuration file left untouched with an actionable warning, so that a failed provider save cannot destroy recoverable configuration.
29. As a user, I want a provider to remain active when activation succeeded but persistence failed, so that a disk-write problem does not interrupt the current session unexpectedly.
30. As an existing user of `/custom`, I want the command to keep working, so that the TUI improvement does not break scripts or established workflows.
31. As an existing user of `CUSTOM_PROVIDER_*`, I want environment-based setup to retain its behavior, so that I do not need to migrate credentials or endpoint settings solely because the TUI gained a wizard.

## Implementation Decisions

- Keep one persisted `Custom` provider slot. Do not introduce multiple custom-provider profiles, profile management, or a new provider database.
- Use a shared pure setup state machine for onboarding and in-session flows. It owns field transitions, validation state, probe outcomes, retry behavior, save-anyway behavior, cancellation, and edit-mode prefill. Onboarding and session TUI renderers remain separate because they have different runtime and persistence lifecycles.
- Represent the normalized setup input as compatibility, canonical base URL, API-key environment-variable name, credential source, and model ID. Credential source distinguishes preserving the current value from explicitly storing a pasted override.
- Accept HTTP(S) endpoint origins and paths ending in `/v1`. Normalize trailing slashes while preserving an accepted path prefix before the adapter appends its request path. Reject credentials, query strings, fragments, and URLs that already contain `/chat/completions` or `/messages`.
- Validate API-key environment-variable names using the portable shell form `[A-Za-z_][A-Za-z0-9_]*`.
- Keep runtime credential precedence as inline key over environment lookup. A blank credential input preserves an existing inline key when one exists; otherwise it persists only the environment-variable name and never materializes the resolved environment secret into TOML.
- Use protocol-specific cheap probes: OpenAI-compatible Chat and Responses endpoints use `GET /v1/models` with Bearer authentication; Anthropic-compatible endpoints use a minimal `/v1/messages` request with the required Anthropic headers. Probe errors are sanitized before display and must not include secrets.
- Treat malformed URLs, invalid environment-variable names, and missing credentials as hard blockers. Network, authentication, protocol, or endpoint failures offer Retry, Save anyway, or Cancel.
- Save anyway activates the manually configured provider and completes onboarding when used from first-run onboarding. It does not make model discovery mandatory.
- Require the existing streaming/SSE behavior, correct protocol authentication headers, and agent-capable tool support. Do not add non-streaming or text-only fallbacks.
- Route `Custom` from onboarding and `/connect` into configuration. Route configured `Custom` from `/provider` into activation. Keep editing an explicit action, with public fields prefilled and the secret omitted.
- Update provider/model picker behavior so an unconfigured Custom selection opens setup rather than failing without a recovery path.
- Apply a candidate provider configuration to the current runtime before attempting persistence. If provider construction fails, retain the prior runtime and configuration. If persistence fails after activation, retain the new active provider and report the failure.
- Replace full-config provider saves with targeted global and workspace provider patches. Global onboarding may update the selected provider fields, custom settings, active provider, and onboarding-completed flag. Session actions update only the provider fields changed by that action.
- Implement TOML patches with a syntax-preserving document editor. Preserve comments and unknown keys, create minimal sections when a file is absent, and write atomically. If parsing or patching fails, leave the original file untouched and return a persistence warning.
- Route existing `/custom` command input and `CUSTOM_PROVIDER_*` environment configuration through the same URL normalization, credential-source, protocol validation, and targeted persistence rules where persistence is involved.
- Keep model discovery automatic and best-effort. Manual model IDs are accepted without catalog confirmation.
- Expose status as `Custom · <sanitized-host>` and retain detailed configuration visibility without displaying secrets.
- Update the custom-provider documentation and the existing compatibility plan to describe both TUI entry points, credential precedence, URL rules, probe behavior, and persistence scope.

## Testing Decisions

- Test observable behavior at the highest available seam: the shared custom-provider setup/probe/persistence boundary. Avoid tests coupled to individual widget rendering helpers or private serialization implementation.
- Test the pure setup state machine for new setup, edit setup, invalid input, missing credentials, retry, save-anyway, cancellation, and successful completion.
- Test URL normalization with origin URLs, `/v1` and prefixed `/zen/v1` URLs, trailing slashes, final endpoint paths, credentials, queries, fragments, and malformed schemes.
- Test credential behavior with environment-only credentials, existing inline credentials, explicit overrides, blank edits, custom environment-variable names, invalid names, and secret redaction.
- Test provider probes with deterministic HTTP fixtures for OpenAI model listing, Responses model listing, and Anthropic Messages requests. Assert request paths, authentication headers, required protocol fields, sanitized errors, and retry/save/cancel outcomes.
- Test targeted global and workspace persistence using temporary configuration files. Assert that unrelated provider credentials, workspace settings, environment-derived secrets, comments, and unknown keys are preserved appropriately.
- Test malformed TOML and write failures to confirm the original file remains intact and the active runtime behavior matches the persistence contract.
- Test TUI routing at the modal-command boundary for onboarding Custom selection, `/connect`, configured activation, unconfigured picker recovery, and explicit editing.
- Test both compatibility adapters end to end for streaming, tool calls, and empty completions. Empty completions must fail loudly.
- Retain regression coverage for `/custom`, `CUSTOM_PROVIDER_*`, provider switching, model switching, and onboarding completion.
- Follow existing Rust test conventions in the common configuration, core provider, runtime model/probe, and TUI state/input modules. Use mocked or local deterministic HTTP fixtures rather than live provider services.

## Out of Scope

- Multiple custom-provider profiles or named provider accounts.
- A general provider-management UI beyond configuring, activating, and explicitly editing the existing Custom slot.
- Keyless custom endpoints.
- Non-streaming fallback behavior.
- Automatic migration of arbitrary external configuration formats.
- Persisting resolved environment secrets into any configuration file.
- Changing the default MiniMax provider or its primary integration path.
- Replacing the existing Unix-socket IPC, session event model, Supervisor lifecycle, or agent tool protocol.
- Publishing this specification or creating an issue in GitHub.

## Further Notes

The existing custom-provider compatibility plan covers the backend foundation; this specification completes the TUI lifecycle and persistence contract that foundation currently lacks. Implementation should begin by recording this specification in the project plan documentation, then proceed through the shared setup/probe boundary before wiring the separate onboarding and session renderers.
