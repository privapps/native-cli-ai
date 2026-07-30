# Reasoning Effort Implementation Plan

## Goal

Add the global `model.reasoning_effort` setting to OpenAI-compatible Chat Completions requests, with the literal `nil` sentinel omitting the JSON property, then expose it through the CLI and TUI.

## Boundaries

- Shared OpenAI-compatible request-body construction is the primary wire seam.
- Existing provider request-capture tests observe OpenAI, OpenRouter, Custom OpenAI-compatible, and Anthropic-compatible behavior.
- Existing configuration, CLI, and TUI command boundaries cover user-facing configuration behavior.

## Bounded steps

1. [x] Extend model configuration with a string defaulting to `nil`; add failing request/config tests; implement trimming and conditional JSON emission for OpenAI, OpenRouter, and Custom OpenAI-compatible providers while leaving Anthropic-compatible requests unchanged.
2. [x] Add a run-scoped CLI override and CLI model/configuration visibility; add failing CLI tests before implementation.
3. [x] Add the persistent `/reasoning-effort` TUI command and status visibility; add failing TUI command-boundary tests before implementation.
4. [x] Update configuration, provider, command, and interactive-mode documentation; run focused tests after each step and the full workspace suite at the end.
5. [x] Review the final diff against the approved spec and tickets, then commit only the implementation changes and this plan.

## Invariants

- `nil` and empty values omit `reasoning_effort`.
- Non-`nil` values are trimmed and passed through unchanged.
- Anthropic-compatible requests never receive the OpenAI-only field.
- Existing thinking settings and temperature behavior remain independent.
- No model capability detection or fallback retry is introduced.
