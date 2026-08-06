# Ticket 04 — `$` Skill Reference Documentation and Acceptance Tests

**Type:** Implementation plan  
**Status:** Completed
**Implementation status:** completed
**Date:** 2026-08-03  
**Related:** [dollar-prefixed skill references](2026-08-03-dollar-prefixed-skill-references.md)  
**Last verified:** 2026-08-06 (focused TUI tests, TUI integration tests, workspace tests, Clippy, docs/spec validation, formatting, and scoped diff checks)

## Scope

Complete the interactive user documentation and verify ticket 04 through the
existing `nca-tui` REPL/TUI runtime seams. The tests use local discovered
skills and scripted custom-provider fixtures, covering prepared prompt content,
lifecycle/recovery, completion dispatch, and provider-neutral request
serialization. One-shot, server, and subagent protocol behavior remains out of
scope.

## Bounded steps

1. Document exact matching, escaping, preservation rules, non-execution, size
   bounds, truncation, and the existing `/skill` and `invoke_skill` paths.
2. Add application-level tests that submit through the shared interactive
   preparation/runtime path and inspect provider requests, session messages,
   outcomes, and recovery state.
3. Add completion-dispatch and Chat Completions/Responses-equivalence checks,
   reusing existing test helpers while keeping unrelated changes untouched.
4. Run focused formatting, TUI tests, provider-path tests, and diff checks
   before updating ticket evidence.

## Invariants

- `$` references select bounded untrusted guidance; they never execute skills or
  alter model, permission, capability, or session policy.
- Only the original interactive input is parsed; `@file` expansion and skill
  bodies are not recursively parsed.
- Failed preparation or provider turns do not commit loaded-context state or a
  partial user message.
