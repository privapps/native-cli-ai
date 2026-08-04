# Design Documentation

Design documents describe proposed or historical implementation work.

## Plans

Implementation plans are collected in [plans](plans/). Current work remains in the plans directory during migration; completed or historical plans are in [plans/archive](plans/archive/). New plans should record their status, date, related specification, and verification state.

## Specifications

Feature specifications are collected in [specs](specs/). Historical specifications are in [specs/archive](specs/archive/). A specification records user-facing behavior, implementation decisions, testing decisions, and explicit scope boundaries.

The [documentation organization specification](specs/2026-07-31-documentation-organization.md) defines the migration represented by this structure.

## Local feature workflow

Implementation work is tracked in a local bundle before it becomes durable
design history:

1. `$to-spec` writes `.scratch/<feature-slug>/spec.md` and records unresolved
   assumptions in a `Draft` document.
2. `$to-tickets`, `$implement`, and `$validate` reuse that same bundle.
3. `$archive` requires an exact `PASS` in the latest validation entry and moves
   only the approved bundle to `.archive/<feature-slug>/`.

New workflow specs are not written under `docs/` and existing specs are never
overwritten. See the [documentation hub](../README.md) for the complete
artifact and validation rules.

The completed [`$`-prefixed skill references archive manifest](../../.archive/dollar-prefixed-skill-references/archive-manifest.md)
records the validated workflow bundle, its `PASS` report, and the files moved
out of `.scratch/`.
