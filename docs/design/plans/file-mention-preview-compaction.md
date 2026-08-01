# File Mention Preview Compaction Plan

**Type:** Implementation plan
**Status:** Implemented locally
**Date:** Unknown (legacy document)
**Related:** None
**Last verified:** 2026-07-31 (documentation review)

## Goal

Keep expanded `@file` context in the model payload while showing a compact, human-friendly preview in the transcript.

## Implementation

- Add a shared preview compactor that rewrites expanded ````file:path` fences back to `@path`.
- Use that compacted preview for user `MessageReceived` events.
- Keep assistant/tool previews unchanged.

## Validation

- `cargo test -p nca-common`
- `cargo test -p nca-cli`
- `cargo build --release`
