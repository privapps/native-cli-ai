# Custom Responses API validation implementation plan

This bounded plan is based on the canonical feature spec, all five tickets,
and the existing implementation at `49dc531`. Work stays inside the isolated
worktree and does not copy or rewrite the base worktree's unrelated changes.

## 1. Establish the red feedback loop

- Run the existing Custom-provider Responses tests and inspect the public
  provider stream seam, request-capture fixtures, setup/configuration surfaces,
  and agent tool-loop seam.
- Add deterministic regression tests for the reported failures:
  configured Responses `temperature` is preserved, unsupported/unreadable image
  attachments fail before emission, explicit `call_id` wins over item identity,
  stable item identity/output index is normalized for tool results, and absent
  identity fails explicitly.

## 2. Repair the Responses request boundary

- Preserve the configured temperature in Responses request JSON while keeping
  Chat Completions and Anthropic serialization unchanged.
- Validate attachment readability and the supported vision media-type allowlist
  before producing `input_image` blocks; never emit an unchecked image.
- Keep endpoint construction, bearer credentials, complete history, streaming,
  `store: false`, token budget, and nested reasoning behavior intact.

## 3. Repair the Responses stream boundary

- Track function calls by stable provider item identity or output index, while
  preserving an explicit `call_id` as the identity sent to the agent/tool loop.
- Reject a function call that has no usable identity, name, valid JSON
  arguments, or complete output; retain stream order and split UTF-8 safety.
- Preserve unknown-event tolerance, explicit known-event/provider/transport/
  empty-completion errors, usage, cancellation, and existing `StreamChunk`
  behavior.

## 4. Complete deterministic public fixture coverage

- Extend local HTTP/SSE fixtures for endpoint/authentication/history, text,
  native tools and tool-result loops, multiple calls, malformed events and
  arguments, images, generation/reasoning settings and provider rejection,
  UTF-8 chunk splitting/invalid UTF-8, discovery/setup behavior, cancellation,
  and agent-loop integration.
- Verify Responses-only manual model setup remains possible when discovery is
  unavailable, and verify legacy OpenAI-compatible and Anthropic-compatible
  paths remain unchanged.

## 5. Verify and hand off

- Run focused Responses tests, formatter, Clippy, full workspace tests/checks,
  and release/build checks.
- Review the diff for scope, unrelated changes, and requirement coverage;
  commit only the isolated worktree implementation and report its identifier.
