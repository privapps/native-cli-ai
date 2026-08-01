# AGENTS.md-backed instructions and skills

**Type:** Implementation plan
**Status:** Implemented
**Date:** Unknown (legacy document)
**Related:** [Agents skills picker specification](../specs/2026-07-31-agents-skills-tui-picker.md)
**Last verified:** 2026-07-31 (implementation and test review)

## Goal

Treat the repo root `AGENTS.md` as both:
- an extended system-prompt instruction source that can steer agent behavior
- a lightweight skill manifest for reusable slash skills

## Why

- Teams already maintain `AGENTS.md` as durable project guidance.
- `nca` already discovers skills from `.nca/skills/` and home-level skill directories.
- Reusing `AGENTS.md` lowers setup friction for repo-local skills and makes skill discovery visible in version control.
- Project guidance in `AGENTS.md` should shape how the model reasons even when no explicit skill is invoked.

## Design

- The full repo-root `AGENTS.md` is loaded into the layered system prompt as an additional instruction block.
- The configured `workspace_root` is authoritative: nca does not walk parent or descendant directories for additional `AGENTS.md` files. An absent root file contributes no layer.
- `AGENTS.md` extends the built-in prompt; it does not replace built-in policy, `.ncarc`, or local instructions.
- Prompt order should stay stable: built-in -> permission mode -> `AGENTS.md` -> `.ncarc` -> local instructions -> skill summaries -> orchestration.
- Each root-level `## Heading` in `AGENTS.md` is parsed into one discovered skill.
- Optional directive bullets at the top of a section configure:
  - `model=<alias|inherit>`
  - `permission_mode=<plan|accept-edits|dont-ask|bypass-permissions|inherit>`
  - `context=<inline|fork>`
- `AGENTS.md` skills are loaded before filesystem skills and win on command conflicts.
- The complete instruction block remains separate from the root-level `##` skill projections; explicit skill loading remains the only way to load a skill body.
- All discovery surfaces should show the source so users can tell `AGENTS.md` skills from directory-based skills.

## User-facing surfaces

- Harness/system prompt layering
- `nca skills` and `nca skills --json`
- Harness skill summary in the system prompt
- TUI slash palette and REPL slash execution

## Documentation updates

- Document `AGENTS.md` as an instruction source in `README.md`.
- Document `AGENTS.md` as a skill source in `README.md`.
- Document the default skill directories alongside the new manifest source.
- Show that `nca skills` includes source metadata for discovered skills.

## Current status

- `crates/core/src/skills.rs` parses `AGENTS.md` sections into `Skill` entries.
- `crates/cli/src/tui/app.rs` includes discovered skills in the slash palette.
- Prompt layering, workspace-root scope, refresh behavior, child-session propagation, and focused regression coverage are complete. The local feature spec records the acceptance evidence for this implementation.
