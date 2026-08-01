# Validation failures follow-up

**Type:** Implementation plan
**Status:** Completed locally
**Date:** 2026-08-01
**Related:** `.scratch/agents-skills-tui-picker/`, `.scratch/custom-provider-tui/`, `.scratch/to-spec-local-first/`, `.scratch/web-search-failure-handling/`
**Last verified:** 2026-08-01
**Verification:** Focused workflow checks, locked workspace tests, Clippy, formatting, and scoped diff checks pass

## Goal

Bring the reported validation failures to a clean, reproducible state without
modifying unrelated user changes already present in the worktree.

## Bounded steps

1. Align the `to-spec` skill and validator with the canonical local-first
   `.scratch/<feature>/spec.md` workflow, including safe collision handling and
   representative fixture assertions.
2. Normalize the affected ticket triage metadata while preserving completed
   implementation status and dependency order.
3. Inspect the reported workspace-test interruption and make only the
   feature-relevant test/check changes needed for deterministic validation.
4. Adjust validation guidance so pre-existing unrelated dirty-worktree
   whitespace is reported as a limitation rather than attributed to a feature.
5. Run focused workflow, formatting, lint, and test checks; leave prior
   validation entries intact as historical evidence for the pre-fix state.

## Non-goals

- Do not rewrite or clean unrelated staged/unstaged files.
- Do not stage, commit, branch, reset, checkout, or push.
- Do not change the already passing web-search implementation.
