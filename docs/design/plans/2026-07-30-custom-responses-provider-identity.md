# Custom Responses Provider Function-Call Identity Plan

**Type:** Implementation plan
**Status:** Implemented locally
**Date:** 2026-07-30
**Related:** [Custom Provider OpenAI Responses specification](../specs/2026-07-29-custom-provider-responses-api.md)
**Last verified:** 2026-07-31 (documentation review)

## Goal

Allow the Custom OpenAI Responses adapter to consume function-call streams from
gateways that provide a stable function-call item identity in `item.id` but omit
the native Responses `call_id` field.

## Scope

- Preserve an explicit `call_id` when the provider sends one.
- Use the function-call item's stable `id` as the internal tool-call identity
  when `call_id` is absent.
- Keep argument accumulation keyed by the output item identity.
- Add a public-provider-stream regression fixture for the missing-`call_id`
  shape.
- Preserve explicit errors for function calls that have neither a usable call
  identity nor a name, invalid JSON arguments, malformed known events, and
  empty completions.

## Bounded steps

1. [x] Add a failing Responses SSE regression test for a function-call item
   with `id`, `name`, and arguments but no `call_id`.
2. [x] Normalize the function-call identity at the output-item parser seam.
3. [x] Run focused provider tests, formatting, typechecking, and the full
   workspace suite.
4. [x] Review the diff without touching the unrelated dirty worktree changes.

## Constraint

The original diagnosis did not capture a raw provider SSE response, so the
fixture covers the leading reported shape rather than asserting that this is
the only gateway variation. Further provider-specific event shapes should get
their own fixtures when raw traces are available.

## Follow-up reported failure

The live gateway still reports a missing function-call identity after the
item-ID fallback. The next compatibility fixture covers a function-call item
that supplies only `output_index`; argument events use the same index. The
CLI's event-log writer also must not panic while recording the provider error.

## Diagnosis instrumentation

- [x] Extend the existing opt-in custom-provider debug log with redacted
  Responses event identity metadata so live gateway event shapes can be
  compared with the parser's map keys.
- [x] Reproduce the live `--yolo` flow with `NCA_DEBUG_REQUEST=1` and use the
  captured event metadata to identify the stable output_index and changing
  item_id values in argument events.
- [x] Add a regression fixture for changing argument-event item_id values.
- [x] Prefer output_index over item_id for Responses argument events.
- [x] Prefer output_index over changing output-item id values when both are present.
- [x] Rebuild and rerun the live --yolo flow.

## Cleanup

- [x] Remove temporary response-shape diagnostics after identifying the
  gateway's identity behavior.
- [x] Keep focused public-provider SSE fixtures for stable item IDs and
  rotating item IDs keyed by output index.
