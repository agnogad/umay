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
  casts, `if`/`while` branches, direct and indirect calls, and returns lower to
  AArch64.
- String literals become `adrp`/`add` addresses, references take the address of
  a place, pointer offset and dereference load and store at the pointee's width,
  and fixed arrays support repeat-initialisation, indexing and byte copies.
- Struct literals are laid out with the shared `Layouts` code, field access adds
  the computed field offset, and aggregate values are copied byte by byte.
  Enum fields, tuple fields and generic substitution are still refused.
- `umay asm --target aarch64-unknown-linux-gnu` emits assembly that clang
  assembles into an ELF object with `e_machine = EM_AARCH64` (183).
- `umay build --target aarch64-unknown-linux-gnu --emit-object-only` compiles
  the program and the runtime into two AArch64 objects.
- Enum fields, floats, more than eight arguments, aggregate arguments and
  returns, generic struct fields, drops of values with destructors, and frames
  larger than 4095 bytes are refused with a `CodegenError` that names the
  construct. They are not miscompiled.
- A CI job installs `gcc-aarch64-linux-gnu` and `qemu-user-static`, then builds
  and runs a `println` program, so the ABI, frame and runtime path are checked
  by execution rather than by the assembler alone.

## Next steps, in order

1. **Calls with more than eight arguments and aggregate passing.** Implement the
   AAPCS64 stack-argument area and the rule for aggregates passed and returned
   in registers or through a hidden pointer.
2. **Enum and tuple fields, floats, generic substitution and drops.** These are
   the remaining value shapes a program can build.
3. **The end-to-end corpus on AArch64.** Run the existing program tests under
   qemu and compare exit codes and output with x86_64.
4. **Register allocation.** Replace the stack machine with a linear scan over
   AArch64 registers once correctness is pinned.
5. **macOS and Windows.** Mach-O and COFF dressing, the Darwin and Win64 ARM64
   ABIs, and artifacts for each.

## Consequences

- Two backends to maintain. The shared part is the MIR contract and the test
  corpus, not the instruction model.
- A scalar program produces more instructions than it needs. That is a
  deliberate first step; the benchmark score for this backend starts after
  correctness, not before it.
- `umay asm` is useful before `umay build` is. A user can inspect and assemble
  the output while the runtime path is still being completed.
