# ADR 0015: The AArch64 backend

Status: **Linux backend implemented; performance and other systems remain**. See
"What exists today" and "Known limitations".

## Context

Every supported target is x86_64. That covers Linux, Intel macOS and Windows on
Intel, but not Apple Silicon, ARM servers, or the phones and single-board
computers the language may reasonably reach. An architecture with no backend is
refused before any work is done, which is correct but is not a plan for reaching
one.

The frontend, HIR, MIR, borrow checker, drop elaboration and optimisation are
architecture-independent. What differs is the layer from MIR down: instruction
selection, register allocation, assembly syntax, the calling convention, and the
object-file dressing.

## Decision

The AArch64 backend is a second lowering path from MIR, not an abstraction over
the x86_64 selector. The two instruction sets disagree about registers,
addressing modes, condition flags and nearly every encoding; a shared machine IR
would have to be so abstract that both selectors would carry most of their own
knowledge anyway.

The first path is a **stack machine**:

- Every local has an eight-byte slot relative to `x29`.
- Every value is computed in `x0` and `x1`.
- Results are normalised to their declared width after each operation, so
  `i8`, `i32` and `u32` arithmetic wraps at the right width.
- Direct calls follow AAPCS64: eight integer argument registers, result in
  `x0`, frame records at `x29`/`x30`.

This is slower than register allocation and much easier to make correct. A later
pass can replace it without changing the MIR interface or the tests that pin
behaviour.

Linux is the first system. It reuses the existing Linux runtime platform layer:
the C symbols and constants are the same; only the ABI and instructions differ.
The object is assembled by `clang --target=aarch64-unknown-linux-gnu`, which
needs no cross C library.

## What exists today

- `aarch64-unknown-linux-gnu` (and `arm64-linux`) parse to a target with
  `Abi::Aapcs64`.
- The full MIR surface the reference backend supports is lowered: constants,
  locals, integer, bitwise and floating-point operations, comparisons, casts,
  branches, direct and indirect calls, returns, `Drop` (a no-op until
  destructors exist), storage and drop-flag bookkeeping.
- Values:
  - string literals as `adrp`/`add` addresses;
  - references, pointer offsets and dereferences;
  - fixed arrays with repeat, literal, index and copy;
  - structs, tuples, enums (unit, tuple and struct variants) and generics,
    with the shared `Layouts` offsets and field-type substitution;
  - function pointers and `(*f)()` calls, using the pointer value as the entry
    point the way the reference backend does.
- AAPCS64 calls:
  - integer arguments in `x0`-`x7`, floats in `v0`-`v7`;
  - a two-register composite is kept on an even register;
  - composites up to 16 bytes travel in one or two general registers;
  - larger composites travel by reference;
  - arguments that do not fit go to a 16-byte-aligned outgoing stack area, and
    a ninth or later integer/float argument is read from the caller's area;
  - returns of up to 16 bytes come back in `x0`/`x1`; larger returns use the
    hidden pointer in `x8`; scalar floats return in `d0`/`s0`.
- Frames are laid out per local with the shared `Layouts` code and support
  frames larger than the 4095-byte `sub` immediate.
- `umay asm --target aarch64-unknown-linux-gnu` emits assembly that clang
  assembles into an ELF object with `e_machine = EM_AARCH64` (183).
- `umay build --target aarch64-unknown-linux-gnu` compiles the program and the
  runtime. With an AArch64 C library and linker the result runs: the CI job
  links a program that exercises strings, structs, aggregate arguments and
  returns, enums, tuples, floats, nine arguments and the runtime's integer
  formatting, then compares its output under qemu.
- The compiler itself is built as a static `aarch64-unknown-linux-musl` binary
  with `rust-lld`, published as the `aarch64-linux-musl` archive and selected by
  `install.sh` on ARM64 Linux. CI builds it and runs `umay version` under qemu.

## Known limitations

- **HFA arguments and returns are not AAPCS64.** An aggregate made only of
  floats travels through the general-register path instead of `v` registers.
  Calls between UMAY functions are consistent because both sides use the same
  rule; a C function with an HFA parameter or return would not be.
- **No register allocation.** Every local lives in a stack slot and every value
  is computed in `x0`/`x1` or `d0`/`d1`. Correctness first; speed is a later
  pass.
- **Linux only.** Mach-O, COFF, the Darwin and Windows ARM64 ABIs, and the
  per-system runtime layers are the next systems, not current targets.
- Slices and `i128` are not in the reference backend either, so they are not
  AArch64-specific gaps.

## Next steps, in order

1. **The rest of the corpus under qemu.** Run every executable program test
   against the AArch64 target and compare output and exit status with x86_64.
2. **HFA and C interop.** Classify homogeneous float aggregates and use the
   `v` registers where AAPCS64 requires; then a C struct of floats can cross.
3. **Register allocation.** Replace the stack machine with a linear scan over
   AArch64 registers once correctness is pinned by the corpus.
4. **macOS and Windows.** Mach-O and COFF dressing, the `@PAGE`/`@PAGEOFF`
   relocations, the Darwin and Windows ARM64 ABIs, and artifacts for each.

## Consequences

- Two backends to maintain. The shared part is the MIR contract and the test
  corpus, not the instruction model.
- A scalar program produces more instructions than it needs. That is a
  deliberate first step; the benchmark score for this backend starts after
  correctness, not before it.
- `umay asm` is useful before `umay build` is. A user can inspect and assemble
  the output while the runtime path is still being completed.
