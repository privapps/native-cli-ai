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

## Bounded changes

1. Version the repository autoresearch skill while leaving unrelated local
   `.agents` content ignored.
2. Move the `socket_path` declaration into the Unix configuration block so
   Windows builds remain warning-free without changing cancellation behavior.
3. Verify the focused acceptance test, Unix tests, formatting, and a Windows
   target check when the target/toolchain is available.

## Verification

- `cargo test -p nca-core --test autoresearch_agent_acceptance` — passed (5).
- `cargo check -p nca-cli` — passed.
- `cargo fmt --all -- --check` — passed.
- `cargo test --workspace` — passed.
- Windows target check was attempted, but this environment does not have the
  `x86_64-pc-windows-gnu` standard library installed.

## Acceptance criteria

- A clean checkout discovers the `autoresearch` skill and marks it
  manual-only.
- `nca-cli` compiles with warnings denied on Unix and Windows.
- Existing cancellation behavior and focused tests remain green.
