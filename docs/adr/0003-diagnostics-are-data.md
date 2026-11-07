# ADR 0003 — Diagnostics are pure data with two renderers

**Status:** accepted
**Date:** Phase 1

## Context

UMAY's diagnostics are a defining feature and must serve both humans and
programs (editors, CI, AI agents). Rendering inside compiler phases would couple
every phase to output formats and make the JSON schema unstable.

## Decision

Compiler phases construct a `Diagnostic` value and push it into a collection.
Rendering happens only in the `diagnostics` module, which provides:

- a human renderer (carets, colours, causes, suggestions),
- a JSON emitter implementing the versioned schema.

Diagnostics are sorted deterministically before rendering.

## Consequences

- The JSON schema is defined and versioned in one place, and covered by tests.
- Phases cannot format output themselves, which prevents format drift.
- All diagnostics are collected; a phase cannot "print and abort", so a single
  run reports as many problems as it can.

## Alternatives considered

- **Renderer callbacks passed into phases**: rejected — couples phases to
  presentation and invites inconsistent output.
- **External JSON library**: rejected for the schema emitter — a hand-written
  emitter keeps the byte-level shape stable and reviewable, and adds no
  dependency.
