# v2 performance budget

Status: plan. Numbers come from [`docs/measurements.md`](../measurements.md) (one idle 12-core machine, medians of 5 builds / 7 runs). The point is the ratio against a recorded baseline, not an absolute number on another machine. All thresholds marked `(proposed)` are decisions to confirm.

## 1. Two metric classes — keep them separate

| Class | Measured on | Why separate |
| --- | --- | --- |
| **Compiler-host** | the `umay` compiler process | Compile time, compiler RSS, compiler binary size. Changes here affect every developer. |
| **Generated-program** | the executable `umay build` emits | Binary size, runtime peak RSS, runtime speed. Changes here affect every user of UMAY programs. |

A GUI/mobile bundle is **not** a generated-program binary in the 91 KB sense — it contains assets and platform frameworks. Bundles get their own recorded baseline per target (section 4, last row) and are excluded from the core-size budget.

## 2. Baseline command (reproducible)

```sh
export UMAY_CACHE_DIR=/tmp/bench-cache          # throwaway; no warm-cache hits
cargo build --release --manifest-path compiler/Cargo.toml

# Build time, binary size, runtime and peak RSS (wait4 peak of the child) for
# every workload and available toolchain; medians of 5 builds / 7 runs:
python3 scripts/bench.py --json

# One workload only (substring match), e.g. the 1000-function call chain:
python3 scripts/bench.py --only callchain

# Optionally compare current medians to the recorded baseline (ratio table;
# WARN/HARD-FAIL lines appear only with this flag, never by default):
python3 scripts/bench.py --baseline docs/measurements.baseline.json
```

Record the values `bench.py --json` prints: median build and run wall seconds, peak RSS KB, emitted binary bytes, and the median across repetitions. The first run is stored as `docs/measurements.baseline.json` (informational, host-specific). Use the same toolchain and runner class for every comparison; on shared/GitHub runners compare ratios only and require ≥3 runs. A pinned self-hosted x86_64 Linux runner is preferred for the perf job to remove machine variance.

## 3. Compiler-host budget

| Metric | Baseline (measurements.md) | Warn (proposed) | Hard fail (proposed) | Rationale |
| --- | --- | --- | --- | --- |
| `umay check`, 5000-func chain | 0.83 s | > 0.95 s | > 1.10 s | ~15% / ~30% headroom on the worst frontend case; frontend grows mildly superlinearly. |
| `umay build` cold, http-router (36 lines) | 0.21–0.30 s | > 0.35 s | > 0.45 s | Link dominates; adding a codegen pass must not double a sub-second build. |
| `umay build` cold, 5000-func chain | 1.55 s | > 1.85 s | > 2.20 s | Same factor as check, applied to the full pipeline. |
| `umay build` warm (cached), http-router | 0.17–0.23 s | > 0.30 s | > 0.40 s | The executable-cache win (0.30 → 0.18 s) must survive v2. |
| Compiler peak RSS, 5000-func chain | ~68 MB | > 90 MB | > 120 MB | RSS tracks per-function state; a cliff here means a leak or unbounded map. |
| Compiler binary size | **record at Phase 0** (not in measurements.md) | > 1.5× baseline | > 2× baseline | The compiler is dependency-free; a 2× jump means an accidental dependency or embedded blob. |

## 4. Generated-program budget

| Metric | Baseline (measurements.md) | Warn (proposed) | Hard fail (proposed) | Rationale |
| --- | --- | --- | --- | --- |
| Binary size, http-router | 91,048 B | > 112 KB | > 128 KB | Binary size is a property of the runtime (compiled from `runtime/runtime.um`); v2 runtime additions must be paid for, not hidden. |
| Binary size, `fib` | 83 KB | > 104 KB | > 120 KB | Same as above on the cross-language benchmark program. |
| Binary size, 5000-func chain | 884 KB | > 1.0 MB | > 1.2 MB | Guards per-function codegen bloat. |
| Closure-heavy program vs non-closure twin | **record at Phase 1** | > +15% | > +25% | Monomorphized closure types duplicate code; bound the duplication. |
| `dyn` program vs static-dispatch twin | **record at Phase 2** | > +8% | > +15% | A vtable is a few words per type; anything larger is accidental monomorphization. |
| Boxed-async program vs hand-written state machine | **record at Phase 4** | > +20% | > +35% | Boxing and per-state fields cost size; bound the overhead. |
| Generated-program peak RSS (heap benchmark, N = 1,000,000 live objects) | **record at Phase 0** | > 1.15× | > 1.3× | Runtime memory of user programs; new allocation paths must not leak. |
| Runtime speed, `compute` / `fib` / `alloc` kernels | 0.589 / 0.663 / 0.058 s | +5% | +10% | after regalloc work; v2 abstractions must not reintroduce the spill/reload tax. |
| GUI / mobile bundle size, per target (`.ipa`, `.apk`, packaged zip) | **record at Phase 5/6** | > 1.2× | > 1.5× | Tracked separately; includes assets/frameworks, not comparable to the 91 KB core. |

Baselines marked "record at Phase N" do not exist yet; the first CI run at that phase establishes them and stores them as JSON alongside `--json`. Until a baseline exists, the corresponding row is informational.

**Why the custom backend matters:** there is no LLVM optimizer to absorb new abstractions. Every monomorphized closure, vtable, or async state machine lands in our own `isel`/`regalloc` output and in the cache key (a compiler rebuild invalidates every artifact — `build-system.md` §5.1). A frontend feature that is "free" with LLVM is a measured cost here, which is why each v2 cost gets its own bounded row.

## 5. Regression policy

| Delta | Action |
| --- | --- |
| ≤ Warn threshold | Pass, silent. |
| > Warn and ≤ Hard fail | Non-blocking warning annotation on the PR with the metric and delta; logged to the budget JSON. |
| > Hard fail | Fail the perf job and block merge. |
| Any baseline row with no baseline yet | Informational until recorded, then enforced from the next phase onward. |
| Measurement suspected to be noise | Re-run 3×; use median. Requires a reviewer note, not an exception. |

**Exception:** a hard-fail override needs approval from the compiler owner (CODEOWNERS), a linked issue with the cause and a remediation target, and a `perf-exception` label on the PR. It expires at the end of the next phase — it is a deferral, not a permanent waiver. Granting an exception updates the budget file with the new accepted baseline and a note in this file naming the ceiling and the upgrade path.

Perf job runs on `ubuntu-latest` with pinned toolchain; it reports to PRs but its hard-fail gate is part of the blocking class in [`ci-matrix.md`](ci-matrix.md) §5.
