# TUI streamed-display line-break plan

## Problem

The full-screen TUI can show assistant text with line endings that are not
present in the raw response. Copying the same response preserves the expected
text, so the defect is in the Markdown-to-ratatui display path rather than in
stream accumulation or clipboard handling.

## Scope

- Preserve inline Markdown spans on the same terminal line when the source
  paragraph has no line break.
- Keep explicit Markdown soft/hard breaks, paragraphs, lists, and code blocks
  as separate display lines.
- Leave the raw streamed text and clipboard behavior unchanged.

## Bounded steps

1. Add a focused renderer regression using the smallest inline-Markdown sample
   and run it red.
2. Change the renderer's text-event handling so wrapping is performed across
   the current display line rather than forcing a flush at each parser event.
3. Run the focused TUI tests, formatting, and the workspace test suite; inspect
   the final diff to ensure unrelated worktree changes remain untouched.

## Acceptance criteria

- `**nca**` and adjacent plain text render on one terminal line when they fit.
- Source newlines and Markdown block boundaries still produce expected lines.
- The raw response returned by the copy path is unchanged.
- The regression and existing workspace tests pass.
