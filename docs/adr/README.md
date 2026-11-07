# Architecture Decision Records

Long-term decisions are recorded here. Each ADR is short: context, decision,
consequences, alternatives.

| ADR | Title | Status |
|-----|-------|--------|
| [0001](0001-rust-implementation-language.md) | Implement the compiler in Rust | accepted |
| [0002](0002-custom-backend.md) | Custom native backend, no LLVM or Cranelift | accepted |
| [0003](0003-diagnostics-are-data.md) | Diagnostics are pure data with two renderers | accepted |
| [0004](0004-optional-semicolons.md) | Optional semicolons with newline-aware termination | accepted |
| [0005](0005-blocking-on-init.md) | `let` always initializes; deferred init rejected | accepted |
| [0006](0006-semantic-analysis.md) | Three passes over items, body-local inference, conservative exhaustiveness | accepted |
| [0007](0007-mir.md) | CFG-based MIR, sugar removed during lowering, drop elaboration as a pass | accepted |

## Template

```markdown
# ADR NNNN — Title

**Status:** proposed | accepted | superseded by ADR-NNNN

## Context
What problem or force requires a decision.

## Decision
What we decided, stated plainly.

## Consequences
What becomes easier, what becomes harder, what is now impossible.

## Alternatives considered
What else was on the table and why it lost.
```
