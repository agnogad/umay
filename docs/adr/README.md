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
| [0008](0008-ownership-borrowck.md) | Path-based move tracking, definite-init dataflow, borrows live to last use | accepted |
| [0009](0009-x86_64-backend.md) | Stack slots per local, virtual registers per value, linear-scan allocation, assembly text as the backend boundary | accepted |
| [0010](0010-runtime-in-umay.md) | The runtime is UMAY source compiled with every program; the C surface is `malloc`, `free`, `write`, `exit` | accepted |
| [0013](0013-send-and-sync.md) | `Send` and `Sync` are markers checked at the spawn boundary | accepted |
| [0012](0012-json-over-counted-bytes.md) | JSON is read and written over counted bytes | accepted |
| [0011](0011-concurrency.md) | Concurrency is process-based (`fork`) until `Send`/`Sync` are specified; async is not the plan | accepted |

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
