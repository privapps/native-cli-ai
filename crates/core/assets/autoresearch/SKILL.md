---
name: Autoresearch
command: autoresearch
description: Discover and explicitly run bounded metric-driven research programs
disable-model-invocation: true
---

# Autoresearch

This is a manual-only workflow. Select `/autoresearch` after the user has
explicitly asked to run a research program. Merely discovering this skill or a
program must never execute a command.

Use the `autoresearch` agent tool with `operation: "discover"` to list valid
Markdown programs. Explain the selected program's editable and fixed files,
metric goal, per-experiment budget, and constraints before requesting
execution. Start a session with `operation: "start"`; use `status` and
`results` to inspect durable state.

Continuation requires normal tool approval and must provide the agent's
chosen experiment descriptions, a direct executable plus argument array, and a
bounded policy. The host enforces at most 8 experiments and 3,600 seconds per
continuation. Treat crash, timeout, malformed-metric, and policy-failure
records as evidence; do not report them as successful experiments. Use
`operation: "cancel"` when the user asks to stop. Secondary constraint
violations are separate from primary metric regressions and are reported in
the audit record. A restarted continuation reuses the durable session-derived
experiment id only through explicit identity-checked worktree recovery;
unknown or tampered worktrees are refused.
