# Custom Provider Compatibility and TUI Plan

## Status

The Custom provider implementation uses one configurable provider slot and supports endpoints that speak either the OpenAI-compatible Chat Completions protocol or the Anthropic-compatible Messages protocol. The TUI exposes configuration during first-run onboarding and during an existing session. Focused source/test coverage and full locked workspace verification are present.

The complete TUI lifecycle and acceptance contract is recorded in [Custom Provider TUI Configuration](../superpowers/specs/2026-07-27-custom-provider-tui-spec.md).

## User entry points

- First run: select **Custom** in the `/connect`-style onboarding provider list.
- Existing session: select **Custom** from `/connect` to configure it.
- Existing session: use `/provider` → **Add custom provider…** to add or edit the slot.
- Activation: use `/provider custom` or select **Custom (BYO endpoint)** in the provider picker.
- Legacy/script path: `/custom <openai|anthropic> <base-url> [api-key] [model]` remains supported.

An unconfigured Custom selection opens setup rather than attempting to use an empty endpoint. The model/provider picker has the same recovery behavior.

## Configuration contract

The Custom slot contains `compatibility`, `base_url`, `api_key_env`, optional inline `api_key`, `model`, and `temperature`.

- Compatibility is either `openai` or `anthropic`.
- The base URL is normalized from an HTTP(S) origin or a path ending in `/v1`; any accepted path prefix is preserved. Credentials, query strings, fragments, and final endpoint paths are rejected.
- The API-key environment-variable name is editable but must use a portable shell-variable format. `CUSTOM_PROVIDER_API_KEY` is the default.
- Credential resolution prefers an explicit inline key over the named environment variable. A blank key during editing preserves the existing source. Resolved environment secrets are never materialized into persisted TOML.
- Model IDs are manual and are not dependent on successful model discovery.

## Protocol and activation behavior

- OpenAI-compatible probes call `GET <base path>/models` with Bearer authentication; chat calls `POST <base path>/chat/completions` with streaming and OpenAI tool-call shapes.
- Anthropic-compatible probes send a minimal `POST <base path>/messages` with `x-api-key` and `anthropic-version: 2023-06-01`; chat uses the same endpoint and headers with streaming and Anthropic tool-use shapes.
- Malformed URLs, invalid environment-variable names, and missing credentials block setup.
- Network, authentication, protocol, and endpoint probe failures offer **Retry**, **Save anyway**, or **Cancel**. Save anyway activates the manually configured provider and, during onboarding, completes onboarding.
- Probe errors are sanitized. Empty streaming completions are errors rather than successful turns.
- Model discovery is best-effort and uses the selected protocol's `<base path>/models` behavior; manual IDs remain valid when discovery returns no models. Deterministic local fixtures cover both protocol paths, authentication, parsing, pagination, and provider failures.

## Persistence contract

- First-run onboarding writes the selected Custom fields, default provider, and onboarding-completed flag to global configuration.
- In-session setup and provider activation write the relevant Custom fields and default provider to the current workspace's `.nca/config.local.toml`.
- Global and workspace writes use targeted, syntax-preserving TOML patches rather than serializing the merged runtime configuration. Comments, unknown keys, unrelated provider credentials, and environment-derived secrets are preserved appropriately.
- Missing config files are created minimally. Malformed or unsafe TOML is left unchanged. Writes are atomic.
- Runtime activation is applied before persistence. If construction fails, the prior runtime remains active; if persistence fails after activation, the active runtime remains usable and the user receives a warning.

## Verification

Focused test coverage is present for URL normalization, portable environment-variable names, credential precedence and redaction, protocol probes, sanitized errors, streaming/tool behavior, empty completions, onboarding transitions, TUI setup actions, model discovery, targeted TOML persistence, REPL command-boundary routing, explicit editing, picker recovery, legacy `/custom` activation, and the Rust-native HTML normalization used by the web tools. Protocol adapter tests assert exact endpoint paths, authentication headers, request bodies, message contents, model settings, stream options, tool schemas, and parsed text, usage, and tool-call chunks. Formatting, diff, locked metadata, workspace tests, and benchmark compilation pass. The full benchmark run completes with a few Criterion performance-regression warnings.

## Known follow-ups

- Ticket 05 acceptance coverage is complete, including production event-loop routing for onboarding Custom selection, `/connect`, unconfigured recovery, configured editing, secret preservation, and setup submission.
- Chat-request body assertions, deterministic custom model-discovery fixtures, and full activation/persistence integration coverage are complete.
- The TUI status bar renders `Custom · <sanitized-host>` for an active custom endpoint; credentials and URL paths are excluded.
