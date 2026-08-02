# Configuration

nca uses a layered TOML configuration system with environment variable overrides.

## Config File Locations

| File | Scope | Description |
|------|-------|-------------|
| `~/.local/share/ncacli/config.toml` (or `$NCA_HOME` / `$XDG_DATA_HOME/ncacli`) | Global | User-wide defaults; falls back to reading `~/.nca/config.toml` if missing |
| `<workspace>/.nca/config.local.toml` | Workspace | Project-specific overrides |

Settings merge in order: **defaults → global → workspace → environment variables**. Later sources override earlier ones.

## Full Configuration Reference

### `[provider]` — LLM Provider Settings

```toml
[provider]
default = "minimax"   # "minimax" | "openrouter" | "anthropic" | "openai"

[provider.minimax]
api_key_env = "MINIMAX_API_KEY"     # Environment variable to read
api_key = ""                         # Or set directly (not recommended)
base_url = "https://api.minimax.io/anthropic"
model = "MiniMax-M2.5"
temperature = 0.7

[provider.openai]
api_key_env = "OPENAI_API_KEY"
base_url = "https://api.openai.com"
model = "gpt-4o-mini"
temperature = 0.7

[provider.anthropic]
api_key_env = "ANTHROPIC_API_KEY"
base_url = "https://api.anthropic.com"
model = "claude-3-7-sonnet-latest"
temperature = 1.0

[provider.openrouter]
api_key_env = "OPENROUTER_API_KEY"
base_url = "https://openrouter.ai/api"
model = "openai/gpt-4o-mini"
temperature = 0.7
site_url = ""       # Optional referrer URL
app_name = ""       # Optional app name header

[provider.custom]
compatibility = "openai"   # "openai" | "openai-responses" | "anthropic"
base_url = "https://gateway.example"
api_key_env = "CUSTOM_PROVIDER_API_KEY"
model = "gateway-model"
temperature = 0.7           # Omitted from Responses requests
```

### `[model]` — Model Settings

```toml
[model]
default_model = "MiniMax-M2.5"
max_tokens = 8192
enable_thinking = false
thinking_budget = 5120
reasoning_effort = "nil"   # nil/empty = omit; otherwise send to OpenAI Chat/Responses providers

[model.aliases]
# Built-in aliases (pre-configured):
# default    → MiniMax-M2.5
# minimax    → MiniMax-M2.5
# m2.5       → MiniMax-M2.5
# coding     → MiniMax-M2.5
# reasoning  → MiniMax-M2.5
# openai     → gpt-4o-mini
# gpt4o      → gpt-4o
# claude     → claude-3-7-sonnet-latest
# openrouter → openai/gpt-4o-mini

# Add your own:
fast = "gpt-4o-mini"
smart = "claude-3-7-sonnet-latest"
```

`model.reasoning_effort` is a string passed to OpenAI, OpenRouter, and Custom
providers configured with `compatibility = "openai"` or
`compatibility = "openai-responses"`. Surrounding whitespace
is trimmed. The default sentinel `"nil"` (case-sensitive), an empty string,
or a whitespace-only value omits the `reasoning_effort` JSON property entirely.
Other values such as `"none"`, `"low"`, `"medium"`, `"high"`, `"xhigh"`, or a
gateway-specific value are sent unchanged. Responses providers receive the
value as `reasoning.effort`. The setting is independent of
`enable_thinking`, `thinking_budget`, and `temperature`, and is not sent to
MiniMax, Anthropic, or a Custom provider using the Anthropic-compatible
protocol.

Custom providers using `compatibility = "openai-responses"` retain the shared
`temperature` setting for configuration compatibility, but omit it from
Responses requests because model support varies. This keeps models that reject
the parameter usable; `max_tokens` is sent as `max_output_tokens`.

### Custom provider request diagnostics

Set `NCA_DEBUG_REQUEST=1` to emit request-only diagnostics for custom providers
using `compatibility = "openai"` or `compatibility = "openai-responses"` to
stderr and append the same record to `./debug.log`. Each timestamped record
contains the final HTTP method, URL, redacted headers, and pretty-printed JSON
request body. The log is append-only across requests and runs.

Only the exact value `1` enables this behavior. Custom Anthropic requests are
not logged. Responses, streamed events, provider errors, and completion output
are never emitted to stderr or written to the file. Authorization and API-key
values are redacted, but request bodies can still contain prompts, file
contents, images, and tool schemas, so protect `debug.log` appropriately. If
the file cannot be written, nca warns on stderr and continues the provider
request.

### `[permissions]` — Permission System

```toml
[permissions]
mode = "default"   # "default" | "plan" | "accept-edits" | "dont-ask" | "bypass-permissions"

# Pattern-based allow/deny lists (supports wildcards)
allow = []         # e.g., ["execute_bash:cargo *", "write_file:src/*"]
deny = []          # e.g., ["execute_bash:rm *", "delete_path:*"]
ask = []           # Force ask for specific patterns
```

See [Permissions](./permissions.md) for full details on each mode.

### `[session]` — Session Management

```toml
[session]
# Default ".nca/sessions" / ".nca/.last_session" sentinels resolve under
# ~/.local/share/ncacli/workspaces/<workspace-id>/. Other relative paths stay workspace-local.
history_dir = ".nca/sessions"
max_turns_per_run = 128             # Max agent turns per session run
max_tool_calls_per_turn = 200       # Max tool calls in a single turn
max_goal_iterations = 20            # Outer turns for a YOLO-only /goal run
checkpoint_interval = 5             # Save checkpoint every N turns
last_session_file = ".nca/.last_session"
auto_compact_on_finish = false      # Auto-summarize when session ends
```

`max_goal_iterations` applies only to the interactive `/goal` loop. It is
independent of the per-turn model/tool budget, defaults to `20` when omitted,
and must be greater than zero.

### `[harness]` — System Prompt and Instructions

```toml
[harness]
built_in_enabled = true                           # Include nca's built-in system prompt
project_instructions_path = ".ncarc"              # Project instructions file
local_instructions_path = ".nca/instructions.md"  # Local (personal) instructions
skill_directories = [".nca/skills", ".claude/skills"]  # Skill discovery paths
```

The harness rebuilds a **dynamic** system prompt each turn from a `HarnessSnapshot`
(workspace cwd, git branch, model, permission mode, capped todos, capped memory notes)
plus static layers (identity, AGENTS.md / project / local instructions, skills, tool playbook).

### `[mcp]` — Model Context Protocol

```toml
[mcp]
expose_in_safe_mode = false   # Allow MCP tools in safe/read-only mode

[[mcp.servers]]
name = "my-server"
command = "npx"
args = ["-y", "@my/mcp-server"]
env = { API_KEY = "..." }
cwd = "/optional/working/directory"
enabled = true
```

### `[memory]` — Persistent Memory

```toml
[memory]
# Default ".nca/memory.json" resolves under ~/.local/share/ncacli/workspaces/<id>/memory.json
file_path = ".nca/memory.json"
max_notes = 128
auto_compact_on_finish = false

[memory.context]
context_window_target = 0              # 0 = auto-detect from provider
auto_detect_context_window = true
query_provider_models_api = true       # Fetch model limits from provider API
max_retained_messages = 50
auto_summarize_threshold = 75          # Percentage of context window used before summarizing
enable_auto_summarize = true
smart_compaction_mode = "off"          # off | dry_run | on (provider-request view only)
```

`smart_compaction_mode` is opt-in and defaults to `off`:

When context auto-detection is enabled, provider catalog values take precedence
over the built-in model table. Unknown models use a 128,000-token fallback for
both context-window sizing and the recommended output limit. The recommended
output limit is model metadata; it does not change the configured
`model.max_tokens` request budget.

| Mode | Behavior |
|------|----------|
| `off` | Send the full canonical history (default). |
| `dry_run` | Compute savings diagnostics and emit `ContextCompaction` with phase `dry_run`, but still send the full history. |
| `on` | Send a compact cloned provider view; session JSON / `AgentLoop.messages` stay complete. |

### `[hooks]` — Lifecycle Hooks

```toml
# Shell commands that run at various lifecycle points
[hooks]
session_start = []
session_end = []
pre_tool_use = []
post_tool_use = []
post_tool_failure = []
approval_requested = []
subagent_start = []
subagent_stop = []
```

Each hook is an object with:

```toml
[[hooks.session_start]]
command = "echo 'session started'"
matcher = ""        # Optional regex to match on
blocking = false    # If true, waits for completion
```

### `[web]` — Web Request Settings

```toml
[web]
timeout_secs = 15
max_fetch_chars = 25000
default_search_limit = 5
search_min_interval_ms = 1000
search_cooldown_ms = 5000
search_max_cooldown_ms = 60000
search_challenge_retries = 3
search_retry_attempts = 1
user_agent = "nca/0.5 (+https://github.com/user/native-cli-ai)"
```

`web_search` searches Bing RSS first and uses DuckDuckGo HTML as a fallback when Bing returns an empty, blocked, malformed, or failed response. The DuckDuckGo fallback has a process-wide serialized limiter shared by runtime sessions, with minimum request-start spacing and bounded anti-bot cooldown/backoff; Bing does not consume that DuckDuckGo limiter. Transport failures and HTTP 408, 425, 429, or 5xx responses use `search_retry_attempts`; recognized DuckDuckGo HTTP 202 anti-bot challenges use `search_challenge_retries` (three retries by default) through the same limiter and bounded cooldown. Empty results and parser failures immediately move to fallback. Both retry settings are capped at 10; when only the legacy `search_challenge_retries` key is present it remains a compatibility alias for transient retries as well. These settings do not limit unrelated tools. Separate nca processes have separate DuckDuckGo limiters. Search requests use the neutral `user_agent` identity; the legacy `search_user_agent` key remains loadable for configuration compatibility but is ignored, so nca does not spoof a browser.

### `[ui]` — Interface Settings

```toml
[ui]
editor = ""              # External editor command (e.g., "vim", "code --wait")
theme = ""               # UI theme (optional)
hide_tips = false         # Hide usage tips
scroll_speed = 3          # Scroll speed in TUI
onboarding_completed = false
```

---

## Environment Variables

Environment variables override config file values.

### Provider Selection and Keys

| Variable | Description |
|----------|-------------|
| `NCA_DEFAULT_PROVIDER` | Override default provider (`minimax`, `openrouter`, `anthropic`, `openai`) |
| `NCA_MODEL` | Override the active model |
| `MINIMAX_API_KEY` | MiniMax API key |
| `MINIMAX_BASE_URL` | MiniMax API base URL |
| `MINIMAX_MODEL` | MiniMax model name |
| `OPENAI_API_KEY` | OpenAI API key |
| `OPENAI_BASE_URL` | OpenAI-compatible base URL |
| `OPENAI_MODEL` | OpenAI model name |
| `ANTHROPIC_API_KEY` | Anthropic API key |
| `ANTHROPIC_BASE_URL` | Anthropic API base URL |
| `ANTHROPIC_MODEL` | Anthropic model name |
| `OPENROUTER_API_KEY` | OpenRouter API key |
| `OPENROUTER_BASE_URL` | OpenRouter base URL |
| `OPENROUTER_MODEL` | OpenRouter model name |
| `OPENROUTER_SITE_URL` | OpenRouter site URL header |
| `OPENROUTER_APP_NAME` | OpenRouter app name header |

### Runtime Behavior

| Variable | Description |
|----------|-------------|
| `NCA_EDITOR` | Override external editor command |
| `NCA_EDITOR_MODE` | Set to `vi` or `vim` for vi keybindings in REPL |
| `NCA_MEMORY_PATH` | Override memory file path |
| `NCA_WEB_TIMEOUT_SECS` | Override web request timeout |
| `NCA_WEB_MAX_FETCH_CHARS` | Override max characters for web fetches |
| `NCA_DEBUG_REQUEST` | Set to `1` to print MiniMax request bodies to stderr or append custom OpenAI-compatible request method, URL, redacted headers, and body to `./debug.log`; response data is never logged |
| `NCA_SKIP_CONTEXT_API` | Set to `1` to skip provider model API queries |
| `NCA_CONTEXT_API_CACHE_TTL_SECS` | Cache TTL for model context API |
| `XDG_RUNTIME_DIR` | IPC socket directory (fallback: `/tmp/nca/`) |

Custom-provider request bodies may contain prompts, file contents, images, and tool schemas; review diagnostic output before sharing it.

### Orchestration (CI/Automation)

| Variable | Description |
|----------|-------------|
| `NCA_ORCH_NAME` | Orchestrator name |
| `NCA_ORCH_RUN_ID` | Orchestration run identifier |
| `NCA_ORCH_TASK_ID` | Task identifier |
| `NCA_ORCH_TASK_REF` | Task reference |
| `NCA_ORCH_PARENT_RUN_ID` | Parent run ID |
| `NCA_ORCH_CALLBACK_URL` | Callback URL for orchestrator |
| `NCA_ORCH_META_*` | Arbitrary metadata (prefix stripped, key lowercased) |

---

## Example Configurations

### Minimal Setup

```toml
# ~/.local/share/ncacli/config.toml
[provider]
default = "minimax"

[provider.minimax]
api_key = "your-key-here"
```

### Multi-Provider Setup

```toml
# ~/.local/share/ncacli/config.toml
[provider]
default = "minimax"

[provider.minimax]
api_key_env = "MINIMAX_API_KEY"

[provider.anthropic]
api_key_env = "ANTHROPIC_API_KEY"

[provider.openai]
api_key_env = "OPENAI_API_KEY"

[model.aliases]
fast = "gpt-4o-mini"
smart = "claude-3-7-sonnet-latest"
default = "MiniMax-M2.5"
```

### CI/Automation Setup

```toml
# .nca/config.local.toml (in the project)
[permissions]
mode = "bypass-permissions"

[session]
max_turns_per_run = 50
auto_compact_on_finish = true
```

### Workspace with Custom Instructions and MCP

```toml
# .nca/config.local.toml
[harness]
project_instructions_path = ".ncarc"
local_instructions_path = ".nca/instructions.md"

[[mcp.servers]]
name = "database"
command = "npx"
args = ["-y", "@modelcontextprotocol/server-postgres"]
env = { DATABASE_URL = "postgresql://localhost/mydb" }
enabled = true
```
