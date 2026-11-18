# UMAY v2 Documentation

Status: **active specification track**. The v1 specification
(`docs/language-specification.md`, status stable, version `1.0`) is **frozen** and
is not edited by v2 work. v2 is a separate track built on top of it: a v2 program
is a v1 program plus explicitly gated additions, and every v1 program remains
valid. The contract that guarantees this is
[backward-compatibility.md](backward-compatibility.md).

The compiler is the final arbiter, as in v1: when the compiler and this track
disagree, fix the one that is wrong. Diagnostics-first still holds.

## Reading order

Documents are listed in dependency order. Do not settle a later stage before the
stage it depends on.

```text
ADRs 0017-0021  ->  specification.md  ->  feature-flags.md
                                      ->  backward-compatibility.md
                 ->  ci-matrix.md + performance-budget.md + benchmarks.md
                 ->  ffi-spike.md + native-dependencies.md + webview-decision.md
```

| Stage | Document | Purpose | Depends on |
|-------|----------|---------|-----------|
| 0 | ADR [0017](../adr/0017-closure-semantics.md) | Closure capture and call semantics. | — |
| 0 | ADR [0018](../adr/0018-trait-object-abi.md) | Trait system, object safety and `dyn Trait` ABI. | — |
| 0 | ADR [0019](../adr/0019-async-model.md) | Poll-based async model and executor/reactor split. | 0017, 0018 |
| 0 | ADR [0020](../adr/0020-gui-backend-strategy.md) | Desktop GUI backend strategy. | 0017 |
| 0 | ADR [0021](../adr/0021-mobile-strategy.md) | Mobile shell, cross-compilation and packaging. | 0020 |
| 1 | [specification.md](specification.md) | Normative v2 grammar and semantics, versioned `2.0.x`. | Stage 0 |
| 2 | [feature-flags.md](feature-flags.md) | Exact gate identifiers, diagnostics, channels and tool impact. | Stage 1 |
| 2 | [backward-compatibility.md](backward-compatibility.md) | v1 freeze contract, breaking-change rule, deprecation, edition question, corpus CI. | Stage 1 |
| 3 | [ci-matrix.md](ci-matrix.md) | Test and CI jobs that execute the v2 matrix. | Stage 2 |
| 3 | [performance-budget.md](performance-budget.md) | Ratios against recorded baselines; the cost ceiling per feature. | Stage 2 |
| 3 | [benchmarks.md](benchmarks.md) | One scenario, metric, command and exit criterion per phase. | Stage 2, 3 |
| 4 | [ffi-spike.md](ffi-spike.md) | Per-target ABI probe and `umay-bindgen` design. | Stage 3 |
| 4 | [native-dependencies.md](native-dependencies.md) | Build model for native libraries and build scripts. | Stage 3 |
| 4 | [webview-decision.md](webview-decision.md) | Webview runtime per OS and the shared IPC/asset bridge. | Stage 3 |

ADRs own decisions. The spec cites them by number and does not re-derive them;
the spike and design drafts constrain later ADRs rather than override them.

## Scope of 2.0.0

Closures, trait definitions/impls with `dyn Trait` and `impl Trait`, extern blocks
and FFI declarations, and `async fn` / `.await`. GUI, mobile and proc-macro are
named here for ordering only; their syntax arrives in later versions of this
track and their platform decisions are owned by ADR 0020 and ADR 0021.

Planned later: `async.md` (executor/reactor split and the blocking-escape
hatches named by [benchmarks.md](benchmarks.md), Phase 4).
