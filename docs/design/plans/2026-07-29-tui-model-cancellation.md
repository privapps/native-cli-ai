# TUI model-request cancellation plan

**Type:** Implementation plan
**Status:** Implemented locally
**Date:** 2026-07-29
**Related:** None
**Last verified:** 2026-07-31 (documentation review)

## Problem

After changing model or provider in the TUI, a subsequent provider request can
remain in `BusyState::Thinking` while the provider waits for an HTTP response.
The TUI sets the shared cancellation flag on `Esc`, but `AgentLoop` currently
awaits `Provider::chat` without observing that flag. The command loop also
cannot process the queued `CancelTurn` command until the turn returns. On a
provider error, the TUI clears only the legacy busy boolean and can leave the
detailed state (and the spinner/cancel hint) stuck on `Thinking`.

## Scope

- Make the provider-request await cancellation-aware without changing the
  `Provider` trait or normal streaming behavior.
- Emit the existing cancellation error shape when cancellation wins during a
  provider request so the TUI can finish the turn consistently.
- Reset the detailed TUI busy state on provider errors and cancellations while
  preserving the normal success and retry states.
- Keep model-picker `Esc` behavior unchanged while ensuring `Esc` after a
  selection cancels an active request.

## TDD slices

1. Add a red agent-level test with a provider whose request never resolves;
   enter `Thinking`, set the public cancellation handle, and require the turn
   to terminate with a cancellation error.
2. Add a red TUI-state regression for a failed turn; require the detailed
   state to leave `Thinking`, hide the cancellation affordance, and accept the
   next input.
3. Implement the smallest cancellation and cleanup changes, then run focused
   tests, formatting, and the complete workspace suite.

## Acceptance criteria

- `Esc` cancels a pending provider request within a bounded test timeout,
  including the Custom provider path.
- Existing streaming, tool, approval, and picker cancellation behavior remains
  intact.
- Provider errors and cancellations cannot leave the TUI displaying
  “waiting for model” or “Esc cancel” after the turn has ended.
- A new message can be submitted after either outcome.
- `cargo fmt --all`, focused tests, and `cargo test --workspace` pass.
