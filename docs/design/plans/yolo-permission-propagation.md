# `--yolo` and permission propagation

**Type:** Implementation plan
**Status:** Implemented locally
**Date:** Unknown (legacy document)
**Related:** None
**Last verified:** 2026-07-31 (documentation review)

## Goal

Add an invocation-scoped `--yolo` authorization context that bypasses nca-level
authorization and safety gates while preserving operating-system permissions
and any outer sandbox. Repair resume and auto-resume so the requested
permission mode is not discarded.

## Bounded implementation steps

1. Add a serializable shared execution/authorization context with a latched
   `yolo` bit, thread it through CLI requests, supervisor initialization and
   resume, child sessions, and tool execution.
2. Add global `--yolo` parsing for all CLI entry paths, reject `--safe --yolo`,
   and ensure yolo is forwarded to children but never persisted as a future
   launch default.
3. Make approval precedence `deny > ask > allow`, preserve hard `plan` and
   `dont-ask` restrictions, and make yolo bypass nca policy without disabling
   OS-level failures or hook execution. Keep `bypass-permissions` distinct.
4. Apply the context to safe-mode registration, workspace/path checks,
   validation command checks, financial opt-ins, MCP restrictions, direct REPL
   shell execution, skills/profiles, and blocking hooks.
5. Expose the active context in startup warnings, prompts/status, session
   metadata, and event/JSON output with backward-compatible deserialization.
6. Add focused approval, propagation, serialization, CLI, tool, hook, and
   child-session tests; run formatting, focused tests, full workspace tests,
   and Clippy.

## Constraints

- `--safe --yolo` is invalid.
- Yolo is invocation-scoped and cannot be downgraded by profiles, skills, or
  `/permissions`.
- Only yolo parents create yolo children; non-yolo child approval requests
  fail loudly when no interactive approval path exists.
- No JavaScript/web wrapper or implicit installation is introduced.
