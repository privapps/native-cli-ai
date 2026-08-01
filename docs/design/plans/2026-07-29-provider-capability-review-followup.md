# Provider Capability Review Follow-up Plan

**Type:** Implementation plan
**Status:** Completed locally
**Date:** 2026-07-29
**Related:** [Provider capability dispatch specification](../specs/2026-07-29-provider-capability-dispatch.md)
**Last verified:** 2026-07-31 (documentation review)

## Objective

Close the remaining provider-capability review gaps without changing provider request behavior.

## Bounded slices

1. Remove the runtime-only provider-settings wrapper and use the common resolved settings view directly, keeping normalization and capability endpoint logic at the capability seam.
2. Add an explicit cache-key regression test proving compatibility changes invalidate capability identity while credentials remain redacted.
3. Add a compile-time provider-support checklist in the common provider model and wire runtime capability coverage to it, so adding a `ProviderKind` without updating support metadata fails compilation.
4. Run focused tests, formatting, Clippy, the full workspace suite, and inspect the final diff.

## Completion gates

- Runtime contains no duplicate provider-settings struct.
- Compatibility changes produce distinct cache keys.
- Provider support metadata is compile-time exhaustive over `ProviderKind`.
- `cargo fmt --all -- --check`
- `cargo clippy --workspace --all-targets -- -D warnings`
- Full workspace tests pass.
- `git diff --check`

## Verification record

- Runtime capability tests pass, including cache identity changes for model,
  endpoint, credential, and compatibility.
- Runtime capability adapters now consume `ResolvedProviderSettings` directly;
  no runtime provider-settings wrapper remains.
- `ProviderKind::capability_support` is exhaustive and the runtime capability
  checklist is evaluated in a const context.
- `cargo fmt --all -- --check`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo test --workspace --all-targets`
