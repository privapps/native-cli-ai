---
name: Financial Research
command: financial-research
description: Evidence-bounded financial report research with explicit verification and fallback states.
disable-model-invocation: true
---

Use this skill only when the user explicitly requests financial-report research.

- Treat the runtime-provided `as_of` as one immutable UTC calendar date for the whole turn. Do not accept or invent a per-tool date.
- Gather evidence with `web_search` and `fetch_url`; prefer official issuer investor-relations pages and regulatory filings. Preserve source URLs, publication dates, response status, retrieval timestamps, and authority.
- Keep reported/filed actuals separate from guidance, estimates, earnings commentary, and future periods. Use `validate_financial_report` before `resolve_latest_financial_report`.
- Resolve the requested cadence exactly: `latest` is the newest eligible completed result, while `annual` and `quarterly` are strict. An unavailable annual may return an explicitly labelled quarterly fallback with its limitation; never call it annual.
- Treat missing publication or period metadata, unknown issuers, issuer mismatches, secondary-only evidence, conflicts, and unavailable validation as visible unverified states.
- Include the shared `as_of` date, issuer, period type, period end, publication status/date, source URL, retrieval metadata, and any limitation in the final report. Do not hide conflicts or fallback disclosures.
- Use `write_validated_financial_report` for persistence. It accepts only the report capability produced by successful validation/resolution. Generic `write_file` remains domain-neutral.
