# Question Modal Picker Implementation Plan

This archived plan records the completed work that replaced number-key question
selection with an arrow-key modal picker.

## Outcome

- Added modal-open, selected-option, and scroll state to the TUI session state.
- Opened the modal when a question request arrives and closed it when the
  question resolves or the user cancels it.
- Routed Up/Down, Home/End, PageUp/PageDown, Enter, and Escape to the modal
  while it is open.
- Rendered the question and options in the same centered-popup style used by
  the other TUI pickers.
- Suppressed the composer hint while the modal owns keyboard input.
- Preserved the existing approval precedence and the free-form answer path.

## Verification record

The implementation was checked with the focused TUI tests and the workspace
format, lint, and test suites available at the time. This document is retained
for historical context; the archived [question modal
specification](../../specs/archive/2026-03-24-question-modal-picker-design.md)
contains the behavior contract.

## Historical task outline

1. Add modal state and lifecycle helpers.
2. Open and close the modal from question events.
3. Handle navigation and selection keys while the modal is active.
4. Render the centered popup and option state.
5. Hide the composer hint while the popup is active.
6. Update state, rendering, and interaction tests.
7. Perform manual terminal verification and cleanup.
