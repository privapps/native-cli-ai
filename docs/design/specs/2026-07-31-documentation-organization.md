# Documentation Organization

**Type:** Feature specification  
**Status:** Implemented locally  
**Date:** 2026-07-31  
**Related:** [Documentation organization implementation plan](../plans/2026-07-31-documentation-organization-implementation.md)  
**Last verified:** 2026-07-31 (documentation review)

## Problem Statement

The documentation is difficult to navigate because it mixes user guidance, product requirements, technical references, research, implementation plans, and design specifications across several partially overlapping hierarchies. User documentation is grouped separately from other documentation, while plans and specifications are split across multiple locations and some planning documents remain loose at the documentation root.

There is no single documentation landing page that explains these audiences and document lifecycles. As a result, users may miss relevant guidance, contributors may not know which document is authoritative, and maintainers may leave duplicate, superseded, or stale material active. The root README also carries a large amount of documentation navigation that should be easier to maintain centrally.

The repository needs a staged reorganization that improves discoverability without breaking existing links or changing nca runtime behavior. The migration must also account for stale architecture, dependency, and product claims and for duplicated provider-debugging material.

## Solution

Create a single documentation hub that routes readers to clearly owned areas for user guidance, product information, technical reference, design work, research, and images. Establish explicit lifecycle rules for active, archived, superseded, and draft documents.

Move the current user guides into a dedicated user area; group product requirements and roadmap material together; group architecture, technology, orchestration, context, and generated command reference material as technical reference; and consolidate plans and specifications under a design area with active and archived lifecycle subdirectories. Keep research separate from plans and specifications.

Reduce the root README to product orientation, installation, quick start, common commands, and links to the documentation hub. Replace the current command reference with a hand-written workflow guide plus a Rust-native generated CLI reference derived from the actual Clap definitions.

Add consistent metadata to plans and specifications, reconcile stale documents with the current Rust implementation, merge duplicate provider-debugging plans, and convert the existing todo document into a concise actionable roadmap. Preserve compatibility stubs for moved public pages and tracked references for at least one release cycle.

## User Stories

1. As a new nca user, I want one documentation entry point, so that I can quickly find installation, configuration, interactive-mode, provider, tool, permission, session, and advanced usage guidance.
2. As a user looking for a command or flag, I want a reliable CLI reference generated from the actual command definitions, so that the documentation does not drift from the shipped binary.
3. As a contributor, I want documentation categories to have clear ownership, so that I know where a new guide, reference, plan, specification, or research note belongs.
4. As a contributor, I want active and archived design documents separated, so that I can distinguish current implementation direction from historical context.
5. As a maintainer, I want every canonical document linked from an appropriate hub or index, so that orphaned documentation is discoverable and reviewable.
6. As a maintainer, I want each plan and specification to identify its type, status, date, related documents, and last verification date, so that I can assess whether it is current before relying on it.
7. As a maintainer, I want superseded or duplicate plans identified and consolidated, so that implementation work is not guided by conflicting documents.
8. As a reviewer, I want documentation claims checked against the current Rust source and dependency configuration, so that architecture, provider, permission, MCP, image, and capability descriptions remain accurate.
9. As a contributor following an existing link, I want moved public pages to resolve through compatibility stubs, so that the reorganization does not break established workflows.
10. As a maintainer, I want a repository-level documentation validation check, so that broken links, missing indexes, misplaced canonical documents, and CLI-reference drift are detected before release.
11. As a release maintainer, I want the documentation migration to be staged and reversible during the compatibility period, so that an incorrect move can be corrected without losing content or invalidating references.
12. As a project planner, I want the roadmap to contain only actionable work, so that completed, obsolete, and vague todo entries do not obscure current priorities.

## Implementation Decisions

- Adopt a canonical documentation taxonomy with separate user, product, reference, design, research, and asset responsibilities.
- Treat the documentation hub as the primary navigation surface for all audiences; keep the root README focused on product entry and high-value links.
- Move existing user-facing pages into the user area and technical/product material into the appropriate owned areas using staged moves.
- Consolidate all plans and specifications into the design area, separating active work from archived, completed, or superseded work.
- Preserve compatibility stubs for public or tracked references during at least one release cycle; stubs must point to the canonical destination and be clearly marked as compatibility pages.
- Add standard metadata to plans and specifications: document type, lifecycle status, date, related plan or specification, and last verification date.
- Replace the duplicated command reference with a workflow-oriented guide and a Rust-native generated reference derived from Clap help definitions. No JavaScript, Node.js, or web tooling is required.
- Merge duplicate custom-provider debugging plans and explicitly identify any superseded material rather than silently deleting it.
- Reconcile architecture, dependency, provider, permission, MCP, image, and product documents against the current implementation. Runtime behavior and public Rust APIs remain unchanged.
- Convert the todo document into a concise roadmap containing actionable items only.
- Update repository-maintainer guidance so that canonical locations, lifecycle states, ownership, compatibility stubs, and documentation validation rules are explicit.
- Use the existing documentation tree as the migration source and preserve unrelated user changes. External issue-tracker publication, branch creation, staging, and commits are outside this work.

## Testing Decisions

Use one primary testing seam: a repository-level documentation validation check that evaluates the documentation tree as a reader and maintainer would encounter it.

- Tests should verify external documentation behavior rather than internal implementation details: links and anchors resolve, hubs expose canonical documents, compatibility stubs point to moved pages, lifecycle metadata is present, and generated CLI content matches the shipped Clap definitions.
- The check should fail for orphaned canonical documents, duplicate active documents, documents in disallowed legacy locations after migration, broken relative links, missing compatibility targets, or command-reference drift.
- Exercise the seam against both the migrated tree and representative failure fixtures, including a broken link, an unindexed document, a stale stub, and a changed CLI flag.
- Existing README link tables and Markdown documentation conventions provide prior art for navigation, but the repository currently has no unified documentation validation seam; a small repository-native check is therefore justified.
- Manual source cross-checks remain part of review for factual claims that cannot be safely inferred by link or structure validation.

## Out of Scope

- Changes to nca runtime behavior, provider protocols, CLI APIs, TUI behavior, session lifecycle, or public Rust interfaces.
- A complete rewrite of all documentation prose or a redesign of the product documentation’s visual style.
- Localization, external website publication, issue-tracker publication, or a new web documentation platform.
- Introducing JavaScript, Node.js, Electron, Tauri, or other web wrappers for documentation generation or validation.
- Removing historical material without preserving its useful context or recording why it was superseded.
- Expanding the roadmap beyond cleanup of the existing actionable work.

## Further Notes

This spec synthesizes the July 31, 2026 conversation in which the documentation tree was identified as poorly organized and a reorganization proposal was requested. It was initially saved under the legacy design-specification area and is now located in the canonical design-specification area as part of the staged migration.

The migration should begin with the documentation hub and metadata conventions, then move content category by category, update links and compatibility stubs, reconcile stale claims, and finally enable the validation check. The intended outcome is a documentation tree in which location communicates audience, lifecycle, and authority.
