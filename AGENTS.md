# nca Development Instructions

Apply this section only when the request concerns developing, testing, building, documenting, configuring, or deploying the nca product or this repository. Do not apply it to unrelated conversation, research, writing, or other general-purpose tasks merely because the current directory is nca.

- Use Rust-native solutions for nca; do not introduce JavaScript, Node.js, Electron, Tauri, or web wrappers unless the nca task explicitly requires them.
- MiniMax M2.5 is the primary provider; prioritize MiniMax integration, quality, configuration, and diagnostics while preserving provider abstraction.
- The CLI (`nca`) is the product surface: terminal UX, JSON/NDJSON streams, and Unix-socket IPC.
- Keep each crate focused: `common` for shared types, `core` for agent logic, `runtime` for session lifecycle, and `cli`/`tui` for terminal presentation and entrypoint concerns.
- Empty provider completions must fail loudly and never silently succeed.
- For non-trivial nca changes, create a plan document first and implement from it in bounded, testable steps.
- For nca source changes, inspect with `list_directory`, `search_code`, `read_file`, and `query_symbols` before editing.
- Prefer nca edit tools in this order: `replace_match` for an exact search hit, `edit_file` for a unique exact replacement, `apply_patch` for multiple hunks, and `write_file`/`create_directory` only for new paths.
- Prefer efficient algorithms and fast execution; nca is intended to spawn and supervise heavy tasks.
- Sub-agents must use isolated worktrees, with parent/child session lineage visible in session metadata and events.
- User CLI testing may use separate workspace directories such as `test-makan` or `for-test`.
- When explicitly asked to install nca, build with `cargo build --release` and copy `target/release/nca` to `/usr/local/bin/`; do not install as an implicit part of ordinary changes.

# General Agent Behavior

- Inspect only the context relevant to the current request.
- Prefer fast local signals and concrete verification before claiming success.
- State important constraints, risks, assumptions, and verification results plainly.

# Workspace Facts

These facts describe the nca implementation; use them as reference only when the task concerns nca.

- Rust workspace with crates for shared types (`nca-common`), agent logic (`nca-core`), session lifecycle (`nca-runtime`), terminal UI (`nca-tui`), CLI entrypoint (`nca-cli`), and autoresearch helpers.
- IPC between CLI and runtime uses Unix domain sockets with newline-delimited JSON.
- Product home is `$NCA_HOME`, `$XDG_DATA_HOME/ncacli`, or `~/.local/share/ncacli/`; sessions, memory, last-session state, and the CLI index live under `workspaces/<workspace-id>/`.
- Sessions persist as `<id>.json` state plus `<id>.events.jsonl` event logs under the workspace sessions directory.
- MiniMax uses `https://api.minimaxi.chat/v1/text/chatcompletion_v2`.
- Global config is `~/.local/share/ncacli/config.toml`; legacy `~/.nca/config.toml` is read as a fallback.
- Git worktrees for isolated agent runs live at `<repo>/.nca/worktrees/<session-id>`.
- The shipped app is the single `nca` CLI binary.
- Tokio provides the async runtime; `async-trait` supports tool executor and approval handler interfaces.
- Session lineage records parent/child IDs, inherited summaries, and spawn reasons in `SessionMeta`.
- `AgentEvent` is the shared event bus for CLI rendering, IPC broadcast, and disk persistence.
- Runtime sockets default to `$XDG_RUNTIME_DIR/nca/` or `/tmp/nca/`.
- The supervisor builds a `HarnessSnapshot` and refreshes the system prompt each turn.
