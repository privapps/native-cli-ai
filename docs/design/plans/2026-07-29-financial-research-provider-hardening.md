# Financial Research and Provider Hardening Implementation Plan

**Type:** Implementation plan  
**Status:** Completed locally  
**Date:** 2026-07-29  
**Related:** [Financial research hardening specification](../specs/2026-07-29-financial-research-hardening.md)  
**Last verified:** 2026-07-31 (documentation review)

## Objective

Deliver the local financial-research hardening and provider-capability review changes while keeping the CLI Rust-native, the financial workflow opt-in, and the global `as_of` value date-only and runtime-owned.

## Bounded slices

1. Establish one `NaiveDate` turn context and thread it through harness, evidence, validation, resolution, and structured output.
2. Keep generic web/file tools domain-neutral; retain evidence observations and add validated financial-report writing.
3. Make financial research discoverable through the tracked skill and enable its tools only after explicit capability activation.
4. Harden Custom OpenAI/Anthropic protocol probing and expose their catalog adapters for runtime capability reuse.
5. Dispatch model discovery and context-window lookup through one runtime capability seam with provider/endpoint/protocol/model/credential-tag cache identity.
6. Update product, architecture, provider, tool, and skill documentation plus local acceptance tickets.

## Completion gates

- `cargo fmt --all -- --check`
- `cargo clippy --workspace --all-targets -- -D warnings`
- Serial package tests for all workspace crates, including financial resolution, provider probes, model capability dispatch, and CLI/TUI structured output.
- `git diff --check`
- Local specs and tickets reflect implementation status; no external issue or spec publication.

## Verification record

The focused capability-dispatch tests cover MiniMax, OpenAI, Anthropic, OpenRouter, Custom OpenAI-compatible, and Custom Anthropic-compatible paths, including path prefixes, authentication, pagination, malformed catalog entries, provider/credential cache separation, static fallback, and secret redaction. Existing workspace tests remain the regression gate for chat, streaming, tools, cancellation, CLI, and TUI behavior.
