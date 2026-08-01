# `.agents` Skills and TUI Skill Picker

**Type:** Feature specification
**Status:** Implemented locally
**Date:** 2026-07-31
**Related:** [AGENTS.md-backed instructions and skills plan](../plans/agents-md-skills.md)
**Last verified:** 2026-07-31 (documentation review)
## Problem Statement

nca users can install or maintain skills in several agent-compatible locations, but `.agents/skills` is not consistently discovered. This prevents project-local and user-global skills from appearing in nca even when they follow the standard `SKILL.md` layout.

The TUI also gives `/skills` a read-only list instead of a usable selection flow. Users must read a list, remember a command, close the list, and type the command manually. Although inline `/` completion can complete some skill commands, it does not provide a dedicated way to search and choose a skill when the user does not know its exact name. The current TUI path also does not use the complete configured skill-directory set when constructing its inline completion catalog.

This creates two related usability problems:

- Agent-compatible skills are invisible unless they happen to live in one of nca's existing locations.
- Discovering and invoking a skill requires unnecessary command memorization and repeated input.

## Solution

Make `.agents/skills` a first-class skill source and make TUI `/skills` open a dedicated searchable picker.

The picker will show all discovered skills, including their command, human-readable description, source directory, and whether they are manual-only. Users can search with text, navigate with the keyboard, and press Enter to insert `/<skill> ` into the composer. The user can then provide the task and submit it through the existing REPL command path.

Existing explicit slash invocation, inline slash completion, `AGENTS.md`-backed skills, nca skill directories, Claude-compatible directories, CLI listing, child-session skill selection, and skill execution semantics remain available.

The change is local to the skill catalog and TUI interaction seam. It does not publish an issue or alter unrelated multiline or paste behavior.

## User Stories

1. As an nca user, I want skills under the workspace `.agents/skills` directory to be discovered, so that agent-compatible project skills work without extra configuration.
2. As an nca user, I want skills under my global `.agents/skills` directory to be discovered, so that reusable personal skills are available across workspaces.
3. As an nca user, I want existing nca, Claude-compatible, and configured skill directories to continue working, so that adding `.agents` support does not require migration.
4. As an nca user, I want an ordinary `.agents/skills/<name>/SKILL.md` layout to be sufficient, so that I do not need nca-specific files to use a skill.
5. As an nca user, I want optional `.agents` metadata to improve labels and invocation behavior, so that skills behave consistently with their originating agent environment.
6. As an nca user, I want a skill with `allow_implicit_invocation: false` or `disable-model-invocation: true` to remain explicitly selectable, so that user-invoked workflows are not accidentally disabled.
7. As an nca user, I want manual-only skills omitted from automatic model skill suggestions, so that the model does not invoke workflows intended only for direct user selection.
8. As an nca user, I want malformed optional metadata to fall back to `SKILL.md`, so that a metadata formatting problem does not make an otherwise valid skill unusable.
9. As an nca user, I want `/skills` in the TUI to open a picker instead of a static list, so that I can choose a skill directly.
10. As an nca user, I want to type into the skill picker to filter results, so that large skill catalogs remain manageable.
11. As an nca user, I want filtering to match skill commands, names, and descriptions, so that I can find a skill by either its identifier or what it does.
12. As an nca user, I want to navigate picker results with Up/Down or `j`/`k`, so that keyboard selection matches the other TUI pickers.
13. As an nca user, I want Enter to insert the selected skill command into the composer, so that I can supply a task before execution.
14. As an nca user, I want selecting a skill not to execute it immediately, so that an accidental selection cannot start a turn with an incomplete task.
15. As an nca user, I want the picker to close with Escape or `q`, so that I can return to the current conversation without changing the draft.
16. As an nca user, I want the picker to show a useful empty state, so that I understand whether no skills are installed or my search has no matches.
17. As an nca user, I want the picker to show the source directory, so that duplicate names and project/global origins are understandable.
18. As an nca user, I want `/skills <search text>` to open with that text as the initial query, so that I can jump directly to a known category or partial name.
19. As an nca user, I want inline `/` completion to include the same configured and implicit skill sources, so that the fast path and the dedicated picker do not disagree.
20. As an nca user, I want existing slash commands to remain distinguishable from skills, so that selecting `/help`, `/model`, or another built-in command does not accidentally invoke a skill.
21. As an nca user, I want existing skill command precedence to remain stable, so that adding `.agents` skills does not unexpectedly replace an existing command with the same name.
22. As a CLI user, I want `nca skills` and its JSON form to include `.agents` skills, so that non-interactive discovery agrees with the TUI.
23. As an orchestrator, I want explicitly requested child-session skills to resolve from `.agents` directories, so that parent-to-child skill propagation works with agent-compatible catalogs.
24. As a maintainer, I want skill discovery to remain the shared source of truth, so that CLI listing, model manifests, REPL execution, child-session resolution, and TUI selection do not implement separate discovery rules.
25. As a maintainer, I want picker behavior to be testable through a small interaction seam, so that keyboard and filtering behavior can be verified without rendering or running a provider.

## Implementation Decisions

- Extend the skill catalog's implicit roots to include workspace `.agents/skills` through the default harness directories and global `~/.agents/skills` alongside the existing global-compatible roots.
- Preserve explicit `harness.skill_directories` override semantics. Existing source precedence remains stable: `AGENTS.md` skills retain priority, followed by configured and implicit filesystem roots in their existing order, with `.agents/skills` added after the existing `.claude/skills` workspace source.
- Treat `SKILL.md` as the required skill contract. Existing frontmatter fields continue to determine the command, instruction body, model override, permission mode, and context mode.
- Read optional `agents/openai.yaml` metadata when it exists. Use `interface.display_name` and `interface.short_description` as presentation metadata, and use `policy.allow_implicit_invocation` to determine whether the skill belongs in automatic model discovery.
- Treat `disable-model-invocation: true` in `SKILL.md` as the equivalent manual-only policy. The effective default is model-invocable unless either supported policy explicitly disables implicit invocation.
- Keep explicit user invocation independent from the implicit-invocation policy. A manual `/skill` command, a TUI picker selection, and an explicitly requested child-session skill remain valid. The model-facing skill manifest and model-only invocation path must respect the manual-only policy.
- Keep optional metadata parsing tolerant. If `agents/openai.yaml` is missing or cannot be parsed, retain the skill using `SKILL.md` fields and default invocation behavior.
- Extend the shared skill record with optional presentation metadata and an implicit-invocation flag without changing the command identity or instruction-body loading contract. The TUI will use an owned display projection rather than carrying full instruction bodies into overlay state.
- Make the TUI receive the configured skill-directory list from the runtime rather than constructing a hardcoded `.nca/skills` list. The inline slash catalog and `/skills` picker will therefore use the same configured roots and the catalog's implicit roots.
- Replace TUI `/skills`'s read-only information modal with a dedicated skill-picker overlay. The overlay owns a search query, selected row, scroll position, and lightweight skill entries containing command, display label, description, source, and invocation visibility.
- Open the picker by discovering skills at command time. If `/skills` has trailing text, use the trimmed trailing text as the initial query. A discovery failure is surfaced as an error; an empty successful catalog renders an empty state.
- Filter case-insensitively by command, display name, and description. Reset the selected row and scroll position when the query changes. Selection is bounded to the filtered result set.
- Support Up/Down and `j`/`k` navigation, character input, Backspace, Enter, Escape, and `q`. Enter closes the picker and inserts `/<command> ` into the composer at the end of the current draft; it does not submit or execute the skill.
- Keep inline slash completion as the fast path. It should display discovered skill entries with descriptions and source hints, while built-in commands remain separate entry kinds and preserve their existing behavior.
- Keep the CLI skill list, JSON shape, skill instruction expansion, provider/model overrides, permission overrides, and child-session skill request semantics unchanged except for the newly discoverable sources and metadata fields needed for compatibility.
- Use the existing TUI overlay and picker conventions as the primary seam. Introduce only the smallest dedicated skill-picker input/rendering module needed to keep discovery, filtering, state transitions, and drawing testable.

## Testing Decisions

- Tests must assert observable behavior at the highest available seam: discovered skill records, picker actions, command insertion, and user-facing CLI/TUI integration. Tests should not assert private rendering loops, widget construction details, or match-expression structure.
- The skill catalog tests will cover workspace `.agents/skills` discovery, global-root inclusion through the root-resolution seam, optional metadata mapping, manual-only policy handling, malformed optional metadata fallback, duplicate-command precedence, and regression behavior for `AGENTS.md`, nca, Claude-compatible, and configured directories.
- The model-manifest and invocation tests will verify that implicit-only skills are advertised and model-invocable while manual-only skills remain explicitly usable but are absent from automatic model discovery.
- The TUI picker module tests will cover empty catalogs, no-match queries, case-insensitive command/name/description filtering, selection bounds, scrolling, query reset, Escape/`q`, and Enter producing the exact inserted command with a trailing space.
- The REPL/TUI integration tests will verify `/skills` opens the picker, passes an initial query, and does not execute a skill until the resulting composer command is submitted through the existing command path.
- Slash-completion tests will verify that `.agents` skills appear alongside configured skill sources and that built-in command aliases remain distinct.
- CLI tests will verify that human and JSON skill listings include `.agents` discoveries without changing existing output for other sources.
- Verification will include focused core, TUI, and CLI tests, workspace formatting, Clippy, and the full workspace test suite as appropriate for the implementation changes.

## Out of Scope

- Publishing an issue or applying issue-tracker labels; this specification is local-only.
- Creating, installing, removing, or updating skills from `.agents` repositories.
- Executing a selected skill directly from the picker.
- A preview pane or full skill-body viewer in the picker.
- Folding dynamic skills into the Ctrl+P command palette; `/skills` remains a dedicated picker entry point.
- Changing existing skill command precedence beyond adding the new source at the agreed precedence position.
- Changing skill instruction expansion, supporting-file resolution, model/provider behavior, permission semantics, or child-session worktree behavior except where metadata visibility requires a minimal shared-record extension.
- Multiline composer, paste, image, or unrelated dirty-worktree changes.
- Git staging, branch creation, commits, or installation to system paths.

## Further Notes

- The local specification is intentionally implementation-ready but keeps file-level details out of the implementation decisions so the eventual seam can be placed cleanly during execution.
- The existing worktree contains unrelated uncommitted TUI and documentation edits. They must be preserved and reviewed separately from this feature.
- The recommended implementation order is: shared catalog/root and metadata support; catalog and manifest tests; TUI picker state/input seam; REPL integration; inline completion alignment; documentation and full verification.
