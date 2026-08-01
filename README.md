# nca

`nca` is a Rust-native, terminal-first AI assistant for research, writing,
planning, analysis, coding, and tool-driven workflows. It ships as one binary
with a full-screen TUI, line-oriented REPL, one-shot runs, detached sessions,
JSON/NDJSON output, IPC, and optional worktree-isolated subagents.

## Quick start

Install a release on macOS or Linux:

```bash
curl -fsSL https://nca-cli.com/install | bash
```

Or build from source with a recent Rust toolchain:

```bash
cargo build --release
cp target/release/nca /usr/local/bin/
```

Configure the default MiniMax provider and start a session:

```bash
export MINIMAX_API_KEY="your-api-key"
nca
```

Useful non-interactive forms:

```bash
nca --no-tui
nca run --prompt "Explain this repository"
nca spawn --prompt "Inspect the repository and draft a plan"
nca sessions
nca status <session-id>
nca attach <session-id>
```

Windows users can download the matching archive from the [GitHub Releases
page](https://github.com/madebyaris/native-cli-ai/releases).

## Documentation

The [documentation hub](docs/README.md) is the authoritative map:

- [User guide](docs/user/index.md) — installation, commands, configuration,
  providers, tools, sessions, permissions, and skills.
- [CLI reference](docs/reference/cli-reference.md) — generated top-level
  commands and options.
- [Technical reference](docs/reference/index.md) — architecture, persistence,
  orchestration, context management, and technology choices.
- [Design documentation](docs/design/index.md) — active plans and
  specifications, plus explicitly archived history.
- [Product roadmap](docs/product/index.md) — product scope and current work.

### Workspace instructions and skills

`nca` reads a workspace-root `AGENTS.md` as additive project guidance for
every model turn. It remains separate from the built-in safety rules and from
the configurable `.ncarc` and `.nca/instructions.md` layers. The prompt keeps
these sources in a stable order, followed by skill summaries and optional
orchestration context; refreshing a session updates the instruction block
without duplicating the conversation history.

The same `AGENTS.md` can expose reusable skills through its root-level
`##` sections. Those skills appear in `nca skills` and the TUI skill surfaces
with their source identified as `AGENTS.md`; they are only executed after an
explicit invocation. Directory-based skills remain available through the
workspace and configured skill directories.

## Project shape

The workspace is split into focused Rust crates:

| Crate | Responsibility |
|---|---|
| `crates/common` | Shared config, events, sessions, messages, and tool types |
| `crates/core` | Agent loop, providers, harness, skills, and tools |
| `crates/runtime` | Session lifecycle, IPC, persistence, worktrees, and supervision |
| `crates/tui` | Full-screen TUI, line REPL, overlays, and transcript rendering |
| `crates/cli` | `nca` entrypoint, argument parsing, and stream/lifecycle commands |
| `crates/autoresearch` | Metric-driven research helpers |

The product surface is the single `nca` binary. It is intentionally native
Rust; JavaScript, Node, Electron, and web wrappers are not runtime
dependencies.

## Contributing

Run the relevant checks before handing off changes:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
make docs-check
```

See the repository documentation and existing design plans for contribution
conventions. The project is licensed under MIT.
