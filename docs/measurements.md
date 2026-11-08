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

- Runtime speed. Every program in the comparison above runs once and exits, so a
  timing would measure process start, not the compiler. A fair runtime benchmark
  needs a workload long enough to dominate startup, and none is built yet.
- Zig, which is not installed on the machine these numbers were taken on. The
  comparison names it rather than dropping it silently.

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

The same program (a 1000-function call chain ending in `return (r & 127)`), the
same machine, each toolchain at the version below. This compares *build time,
binary size and output*, not runtime speed: the program runs once and exits,
which is dominated by process start in every language and would measure the
loader rather than the compiler.

| Toolchain | Version | Build time | Binary | Program output |
| --- | --- | --- | --- | --- |
| UMAY | 0.1.0 (this tree) | 0.35 s | 240 KB | exit 1 |
| rustc | 1.98.0 | 0.50 s | 4.4 MB | exit 1 |
| clang | 22.1.8 | 0.48 s | 88 KB | exit 1 |
| gcc | 16.2.1 | 4.31 s | 96 KB | exit 1 |
| go | go1.27.0 | 1.08 s | 2.2 MB | exit 1 |
| zig | not installed | -- | -- | -- |

What the numbers say, and what they do not:

- **The output agrees.** Every toolchain computes the same value and exits 1.
  That is the claim worth checking -- a build time means nothing if the program
  is wrong -- and it holds for a program with a thousand inter-function calls.
- **UMAY is the fastest to build here**, ahead of rustc and clang by a little and
  of gcc by an order of magnitude. The gcc figure is real for this shape (a
  thousand heavily-inlined functions with `%`), not a mistake, and it says
  something about that optimiser on this input rather than about gcc in general.
- **The binary is small.** 240 KB against rustc's 4.4 MB and go's 2.2 MB, larger
  than C's because it links the UMAY runtime as ordinary object code. That is
  the design choice in ADR 0010 showing up as a number.
- **This is one program.** A build-time comparison over one call-heavy program
  is a data point, not a benchmark suite; a second shape (parsing, allocation,
  string handling) might reorder clang and UMAY. It is recorded as a data point
  with the toolchain versions stated, which is the most a single measurement can
  honestly be.

Zig is listed as not installed rather than omitted: a comparison a reader cannot
see the gaps in is not one they can check.
