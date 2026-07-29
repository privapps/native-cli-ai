# Custom Provider Handoff Verification Plan

## Scope

Continue the local custom-provider implementation from the handoff without
publishing, staging, or committing changes. The implementation already routes
the real TUI command path through the shared custom-provider setup state
machine and has focused protocol, persistence, and modal-boundary coverage.

## Work items

- Re-run formatting, diff, metadata, dependency-audit, and workspace test
  checks.
- Inspect the real TUI routing and protocol fixtures for regressions exposed by
  the handoff.
- Keep the completed production app-event-loop acceptance coverage and
  executable workspace protocol verification recorded in the handoff.
- Run the final code review against the custom-provider specification and repo
  standards.

## Decision

The production dispatch seam now provides the required Ticket 05 acceptance
coverage, so no test-only event loop or broad injectable terminal architecture
is required for this continuation. The existing tests exercise production
routing rather than a duplicate dispatcher.

## Verification result

Formatting, diff hygiene, locked metadata, dependency resolution, workspace
tests, and benchmark compilation pass. The full benchmark run also completes;
Criterion reports a few performance-regression warnings but exits successfully.

The review also exposed and fixed two bounded issues: custom model discovery
now strips an optional `/v1` suffix before appending `/v1/models`, and the TUI
status host is obtained from the shared standards-compliant URL parser. The
protocol tests cover exact endpoint paths, authentication headers, complete
request bodies, and parsed stream chunks; deterministic local fixtures cover
custom model discovery paths, headers, parsing, pagination, and failures.

## Dependency-resolution follow-up

The workspace lock conflict between `toml_parser` versions used by `toml` and
`toml_edit` is resolved while preserving locked reproducibility, benchmark
targets, and the existing custom-provider implementation.

### Resolution

The workspace now pins `toml = "1.1.3"`, regenerates the lockfile, and resolves
`toml_parser` to one generation (`1.1.3`) alongside `toml_edit 0.25.13`.
`criterion 0.7.0` remains enabled and locked; benchmark targets were not
removed.

### Acceptance result

Ticket 05 acceptance coverage is complete. The production event-loop seam now
exercises onboarding Custom selection, `/connect`, unconfigured recovery,
configured editing, secret preservation, and setup submission routing. Core
protocol fixtures cover request headers, endpoint paths, complete body fields,
and stream parsing for both adapters. Full activation/persistence integration
tests verify both failure-ordering contracts.
