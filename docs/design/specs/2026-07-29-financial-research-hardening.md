# Verified, Time-Bounded Financial Research and Review Hardening

**Type:** Feature specification
**Date:** 2026-07-29
**Status:** Implemented locally
**Related:** [Financial research and provider hardening plan](../plans/2026-07-29-financial-research-provider-hardening.md)
**Last verified:** 2026-08-03 (workspace tests, Clippy, formatting, and diff checks)

## Problem Statement

Users need nca to answer financial-report questions with authoritative, date-bounded evidence rather than plausible but unverifiable summaries. They need to distinguish reported results from guidance, estimates, future periods, and quarterly fallbacks, while seeing the exact as-of boundary and source provenance used by the answer.

The current implementation contains useful financial-research behavior, but its domain rules are mixed into generic web tools, the generic file-writing path, the default assistant prompt, and one large research module. The research state also maintains its own as-of value even though the runtime already builds an as-of value for the harness. These parallel values can drift and make a result difficult to audit.

The current branch also needs the related review corrections: custom-provider probes must validate successful response bodies instead of trusting HTTP status alone, and compatibility-specific provider behavior should be localized behind a clear protocol seam.

## Solution

Make financial research an explicit, opt-in capability with a domain-specific skill and toolset, while keeping the default assistant prompt general-purpose. Establish one immutable, runtime-owned date-only `as_of` value for each agent turn and use it everywhere: harness context, web evidence, financial validation, resolution, warnings, and final-report metadata.

Place generic evidence collection behind a small evidence-ledger interface and place financial rules behind a deep financial-research interface. Financial output must carry an explicit verification state. Writing a financial report uses a domain-specific validated-report path; generic `write_file` does not infer financial content from text.

As supporting hardening, validate OpenAI-compatible and Anthropic-compatible custom-provider probe envelopes, redact probe failures, and isolate protocol-specific endpoint, authentication, probe, request, and stream behavior behind two internal adapters.

## User Stories

1. As a terminal user, I want to ask for the latest financial report for an issuer, so that I can obtain a useful current result without manually designing a research workflow.
2. As a terminal user, I want to request an annual result, so that a quarterly result is not silently presented as an annual result.
3. As a terminal user, I want to request a quarterly result, so that the resolver selects a completed eligible quarter.
4. As a terminal user, I want an unspecified cadence to resolve the newest eligible completed result, so that “latest” has a precise meaning.
5. As a terminal user, I want every financial-research turn to use one global as-of date, so that all tools and the final answer agree about the knowledge boundary.
6. As a terminal user, I want the as-of date shown in the result, so that I can audit what information was considered current.
7. As a terminal user, I want periods ending after the as-of date rejected, so that future or incomplete reporting periods are not treated as actuals.
8. As a terminal user, I want sources published after the as-of date rejected, so that later information cannot leak into a historical answer.
9. As a terminal user, I want source publication dates included, so that I can distinguish dated evidence from undated pages.
10. As a terminal user, I want official issuer and regulatory sources preferred, so that financial facts are grounded in authoritative evidence.
11. As a terminal user, I want secondary-only or unknown sources marked insufficient for verification, so that search snippets are not mistaken for official reports.
12. As a terminal user, I want guidance and estimates excluded from reported-result resolution, so that forecasts are not presented as historical results.
13. As a terminal user, I want missing metadata to produce an explicit unverified result, so that uncertainty is visible rather than hidden.
14. As a terminal user, I want conflicting evidence preserved and surfaced, so that disagreement between sources is not silently discarded.
15. As a terminal user, I want an annual request with no eligible annual result to return an explicit quarterly fallback and limitation, so that the answer remains useful without being misleading.
16. As a terminal user, I want an unknown or mismatched issuer rejected, so that evidence for one company cannot be attributed to another.
17. As a terminal user, I want source URLs, period type, period end, publication status, and retrieval metadata included in a verified report, so that the answer is auditable.
18. As a terminal user, I want ordinary conversations to receive general-purpose guidance, so that financial-report instructions do not distort unrelated tasks.
19. As a terminal user, I want financial-research instructions available through an explicit skill, so that the specialized workflow is discoverable when needed.
20. As a terminal user, I want generic web search and URL fetching to remain useful outside finance, so that financial parameters do not become mandatory for ordinary research.
21. As a terminal user, I want a generic file write to behave generically, so that unrelated text is not classified by fragile financial keywords.
22. As a terminal user, I want a financial report write operation to require a validated report, so that unverified content cannot be saved through the financial workflow.
23. As a terminal user, I want an unverified report to remain available with a clear warning when validation cannot be completed, so that temporary source or network failures do not erase useful partial research.
24. As a maintainer, I want one runtime-owned as-of date per turn, so that the harness, tools, and domain logic cannot silently use different dates.
25. As a maintainer, I want deterministic tests to inject the turn date, so that date-sensitive behavior can be tested without waiting for wall-clock time.
26. As a maintainer, I want generic evidence storage separated from financial-period rules, so that each module has one coherent reason to change.
27. As a maintainer, I want a small financial-research interface hiding evidence merging, metadata extraction, validation, conflict handling, and resolution, so that callers do not duplicate domain rules.
28. As a maintainer, I want financial tools to return structured verification and fallback states, so that human, JSON, and NDJSON clients observe the same semantics.
29. As a custom-provider user, I want a malformed successful probe response rejected, so that an HTML page or wrong protocol cannot activate as a healthy provider.
30. As a custom-provider user, I want valid empty model catalogs accepted, so that manual model configuration remains possible when discovery is unavailable.
31. As a custom-provider user, I want probe errors sanitized, so that credentials and sensitive endpoint details are not exposed in diagnostics.
32. As a custom-provider user, I want OpenAI-compatible and Anthropic-compatible requests to retain their protocol-specific headers and bodies, so that switching compatibility does not break streaming or tools.
33. As a maintainer, I want protocol-specific custom-provider behavior localized behind adapters, so that endpoint, authentication, probing, request construction, and stream parsing do not require repeated branching throughout the provider.
34. As a maintainer, I want the existing provider abstraction preserved, so that this feature does not introduce a speculative provider-management framework.

## Implementation Decisions

- Treat `as_of` as a global immutable per-turn calendar date. The runtime creates it once before refreshing the harness and beginning research. It is the current UTC date by default, represented as a date-only domain value rather than a timestamp. All date-sensitive tools, evidence ledgers, validators, resolvers, warnings, and final output receive the same value. Deterministic runtime entry points may inject a date for tests. No tool may override or create a competing as-of value.
- Remove the caller-supplied as-of assertion from the web-search contract. Web search and URL fetching read the authoritative turn context directly and include that value in their structured results.
- Keep the turn context runtime-owned and explicit rather than using mutable process-global state. A turn context contains the as-of date and the evidence ledger used by the tools in that turn.
- Compare publication timestamps by their UTC calendar date when applying the as-of rule. A source published on the as-of date is eligible; a source published on a later UTC date is not. Preserve full publication and retrieval timestamps as provenance metadata.
- Define a generic evidence ledger that records normalized source URL, retrieval time, response status, HTTP date, publication date, title/snippet or content metadata, and source authority. It may record conflicts, but it does not decide whether a financial report is valid.
- Define a financial-research module that consumes the evidence ledger and owns issuer matching, reporting-period metadata, report status, fiscal/calendar interpretation, candidate validation, cadence resolution, fallback disclosure, and final-output verification.
- Preserve the domain vocabulary: an observed source becomes an evidence record; a proposed result is a report candidate; a candidate that passes the as-of, authority, period, publication, issuer, and status checks becomes a validated report; cadence selection produces a report resolution with resolved, fallback, or unavailable status.
- Expose one small financial-research interface to callers. It supports recording or querying evidence, validating a candidate, resolving a cadence, and producing verification metadata or an explicit unverified warning. Evidence merging, parsing, conflict detection, and selection remain behind the interface.
- Split the current large research implementation into cohesive internal modules for generic evidence, metadata extraction, validation, resolution, and output verification while preserving the single public financial-research interface.
- Keep the general-purpose built-in prompt free of financial workflow instructions. Retain generic high-stakes guidance. Provide detailed financial workflow instructions through an opt-in financial-research skill using the existing skill-loading mechanism.
- Keep financial tool descriptions and user-facing documentation explicit about authoritative sources, as-of behavior, validation, and fallback states. Ordinary web research must not require financial-only arguments or assumptions.
- Remove financial-report detection based solely on keyword heuristics from the generic file-writing path. Add a financial-specific write operation or validation-aware adapter that accepts only a validated-report capability/handle and preserves the verification state in its result.
- The financial-specific write operation must refuse candidates that have not passed validation, must leave the target untouched on refusal, and must report the exact validation reason. Generic file writing remains governed by its existing permissions and does not infer a financial domain.
- Keep financial research available in the product’s toolset through an explicit capability. The default workflow may expose generic web tools; financial validation and resolution are selected by the financial-research skill or equivalent explicit capability rather than by hidden prompt behavior.
- Ensure every structured financial result includes the global as-of date, source authority, publication status, period type, period end, source publication date, and any fallback limitation needed to interpret it.
- Reject future periods, post-as-of publications, unknown publication dates, issuer mismatches, non-authoritative-only evidence, guidance, estimates, and unobserved sources as verification failures. Preserve an explicit unverified state when the user can still benefit from partial information.
- Preserve source conflicts and never silently choose a less authoritative or later-ineligible source merely because it is easier to parse.
- For custom-provider probes, require protocol-specific successful response envelopes rather than treating every `2xx` response as valid. OpenAI-compatible probes validate a models response with a well-formed data collection, allowing an empty collection. Anthropic-compatible probes validate a well-formed Messages response envelope.
- Map malformed successful probe bodies to sanitized retryable provider errors. Continue redacting credentials from all status, body, and transport diagnostics.
- Introduce two internal custom-protocol adapters, one for OpenAI-compatible behavior and one for Anthropic-compatible behavior. Each adapter owns endpoint construction, authentication, probe request/response validation, request-body construction, and stream handling; the common provider owns credentials, client lifecycle, common status mapping, and shared error policy.
- Do not introduce a broad new `ProviderKind` abstraction solely to remove simple configuration matches. Keep that lower-value cleanup separate unless the protocol seam demonstrates a real shared interface.
- Record this work in a bounded implementation plan before source changes. Implement the plan in slices: global turn context, evidence/domain split, opt-in skill and tool contracts, validated financial writing, custom probe validation and adapters, documentation, then full verification.
- Update the product PRD and user documentation to describe financial research as a supported, evidence-bounded capability rather than implying that every nca request is a financial workflow.

## Testing Decisions

- The primary financial seam is the public financial-research interface exercised through tool calls with an injected deterministic turn context. Tests should assert external evidence, validation, resolution, warning, and persistence behavior rather than private parser structure.
- Add an end-to-end deterministic turn test proving that the same as-of date appears in the harness, web-search/fetch results, evidence ledger, validation output, resolution output, and final response metadata.
- Add tests proving that no tool can override the global as-of date, that same-day publication timestamps are accepted, and that later UTC dates are rejected.
- Test evidence collection with authoritative, secondary, unknown, conflicting, undated, redirected, and issuer-mismatched sources.
- Test validation with completed and future periods, reported/filed results, guidance, estimates, unknown statuses, future publication dates, missing publication dates, missing source observations, and metadata mismatches.
- Test latest, annual, and quarterly resolution, including explicit quarterly fallback for an unavailable annual result and preservation of limitations.
- Test final-output verification and unverified annotation through the public financial-research interface.
- Test that generic web search, URL fetch, and generic file writing remain usable without financial-only fields or hidden financial classification.
- Test the financial-specific write operation with valid, invalid, unverified, and conflicting report capabilities; assert that rejected writes do not modify the target.
- Test the financial-research skill’s instructions and tool availability without adding financial instructions to the built-in general-purpose prompt.
- Test custom-provider probes through deterministic local HTTP fixtures: valid OpenAI responses, empty model catalogs, valid Anthropic Messages responses, HTML bodies with `2xx`, malformed JSON, wrong-protocol JSON, authentication failures, and redaction of credentials.
- Test custom-provider adapters through the existing public provider chat seam for endpoint paths, authentication headers, request bodies, streaming, tool calls, and empty-completion errors.
- Retain existing workspace, provider, CLI, TUI, cancellation, and documentation regression tests. Finish with formatting, Clippy, workspace tests, and the relevant release/build checks.

## Out of Scope

- Investment recommendations, trading decisions, portfolio management, or personalized financial advice.
- Live market-data subscriptions, brokerage integrations, paid data providers, or persistent financial databases.
- Automatic issuer identity resolution beyond evidence that visibly names the requested issuer.
- User-controlled historical as-of dates through an individual tool call. A future run-level clock or explicit historical-research mode may be designed separately; within this feature, the turn context is authoritative.
- Multiple named financial-research profiles or user-configurable validation policies.
- Making every nca conversation financial-aware or adding financial workflow text to the default built-in prompt.
- A generic provider-capability framework unrelated to custom-provider protocol handling.
- Non-streaming custom-provider fallbacks or silent retry with a different request protocol.
- Changing the default MiniMax provider or replacing the existing provider abstraction.

## Further Notes

“Global as-of” means one immutable UTC calendar date shared by every operation in an agent turn. It is intentionally not a mutable process-wide singleton or a persistent configuration value. Publication and retrieval timestamps remain detailed provenance, but eligibility is decided by the publication date relative to the global as-of date. The runtime’s injected turn date remains the test seam and the source of truth for reproducible historical fixtures.

The financial-research capability should be treated as a high-stakes evidence workflow: a result can be useful without being verified, but the verification state and limitation must never be hidden. The design keeps generic nca behavior domain-neutral while giving users a deliberate path to stronger financial research guarantees.
