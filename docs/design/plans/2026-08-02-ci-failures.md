# CI failure fixes

**Type:** Implementation plan
**Status:** Complete
**Date:** 2026-08-02
**Related:** None
**Last verified:** 2026-08-02

## Diagnosis

- The autoresearch acceptance test passes locally because
  `.agents/skills/autoresearch/SKILL.md` exists in the workspace, but the
  entire `.agents` directory is ignored and the skill is not tracked. A clean
  GitHub checkout cannot discover it.
- `cancel_session` declares `socket_path` unconditionally, but only uses it in
  a Unix-only cleanup block. Windows treats the declaration as unused under
  `-D warnings`.
- The previous commit added an ignore exception but did not contain the
  ignored skill file, so a clean checkout still lacks autoresearch.
- The Windows CLI test process overflows the default Windows main-thread stack
  while polling the large async CLI dispatcher; both valid and invalid
  autoresearch commands fail before command-specific output.

## Bounded changes

1. Ship autoresearch as a tracked built-in skill while leaving unrelated local
   `.agents` content ignored.
2. Run the CLI dispatcher on an explicitly sized stack suitable for Windows.
3. Verify the focused acceptance test, CLI lifecycle tests, formatting, and
   workspace tests.

## Verification

- `cargo test -p nca-core --test autoresearch_agent_acceptance` — passed (5).
- `cargo check -p nca-cli` — passed.
- `cargo fmt --all -- --check` — passed.
- `cargo test --workspace` — passed.
- Windows target check was attempted, but this environment does not have the
  `x86_64-pc-windows-gnu` standard library installed.
- `cargo clippy --workspace -- -D warnings` — passed.
- `cargo test -p nca-cli --test autoresearch_commands` — passed (2).

The Windows stack-overflow fix cannot be executed on this host, but the CLI
entrypoint now uses the same explicit stack size on every platform, and the
affected process-level tests pass on Unix.

## Acceptance criteria

- A clean checkout discovers the `autoresearch` skill and marks it
  manual-only.
- `nca-cli` compiles with warnings denied on Unix and Windows.
- Existing cancellation behavior and focused tests remain green.
