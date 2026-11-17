# ADR 0015: The AArch64 backend

Status: **Implemented for Linux, macOS and Windows ARM64, with scalar register
promotion.** A full register allocator remains; see "Known limitations".

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

- `aarch64-unknown-linux-gnu` (and `arm64-linux`) and
  `aarch64-apple-darwin` (and `arm64-macos`) parse to targets with
  `Abi::Aapcs64`. The macOS target emits Mach-O with `@PAGE`/`@PAGEOFF`
  relocations, ELF-only `.type`/`.size` directives omitted, and the system
  `cc` as linker.
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
- The macOS ARM64 compiler is built natively on a `macos-15` runner, emits
  Mach-O ARM64, links with the system `cc` and runs a program in CI. The
  `aarch64-darwin` archive is part of a tag release.

## Known limitations

- **HFA aggregates use the `v` registers and are checked against C.** A
  composite of one to four floats of one width is classified as an HFA, passed
  and returned in `v0`-`v3`, and a C helper exercises both directions. A float
  aggregate that is not homogeneous (for example `{ i64, f64 }`) still travels
  through the integer-register path; UMAY's own calls agree on that, but it is
  not AAPCS64 C interop.
- **No full register allocator.** Scalar locals are promoted to callee-saved
  registers when their address is never taken and no projected place names
  them. Values still pass through `x0`/`x1` or `d0`/`d1`, and floats,
  aggregates and address-taken locals remain in stack slots. On a three-million
  iteration loop the promotion roughly halved qemu wall time (0.064 s to
  0.030 s median) and reduced the loop's stack loads and stores from 21 to 2.
- **Windows ARM64 C aggregate calls follow Microsoft's ABI, not AAPCS64's
  HFA rule.** UMAY-to-UMAY calls are internally consistent. The runtime's
  external calls pass scalars, so startup, file and console calls are not
  affected.
- Windows ARM64 is cross-linked with llvm-mingw and the PE header is checked
  (`COFF-ARM64`, console subsystem). The native CI job runs the executable on a
  Windows ARM64 runner.
- Slices and `i128` are not in the reference backend either, so they are not
  AArch64-specific gaps.

## Next steps, in order

1. **The rest of the corpus under qemu/native runners.** Run every executable
   program test against the AArch64 targets and compare output and exit status
   with x86_64.
2. **A full register allocator.** Replace the remaining stack traffic with a
   linear scan over `x0`-`x17` and `d0`-`d7`, keeping the static promotion as
   the fallback for address-taken locals.
3. **Windows aggregate C interop.** Match Microsoft's composite classification
   when a UMAY aggregate crosses into a C function on Windows ARM64.

## Consequences

- Two backends to maintain. The shared part is the MIR contract and the test
  corpus, not the instruction model.
- A scalar program produces more instructions than it needs. That is a
  deliberate first step; the benchmark score for this backend starts after
  correctness, not before it.
- `umay asm` is useful before `umay build` is. A user can inspect and assemble
  the output while the runtime path is still being completed.
