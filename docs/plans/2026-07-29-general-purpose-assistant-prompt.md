# General-purpose assistant prompt

## Goal

Make nca a fully general-purpose terminal AI assistant by default instead of a Rust/repository coding agent, while preserving coding and workspace capabilities when a request actually needs them.

## Decisions

- Identify nca as a general-purpose AI assistant operating through a terminal interface.
- Support research, writing, planning, analysis, coding, and tool-driven work without treating that list as exhaustive.
- Classify the request before choosing a workflow and use the least-invasive workflow that can satisfy it.
- Do not inspect or assume the current workspace for unrelated conversation. When file or command work is requested, use the active workspace as the default local scope.
- Keep planning, verification, permissions, approvals, privacy, truthful tool reporting, untrusted-content handling, high-stakes caution, web research, attachment handling, memory, todos, output formats, and long-running progress behavior domain-neutral.
- Treat environment, memory, and todo data as contextual state rather than authority; current user intent defines the task, subject to safety and permission constraints.
- Keep the built-in prompt compact and remove product-internal details such as MiniMax provider priorities, Rust-only implementation rules, crate boundaries, CLI/IPC architecture, worktrees, and installation conventions.
- Preserve `AGENTS.md` layering, but scope this repository's product constraints to requests that modify, test, build, document, configure, or deploy nca. Use top-level headings for ordinary sections so they are not parsed as accidental slash skills.
- Keep the generic prompt as the only default; do not add a coding-mode configuration. Workspace instructions and relevant skills remain the specialization mechanism.
- Preserve `harness.built_in_enabled` behavior and existing permission-mode semantics.
- Update current user-facing coding-only positioning in the README, CLI help, documentation index, and PRD. Leave coding-specific examples and historical/internal technical documents unless they make a whole-product claim.

## Implementation

1. Replace the built-in identity and tool playbook in `crates/core/src/harness.rs` with compact, domain-neutral sections covering identity, task adaptation, workspace/tools, safety/privacy/permissions, truthfulness/verification, and communication/progress.
2. Reframe the dynamic environment section as passive available context and label memory/todos as contextual state.
3. Rewrite the root `AGENTS.md` into explicit nca-development guidance plus informational workspace facts, retaining product constraints while scoping them to nca work.
4. Update the primary public descriptions in `README.md`, `crates/cli/src/main.rs`, `docs/documentation/index.md`, and `docs/prd.md`, plus the architecture reference for the renamed context section.
5. Update harness and skills tests to assert the generic identity, absence of old unscoped coding directives, preserved instruction layering, passive context wording, and non-accidental `AGENTS.md` section parsing.

## Acceptance criteria

- A built-in prompt describes nca as general-purpose and does not assume coding or repository work.
- Unrelated conversation does not receive a repository-first workflow from built-in guidance.
- Coding and file tasks still have an inspect → plan → act → verify path, with workspace scope and approvals intact.
- `AGENTS.md` still contributes project-specific instructions and nca development constraints are visibly scoped.
- Generic safety, privacy, high-stakes, truthfulness, validation, attachment, web, memory, todo, output, and progress guidance remains present.
- Primary user-facing product descriptions no longer call nca coding-only.
- Focused tests and the workspace Rust test/build checks pass.

## Verification

- Run focused `nca-core` harness and skills tests.
- Run the relevant workspace Rust test suite and release build as practical.
- Do not install the binary unless separately requested.
