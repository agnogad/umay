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
| `umay build`, warm | 0.19–0.30 s | Assembly reused; `as` and `cc` still run. |
| Binary size | 91,048 bytes | Includes the UMAY runtime. |
| Compiler RSS | ~1.9 MB | Peak, on the same program. |

## What the numbers say

**The cache saves little on a full build.** Reusing the assembly removes about
0.01 s of a 0.21 s build, because the remaining work — starting two `as`
processes and one `cc` — dominates. The cache's value is correctness of reuse
(an unchanged rebuild does not recompile) rather than wall-clock time at this
size. A build that skipped `as` and `cc` when their inputs were unchanged would
save more, and is the obvious next step; it was not done here because the
existing tests do not yet distinguish "the objects are current" from "the objects
were produced".

**Codegen is not the bottleneck.** Splitting program and runtime codegen across
threads would buy a few milliseconds of a 100 ms compile step for the risk of
shared mutable state, which is why it was measured and rejected rather than
implemented.

**The binary is mostly runtime.** A 153-line program links to 91 KB. That is
`runtime/runtime.um` compiled by the same backend, not a C library, so the size
is honest — but it also means binary size is a property of the runtime rather
than of the program, and shrinking it is a runtime question.

## What is not measured

- Comparison against Go, Rust, Zig and C equivalents. Doing that fairly needs
  the same program in each language and a stated version of each toolchain; a
  number without both is a claim nobody can check.
- Anything at scale. The largest program in the repository is 153 lines.

Both are Phase 9 work, and both are listed in `ROADMAP.md` as not done.
