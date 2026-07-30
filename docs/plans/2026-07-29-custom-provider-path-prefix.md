# Custom provider path-prefix support

## Goal

Allow custom providers whose API is mounted below an origin, such as
`https://opencode.ai/zen/v1`, while preserving the configured path prefix for
chat and probe requests. Continue rejecting credentials, query strings,
fragments, and paths that are already individual requests.

## Public seam

Test the behavior through `CustomProvider`'s public `Provider::chat` method and
a local HTTP fixture. A custom OpenAI-compatible provider configured with an
origin path of `/zen/v1` must send the chat request to
`/zen/v1/chat/completions`.

## Bounded implementation slices

1. Add the failing public chat regression test for an OpenAI-compatible
   `/zen/v1` endpoint.
2. Normalize and preserve valid path prefixes ending in `/v1`, then use the
   preserved prefix when constructing chat, probe, and model-discovery
   endpoints for both compatibility modes.
3. Update validation wording and provider documentation to describe origin
   paths ending in `/v1`.
4. Run focused tests, formatting, and the full workspace test suite.

## Verification

- The regression test is red before the URL change and green after it.
- Existing origin and `/v1` endpoint behavior remains unchanged.
- Probe paths preserve the same configured prefix as chat paths.
- Runtime model discovery preserves the same configured prefix.
- `cargo fmt --check` and `cargo test --workspace` pass.
