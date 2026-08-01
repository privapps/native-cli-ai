# TUI Multiline Input and Atomic Paste

**Type:** Feature specification
**Status:** Implemented — fullscreen and line-oriented acceptance complete
**Date:** 2026-07-31
**Related:** None
**Last verified:** 2026-07-31 (documentation review)

## Problem Statement

Users expect a paragraph or copied document to become one coherent request.
When line breaks are delivered as ordinary Enter events, the current input
surface can submit each physical line independently. This cuts off pasted
content, creates multiple requests, and loses paragraph structure.

The product has two interactive input surfaces with different implementations.
The fullscreen TUI has a native composer and supports a multiline draft,
atomic bracketed paste, modified-Enter newline insertion, display-cell-aware
wrapping, dynamic draft rows, and cursor movement. The line-oriented Reedline
surface enables explicit bracketed-paste handling so a multiline paste cannot
be interpreted as a sequence of submissions.

Session history and prompt history are separate concerns. Canonical session
messages and event logs can preserve multiline content, while Reedline prompt
history is a recall mechanism for submitted drafts. Neither history should
turn physical lines inside one draft into separate user turns.

## Solution

Keep the fullscreen TUI's native composer as the primary multiline editing
experience and preserve its established send muscle memory:

- Enter submits the complete draft once.
- Shift+Enter and Alt+Enter insert a newline without submitting.
- Bracketed paste inserts the complete clipboard payload as one draft edit.
- CRLF and CR line endings normalize to LF while blank lines and paragraph
  boundaries remain intact.
- The composer grows to a bounded number of visible rows and follows the
  cursor after reaching the height cap.

Complete the cross-surface behavior by enabling Reedline's bracketed-paste
mode. Reedline should receive a pasted paragraph as one multiline buffer and
return one successful input when the user submits it. Single-line slash and
shell commands retain their existing behavior; a multiline draft beginning
with either prefix is ordinary user content.

Long-form composition remains available through the existing external editor.
A dedicated modal editor or a third-party textarea is an alternative for a
future redesign, not a prerequisite for safe paragraph paste.

## User Stories

1. As a fullscreen TUI user, I want to insert newlines without submitting, so that I can compose structured prompts.
2. As a fullscreen TUI user, I want Enter to submit the complete draft once, so that normal one-line sending remains fast.
3. As a TUI user, I want a pasted paragraph to remain one draft, so that embedded line breaks cannot create multiple requests.
4. As a TUI user, I want pasted blank lines and paragraph boundaries preserved, so that copied document structure remains meaningful.
5. As a TUI user, I want CRLF and CR input normalized consistently, so that text copied across platforms behaves predictably.
6. As a TUI user, I want pasted text inserted at the cursor, so that I can revise an existing draft rather than only append content.
7. As a TUI user, I want the cursor to move correctly through Unicode and multiline content, so that editing does not corrupt or misplace text.
8. As a TUI user, I want the composer to remain visible and usable as it grows, so that longer drafts do not obscure the transcript or hide the cursor.
9. As a TUI user, I want Up and Down to navigate a non-empty draft while retaining transcript scrolling on an empty draft, so that existing navigation remains useful.
10. As a TUI user, I want single-line slash and shell commands to keep their behavior, so that multiline support does not break command workflows.
11. As a TUI user, I want a multiline draft beginning with `/` or `!` treated as ordinary text, so that copied documents cannot accidentally invoke commands.
12. As a line-oriented REPL user, I want a multiline paste to be inserted atomically, so that pasted paragraphs do not execute as separate requests.
13. As a line-oriented REPL user, I want one submitted multiline draft recorded as one prompt-history entry, so that history recall restores the complete request.
14. As a user, I want API-key, picker, search, approval, and other single-line controls to remain single-line, so that global paste support does not alter their safety or confirmation behavior.
15. As a user, I want the external editor to remain available, so that I can compose very long or highly structured requests reliably.
16. As a session operator, I want canonical session messages and event logs to retain multiline user content, so that replay and export remain faithful to the submitted turn.
17. As a maintainer, I want one submitted draft to correspond to one request and one prompt-history entry, so that physical line breaks never define request boundaries.
18. As a maintainer, I want terminal restoration to disable bracketed-paste mode, so that leaving nca does not change subsequent shell paste behavior.
19. As a maintainer, I want terminal and editor compatibility failures to have a documented fallback, so that unsupported bracketed paste does not silently promise behavior it cannot provide.

## Implementation Decisions

- Keep the existing native fullscreen composer as the primary editing module;
  do not replace it with a third-party textarea dependency.
- Keep the draft as one normalized LF `String` with a UTF-8-safe character
  cursor. Newline characters are content, not request or message boundaries.
- Keep the runtime, IPC, and turn-submission contract unchanged: submission
  carries one complete `String` draft.
- Keep paste handling atomic. The paste handler inserts text and never submits;
  only a later explicit send action can create a request.
- Route paste by input context. Chat and permitted custom answers preserve
  newlines; API-key, picker, search, provider setup, and approval fields use
  single-line sanitization and retain their existing confirmation rules.
- Normalize CRLF and CR to LF at the composer insertion boundary while
  preserving internal whitespace, blank paragraphs, and trailing newlines.
- Preserve the fullscreen key contract: Enter sends, Shift+Enter and Alt+Enter
  insert newlines, and completion selection gets its existing chance to consume
  Enter before submission.
- Keep slash completion restricted to single-line drafts. Multiline drafts
  beginning with slash or shell prefixes go through ordinary message handling.
- Keep the dynamic fullscreen composer viewport bounded and cursor-following.
  Rendering accounts for visual wrapping and display width, not only logical
  newline count, so long unbroken lines and wide Unicode remain usable.
- Enable Reedline bracketed paste in the line-oriented editor. Rely on
  Reedline's multiline buffer and existing modified-Enter behavior where
  available, while preserving ordinary Enter submission.
- Treat terminal support as a compatibility boundary. When bracketed paste is
  unavailable, the external editor and explicit modified-Enter flow remain the
  reliable fallbacks; the product must not guess whether an ordinary Enter was
  typed or came from an unsupported paste.
- Keep prompt history distinct from canonical session history and event-log
  replay. Prompt history records submitted drafts, not partial paste chunks or
  expanded display previews.
- Preserve multiline prompt-history entries as one logical record. Retain the
  existing Reedline history format if it safely round-trips all text; otherwise
  introduce an escaped structured format with a compatibility reader rather
  than using raw physical lines as records.
- Do not add a dedicated compose modal, change Enter to newline/Ctrl+Enter send,
  or make the external editor mandatory in this scope. Those remain alternative
  UX directions if the native composer proves insufficient.

## Testing Decisions

- Use one application-level composer input seam as the primary test seam. Feed
  it typed keys, modified-Enter, paste events, cursor movement, and submit
  actions, then assert the resulting draft, cursor, rendered viewport, and
  submission outcome. Tests should observe behavior rather than private event
  match structure.
- Verify that a multiline paste produces exactly one draft insertion and zero
  submissions until explicit send. Verify that explicit send produces exactly
  one complete request containing all paragraph breaks.
- Cover CRLF/CR normalization, blank paragraphs, trailing newlines, insertion
  at the beginning/middle/end, Unicode cursor positions, line joining,
  vertical movement, capped viewport scrolling, and long visual lines.
- Exercise every paste context through the same input-routing seam and assert
  multiline preservation only for chat/custom-answer contexts; assert
  single-line sanitization for credentials and picker/modal fields.
- Test command classification for single-line slash/shell commands versus
  multiline drafts that begin with those prefixes.
- Add a line-editor acceptance test at the highest available Reedline seam,
  proving bracketed paragraph paste yields one multiline success signal and one
  prompt-history item. Include existing-history round-trip coverage and a
  compatibility test for any migration reader if the storage format changes.
- Verify canonical session messages, event-log replay, and export preserve the
  submitted multiline user content. Do not use compact display previews as the
  source of truth for exact request text.
- Reuse existing composer, TUI application, transcript, cancellation, and CLI
  test patterns. Keep terminal escape-sequence assertions secondary to
  observable state and submission behavior.
- Run formatting, clippy, focused TUI/REPL tests, and the workspace test suite
  after implementation. Record unsupported-terminal or environment-only
  limitations separately from product regressions.

## Acceptance Criteria

1. Fullscreen drafts wrap by terminal display cells, preserve wide and
   combining Unicode, follow the cursor, and retain exact logical text through
   editing and submission.
2. Fullscreen typed input, modified-Enter, atomic paste, Unicode editing,
   visual wrapping, and submission are covered by application-level tests.
3. Reedline bracketed paste submits one multiline request, records one history
   entry, classifies multiline slash and shell prefixes as ordinary content,
   and restores the terminal after the loop exits.
4. Canonical messages, event logs, and the existing export path preserve the
   exact submitted multiline content; single-line commands and modal fields
   retain their existing behavior.
5. Unsupported bracketed-paste environments have documented external-editor
   and modified-Enter fallbacks without guessed submission boundaries.
6. Formatting, focused TUI/REPL tests, and relevant workspace checks pass.

## Out of Scope

- Replacing the native composer with a third-party textarea widget.
- Changing the default fullscreen send contract to Enter-as-newline and
  Ctrl+Enter-as-send.
- Adding selection, word-wise movement, undo, cut/copy editing, or a complete
  modal-editor state machine.
- Redesigning canonical session message schemas, runtime turn APIs, or IPC
  protocols unless testing proves exact user-content preservation is impossible
  through the current event seam.
- Treating clipboard image attachment as text paste.
- Automatically reconstructing already-split historical requests caused by
  older input behavior.
- Guessing paste boundaries when a terminal or multiplexer does not support
  bracketed paste.
- Publishing this spec to an issue tracker or creating implementation tickets.

## Verification

The complete behavior is implemented and verified in the current checkout:

- Fullscreen composer tests cover normalized CRLF/CR paste, paragraph and
  trailing-newline preservation, insertion at a Unicode cursor, modified-Enter
  newline insertion, multiline command classification, vertical movement, and
  the capped cursor-following viewport.
- Fullscreen input routing covers atomic chat paste, single-line API-key paste,
  approval/custom-answer handling, and paste insertion without submission.
- The line-oriented editor enables Reedline bracketed paste, classifies
  multiline `/` and `!` buffers as messages, and preserves multiline entries
  through the existing `FileBackedHistory` format.
- Canonical plain-text multiline user content is emitted through the existing
  `MessageReceived` event, replayed from the event log, and included by the
  existing export path without changing the runtime or IPC schema.
- `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets
  -- -D warnings`, `cargo check --workspace`, `cargo test -p nca-tui --lib`,
  `cargo test -p nca-tui --test terminal_harness`, and the relevant workspace
  checks passed during implementation verification.

## Further Notes

The Codex session history for this topic recorded the original exploration and
the selected fullscreen-TUI design before the current implementation landed.
The current checkout implements both input paths, including fullscreen atomic
paste, display-cell-aware wrapping, and Reedline bracketed paste. The PTY
harness drives the production Reedline loop and verifies one submission plus
one persisted history entry. Terminal restoration is covered by the existing
fullscreen restore guard and Reedline exits its bracketed-paste guard after
each `read_line` call.

There are two useful future directions if the native composer remains awkward:
an explicit compose mode with Enter-as-newline and a dedicated send action, or
an editor-first workflow. Both provide clear document composition but carry
more interaction and integration cost than completing atomic paste in both
existing input surfaces.
