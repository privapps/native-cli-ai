# `$`-Prefixed Skill References

**Type:** Implementation plan
**Status:** Completed locally
**Date:** 2026-08-03
**Related:** [skill reference proposal](../../../skill_p.md)
**Last verified:** 2026-08-03 (workspace tests, Clippy, formatting, and diff checks)

## Objective

Support exact `$skill` references in interactive prompts. A reference selects
bounded local skill guidance for the model; it does not execute the skill or
change tool permissions.

## Bounded implementation steps

1. Keep parsing, exact catalog resolution, escaping, deduplication, loaded
   context, and content limits in the pure `nca-tui` skill-reference utility.
2. Apply the utility before file-mention expansion in the line REPL and
   fullscreen composer, marking context loaded only after a successful turn.
3. Add `$` completion that uses the shared skill catalog while preserving
   slash-command and `@`-file completion behavior.
4. Add focused parser, completion, and integration tests; run formatting,
   focused tests, Clippy, and the workspace test suite as practical.

## Invariants

- Unknown, shell-like, path-like, and escaped `$` forms remain ordinary text.
- Exact references are deduplicated in first-appearance order.
- Skill bodies are labeled as untrusted task guidance and bounded per skill and
  in aggregate.
- Failed turns do not mark selected skill bodies as loaded.
- A `$` reference never directly executes a skill or grants permission.

## Verification

- `cargo fmt --all -- --check`
- `cargo clippy -p nca-tui --all-targets -- -D warnings`
- `cargo test -p nca-tui --lib`
- `cargo test --workspace`
- `git diff --check`
