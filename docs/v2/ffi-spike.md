# FFI spike and `umay-bindgen` design

**Status:** v2 Phase 3 design/spike. Not implemented. ADR 0018 owns the trait-object
vtable ABI; this document does not restate it.

## 1. What `extern fn` already does

### 1.1 Surface as built

Two declaration forms, both parsed in `compiler/src/parser/items.rs:36-77`:

```umay
extern fn malloc(size: usize) -> *mut u8              // symbol == UMAY name
extern fn "sqlite3_open_v2" open(p: *const u8, ..) -> i32  // separate link symbol
```

The separate symbol exists so the prelude can declare `read`/`write` without
occupying those names for every program (`parser/items.rs:53-57`). `extern fn`
has no body (`E0112`, `sema/check_item.rs:979`); a body-less non-extern function
is `E0113`. In v1 `extern` carries no `unsafe` at the declaration
(`docs/language-specification.md:124-128`); the v2 FFI spec
(`docs/v2/specification.md:87-111`, gate `[unstable: extern_abi]`) keeps that for
declarations but makes a **call** to an `extern` item an unsafe operation. It
also adds `extern "C" { fn …; type …; static …; }` blocks with a named ABI and
opaque `extern type`s. Bindgen targets that v2 surface, not a new syntax of its
own. Parameter types
are mandatory. A definition in the program gets a `umay_user_` prefix; `main`,
the runtime's `umay_*` surface and `extern fn` are exempt
(`ROADMAP.md:343-347`). External symbols are found by the linker through
`codegen.rs:85-124`, and object-format symbol prefixes (`_`) come from
`target.rs:112-128`.

### 1.2 Capability matrix per target

Targets are the six in `compiler/src/target.rs:354-362`: `LINUX`, `MACOS`,
`WINDOWS`, `AARCH64_LINUX`, `AARCH64_MACOS`, `AARCH64_WINDOWS`. ABIs:
`SysV` / `Win64` / `Aapcs64` (`target.rs:34-56`); `AARCH64_WINDOWS` uses
`Aapcs64` for scalars but needs Microsoft's aggregate variant
(`target.rs:336-352`).

| Capability | SysV x86_64 | Win64 x86_64 | AAPCS64 Linux/macOS | Windows ARM64 |
|---|---|---|---|---|
| C function calls, scalar args/returns | yes | yes | yes | yes (scalars) |
| Function pointers / indirect calls | yes (`fn(A)->B`, `(*f)()`) | same | same | same |
| Struct by value | yes, SysV eightbyte classes (≤2 eightbytes in regs, larger on stack; `ROADMAP.md:366-370`) | aggregates passed by pointer (`target.rs:39-43`) | yes — HFA proven by CI helper (`.github/workflows/ci.yml:119-143`) | **gap**: aggregates to C follow Microsoft's variant, not HFA; only internal calls are consistent (`target.rs:338-341`) |
| Callbacks passed to C | works as raw address (`pthread_create`, `CreateThread` in `stdlib/platform/thread_*.um`), but untyped in C signatures | same | same | same |
| Variadic C functions | **gap**: `%al` forced to 0 for every external call (`isel/term.rs:431-437`, `asm.rs:303-307`) but UMAY cannot declare `...` (`explain.rs:282`) | non-variadic ABI | non-variadic ABI | non-variadic ABI |

Additional concrete gaps found in code/CI:

1. **No variadic declaration.** `...` is not in the grammar; the `%al=0`
   convention is emitted unconditionally because a non-variadic callee ignores
   it. A variadic call with a floating-point argument would need `%al` = number
   of vector registers used, which is not computed.
2. **Indirect-call return limit.** A function pointer cannot return more than
   16 bytes; the HTTP router uses an out-parameter for this reason
   (`docs/web-backend-plan.md:23-24`).
3. **Windows ARM64 C aggregates incomplete.** The `windows-arm` CI job runs
   only `println` and a scalar add (`.github/workflows/ci.yml:280-298`); the HFA
   test exists only for AArch64 Linux (`ci.yml:119-143`). This is the exact
   gap the v2 exit criterion names.
4. **Callbacks are untyped.** `sqlite3_exec`'s callback is declared
   `*const u8` (`stdlib/sqlite.um:21`), not a typed `fn(...)`; there is no
   marker distinguishing a C-ABI callback from a UMAY-ABI function pointer.
5. **No C layout declaration.** Struct layout is declaration order with
   max-field alignment (`docs/type-system.md:56-65`), but field reordering is
   reserved as an optimisation; a generated binding needs a way to pin it.
6. **Linking is a prefix heuristic.** `Target::system_libraries` scans the
   assembly for `sqlite3_` and adds `-lsqlite3`
   (`compiler/src/target.rs:391-406`). There is no general native-library
   manifest; that is [native-dependencies.md](native-dependencies.md).

## 2. Spike plan

### 2.1 Targets

Start with **SQLite**, then **SDL3**. SQLite is already bound by hand in
`stdlib/sqlite.um:17-36` and linked by the heuristic above, so the baseline is
measurable and every CI target has a C compiler. SDL3 is the harder case:
`SDL_Event` (union, by value) and `SDL_AudioSpec` (struct containing a callback
function pointer) exercise the two features SQLite does not. miniaudio is a
smaller alternative for the audio-callback case but adds no new ABI surface over
SDL3, so it is deferred.

### 2.2 Steps

1. From `sqlite3.h`, generate extern declarations for the functions in
   `stdlib/sqlite.um` with no UMAY wrapper functions: direct calls from UMAY,
   `open` returns a raw `*mut u8`, no helper `.c` shim.
2. Run the existing SQLite path and compare behaviour and binary output against
   the hand-written version. Record the generated declarations next to the
   source for review.
3. Bind the SDL3 subset: `SDL_Init`, `SDL_Quit`, `SDL_CreateWindow`,
   `SDL_DestroyWindow`, `SDL_PollEvent`, `SDL_OpenAudioDevice`,
   `SDL_PauseAudioDevice`, `SDL_AudioSpec`, `SDL_Event`, `SDL_EventType`.
   Open a window, pump events, and install a non-silent audio callback.
4. Add the Windows ARM64 aggregate case to the `windows-arm` job, mirroring the
   AArch64 HFA test in `ci.yml:119-143`.
5. Run the ABI test matrix in §4 as the gate.

"Without hand wrappers" means: no C shim (the current HFA test's `helper.c` is
acceptable only because it *defines* the C functions under test, not because it
adapts their signatures), and no UMAY function whose only job is to cast a
pointer or repack a struct before calling C. Thin safe *ownership* helpers
(`open`/`close` pairing a handle) are allowed and live in the stdlib, not in the
binding.

### 2.3 What to measure

- Generated lines vs hand-written lines (binding + removed wrappers).
- Compile time of the generated module; size and link time of the resulting
  binary.
- Number of platform-specific `#ifdef` branches in the generated output.
- ABI conformance pass/fail per target; Windows ARM64 aggregate result is the
  headline number.
- Every diagnostic emitted when the header uses an out-of-scope construct: it
  must name the header and construct, not fail silently.

### 2.4 Exit criteria (from the v2 plan)

1. A C library such as SDL3, miniaudio or SQLite is bound and used without hand
   wrappers on all supported targets.
2. Callback-based C APIs work — SDL events (`SDL_PollEvent`) and an audio
   callback (`SDL_AudioSpec.callback`) round-trip real data.
3. Windows ARM64 C aggregate interop is complete: a C function taking and
   returning a multi-field struct is called correctly from a UMAY program built
   for `aarch64-w64-mingw32`, with a CI job that proves it.

## 3. `umay-bindgen` architecture

A separate binary, `umay-bindgen`, run in the build pipeline (see
[native-dependencies.md](native-dependencies.md)). It is a tool, not language
syntax — the v2 spec explicitly leaves bindgen to a tooling spec and targets it
at `2.1.0` (`docs/v2/specification.md:109-111`). It is not part of the compiler
crate: the compiler has no dependencies and denies `unsafe`
(`docs/compiler-architecture.md:191-194`), while a header parser wants libclang.
It emits v2 `extern "C"` declarations behind `[unstable: extern_abi]`.

```text
.h (+ -D/-I flags from native dep resolution)
   │  libclang C API: cursor/AST walk, target triple from -target
   ▼
IR: CType, CStruct { name, fields, size, align, bitfield? },
    CEnum, CTypedef, CFn { name, params, ret, variadic }, CConstant
   ▼
emitter: one .um module: extern "C" blocks + layout-pinned
         structs/enums + extern types + consts
   ▼
compiler: parse/typecheck/link as ordinary UMAY source
```

- The target triple is an input, so one header emits different declarations per
  target (pointer width, `long`, packing, HFA shape). Emit into the OS user
  cache, never the project (build-system.md hard constraint 1,
  `docs/build-system.md:9-12`).
- Structs are emitted with a layout-freezing attribute (proposed `#[repr(C)]`,
  which fits the existing generic attribute parser — `parser/items.rs:30-34`,
  `explain.rs` E0120). This is an OPEN spec item, not yet in
  `docs/v2/specification.md`: without it a future field-reordering optimisation
  (`docs/type-system.md:63-65`) would silently break every binding.
- Opaque C handles (e.g. `SDL_Window`) become v2 `extern type` declarations used
  only behind pointers (`docs/v2/specification.md:102-103`), not `*mut u8`, so
  values cannot be confused.

### 3.1 Parser scope for v2

- Integer/float/`bool`/pointer types, `void`, `size_t`/`ptrdiff_t`, typedefs.
- Named `struct`, `union`, `enum`, forward declarations, opaque types;
  explicit sizes/alignments from clang are validated, not guessed.
- Function declarations: parameters, returns, function-pointer parameters and
  returns, `const` on pointee.
- Integer `#define` constants and simple `enum` constants.
- Variadic functions per §3.3.
- Header include resolution through libclang's `-I` paths supplied by the
  native dependency's include paths.

### 3.2 Explicitly out of scope

- C++ (classes, templates, name mangling, inheritance, exceptions). A C++ class
  is emitted as an opaque pointer only; any C++ vtable is not a UMAY vtable.
- Bitfields beyond the simple integer case; packed/aligned attributes beyond
  what clang reports as a single size/alignment pair.
- Macros that expand to expressions or statements, function-like macros,
  `#include`-generated declarations with no source location.
- VLAs, `_Complex`, `_Atomic`, vector/extended types, `__int128` ABI edge cases.
- COM/C++ vtables and dynamic dispatch. UMAY trait-object vtable layout is
  ADR 0018's decision; bindgen neither defines nor assumes it. If a later phase
  passes a UMAY `dyn Trait` across FFI, its vtable prefix and function-pointer
  slots follow ADR 0018, and bindgen continues to treat foreign vtables as
  opaque data. This document adds no vtable rule.

Unsupported constructs produce a diagnostic naming the file, line and
construct. They are never approximated into a wrong declaration.

### 3.3 Variadic C functions — decision

Emit variadic declarations such as `printf(fmt: *const u8, ...) -> i32` in an
`extern "C"` block, requiring at least one fixed parameter. The v2 grammar
(`docs/v2/specification.md:92-98`) has no `...` today, so this is a required
amendment to spec §5 (language owner): accept a terminal `...` on `extern`
function items only. Parser, type representation and codegen then accept it only
there. The existing `%al=0` emission
(`isel/term.rs:433-437`) remains correct only while no floating-point argument
is passed. Decision: in v2, passing a float argument to a variadic C function is
a diagnostic; computing `%al` from the vector-register count is a follow-up, not
part of this spike. This keeps `printf`-style integer/pointer calls working
without shipping a silent miscompile.

### 3.4 Unsafe boundary and FFI safety rules

1. Every generated pointer is a raw `*const u8`/`*mut u8` or a pointer to an
   opaque `extern type`. Dereferencing, offsetting and null checks require
   `unsafe`, matching the existing model (`docs/memory-model.md`,
   `sema/check_expr_ops.rs:79`). Under the v2 `extern_abi` gate a **call** to an
   extern item is itself an unsafe operation (`docs/v2/specification.md:100-101`)
   — the declaration stays safe. That is the boundary the generated module is
   written against.
2. Generated structs are `Copy` only if every field is `Copy`; C ownership is
   never implicit. A C function taking a pointer to a struct requires an
   explicit borrow in `unsafe`.
3. No UMAY reference (`&T`) is passed to C in v2 — raw pointers only. This
   avoids promising that C respects borrow rules the checker cannot see.
4. Callback function pointers use the target's native ABI, which is the C ABI on
   every supported target. Capture-less functions/closures coerce to a plain
   `fn` pointer and cross directly (ADR 0017, ADR 0020 §4;
   `ROADMAP.md:319-322`). Capturing state crosses only as an explicit opaque
   `userdata` pointer to a boxed context that outlives the registration, under
   ADR 0017 escape rules — a closure environment is never passed by value.
5. `bindgen` output is data, not trusted code: generated source is reviewed like
   any other source, and its header inputs are content-hashed into the build
   cache key (see [native-dependencies.md](native-dependencies.md)).
6. FFI misuse (bad pointer, wrong signature) is not a compiler vulnerability —
   `SECURITY.md:35-37`. A generated *wrong signature* due to a bindgen parser
   bug is.

## 4. ABI conformance tests per target

Each test: a C helper compiled by the target's C compiler defines the callee;
a UMAY program calls it directly via generated declarations; CI asserts exact
values. The helper defines functions, it does not adapt their signatures.

| Target | ABI rules under test | Where |
|---|---|---|
| `x86_64-unknown-linux-gnu` | SysV eightbyte classification, ≤2 eightbytes in regs / larger on stack, variadic `%al=0` | existing codegen tests (`ROADMAP.md:366-370`); add variadic and struct cases |
| `x86_64-apple-darwin` | SysV with Mach-O symbol prefix (`target.rs:112-128`) | `ci.yml:22-53` smoke + new ABI case |
| `x86_64-pc-windows-gnu` / `x86_64-w64-mingw32` | Win64: 32-byte shadow space, callee-saved `rsi`/`rdi`/`xmm6-15`, aggregates by pointer (`target.rs:39-47,451-470`) | `ci.yml:208-259` + new ABI case |
| `aarch64-unknown-linux-gnu` | AAPCS64 HFA (float/double homogeneous aggregates) | existing `ci.yml:119-143` |
| `aarch64-apple-darwin` | AAPCS64, Mach-O object shape | `ci.yml:153-206` |
| `aarch64-w64-mingw32` | Microsoft ARM64 aggregate rules (not HFA), return and arg forms | **new** in `ci.yml:260-314` |

The Windows ARM64 case is the gate for exit criterion 3. UMAY-internal calls are
not a substitute: a C callee must be on the other side of the call.
