# ADR 0010: The runtime is UMAY source

## Status

Accepted, Phase 6.

## Context

Every program needs to allocate, print and eventually format values. That code
has to live somewhere, and there are three places it could go:

1. **A binary blob** written in assembly or C and linked into every program. Fast
   to get running, impossible to read, and a second place where bugs live that the
   compiler's own tools cannot see.
2. **Compiler built-ins**: `print` as a special form, allocation as an MIR
   intrinsic. Every new capability is a new special case in the front end, and the
   language grows syntax that only exists for the library's sake.
3. **Ordinary UMAY source**, compiled with the program.

The project's own rules push towards the third. The backend is UMAY's own (ADR
0002), so there is no runtime library inherited from another toolchain. And a
claim like "the compiler type-checks its own library" is only worth making if it
is true.

## Decision

The runtime is `runtime/runtime.um`, embedded in the compiler with
`include_str!` and compiled with every program. `umay build` emits
`dist/umay_runtime.s` and `dist/umay_runtime.o` next to the program's object and
links all three.

The C surface is deliberately tiny: `malloc`, `free`, `write`, `exit`. Everything
else is UMAY. The current runtime is stateless, because the language has no
mutable statics yet; that is why output goes straight to a file descriptor
instead of through buffered C streams, which would have to cache state between
calls.

`extern fn` was added to the language so that these four bindings could be
declared and checked like any other function. A declared symbol carries the name
of the C function, so no name mangling is involved.

## Consequences

**Kept:**

- Every allocation, every byte written and every digit formatted is code the
  front end type-checks and the back end compiles. A bug in it is reproducible
  with `umay mir` and `umay asm`.
- There is no second calling convention to keep in sync.
- Adding to the library needs no compiler change at all.

**Costs:**

- The runtime is recompiled for every program. At this size that is milliseconds,
  and it disappears when the runtime becomes a cached object keyed by its own
  hash.
- C is still required for system calls. That is not avoided, only kept small.
- A program's binary contains the whole runtime, not just the functions it calls.
  Link-time dead-stripping of unused runtime functions is future work.

## What this made visible

Writing the runtime in UMAY found backend bugs that a built-in would have hidden
until much later. Each was a real miscompilation, not a runtime bug, and each is
now covered by a test in `compiler/tests/phase6_runtime.rs` or a unit test in the
module it lives in:

| Found by | Bug |
| --- | --- |
| `byte_len`'s `*s.offset(n)` | Reading through a pointer dropped the dereference and compared the address instead of the byte. |
| `println_i64`'s buffer | A zero-sized local shared an offset with its predecessor, so writing the return value overwrote a parameter. |
| `neg(v)` | Comparisons took signedness from the result type (`bool`), so every negative number compared as huge. |
| `n as usize` | Every load widened to 64 bits, so `-1i32` read back as `4294967295`. |
| Digit loop, spilled addresses | The store rewrite used `%rax` as scratch while `%rax` also held an address; and a spilled address register was never resolved, printing `%!v0`. |
| Extended loads | `movz`/`movs` printed the destination width twice (`movzq`, `movzqq`) instead of source-then-destination (`movzbq`). |

The lesson is not "library code is hard" — it is that a library written in the
language it ships with is a test suite that cannot be skipped.

## Alternatives rejected

- **Runtime in C.** Cheap to write, but it becomes the one part of the system that
  no UMAY tool checks, and it silently assumes a memory model that the checker has
  not proved.
- **Runtime built-ins in MIR.** Removes the C calls but puts library policy into
  the compiler, where every change needs a compiler release.
