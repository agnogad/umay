# UMAY Roadmap

Each phase has explicit exit criteria. A phase is not "complete" until its
criteria are met and the full test suite passes.

Legend: **done** · **in progress** · planned

---

## Phase 0 — Architecture and RFC · **done**

Deliverables:

- [x] `README.md`
- [x] `docs/language-specification.md` (v0.1)
- [x] `docs/compiler-architecture.md`
- [x] `docs/memory-model.md`
- [x] `docs/type-system.md`
- [x] `docs/diagnostics.md`
- [x] `docs/build-system.md`
- [x] `docs/adr/` — Architecture Decision Records
- [x] `ROADMAP.md`
- [x] `CONTRIBUTING.md`
- [x] Minimal Rust workspace with the `umay` binary

Exit criteria: syntax frozen for v0.1; module boundaries documented; diagnostic
code ranges reserved.

---

## Phase 1 — Lexer and Parser · **done**

Deliverables:

- [x] Source manager: `FileId`, `Span`, line/column mapping (character-based)
- [x] Tokenizer with full literal support (radix integers, suffixes, chars,
      strings, escapes, interpolation, raw braces)
- [x] Comment handling including nested block comments
- [x] AST with spans on every node
- [x] Recursive-descent parser: items, types, statements, expressions, patterns
- [x] Precedence climbing; assignment and compound assignment
- [x] Newline-aware optional semicolons
- [x] Error recovery: item-level and statement-level, no duplicate reports
- [x] Nesting-depth guard (`E0052`)
- [x] Diagnostics: human rendering with carets, versioned JSON schema
- [x] `umay explain` database covering every emitted code
- [x] CLI: `check`, `ast`, `explain`, `doctor`, `new`, `clean`, `version`
- [x] Tests: 30 unit + 49 integration, including panic-freedom invariants

Exit criteria (met):

- parse and validate realistic UMAY programs,
- reject malformed programs with a stable code and a cause line,
- never panic on truncated or mutated input,
- JSON output valid and versioned,
- every emitted code has an `umay explain` entry.

Known limitations at the end of Phase 1 are listed in the final report of this
milestone and in §13 of the language specification.

---

## Phase 2 — Semantic Analysis · done

- [x] Scopes and symbol tables: `ItemScope` (items + prelude, duplicate
      detection) and `ValueScopes` (frames, shadowing, same-scope warnings).
- [x] Interning `TyCtxt` with all primitive types plus tuple, array, slice,
      reference, raw pointer, ADT, function pointer, parameter and literal
      inference variables.
- [x] Three-pass checking: name collection, signature resolution, body
      checking, so declaration order never matters (ADR 0006).
- [x] Local inference: integer and float literals adopt the first concrete type
      they meet and otherwise default to `i32`/`f64`.
- [x] Struct, enum, tuple, array, slice and reference typing, including
      recursive types such as `struct Node { next: Option<Node> }`.
- [x] `Option` and `Result` as ordinary enums, with `Some`/`None`/`Ok`/`Err`
      resolving as values and in patterns.
- [x] Exhaustiveness checking for `match` over enums, `bool` and catch-all arms.
- [x] Operator typing, assignment and mutability, `as` cast validation with a
      narrowing warning, and compound assignment desugaring.
- [x] Method resolution with automatic borrows, plus built-in methods on `str`,
      `String` and slices.
- [x] Diagnostics `E1001`–`E1015` and warnings `W0101`–`W0103`, each with an
      `umay explain` entry.
- [x] `umay check` runs semantic analysis and reports schema-v1 JSON.

Not yet: `pub` visibility enforcement, module resolution from an entry point,
trait bound checking. These arrive with the module system and Phase 6.

Exit criteria: 91 accept/reject tests cover every rule; no invalid program in
the suite is accepted; every accepted program produces an HIR.

---

## Phase 3 — HIR and MIR · done

- HIR: names resolved, sugar lowered (`for`, `?`, method calls, interpolation).
- MIR: basic blocks, places, assignments, moves, borrows, drops, calls.
- Lowering from HIR to MIR, with drop elaboration.

Exit criteria: every valid program lowers to MIR; MIR is printable and
round-trippable for tests.

Delivered:

- `compiler/src/mir.rs` — `Body`, `BasicBlock`, `Place` with projections,
  `Operand`, `Rvalue`, `Statement`, `Terminator`, `MirProgram`.
- `compiler/src/lower*.rs` — the lowering, split by concern: expressions and
  places, operators and control flow, `match` and interpolation. Every
  sub-expression becomes a temporary, every branch an explicit block.
- `compiler/src/drop_elab.rs` — a reachability-aware live-drop analysis that
  inserts `Drop` terminators on each exit path and honours moves.
- `compiler/src/mir_print.rs` and `mir_parse.rs` — text format and its parser;
  `umay mir <file>` prints it.
- 30 tests in `compiler/tests/phase3_mir.rs`, including totality over every
  construct, drop placement, round trip, and no-panic over all input prefixes.

Deferred to later phases: optimisation passes (Phase 5), real `Drop` impls and
field-wise drop analysis, `String` as a distinct owning type, and borrow
checking of the MIR (Phase 4).

---

## Phase 4 — Ownership and Borrow Checking · done

- Definite initialisation (CFG dataflow).
- Move analysis; partial moves.
- Borrow analysis with both conflicting origins reported.
- Lifetime inference and elision rules from `docs/memory-model.md`.
- Drop elaboration refined to field-wise precision (Phase 3 handles whole
  locals).
- `unsafe` tracking and raw-pointer restrictions.
- Diagnostics: `E0201`–`E0213`.
- Adversarial test suite: use-after-move, double mutable borrow,
  mutable/immutable conflict, dangling reference, returning local reference,
  invalid pointer deref, invalid drop order, uninitialised read, data race.

Exit criteria: every adversarial fixture is rejected with the documented code;
the accept corpus still compiles. UMAY only claims Rust-comparable safety after
this phase's suite is reviewed and extended by fuzzing.

Delivered:

- `compiler/src/ownership.rs` — definite initialisation and move analysis as a
  forward dataflow over the MIR CFG. Places are tracked as paths, so a partial
  move (`p.x`) is distinguished from moving `p`.
- `compiler/src/borrowck.rs` — borrow conflicts, checked at statement
  granularity rather than block granularity, with each borrow living from its
  creation to its last use. Conflicts report both origins.
- `E0201` use after move, `E0204`/`E0205` overlapping borrows, `E0206`
  assignment to a borrowed place, `E0207` returning a reference to a local,
  `E0210` borrowing a temporary, `E0211` raw-pointer deref outside `unsafe`.
- 29 tests in `compiler/tests/phase4_ownership.rs`: an adversarial suite, an
  accept corpus, and diagnostic-quality assertions.

Not yet enforced, and stated here rather than implied: `E0202` is unreachable
because `let` always initialises (a design choice from ADR 0005), `E0208`–`E0209`
need field-wise drop elaboration, and `E0212`–`E0213` need the concurrency and
raw-pointer models from Phase 6.

---

## Phase 5 — Custom x86_64 Backend · done

- [x] Target description abstraction (`layout.rs`): sizes, alignments, SysV
  eightbyte classification.
- [x] Stack frames (`frame.rs`): one slot per MIR local, type-aligned.
- [x] Machine instruction representation (`machine.rs`) and instruction selection
  from MIR (`isel.rs`, `isel/stmt.rs`, `isel/term.rs`).
- [x] Linear-scan register allocation with spilling (`regalloc.rs`).
- [x] System V AMD64 calling convention, including stack arguments.
- [x] Assembly printing (`asm.rs`).
- [x] Optimisation levels 0–3 (`opt.rs`): constant folding/propagation, DCE,
  unreachable block elimination, copy propagation.
- [x] Linking via the system assembler and linker; `umay build`, `umay run`,
  `umay asm`.

Not done in this phase, and reported as `E3001` rather than miscompiled:
ELF64 object emission with relocations (the system assembler is used instead),
indirect calls through function values, aggregates returned by hidden pointer,
floating-point arithmetic. See [ADR 0009](docs/adr/0009-x86_64-backend.md).

Exit criteria: `umay build` produces a working Linux executable for a program
using arithmetic, control flow, structs, and calls; output is verified by tests.
**Met** — `compiler/tests/phase5_codegen.rs` compiles, links and runs 19 such
programs and checks their exit codes.

---

## Phase 6 — Runtime and Standard Library · in progress

### Step 1 — the runtime · done

- [x] `extern fn` in the language, so a C symbol can be declared and checked:
  `E0112` when an `extern fn` has a body, `E0113` when a function has no body and
  is not `extern`, both with a fix that can be applied mechanically.
- [x] `*const T` / `*mut T` with `as` casts between them, `*mut T` accepted where
  `*const T` is expected, `&str` and `&[u8; N]` castable to `*const u8`, and
  `.offset(n)` for pointer arithmetic.
- [x] `runtime/runtime.um`, embedded with `include_str!` and compiled into every
  program: `alloc`, `dealloc`, `read_byte`, `write_byte`, `byte_len`, `print`,
  `println`, `eprintln`, `println_i64`, `fail`.
- [x] The C surface is `malloc`, `free`, `write`, `exit` and nothing else.
- [x] `umay build` emits `dist/umay_runtime.s`/`.o` and links them with the
  program. See [ADR 0010](docs/adr/0010-runtime-in-umay.md).
- [x] String literals are `&str`, a NUL-terminated byte pointer, so passing one
  to C costs nothing.

Writing the runtime in UMAY exposed seven backend miscompilations, each now
covered by a test: a dropped dereference, a zero-sized local sharing an offset,
signedness read from a comparison's result type, every load widening to 64 bits,
`%rax` used as scratch while it held an address, a spilled address never
resolved, and malformed `movz`/`movs` mnemonics. See ADR 0010.

### Step 2 — a real standard library · in progress

- [x] The prelude is **real UMAY source** (`stdlib/prelude.um`), parsed with every
  program: `print`, `println`, `eprintln`, `println_i64`, `byte_len`, `alloc`,
  `dealloc`, `read_byte`, `write_byte`, `panic`. The placeholder `FnId(u32::MAX)`
  entries are gone, and a call is an ordinary call.
- [x] `stdlib/string.um`: `String`, an owned growable byte buffer with `new`,
  `from_str`, `len`, `is_empty`, `as_ptr`, `push`, `push_str`. Doubling growth, a
  NUL terminator kept one past the last byte so the buffer can be handed to the
  runtime's printers, and byte lengths that never split a multi-byte sequence.
- [x] A method from a real `impl` block now wins over a built-in of the same name,
  which is what lets the standard library define `String::len` in ordinary source.
- [x] Generic **instantiation at the type level**: a struct literal infers its
  type arguments from the values it is given (`Box { value: 7 }` is `Box<i32>`),
  the instance is carried on the value's type, a method's signature is
  substituted from the receiver's arguments (`Box<i64>::get -> T` is `-> i64`),
  and field access checks against the instantiated field type.
- [x] **Monomorphisation** (`compiler/src/mono.rs`). Each generic function, method
  and impl method reached from a concrete call is copied with its type arguments
  substituted, and the call is rewritten to the copy. Discovery runs over the
  bodies to a fixed point, so an instance created inside another instance is
  found too. A template is never lowered: only its instances are.
- [x] `Vec<T>` in `stdlib/vec.um` — `new`, `with_capacity`, `push`, `get`, `set`,
  `len`, `capacity`, `is_empty`, `free`, with doubling growth and a bounds check
  in `get`. The stride is `sizeof(T)`, so `Vec<u8>` packs one byte per element
  and `Vec<i64>` one word.
- [x] `sizeof(Type)` — the size of a type in bytes, folded at compile time.
  Monomorphisation substitutes its operand like any other type, so a generic body
  can size its own parameter.
- [x] `Type::fn(..)` — an associated function in the type's `impl` block with no
  receiver, which is how a constructor is written. A generic type's associated
  function takes its instance from the binding's annotation.

- [x] `fs` in `stdlib/fs.um` — `File::open_read`, `create`, `append`, `read`,
  `write`, `write_str`, `close`, `is_open`, `fd`; free functions
  `read_file_bytes`, `write_file_bytes`, `file_size`, `file_exists`,
  `remove_file`, `rename_path`, `create_dir`. A missing file yields a closed
  `File` and a size of `-1` rather than an abort. A short write is retried until
  the whole buffer is out or an error comes back.
- [x] `Option` and `Result` in `stdlib/option.um` — `is_some`, `is_none`,
  `unwrap_or` on `Option<T>`; `is_ok`, `is_err`, `unwrap_or`, `ok` on
  `Result<T, E>`. An `Option` with no way to name `T` is a compile error rather
  than a guess, and a `Result` may name only one of its two parameters.
- [ ] `Iterator` basics.
- [ ] `HashMap` in `stdlib/hashmap.um` — written (open addressing, FNV-1a,
  linear probing, `insert`/`get_or`/`has_key`/`grow`), but **not** in the prelude
  yet: two parser gaps below still stop it from checking.
- [ ] Parser: a `let` whose initialiser is `(*p).field as usize == 0` and is
  followed by `if` on the next line is not separated from the `if`.
- [ ] Parser: the last two assignments in an `unsafe { .. }` block that ends a
  function body are read as one statement unless each carries a `;`. An
  expression statement at the end of a block should be a tail expression only
  when it can be one; an assignment cannot, so the `;` must not be required.
- [x] `process` in `stdlib/process.um` — `Args` (from `main`'s `argc`/`argv`),
  `len`, `is_empty`, `program`, `get`, `has_flag`, `value_after`; free functions
  `env`, `env_is_set`, `exit`, `last_error`. A missing argument reads as an empty
  string rather than null, so a caller need not check before printing it.
  `file_size` now seeks to the end instead of reading the file, and `is_dir`
  tells a directory from a file.
- [ ] `net`: TCP basics, only once the memory model is stable.

Wiring up `fs` and `process` exposed four defects that had nothing to do with
either:

- A free function's symbol was its bare name, so the standard library's own
  `exit` took over the C function `exit` and called itself until the stack ran
  out. A definition the program contains now carries a `umay_user_` prefix;
  `main`, the runtime's `umay_` surface and an `extern fn` (which must keep the
  name the linker has to find) are exempt.

- A method's symbol was its bare name, so `File::read` took over the C function
  `read` at link time and the first call to it crashed. A method's symbol now
  carries its type's name, and the machine function takes that symbol instead of
  the body's plain name.
- Structs and enums drew `DefId`s from separate counters. With enough
  declarations a struct and an enum shared an id, and since a size and a layout
  are looked up by that id alone, `sizeof` and every aggregate copy then read the
  wrong type — an `Option<i64>` came back as five bytes. Both draw from one
  counter now.
- `sizeof` of a generic enum left the type parameter unsubstituted and ignored
  the tag, reporting `sizeof(Option<i64>)` as `5` instead of `16`.
- A C variadic call now sets `%al` to the vector-register count, as the SysV ABI
  requires; `open` was reading whatever happened to be in it.

Writing `String` exposed three more backend miscompilations and two front-end
gaps, all now fixed and covered by tests:

- Aggregate arguments by value were not passed at all: a 24-byte struct arrived
  as whichever eight bytes happened to be in `%rdi`, so
  `fn push(self, byte: u8) -> String` read garbage. Both sides now classify every
  argument with the same SysV rules — two eightbytes or less in registers, larger
  copies to the caller's outgoing area, and once one argument spills every later
  one does too — and the area is built in one aligned allocation.
- The callee's argument classification did not apply the spill rule, so it read a
  later scalar from a register the caller had left empty.
- `drop(_ret)` on an aggregate return worked, but the return copy was emitted
  after the (no-op) drop rather than at the single epilogue, which the sret paths
  now share.

Front-end gaps found: `impl<T>` parameters were not in scope for a method's own
signature (`fn new() -> Vec<T>` reported an unknown type `T`), now threaded
through; and the prelude's `String::len` was shadowed by the built-in, now
resolved in favour of the `impl`.

Known gaps this step depends on, none of which are covered yet:

- Field-wise drop order is not checked (`E0208`/`E0209` are not enforced).
- An assignment through a computed pointer, `*(p.offset(n)) = v`, is rejected;
  only a write through a named pointer works, which is why `write_byte` exists.
- `&mut T` does not coerce to `&T` (`E1001`).
- A literal local used at a wider type (`let mut i = 0` used as a `usize`) is
  now settled to that type and stored at its width.

### Generics and containers

Monomorphisation landed, and `Vec<T>` with it. Six more defects surfaced on the
way, each now fixed and tested:

- `lower_match` emitted **no tests at all**: every arm's body ran in order and the
  result was whichever ran last. A `match` on an enum always produced its final
  arm's value. Each pattern now emits a `switchInt` on its variant index, its
  literal, or its `bool`, and bindings are stored only in the arm that matched.
- `Discriminant` loaded the **whole enum** instead of its tag byte, so reading a
  tag from anything but an eight-byte payload read past the value.
- A narrow load left the **upper bits of the register** untouched: writing `%al`
  preserves the rest of `%rax`, so a byte read and then compared as a word
  compared whatever the register held before. `W8`/`W16` loads now zero-extend.
- A parameter whose type is a **one-byte aggregate** was stored with a 64-bit
  move into a one-byte slot, overwriting the saved frame pointer beside it. A
  parameter is now stored at its own size.
- A fieldless variant (`E::A`) had **no code path at all** (`E3001`); it is built
  as an aggregate with a tag, which is what it is.
- A pattern binding inside `match o` kept the declared type **`T`** rather than
  the scrutinee's `i64`, because the variant's field types were never
  instantiated from the scrutinee's arguments. They are now.

Inference also learned three things:

- A `let` annotation is an inference hint: `let v: Vec<i64> = Vec { .. }` names
  the instance the literal could not.
- `Option::None` has no payload to infer `T` from, so it takes the annotation's
  instance; with no annotation it is reported ("cannot infer the type parameter
  `T`") rather than guessed.
- A literal written inside `impl<T> Vec<T>` means the `Vec<T>` being defined, so a
  constructor can build its own type without an annotation.

`Vec::new()` is called with a type annotation rather than `Vec::<i64>::new()`:
turbofish is still a parser gap.

Three more defects surfaced on the way:

- `ZeroExtend { from: W32, to: W64 }` printed `movzlq`, which no assembler
  accepts. A 32-bit register write clears the upper half on its own, so it is now
  `movl`.
- Monomorphisation recorded instances for calls made *inside* a generic body
  whose arguments were still parameters, creating copies whose `T` was another
  `T`. Those are now skipped: they are the template's own business.
- `sizeof`'s operand is the one expression a monomorphiser had to substitute
  without evaluating anything, and it was not, so a `Vec<u8>` measured a `T`.

Known gaps this step depends on, now listed rather than assumed:

- `push` takes and returns the vector by value, so a caller must write
  `v = v.push(x)`. An `&mut self` method that reallocates would be the better
  shape, but `&mut T` does not coerce to `&T` yet.

Fixing the aggregate-ABI work also turned up two printed-MIR round-trip holes and
a stack-argument bug, all now fixed and tested:

- The printer wrote `X as isize@27`, but the parser had no cast branch, so no
  printed MIR containing a cast could be read back.
- `offset(...)` was printed but not parsed.
- A literal passed on the stack was parked in a register before the argument
  shuffle, which was free to overwrite it with a neighbour's value: `sum7(1..7)`
  returned 26 instead of 28. A literal is now written straight from its bytes,
  and only the value's own width is written so the callee's slot offsets still
  line up.

---

## Phase 7 — AI Developer Experience · planned

- LSP server: diagnostics, hover, go-to-definition, completion.
- `umay fmt` — deterministic formatter.
- `umay lint` — style and correctness lints (`W*` codes).
- JSON diagnostics for build and run, not just check.
- Stable machine-readable compiler metadata (symbol and type dumps).

Exit criteria: an editor session on a multi-file project shows live diagnostics
and navigation; `umay fmt` is idempotent on the full test corpus.

---

## Phase 8 — Build System and Packages · planned

- Content-addressed cache with the key derivation in
  `docs/build-system.md`.
- Module-level invalidation; parallel compilation.
- Reproducible builds.
- `upm` package manager with `umay.lock`.

Exit criteria: incremental rebuild after a one-module edit does not recompile
independent modules; no `target/` ever appears in a project; two clean builds of
the same input produce identical artifacts.

---

## Phase 9 — Stabilization · planned

- Extended test corpora, property-based and fuzz testing at each phase boundary.
- Benchmarks: clean time, incremental time, compiler RSS, runtime speed, binary
  size — each compared against Go, Rust, Zig and C equivalents where meaningful.
- Documentation review; specification promoted from draft to 1.0.
- Release engineering: versioning, release notes, binary distribution.

Exit criteria: UMAY MVP release.

---

## Self-hosting (after Phase 9)

Only once the specification, compiler architecture and standard library are
stable. The Rust compiler remains the reference implementation until the UMAY
compiler passes the same test corpus.

---

## Rules that do not relax

- No LLVM or Cranelift in the production backend.
- No mandatory garbage collector.
- No project-local `target/` directory.
- No claimed feature without tests.
- No weakening of memory safety to simplify implementation.
