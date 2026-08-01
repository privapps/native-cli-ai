# Skill Supporting Files Inlining Implementation Plan

This archived plan records the completed work that inlined supporting files
referenced by a skill into the invocation prompt.

## Outcome

- Added the `regex` dependency to `nca-core`.
- Extracted `./path`, `@path`, and backtick-wrapped file references while
  excluding well-known instruction files, URLs, and template paths.
- Deduplicated references while preserving first-occurrence order.
- Resolved references relative to the skill directory, its parent skill root,
  and the current workspace, with path traversal checks.
- Added `Skill::expanded_body()` to inline readable supporting files with clear
  delimiters while leaving unresolved references and the original skill body
  safe and usable.
- Updated `prompt_for_task()` to use the expanded body.

## Verification record

Focused extraction, resolution, expansion, prompt, and traversal tests were
added alongside the implementation. The workspace format, lint, and test
suites available at completion were also run.

This document is retained for historical context; the archived [supporting
files specification](../../specs/archive/2026-03-24-skill-supporting-files-design.md)
contains the behavior contract.

## Historical task outline

1. Add the regular-expression dependency.
2. Implement reference extraction and its focused tests.
3. Implement safe three-level reference resolution and tests.
4. Implement body expansion and tests for missing, binary, and repeated files.
5. Use the expanded body when constructing a skill prompt.
6. Run the full build and test verification.
