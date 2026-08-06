# Tech Stack

This document records every dependency choice, the rationale behind it, and the alternatives that were evaluated and rejected.

---

## Runtime and Async

| Crate | Version | Role |
|-------|---------|------|
| `tokio` | 1.x | Async runtime for all I/O: network, filesystem, PTY, IPC |

**Why tokio**: De-facto standard, best ecosystem support (reqwest, tonic, tower all assume tokio). No realistic alternative for a project this broad.

**Rejected**: `async-std` (smaller ecosystem, fewer integrations), `smol` (too minimal for our needs).

---

## CLI and Argument Parsing

| Crate | Version | Role |
|-------|---------|------|
| `clap` | 4.x | CLI argument parsing with derive macros |

**Why clap**: Mature, well-documented, supports subcommands, env vars, config layering, and shell completions out of the box.

**Rejected**: `argh` (simpler but less flexible), `structopt` (merged into clap 3+).

---

## Terminal UI

| Crate | Version | Role |
|-------|---------|------|
| `ratatui` | 0.30.x | Widget-based terminal UI framework |
| `crossterm` | 0.29.x | Cross-platform terminal backend (events, raw mode, colors) |
| `reedline` | 0.49.x | Line editor with history, completions, and hints |
| `arboard` | 3.x | Clipboard access for pasting bitmap images into the TUI composer |
| `image` | 0.25.x | Encode clipboard RGBA payloads to PNG for session attachments |

**Why ratatui + crossterm**: ratatui is the most actively maintained TUI framework in Rust (19M+ downloads). crossterm is its default backend and works on Windows, macOS, and Linux without external dependencies.

**Why reedline**: Provides a nushell-quality line editor with multi-line input, syntax highlighting hooks, history search, and tab completion. Better UX than raw crossterm input handling.

**Clipboard compatibility**: `arboard` remains a required default capability
because the fullscreen TUI supports both copying assistant text and importing
bitmap clipboard images. Its platform backends are retained intentionally;
headless or unavailable backends return an actionable error and the user can
use native terminal Shift+drag selection or `/image <path>` as fallbacks.

**Rejected**: `tui-rs` (unmaintained, ratatui is its successor), `termion` (Linux-only), `cursive` (higher-level but less flexible for custom layouts).

---

## Terminal Rendering

| Crate | Version | Role |
|-------|---------|------|
| `syntect` | 5.x | Syntax highlighting for code blocks |
| `pulldown-cmark` | 0.12.x | Markdown parsing |
| `colored` | 3.x | Simple ANSI color helpers for non-TUI output |

**Why these crates**: `pulldown-cmark` provides a small, predictable Markdown
parser, `syntect` handles syntax highlighting without a browser runtime, and
`colored` keeps the non-TUI stream renderer lightweight.

**Rejected**: browser/WebView rendering (adds a large runtime and conflicts
with the native CLI goal), and a full Markdown renderer in the terminal (too
much layout policy for the current output surfaces).

---

## LLM Provider Abstraction

| Crate | Version | Role |
|-------|---------|------|
| `reqwest` | 0.12.x | Rustls-only HTTP transport used by the provider adapters |

**Strategy**: MiniMax is the first-class provider and the supported provider
adapters use the repository's direct Rust implementations over a shared,
Rustls-only `reqwest` configuration. This keeps auth, base URLs, model naming,
streaming, and tool-call normalization under nca's control without compiling an
unused generic provider stack.

**Current default**: `MiniMax-M2.5` via `provider.minimax` config and `MINIMAX_API_KEY`.

**Rejected**: `genai` and provider-specific client crates (they add unused
transitive stacks and duplicate transport configuration), `llm` (focused on
local models), and rolling our own HTTP client (unnecessary duplication).

---

## MCP (Model Context Protocol)

MCP tool discovery and invocation currently use a small Rust-native stdio client in
`crates/core/src/tools/mcp.rs`. There is no external MCP crate dependency; broader
transport support remains deferred.

---

## HTTP

| Crate | Version | Role |
|-------|---------|------|
| `reqwest` | 0.12.x | HTTP client for direct API calls and fallback |

**Why reqwest**: Tokio-native, supports streaming responses, TLS, and
connection pooling. The workspace explicitly selects `rustls-tls` and disables
default features so the production HTTP graph has no OpenSSL/native-TLS path.

**Rejected**: a provider-specific HTTP client (would duplicate transport code)
and `ureq` (synchronous, which does not fit the streaming agent loop).

---

## PTY and Process Management

| Crate | Version | Role |
|-------|---------|------|
| `portable-pty` | 0.9.x | Cross-platform pseudo-terminal interface |
| `tokio-process` | (via tokio) | Async child process spawning |

**Why portable-pty**: Part of the WezTerm project, battle-tested across platforms. Provides the PTY abstraction needed for sandboxed bash execution and terminal capture.

**Rejected**: Raw `std::process::Command` (no PTY support, can't capture terminal output properly), `pty` crate (unmaintained).

---

## Tmux Integration

| Crate | Version | Role |
|-------|---------|------|
| `tmux_interface` | 0.3.x | Programmatic tmux control via CLI |

**Why tmux_interface**: Most mature Rust tmux library (62K downloads), supports tmux 0.8 through 3.4.

**Deferred**: Phase 3. The adapter will wrap this crate behind a trait so we can support other multiplexers later.

**Rejected**: `tmux-lib` (newer but less proven), shelling out directly (fragile, hard to test).

---

## Serialization and Config

| Crate | Version | Role |
|-------|---------|------|
| `serde` | 1.x | Serialization framework |
| `serde_json` | 1.x | JSON for API payloads, session files, IPC messages |
| `toml` | 1.1.x | TOML for config files |

**Why these crates**: Serde gives all crates one typed serialization model,
`serde_json` matches the provider, session, and IPC formats, and `toml` keeps
human-edited configuration readable.

**Rejected**: YAML (less aligned with the existing config and wire formats) and
hand-written serializers (more code and weaker compatibility guarantees).

---

## Local Persistence

Session state, event streams, memory, and CLI indexes are stored as JSON and JSONL under the unified product home (`$NCA_HOME`, `$XDG_DATA_HOME/ncacli`, or `~/.local/share/ncacli/`) keyed by workspace ID. Workspace-local `.nca/` data is limited to configuration overrides, instructions, skills, and git worktrees; legacy session data may be copied forward during migration. Global user config lives at `<product-home>/config.toml`, with `~/.nca/config.toml` retained as a read fallback. No separate SQLite control-plane database is used in this workspace.

| Crate | Version | Role |
|-------|---------|------|
| `toml_edit` | 0.25.x | Syntax-preserving provider-config patches that retain comments and unknown keys |
| `url` | 2.x | Strict HTTP(S) endpoint parsing and custom-provider URL normalization |

**Why these crates**: Provider setup must update only owned TOML fields while preserving surrounding user-authored content, and URL validation should use a standards-compliant parser rather than string heuristics.

**Rejected**: Serializing the merged config (would leak environment-derived values and discard comments), and ad-hoc URL parsing (would accept malformed endpoint forms).

---

## Filesystem and Search

**Code search**: The current implementation shells out to `rg` (ripgrep) and consumes `--json` output so the tool layer can return structured search matches. This keeps search fast and local without committing to a persistent index yet.

**Editing precision**: File editing remains exact-text based, but the tool layer now distinguishes between ambiguous multi-match replacements and precise match-targeted edits (`replace_match` by path/line/column). We intentionally improved the tool contract before attempting a bespoke local regex index, which matches the current repo scale and keeps the implementation simpler than trigram-index systems such as those described by Cursor, Zoekt, or GitHub Blackbird.

---

## Diff and Patching

| Crate | Version | Role |
|-------|---------|------|
| `similar` | 2.x | Unified diff generation |

**Why similar**: It is a focused, pure-Rust library that produces the unified
diff representation required by the CLI and tool results without invoking a
system executable.

**Rejected**: `diffy` (less actively maintained), `diff` (older API).

---

## Git

| Crate | Version | Role |
|-------|---------|------|
| `gix` | latest | Pure-Rust Git implementation |

**Deferred**: Phase 3. Used for branch detection, status, and commit operations.

**Rejected**: `git2` (libgit2 C binding -- works but adds a C dependency; `gix` keeps us pure Rust).

---

## Error Handling

| Crate | Version | Role |
|-------|---------|------|
| `thiserror` | 2.x | Derive macros for error types |
| `anyhow` | 1.x | Ergonomic error propagation in application code |

**Convention**: Library crates (`core`, `common`, `runtime`) use `thiserror` for typed errors. The application crate (`cli`) uses `anyhow` for convenience.

**Rejected**: string-only errors (poor machine-readable context) and one error
crate everywhere (would either make library errors too opaque or application
propagation too verbose).

---

## Logging and Tracing

| Crate | Version | Role |
|-------|---------|------|
| `tracing` | 0.1.x | Structured, async-aware instrumentation |
| `tracing-subscriber` | 0.3.x | Log formatting and filtering |

**Why tracing**: Structured spans and fields work across asynchronous tasks and
can be filtered without coupling product output to one logging backend.

**Rejected**: `log` alone (less structured context for session/tool spans) and
direct `println!` diagnostics (would corrupt JSON/NDJSON output).

---

## Testing

| Crate | Version | Role |
|-------|---------|------|
| `tempfile` | 3.x | Temporary directories for sandboxed test fixtures |
| `assert_cmd` | 2.x | CLI integration testing |
| `insta` | 1.x | Snapshot testing for rendered output |

**Why these crates**: They provide isolated filesystem fixtures, process-level
CLI assertions, and stable rendering regressions with minimal test harness
code.

**Rejected**: tests that write into the real home directory (unsafe and flaky)
and hand-maintained output comparisons (high maintenance for terminal layouts).

---

## Summary: Dependency Tree by Crate

```
crates/common   -> serde, serde_json, toml, toml_edit, url, thiserror, tracing
crates/core     -> common, autoresearch, reqwest (Rustls-only), serde_json,
                   thiserror, tracing, tokio, regex
crates/tui      -> common, core, runtime, ratatui, crossterm, reedline,
                   syntect, pulldown-cmark, arboard, image
crates/cli      -> common, core, runtime, tui, clap, serde_json, anyhow,
                   tracing, tokio
crates/runtime  -> common, core, portable-pty, tokio, serde_json, tracing,
                   thiserror
```
