# `/copy` latest assistant response fix

## Diagnosis

`/copy` obtains its text from `TuiSessionState::last_assistant_text`. During a
new assistant turn, the completed prior response remains in `blocks` while the
new response accumulates in `streaming_assistant`. The current lookup scans
committed blocks before considering streaming text, which can make `/copy`
select the prior response instead of the newest visible assistant response.

## Feedback loop

Add a deterministic state-level regression test that applies the same event
sequence as the TUI bridge:

1. commit an earlier assistant response;
2. start a newer assistant stream;
3. assert the copy source is the newer streamed text.

Run it with:

```text
cargo test -p nca-tui last_assistant_text
```

## Implementation

- Make `last_assistant_text` prefer non-empty streaming text, then fall back to
  the newest committed assistant block.
- Update the existing preference test to cover the in-progress newer response.
- Run the focused test, the full `nca-tui` test suite, and formatting/checks.

## Validation

The regression test must fail before the implementation change and pass after
it. No clipboard backend is required for the test; it exercises the exact text
selection seam used by `/copy`.

Completed:

- The regression test failed before the fix with `Some("previous response")`.
- The focused test passes after the fix.
- `cargo test -p nca-tui` passes: 117 unit tests, 5 integration tests, and doc
  tests.
- `cargo check --workspace` passes.
- `cargo fmt --all -- --check` passes.

Root cause: selection order favored the newest committed block even when a
newer non-empty assistant stream was visible. The fix prefers that stream and
falls back to committed history when no stream is active.
