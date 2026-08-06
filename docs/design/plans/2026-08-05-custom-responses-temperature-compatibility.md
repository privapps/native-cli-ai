# Custom Responses Temperature Compatibility

**Type:** Compatibility plan
**Status:** Complete
**Date:** 2026-08-05
**Related:** [Custom provider OpenAI Responses API specification](../specs/2026-07-29-custom-provider-responses-api.md)
**Last verified:** 2026-08-05 (Responses generation-setting regression and documentation checks)

## Goal

Restore custom OpenAI Responses compatibility for models that reject the Chat
Completions-only `temperature` parameter.

## Bounded steps

1. Repair the existing custom-provider test helper references so the focused
   provider test can compile.
2. Run the existing Responses regression test and confirm it fails because the
   serialized request contains `temperature`.
3. Stop sending `temperature` in custom OpenAI Responses request bodies while
   preserving it for Chat Completions and other provider protocols.
4. Run the focused regression test and the complete `nca-core` provider tests.

## Verification

The regression test must exercise `CustomProvider::chat`, capture the outgoing
`/responses` request, and fail if `temperature` is present. No live provider
credentials or network service are required.
