# Providers

nca supports five LLM provider backends. You can switch between them at any time via config, environment variables, or the interactive `/connect` and `/provider` commands.

## Supported Providers

| Provider | Default Model | API Style | Description |
|----------|---------------|-----------|-------------|
| **MiniMax** | `MiniMax-M2.5` | Anthropic-compatible | Primary provider. Uses the MiniMax Anthropic-compatible endpoint. |
| **Anthropic** | `claude-3-7-sonnet-latest` | Native Anthropic | Direct Anthropic API for Claude models. |
| **OpenAI** | `gpt-4o-mini` | OpenAI Chat | Standard OpenAI chat completions API. |
| **OpenRouter** | `openai/gpt-4o-mini` | OpenAI-compatible | Aggregator providing access to 100+ models from multiple providers. |
| **Custom** | `custom-model` | OpenAI Chat, OpenAI Responses, or Anthropic-compatible | Bring your own endpoint and choose the wire protocol. |

## MiniMax (Default)

MiniMax is nca's primary provider, using an Anthropic-compatible API endpoint.

### Setup

```bash
export MINIMAX_API_KEY="your-minimax-api-key"
```

Or in config:

```toml
# ~/.local/share/ncacli/config.toml
[provider]
default = "minimax"

[provider.minimax]
api_key = "your-key"
base_url = "https://api.minimax.io/anthropic"
model = "MiniMax-M2.5"
temperature = 0.7
```

### Features

- Anthropic-compatible protocol (`/v1/messages` endpoint)
- Extended thinking support
- Native vision/image processing
- Streaming responses

## Anthropic

Direct access to Claude models via the native Anthropic API.

### Setup

```bash
export ANTHROPIC_API_KEY="your-anthropic-key"
```

```toml
[provider]
default = "anthropic"

[provider.anthropic]
api_key_env = "ANTHROPIC_API_KEY"
base_url = "https://api.anthropic.com"
model = "claude-3-7-sonnet-latest"
temperature = 1.0
```

## OpenAI

Standard OpenAI chat completions API.

### Setup

```bash
export OPENAI_API_KEY="your-openai-key"
```

```toml
[provider]
default = "openai"

[provider.openai]
api_key_env = "OPENAI_API_KEY"
base_url = "https://api.openai.com"
model = "gpt-4o-mini"
temperature = 0.7
```

### OpenAI-Compatible Endpoints

You can point the OpenAI provider at any OpenAI-compatible API by changing `base_url`:

```toml
[provider.openai]
base_url = "https://my-local-llm:8080"
model = "local-model"
```

## OpenRouter

Access to hundreds of models from multiple providers through a single API key.

### Setup

```bash
export OPENROUTER_API_KEY="your-openrouter-key"
```

```toml
[provider]
default = "openrouter"

[provider.openrouter]
api_key_env = "OPENROUTER_API_KEY"
base_url = "https://openrouter.ai/api"
model = "openai/gpt-4o-mini"
temperature = 0.7
site_url = "https://my-app.com"    # Optional
app_name = "my-app"                # Optional
```

### Model Format

OpenRouter uses `provider/model` naming:

```
openai/gpt-4o
anthropic/claude-3-7-sonnet
google/gemini-2.0-flash
meta-llama/llama-3.1-70b-instruct
```

## Custom Provider

Use the single `Custom` provider slot when you want `nca` to talk to a non-built-in endpoint such as a self-hosted gateway or a third-party OpenAI-compatible / Anthropic-compatible service. The slot stores one endpoint, one compatibility choice, one API-key environment-variable name, and one model ID.

### Setup

```toml
[provider]
default = "custom"

[provider.custom]
compatibility = "openai"   # or "openai-responses" or "anthropic"
base_url = "https://sumopod.example"
api_key_env = "CUSTOM_PROVIDER_API_KEY"
model = "my-model"
temperature = 0.7  # Retained for other protocols; omitted from Responses
```

`base_url` must be an HTTP(S) origin, optionally with a path ending in `/v1` (for example, `https://opencode.ai/zen/v1`). `nca` preserves that path prefix and appends the protocol-specific request path. Credentials, query strings, fragments, and final request paths such as `/v1/models` are rejected.

The default API-key environment-variable name is `CUSTOM_PROVIDER_API_KEY`, but it is editable in the TUI wizard and in TOML. Environment-variable names must be portable shell names such as `GATEWAY_API_KEY`.

Environment overrides:

```bash
export CUSTOM_PROVIDER_API_KEY="your-key"
export CUSTOM_PROVIDER_BASE_URL="https://sumopod.example"
export CUSTOM_PROVIDER_MODEL="my-model"
export CUSTOM_PROVIDER_COMPATIBILITY="openai"
```

API-key resolution uses an explicit `api_key` in `[provider.custom]` first, then the environment variable named by `api_key_env`. The TUI never pre-fills an existing secret. When editing, leaving the secret blank preserves the existing inline override (if any) or continues using the environment; entering a secret persists it as an inline override. Environment-resolved secrets are not written to TOML.

### Interactive Setup

There are two TUI entry points:

- `/connect` → **Custom** opens configuration directly.
- `/provider` → **Add custom provider…** opens the explicit add/edit wizard. The **Custom (BYO endpoint)** row activates the configured slot.
- `/provider add-custom` opens the same explicit add/edit wizard from a command.

`/provider custom` activates the configured custom slot. If the slot has no endpoint yet, the TUI opens setup instead. Selecting Custom from the model/provider picker follows the same recovery path.

The wizard collects four fields:

1. OpenAI-compatible Chat Completions, OpenAI Responses, or Anthropic-compatible protocol.
2. Base URL.
3. API-key environment-variable name and optional inline secret.
4. Model ID.

Model IDs are entered manually and remain usable even when model discovery is unavailable.

Reasoning effort is configured separately under `[model]`; it is not a field in
the Custom-provider setup wizard.

**Slash command:**

```
/custom openai https://sumopod.example your-key my-model
/custom responses https://responses-gateway.example your-key my-model
/custom anthropic https://my-gateway.example your-key my-model
```

The `/custom` command remains supported for scripts and existing users. Omitting the optional key keeps the existing credential source; it does not copy an environment-resolved secret into the config file.

### Protocol behavior and probes

OpenAI-compatible custom endpoints use:

- `GET <base path>/models` with `Authorization: Bearer …` for the setup probe (`/v1/models` for an origin, or `/zen/v1/models` for the example above).
- `POST <base path>/chat/completions` with Bearer authentication for chat.
- Server-sent events (`stream = true`) and OpenAI tool-call shapes for streaming agent turns.

Anthropic-compatible custom endpoints use:

- A minimal `POST <base path>/messages` probe with `x-api-key` and `anthropic-version: 2023-06-01`.
- `POST <base path>/messages` with the same headers for chat.
- Server-sent events (`stream = true`) and Anthropic tool-use shapes for streaming agent turns.

OpenAI Responses custom endpoints use:

- `GET <base path>/models` with Bearer authentication for the setup probe when the gateway exposes model discovery.
- `POST <base path>/responses` with Bearer authentication, `stream = true`, and `store = false` for agent turns.
- Native Responses input items, function calls, function-call outputs, and SSE events. Existing nca function tools are supported; provider-hosted tools are not.
- Text and `image/png`, `image/jpeg`, `image/webp`, and `image/gif` attachments are mapped to native input blocks. An unreadable image or unsupported media type fails the request explicitly; it is not silently omitted.
- Function-call arguments may arrive in split events and multiple calls retain stream order. nca preserves an explicit `call_id`; when it is absent, a stable function-item ID or `output_index` becomes the internal identity. Rotating event IDs are associated with that stable identity. Missing identity, function name, valid JSON arguments, or a complete output fails explicitly, as do malformed known events, provider failures, invalid UTF-8, and empty completions.
- By default, the complete canonical session history is sent on every request; nca does not use `previous_response_id`. With `[memory.context].smart_compaction_mode = "on"`, only a compact provider-request view is sent while session JSON and canonical agent history remain complete.
- The configured `temperature` setting is retained for compatibility but omitted from Responses requests because model support varies. No retry or fallback request is made for this field.

The TUI performs the cheap protocol-specific probe before activation. Malformed URLs, invalid environment-variable names, and missing credentials are blocking errors. Network, authentication, protocol, and endpoint failures offer **Retry**, **Save anyway**, or **Cancel**. **Save anyway** activates the manually configured provider without requiring model discovery. Probe errors are sanitized before display.

Model discovery is best-effort: OpenAI-compatible and OpenAI Responses endpoints use `<base path>/models` with Bearer authentication, and Anthropic-compatible endpoints use the paginated `<base path>/models` API with Anthropic headers. Successful responses must have the expected model-catalog shape; malformed, empty, unauthorized, or unavailable catalogs fall back to the existing static/empty result behavior. A failed discovery request does not prevent a manually entered model ID from being used.

Provider capabilities are dispatched through one runtime seam. Settings access (selected model,
base URL, API-key environment name, and credential presence) is centralized, while model catalogs
and context-window lookups use protocol-aware capability adapters. Custom OpenAI and Anthropic
compatibilities select their matching capability adapter, preserving path prefixes and authentication. Cache
identity includes provider, normalized endpoint, compatibility, model where relevant, and a
non-reversible credential tag; raw credentials are never logged or cached. Remote failures and
missing credentials retain the static context-limit or empty-catalog fallback.

### Persistence

- First-run onboarding saves the selected Custom provider fields, default-provider selection, and onboarding-completed flag to the global config (`~/.local/share/ncacli/config.toml`, subject to the product-home overrides).
- In-session setup and provider activation save only the relevant Custom provider fields and default-provider selection to `.nca/config.local.toml` in the current workspace.
- Persistence uses targeted, syntax-preserving TOML patches. Existing comments, unknown keys, and unrelated provider credentials are retained; missing files receive only the necessary sections.
- Writes are atomic. Malformed or structurally unsafe TOML is left untouched and reported as a persistence warning. Runtime activation remains active if a later save fails.

Notes:

- `compatibility = "openai"` selects the OpenAI-compatible wire format.
- `compatibility = "openai-responses"` selects the OpenAI Responses wire format. CLI aliases include `responses`, `openai-responses`, and `openai_responses`.
- `compatibility = "anthropic"` selects the Anthropic-compatible wire format.
- After setup, use `/model <name>` to switch the active model ID.
- Press `c` in the provider picker for a quick-reference help card

## Reasoning Effort

OpenAI-compatible Chat Completions and OpenAI Responses models can receive a configurable
`reasoning_effort` value. Set it globally in `~/.local/share/ncacli/config.toml`
or the workspace override:

```toml
[model]
reasoning_effort = "high"
```

The value is trimmed and passed through as a string. Use `"none"`, `"low"`,
`"medium"`, `"high"`, `"xhigh"`, or a provider-specific value supported by
your gateway. The literal `"nil"` (the default), an empty value, and a
whitespace-only value omit the JSON property. This setting is sent for
OpenAI, OpenRouter, and Custom providers with `compatibility = "openai"` or
`compatibility = "openai-responses"`; Responses sends it as nested
`reasoning.effort`. It is not sent for MiniMax, Anthropic, or Custom providers with
`compatibility = "anthropic"`.

For a single invocation, use the global CLI override:

```bash
nca --reasoning-effort low --prompt "review this code"
nca --reasoning-effort nil --prompt "use the provider default"
```

The override is not persisted. In an interactive session, use
`/reasoning-effort <value>` to persist a workspace setting, or
`/reasoning-effort` to display the current value and whether the active
provider supports it. `reasoning_effort` remains independent from extended
thinking settings and temperature.

## Switching Providers

### Via CLI Flag

```bash
nca --model "claude-3-7-sonnet-latest"
```

### Via Environment Variable

```bash
NCA_DEFAULT_PROVIDER=anthropic nca
NCA_MODEL=gpt-4o nca
```

### Via Interactive Commands

```
/connect           # Opens provider picker UI
/provider openai   # Switch default provider
/provider custom   # Use the configured custom endpoint
/provider          # TUI picker — select "Add custom provider…" to configure
/provider add-custom # Open the custom endpoint wizard directly
/custom openai https://sumopod.example your-key my-model
/model gpt-4o      # Switch model
/models            # Browse available models
```

### Via Config

```toml
[provider]
default = "anthropic"
```

## Model Aliases

nca ships with built-in model aliases for quick switching:

| Alias | Resolves To |
|-------|-------------|
| `default` | `MiniMax-M2.5` |
| `minimax` | `MiniMax-M2.5` |
| `m2.5` | `MiniMax-M2.5` |
| `coding` | `MiniMax-M2.5` |
| `reasoning` | `MiniMax-M2.5` |
| `openai` | `gpt-4o-mini` |
| `gpt4o` | `gpt-4o` |
| `claude` | `claude-3-7-sonnet-latest` |
| `openrouter` | `openai/gpt-4o-mini` |

Add custom aliases in config:

```toml
[model.aliases]
fast = "gpt-4o-mini"
smart = "claude-3-7-sonnet-latest"
local = "ollama/llama3"
```

Use aliases anywhere a model name is expected:

```bash
nca --model fast
```

```
/model smart
```

## API Key Resolution

For each provider, the API key is resolved in this order:

1. **`api_key`** field in config (not recommended for security)
2. **`api_key_env`** — read from the named environment variable (default and recommended)

The default environment variable names are:

| Provider | Variable |
|----------|----------|
| MiniMax | `MINIMAX_API_KEY` |
| Anthropic | `ANTHROPIC_API_KEY` |
| OpenAI | `OPENAI_API_KEY` |
| OpenRouter | `OPENROUTER_API_KEY` |
| Custom | The editable `api_key_env` value; defaults to `CUSTOM_PROVIDER_API_KEY` |

You can change the Custom environment variable name via `api_key_env` in config or through the TUI setup wizard.

## Extended Thinking

Some models support extended thinking (chain-of-thought reasoning). Enable it with:

```bash
nca -t                        # Enable with default budget (5120 tokens)
nca -t --thinking-budget 10000  # Custom budget
```

Or in config:

```toml
[model]
enable_thinking = true
thinking_budget = 10000
```

Toggle visibility in an interactive session:

```
/thinking
```

## Context Window Management

nca auto-detects context window sizes by querying the provider's model API. This enables automatic context compaction when the conversation gets too long.

```toml
[memory.context]
auto_detect_context_window = true
query_provider_models_api = true
max_retained_messages = 50
auto_summarize_threshold = 75     # Trigger at 75% of context window
enable_auto_summarize = true
```

Disable provider API queries if needed:

```bash
export NCA_SKIP_CONTEXT_API=1
```
