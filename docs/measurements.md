# Measurements

Numbers recorded so that a claim about speed can be checked rather than
believed. Everything here is on one machine and says nothing about another; the
point is the ratio between stages, which is what decides where effort is worth
spending.

Measured with:

```sh
export UMAY_CACHE_DIR=/tmp/bench-cache
/usr/bin/time -f "%e s  %M KB" umay build examples/http-router.um
```

## `examples/http-router.um` (153 lines)

| Stage | Time | Notes |
| --- | --- | --- |
| `umay check` | ~0.00 s | Frontend only; below the timer's resolution. |
| `umay asm`, cold cache | 0.10 s | Lex, parse, sema, ownership, codegen to text. |
| `umay asm`, warm cache | 0.09 s | The cached assembly is read instead of generated. |
| `cc` link alone | 0.08 s | Two objects and `-no-pie`. |
| `umay build`, clean | 0.21–0.30 s | Assembly + two `as` runs + `cc`. |
| `umay build`, warm (before the executable cache) | 0.19–0.30 s | Assembly reused; `as` and `cc` still run. |
| `umay build`, warm (executable cached) | 0.17–0.23 s | Nothing is assembled or linked. |
| Binary size | 91,048 bytes | Includes the UMAY runtime. |
| Compiler RSS | ~1.9 MB | Peak, on the same program. |

## What the numbers say

**Caching the assembly alone saves little.** Reusing it removes about 0.01 s of a
0.21 s build, because starting two `as` processes and one `cc` dominates. Caching
the linked executable as well is what the numbers actually asked for: a warm
build now skips assembly and linking and lands around 0.18 s against 0.21–0.30 s
clean. Reuse correctness and wall-clock time happened to agree once the right
thing was cached.

**Codegen is not the bottleneck.** Splitting program and runtime codegen across
threads would buy a few milliseconds of a 100 ms compile step for the risk of
shared mutable state, which is why it was measured and rejected rather than
implemented.

**The binary is mostly runtime.** A 153-line program links to 91 KB. That is
`runtime/runtime.um` compiled by the same backend, not a C library, so the size
is honest — but it also means binary size is a property of the runtime rather
than of the program, and shrinking it is a runtime question.

## What is not measured

- A whole-program benchmark suite of real applications (a parser, an HTTP
  server under load). The workloads below are synthetic kernels, chosen so that
  the same program can be written in six languages and checked for the same
  output; they are not a claim about any real workload's speed.
- Linker behaviour and kernel scheduling under contention. All numbers are from
  an otherwise idle 12-core machine.

## At scale

The largest program in the repository was 153 lines, which says little about how
the compiler behaves on a real one. This measures a generated program instead: a
chain of `N` functions, each calling the previous with an arithmetic step, so
that nothing can be inlined away and every function is reached.

| Program | Lines | `umay check` | `umay build`, cold | `umay build`, warm | Binary | Peak RSS |
| --- | --- | --- | --- | --- | --- | --- |
| 1000 functions | 1,004 | 0.11 s | 0.35 s | 0.27 s | 240 KB | ~20 MB |
| 5000 functions | 5,004 | 0.83 s | 1.55 s | 1.36 s | 884 KB | ~68 MB |

A fivefold increase in input costs about seven times the work in the frontend
(`check` 0.11 s to 0.83 s) and about four times in a full build -- close to
linear, with a mild superlinear term from inference and borrow checking where the
per-function state grows. RSS grows with the function count for the same reason.
Nothing here is a cliff; the numbers are recorded so that a future change that
makes them a cliff is visible.

The generated programs are not checked into the repository: they are a few lines
of Python each, and the command that produced each number is the `umay` binary
with `UMAY_CACHE_DIR` pointed at a throwaway directory.

## Against other languages

Five workloads, written once per language in `scripts/bench.py`, each printing
the same integer so correctness is checked before any timing is believed. This
one *does* measure runtime speed as well as build time, because every workload
runs long enough to dominate process start:

- **callchain** -- 1000 functions, each calling the previous with an arithmetic
  step, so nothing can be inlined away. Measures build time and cross-function
  call overhead.
- **compute** -- 50,000,000 iterations of integer multiply, add and modulo.
  CPU-bound.
- **alloc** -- 2,000,000 small `malloc`/`free` pairs. Allocation-bound.
- **fib** -- `fib(38)`, 63 million recursive calls. Call/return overhead.
- **str** -- a 10 MB buffer filled ten bytes at a time. Memory-copy bound.

All on the same idle 12-core machine, each command repeated (5 builds, 7 runs)
and the **median** reported: one run measures the machine's mood, not the
toolchain. RSS is the peak of the measured child itself (from `wait4`), not a
high-water mark shared with other builds.

### Build time and size across all five

| Toolchain | cc | callchain build | compute build | fib build | binary (fib) | build RSS |
| --- | --- | --- | --- | --- | --- | --- |
| UMAY 0.1.0 | own | **0.34 s** | 0.14 s | 0.14 s | **83 KB** | **12 MB** |
| rustc 1.98.0 | own | 0.53 s | 0.11 s | 0.11 s | 4.5 MB | 123 MB |
| clang 22.1.8 | own | 0.40 s | 0.09 s | 0.09 s | 16 KB | 85 MB |
| gcc 16.2.1 | own | 4.34 s | **0.07 s** | 0.11 s | 16 KB | 35 MB |
| zig 0.17.1 | own | 1.04 s | 0.38 s | 0.37 s | 144 KB | 119 MB |
| go 1.27.0 | own | 0.21 s | 0.17 s | 0.17 s | 2.4 MB | 69 MB |

### Runtime across the four compute kernels (median, seconds)

| Toolchain | compute | alloc | fib | str |
| --- | --- | --- | --- | --- |
| gcc | **0.375** | **0.018** | **0.118** | 0.007 |
| clang | 0.421 | 0.021 | 0.221 | **0.003** |
| zig | 0.541 | 0.025 | 0.222 | 0.003 |
| rust | 0.428 | 0.020 | 0.218 | 0.010 |
| go | 0.438 | 0.020 | 0.362 | 0.026 |
| UMAY | 0.717 | 0.077 | 0.772 | -- |

UMAY has no `str` row: the standard library's `String` does not yet expose the
append-in-a-loop that the other languages use, so the workload was not written
for it rather than written unfairly. The `--` is that omission, shown.

### What the numbers say

- **The output agrees everywhere.** Every language prints the same integer in
  every workload. This is the claim worth checking first; a ratio means nothing
  if the program is wrong. `scripts/bench.py` prints a warning when they differ.
- **UMAY builds fast and small.** It is within 0.1 s of the fastest C compiler on
  the call-heavy program while gcc takes 4.3 s there (a real gcc result for a
  thousand heavily-inlined functions with `%`), and its peak RSS is 12 MB against
  85–147 MB for the others. Its binary is 83–240 KB against rustc's 4.5 MB and
  go's 2.4 MB: the UMAY runtime links as ordinary object code, as ADR 0010 chose.
- **UMAY runs slower, and this is the honest gap.** It is about 1.6x slower than
  the C compilers on `compute`, 1.7x on `fib` and 4x on `alloc`. The cause is not
  a wrong instruction — the programs compute the right answer — but register
  allocation quality: every intermediate value is stored to the stack and
  immediately reloaded instead of staying in a register, so a loop body does
  several memory accesses the C compilers turn into one or two. That is a
  selector/allocator optimisation, not a defect, and it is the clearest place
  where effort would buy speed.
- **`callchain` run times are near the noise floor.** Every language finishes it
  in 3–7 ms; at that size the numbers measure the loader more than the call path,
  which is why they are reported but not leaned on.

### What this does not measure

- Real applications. These are kernels chosen to be writable in six languages and
  checkable; a parser or an HTTP server under load could reorder them.
- Compile time on a large project. `At scale` above covers that separately.
- Zig's `std.debug.print` cost, which is a property of one library call (it takes
  ~24 s to compile in 0.17) and was avoided in the workloads by printing through
  libc's `write` instead. That choice is in the script and is commented there.

## After register-allocation work (2026-11-16)

The table above was recorded before local scalar promotion in `isel` and the
spill/frame fixes in `regalloc` and `asm`. Re-running the same harness with the
current release binary gives:

| Workload | Recorded before | Now | Change |
| --- | --- | --- | --- |
| `compute` | 0.717 s | 0.589 s | ~18% faster |
| `fib` | 0.772 s | 0.663 s | ~14% faster |
| `alloc` | 0.077 s | 0.058 s | ~25% faster |

Same machine, same build and run repetitions (5 builds, 7 runs, median), same
programs and expected outputs. The `fib` median moved between two runs (0.85 s
then 0.66 s); 0.66 s is the repeatable one and is what is recorded. The gap to
gcc is still real -- roughly 1.8x on `compute` and 5x on `fib` -- so this is a
reduction of the spill/reload tax, not its elimination.

Regenerated with:

```sh
cargo build --release
python3 scripts/bench.py --only compute
python3 scripts/bench.py --only fib
python3 scripts/bench.py --only alloc
```

## Reproducing

```sh
cargo build --release
python3 scripts/bench.py            # tables
python3 scripts/bench.py --json     # machine-readable
python3 scripts/bench.py --only fib # one workload
```

The harness needs `rustc`, `gcc`, `clang`, `zig` and `go` on `PATH`; a toolchain
that is missing is skipped rather than reported as a failure.
