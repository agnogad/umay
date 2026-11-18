# ADR 0022 — Macro expansion

**Status:** accepted
**Phase:** v2 foundation
**Supersedes:** nothing

## Context

Phase 8 of the v2 plan is macros (`docs/v2/specification.md` notes proc-macro has
no ADR yet; this record fills that gap). Today attributes are **already
preserved** on structs, functions, enums and consts by `compiler/src/parser/items.rs`,
and `#[derive(Copy)]` is read through `has_derive` but has **no semantic effect**
— implementing that effect is in progress elsewhere. There is no macro
representation, no expansion pass and no `umay expand` tool.

Two hard project rules constrain the design: the compiler crate has **zero
dependencies**, and `[lints.rust] unsafe_code = "deny"` (ADR 0001). A Rust-style
proc-macro is a dylib loaded behind an ABI that needs both foreign code and
`unsafe` — adopting it now would break both rules. So Phase 8 is deliberately
split: a self-contained, dependency-free declarative engine now; a proc-macro /
plugin ABI left as design-only until the rules are consciously changed or a
separate macro-host process with a stable wire protocol is chosen.

## Decision

### 1. Scope split

- **Now:** declarative, token-level `macro_rules!`-style macros, implemented
  inside the compiler crate with no new dependencies and no `unsafe`.
- **Design-only:** proc-macros and plugin ABIs. No dylib loading, no arbitrary
  codegen hooks. Ship nothing in this ABI until this ADR is superseded by an
  explicit decision to relax the no-dependency / no-`unsafe` rules, or to run
  macros in a separate host process behind a versioned wire protocol.

### 2. Representation: source-level, before parsing

A macro is expanded as **tokens over source text, before the parser runs**.
There is no `ast::Item` macro variant and no macro node in HIR/MIR — the existing
item pipeline is untouched. One macro = one rule; matchers are limited to
`literal`, `ident` and `tt` fragments. Output is **deterministic textual
substitution**: identical input bytes always produce identical bytes, with no
timestamps or absolute paths (`docs/build-system.md` §7 reproducibility).

The engine lives in `compiler/src/macro.rs`; the entry point
`lib.rs::expand_source` takes source bytes and returns expanded source;
`cli.rs::cmd_expand` exposes `umay expand`, which prints the expanded source to
stdout. The expansion result is cached as a build artifact
`build.rs::Artifact::MacroExpansion`, written as `"expand.um"` and keyed on the
**source bytes** (content hash), not on mtime or path.

### 3. Hygiene and error mapping

For now expansion is **call-site textual substitution**: tokens are pasted at the
invocation with no fresh-name machinery. Full hygiene — fresh names, a `$crate`
equivalent, and a def-site vs call-site span distinction — is the **next
increment**, not part of this slice.

Errors are mapped in a post-pass: a diagnostic raised at a generated span is
remapped to the original **invocation span** and labelled `in expansion of
<macro>`. The `E0125`–`E0129` range is reserved for macro diagnostics
(definition errors, matcher mismatch, recursion/arity failures, invalid derive
targets). UI snapshots assert the remapped `file:line:col`.

### 4. Derive

`#[derive(Copy)]` is the **first real semantic effect** of an attribute: the
compiler marks the item `Copy` through the existing `has_derive` reading.
A derive-like *codegen* macro (e.g. emitting an `impl`) is implemented at the
source level by this token engine. A true proc-macro derive that runs arbitrary
Rust-style codegen needs the ABI decision in §1 before it can exist.

### 5. Security model

The expansion engine is a pure transformer: **no network access and no arbitrary
code execution**. It only matches and substitutes tokens. Any future plugin ABI
must first define versioning, sandboxing, signed packages and reproducible
expansion; `unsafe` stays denied in the compiler crate itself (ADR 0001), so
out-of-process execution is the preferred route for arbitrary macro code.

### 6. Tooling

`umay expand <file>` prints the fully expanded source, enabling the byte-for-byte
reproducibility check in `docs/v2/benchmarks.md` Phase 8. LSP span mapping for
macros is **deferred**; the language server keeps using the compiler (ADR 0014)
and will consume the invocation-span mapping once hygiene lands.

## Consequences

- Macros ship in v2 without adding dependencies or `unsafe`; the compiler crate
  stays dependency-free.
- Output is reproducible and cacheable by content hash, fitting the existing
  build cache.
- No name hygiene yet: a macro that introduces an identifier can collide with
  caller names — documented limitation until the next increment.
- Proc-macro authors get no arbitrary codegen in v2; the ABI is a later,
  explicitly-gated decision.
- Every new macro diagnostic must go through the invocation-span remap so errors
  never point into generated text.

## Alternatives considered

**Full proc-macro dylib ABI now.** Rejected: requires foreign dylib loading and
`unsafe`, breaking the zero-dependency and `unsafe_code = "deny"` rules
(ADR 0001). Deferred to a design-only item pending a conscious rule change or a
separate macro-host process with a stable wire protocol.

**AST-level macro nodes.** Rejected: a new `ast::Item` variant plus HIR/MIR
handling has a wide blast radius across the parser, semantics and both backends
for a feature that source-level token expansion already covers.

**No macros in v2.** Rejected: Phase 8 is an approved v2 deliverable, and
attributes/derives are already partially in place; deferring entirely would
leave `#[derive(Copy)]` inert.
