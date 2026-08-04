# Archived Feature Documentation Update Plan

**Type:** Implementation plan
**Status:** Complete
**Date:** 2026-08-02
**Related:** Archived feature specifications under `.archive/`
**Last verified:** 2026-08-03 (14-bundle archive audit; docs, reference, spec, and diff checks)

## Goal

Bring the canonical documentation under `docs/user/` and `docs/reference/` in
line with the validated feature bundles archived in `.archive/`, using the
current Rust implementation as the authority where a later feature superseded
an earlier specification.

## Scope

- Document the TUI Markdown renderer shipped after the existing interactive-mode
  guide was last updated.
- Complete the Custom OpenAI Responses behavior that users need to know about:
  native images, normalized function-call identity, and explicit failures for
  malformed or incomplete provider output.
- Correct and expand the web-search adapter documentation to distinguish the
  Bing RSS request from the process-wide DuckDuckGo limiter and to describe
  provider request profiles and redirect resolution.
- Correct the tool-category table so opt-in financial tools and the
  filesystem-writing financial report tool are described accurately, and make
  the shared financial provenance contract explicit for human, JSON, and
  NDJSON output.
- Add the small runtime/session invariants that affect users of Responses and
  resumed sessions: one generated harness prompt per request and restoration
  before resume persistence; retain the existing 128K unknown-model fallback
  documentation.

## Bounded steps

1. Update `docs/user/interactive-mode.md` with the Markdown rendering contract.
2. Update `docs/user/providers.md` and `docs/user/sessions.md` with the
   implementation-backed provider and resume behavior.
3. Update `docs/user/tools.md` and `docs/user/configuration.md` with the exact
   current search behavior and configuration semantics.
4. Correct the financial tool availability/write classification and document
   provider capability fallback and output provenance.
5. Add the corresponding technical invariants to
   `docs/reference/architecture.md` where they clarify crate boundaries or
   lifecycle behavior.
6. Check every archived feature name against the canonical docs, run the
   repository documentation checks, and review the final diff for stale claims.

## Evidence

- Archived feature bundles audited:

  | Bundle | Canonical documentation |
  | --- | --- |
  | `bing-first-search-fallback` | [`user/tools.md`](../../user/tools.md), [`user/configuration.md`](../../user/configuration.md) |
  | `custom-provider-debug-log` | [`user/configuration.md`](../../user/configuration.md), [`user/providers.md`](../../user/providers.md) |
  | `custom-provider-request-debugging` | [`user/configuration.md`](../../user/configuration.md) (superseded by append-only file diagnostics) |
  | `custom-provider-responses-api` | [`user/providers.md`](../../user/providers.md), [`reference/architecture.md`](../../reference/architecture.md) |
  | `custom-provider-runtime-correctness` | [`user/configuration.md`](../../user/configuration.md), [`user/sessions.md`](../../user/sessions.md), [`reference/architecture.md`](../../reference/architecture.md) |
  | `custom-responses-function-call-identity` | [`user/providers.md`](../../user/providers.md), [`reference/architecture.md`](../../reference/architecture.md) |
  | `duckduckgo-search-throttling` | [`user/tools.md`](../../user/tools.md), [`user/configuration.md`](../../user/configuration.md) |
  | `financial-research-and-provider-hardening` | [`user/tools.md`](../../user/tools.md), [`user/configuration.md`](../../user/configuration.md), [`reference/architecture.md`](../../reference/architecture.md) |
  | `financial-research-boundary` | [`user/tools.md`](../../user/tools.md), [`user/skills.md`](../../user/skills.md), [`reference/architecture.md`](../../reference/architecture.md) |
  | `goal-command` | [`user/interactive-mode.md`](../../user/interactive-mode.md), [`user/configuration.md`](../../user/configuration.md), [`reference/architecture.md`](../../reference/architecture.md) |
  | `latest-financial-report-freshness` | [`user/tools.md`](../../user/tools.md), [`reference/architecture.md`](../../reference/architecture.md) |
  | `reasoning-effort` | [`user/providers.md`](../../user/providers.md), [`user/configuration.md`](../../user/configuration.md), [`user/interactive-mode.md`](../../user/interactive-mode.md) |
  | `tui-markdown-rendering` | [`user/interactive-mode.md`](../../user/interactive-mode.md), [`reference/architecture.md`](../../reference/architecture.md) |
  | `tui-newline-shortcuts` | [`user/interactive-mode.md`](../../user/interactive-mode.md) |

- `.archive/tui-markdown-rendering/spec.md`
- `.archive/custom-provider-responses-api/spec.md`
- `.archive/custom-responses-function-call-identity/spec.md`
- `.archive/custom-provider-runtime-correctness/spec.md`
- `.archive/bing-first-search-fallback/spec.md`
- `.archive/duckduckgo-search-throttling/spec.md`
- `.archive/custom-provider-debug-log/spec.md`
- `.archive/custom-provider-request-debugging/spec.md`
- `.archive/goal-command/spec.md`
- `.archive/latest-financial-report-freshness/spec.md`
- `.archive/reasoning-effort/spec.md`
- `.archive/tui-newline-shortcuts/spec.md`
- `.archive/financial-research-and-provider-hardening/spec.md`
- `.archive/financial-research-boundary/spec.md`
- `crates/common/src/config.rs`
- `crates/core/src/research.rs`
- `crates/tui/src/tui/transcript.rs`
- `crates/tui/src/tui/app.rs`
- `crates/tui/src/repl.rs`
- `crates/core/src/provider/custom.rs`
- `crates/core/src/tools/web_search.rs`
- `crates/runtime/src/model_limits.rs`
- `crates/runtime/src/supervisor.rs`
