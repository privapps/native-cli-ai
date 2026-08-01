# Documentation Organization Implementation Plan

**Spec:** `docs/design/specs/2026-07-31-documentation-organization.md`
**Status:** Completed
**Date:** 2026-07-31
**Type:** Implementation plan
**Related:** [Documentation organization specification](../specs/2026-07-31-documentation-organization.md)
**Last verified:** 2026-07-31 (documentation review)

## Steps

1. Establish the canonical documentation taxonomy and landing-page indexes.
2. Move user, product, reference, design-plan, and design-spec content into the canonical areas.
3. Leave compatibility stubs at public legacy paths and update repository links to canonical destinations.
4. Add lifecycle metadata and reconcile duplicate or stale planning/navigation content where the current repository makes the authority clear.
5. Update maintainer documentation rules and add a Rust-native repository documentation validation check.
6. Run the validation check, Markdown/link checks, and the relevant workspace tests; inspect the final diff without staging or committing.

## Verification

- `make docs-reference` completed and regenerated the top-level CLI reference from Clap help output.
- `make docs-check` passed with 95 Markdown files checked.
- `cargo fmt --all -- --check` passed.
- `rustfmt --edition 2024 --check tools/generate_cli_reference.rs tools/validate_docs.rs` passed.
- `cargo test --workspace` passed.
- No branch, staging, or commit operation was performed.

## Boundaries

- Do not change nca runtime behavior or public Rust APIs.
- Do not modify unrelated existing worktree changes.
- Do not create branches, stage files, or commit.
- Preserve legacy documentation paths for at least one release cycle through compatibility stubs.
