# ADR 0015: The AArch64 backend

Status: **in progress**. The first scalar slice is implemented; see "What exists
today".

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
- Scalar constants, locals, integer and bitwise binary operations, comparisons,
  casts, `if`/`while` branches, direct calls and returns lower to AArch64.
- `umay asm --target aarch64-unknown-linux-gnu` emits assembly that clang
  assembles into an ELF object with `e_machine = EM_AARCH64` (183).
- Aggregates, strings in code, indirect calls, floats, stack arguments, drops of
  values with destructors, and frames larger than 4095 bytes are refused with a
  `CodegenError` that names the construct. They are not miscompiled.
- Runtime code is not yet compilable, so `umay build` for AArch64 fails with the
  first unsupported construct rather than producing an executable.

## Next steps, in order

1. **Strings and the runtime surface.** Address a literal with `adrp`/`add`,
   support reference values, and compile the runtime's scalar path.
2. **Calls with more than eight arguments and aggregate passing.** Implement the
   AAPCS64 stack-argument area and the hidden-pointer rule for returns larger
   than 16 bytes.
3. **Projected places and aggregates.** Fields, arrays, `Deref`, struct copies,
   drop flags and destructor calls.
4. **Linking and running.** A cross linker plus `qemu-aarch64`, or a native
   `aarch64` CI runner, to run the existing end-to-end corpus and compare exit
   codes with x86_64.
5. **Register allocation.** Replace the stack machine with a linear scan over
   AArch64 registers once correctness is pinned.
6. **macOS and Windows.** Mach-O and COFF dressing, the Darwin and Win64 ARM64
   ABIs, and artifacts for each.

## Consequences

- Two backends to maintain. The shared part is the MIR contract and the test
  corpus, not the instruction model.
- A scalar program produces more instructions than it needs. That is a
  deliberate first step; the benchmark score for this backend starts after
  correctness, not before it.
- `umay asm` is useful before `umay build` is. A user can inspect and assemble
  the output while the runtime path is still being completed.
