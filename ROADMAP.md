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

## Phase 6 — Runtime and Standard Library · done

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

### Step 2 — a real standard library · done

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
- [x] `Iterator` basics in `stdlib/iter.um` — `Iterator<T>` as a trait, with
  trait bounds on generic parameters. A method call on a bounded parameter is
  resolved during monomorphisation to that type's `impl`, so dispatch is static
  and there is no vtable. A bound with no `impl` is `E3001`, never dropped.
- [x] `HashMap` in `stdlib/hashmap.um`, in the prelude — open addressing with
  linear probing and an FNV-1a hash: `new`, `len`, `is_empty`, `insert`,
  `get_or`, `has_key`, `value_at`, `release`. A missing key yields the caller's
  fallback rather than an abort.
- [x] Statement separation at a line break. A `(` or `[` on a later line begins
  a new statement instead of calling or indexing the previous expression, which
  is what optional semicolons require.
- [x] `process` in `stdlib/process.um` — `Args` (from `main`'s `argc`/`argv`),
  `len`, `is_empty`, `program`, `get`, `has_flag`, `value_after`; free functions
  `env`, `env_is_set`, `exit`, `last_error`. A missing argument reads as an empty
  string rather than null, so a caller need not check before printing it.
  `file_size` now seeks to the end instead of reading the file, and `is_dir`
  tells a directory from a file.
- [x] `net` in `stdlib/net.um` — `Socket`, `Address`, `tcp_socket`,
  `socket_bind`, `socket_listen`, `socket_accept`, `socket_read`,
  `socket_write_all`, `socket_close`. The address layout lives here because a
  port written whole binds a different port and still succeeds.
  `examples/http-router.um` is built on it.

### What a web backend needs that is still missing

Measured against a working router probe, in the order the gaps bite:

- [x] Array to raw-pointer conversion (`routes as *const Route`). Lowering
  takes the array's address rather than trying to move the value.
- [x] `str`/`*const u8` bridging. `&str` to a byte pointer needs nothing special
  (it already is one); the reverse needs `unsafe`, because nothing checks that
  the bytes are NUL-terminated UTF-8.
- [x] Array pointer arithmetic with `sizeof` in a loop. It was not the
  arithmetic: lowering a array-to-pointer cast read the array's first field and
  called that the pointer, so the base address was a string literal.
- [x] `==` and `!=` on `str` compare bytes, not addresses. `str` is a pointer,
  so the pointer comparison called two equal strings different, which is what
  made every routed request answer 404.
- [x] A byte-slice type that carries its length (`stdlib/bytes.um`). `Bytes` is
  an address plus a count and stands in for a body. It is a library type, not a
  language slice: `&[u8]` in the type system is still missing.
- [x] JSON, read and written over counted bytes (ADR 0012). `json_parse`,
  `json_field`, `json_get`, `json_at`, `json_count`, `json_text`, `json_i64`,
  `json_bool`, and the writers `json_quote`/`json_write_i64`. A value borrows from
  the document and a number keeps its decimal text, so `0.1` survives a round
  trip.
- [x] Header parsing and a request/response type in `stdlib/http.um` —
  `Request`, `Response`, `parse_request`, `content_length`, `status_text`.
- [x] `Iterator`, so a caller can loop over a collection without an index.
- [x] Concurrency, process-based (ADR 0011). `spawn`, `is_child` and `wait` in
  `stdlib/process.um`. `examples/http-router.um` accepts in one process and
  hands each connection to a child, so connections are served at once. A pipe
  carries a result back.
- [x] `Send`/`Sync` markers declared (ADR 0013), with `impl`s for the
  primitives, `String`, `Bytes`, `Vec<T: Send>`, `Option<T: Send>`.
- [x] Turbofish (`f::<i64>(..)`, `B::<i64> { .. }`). An explicit argument list
  also outranks what a struct literal's fields would infer.
- [x] A generic function whose return type names its parameter *inside* another
  type (`fn make<T>(v: T) -> B<T>`) can be bound with `let`. The binding takes
  the initialiser's concrete type, and a method call on it adopts that type's
  arguments, so `make(7i64).get()` and `let a = make(7i64)` both work.
- [x] A function used as a value is an address. `f as *const u8` needs
  `unsafe`, and calling through a `*const fn(..)` is an indirect call. Both had
  to work for a thread's entry point to be passed at all; neither did, so a
  function value was unusable.
- [x] The `Send` check itself `thread_spawn::<A, R>(entry, value)` in the
  prelude. A value whose type does not carry `Send` is refused with `E1016` and
  a suggestion naming the `impl` to write. The child runs the work and answers
  through a pipe, so both directions cross as eight bytes.
- [x] A marker bound is checked where the call is made. Without it a caller
  could hand a non-`Send` value to a `fn f<A: Send>` and cross the boundary
  through a forwarder that never claimed it could. Only a marker is enforced:
  a trait with methods is resolved structurally, so demanding its `impl` would
  reject working code.
- [x] A bound is read from the declaration that wrote it. Every generic function
  names its parameter `T`, so a lookup by name alone let one function's
  `T: Send` answer for another's bare `T`.
- [x] `Sync` is read: a borrow may cross the boundary exactly when the value
  behind it carries the marker, and the diagnostic names `Sync` rather than
  `Send` so the reader is sent to the right one. A raw pointer stays refused:
  no borrow checker will look at it again.

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

## Phase 7 — AI Developer Experience · done

- [x] LSP server: diagnostics, hover, go-to-definition, completion, document
  outline and formatting, over JSON-RPC on stdin/stdout. It holds each open
  document's text and nothing else — every request runs the same library
  functions the command-line commands run, so the editor and the build can never
  disagree about what is wrong. See ADR 0014 for why, and for what that costs.
- [x] `umay fmt` — deterministic formatter. Done: rewrites the gaps between
  tokens and nothing else, so every literal, escape and identifier survives
  byte for byte. A gap holding a comment is left alone. Fixed point on the
  whole standard library and examples (`compiler/tests/phase7_fmt.rs`). The one
  thing it will not decide is whether a `<` brackets a type's arguments or
  compares two values: that cannot be told from the token stream, so the
  author's spacing is kept there rather than guessed at.
- [x] `umay lint` — style tips the compiler has no stake in, kept separate from
  `check` because they are opinions a project adopts at its own pace. Five
  rules (`L0101`–`L0105`): trailing whitespace, unfinished-work markers, an
  empty body that promises a value, an `else` after a returning branch, and an
  unused parameter. `--rules` lists them. Every rule works on the syntax tree
  and the source text and never on types, so each one is one sentence long and
  fixable without understanding the program. The standard library and examples
  come out clean, which is what the tests assert.
- [x] JSON diagnostics for build and run, not just check. A failed build now
  emits the same document `check` emits, so a driver needs one parser.
- [x] `build` and `run` report the warnings they see. They used to drop them:
  the same program warned under `check` and was silent under `build`, which
  meant the warning was worth whatever command you happened to run. They now
  render to the same stream as `check` too, so capturing the report does not
  depend on which command produced it.
- [x] Warnings from the prelude are no longer reported. The prelude is the
  compiler's own source, prepended to every program and uneditable by the
  user; a four-line program was reporting thirty-seven warnings about it,
  which buried the one that was about the user's code.
- [x] Stable machine-readable compiler metadata: `umay metadata <file>` lists
  what a program declares — functions, structs, enums, traits, impls, consts and
  aliases — as text or as a versioned JSON document. It answers the question an
  editor, a doc generator or a test harness actually has, without asking them to
  read MIR, which is the compiler's internal form and changes with the backend.
  Types are rendered from the syntax tree rather than sliced out of the source,
  because a path segment's span covers only its name: the first version printed
  `Pair<i64>` as `Pair`, which is a different type. Trait bounds are kept for the
  same reason — `fn f<T: Size>` accepts less than `fn f<T>` and a driver needs to
  know that. Parsing is enough: a program with a type error still declares what it
  declares, and an editor asks while the program is still being written.

Exit criteria: an editor session on a multi-file project shows live diagnostics
and navigation; `umay fmt` is idempotent on the full test corpus.

---

## Phase 8 — Build System and Packages · in progress

- [x] Content-addressed cache with the key derivation in
  `docs/build-system.md`. SHA-256 lives in the tree (`compiler/src/sha256.rs`)
  because the crate has no dependencies; it is checked against the NIST vectors,
  including the 55/56/64-byte padding boundary where a naive implementation
  reuses a block it should not. The key covers the compiler version, artifact,
  target, optimisation level, edition and the exact source bytes, each hashed
  with its name and length so that two input sets cannot concatenate to the
  same hash.
- [x] An empty cache entry is a miss, not an artifact. A build killed mid-write
  leaves a zero-length file, and serving it links an assembly with nothing in
  it, so the failure surfaces as "undefined reference to `main`" much later.
- [x] `umay build` reuses the cached assembly for the program and for the
  runtime, so an unchanged rebuild recompiles neither. An unchanged rebuild adds
  no cache entries, an edit does, and `-O0` and `-O2` never share one.
- [x] `umay doctor` reports the cache's entry count and size, the inputs the key
  is derived from, and the real phase status.
- [x] Modules from files: `mod name;` loads `name.um` or `name/mod.um`. Before
  this, the line parsed and did nothing, so a program that used the module said
  `unknown name` whether the file was there or not -- accepted syntax that
  cannot be debugged. A missing file, a cycle and the same file named twice are
  all errors with the paths in them; an error inside a module is reported
  against that module's file.
- [x] Module-level invalidation: the cache key covers every module file that
  goes into the assembly, not just the entry point. Keying on the entry alone
  let an edited module reuse the previous assembly, so the program silently kept
  its old behaviour.
- [x] `umay metadata` and the language server see modules. Metadata loads them
  from disk before listing symbols. The server keeps the document's `file:` path
  so the checker resolves `mod name;` against the directory the buffer is really
  in; without it every module looked like a missing file in the editor and fine
  on the command line, which is the least useful place for that disagreement.
- [ ] Parallel compilation. There is one unit of work per build today, so there
  is nothing to run in parallel yet.
- [x] Reproducible builds, checked: two clean builds of the same source in two
  different directories produce byte-identical assembly, object files and
  executable (including the linker's build id). Pinned by tests, because an
  absolute path in the output would pass every other test in the suite.
- [x] A build that warned is not cached. The cache stores the assembly and
  nothing else, so a cache hit had no diagnostics with it and the second build
  of a warning program was silent. A warning a user sees once and then never
  again is worse than no warning: it teaches them a quiet build means nothing.
- [x] The manifest (`umay.toml`), read and validated: unknown keys and sections
  are errors rather than ignored, so a typo cannot silently change the build.
  `output-dir` naming `target` -- in any spelling of the path -- is refused,
  which is what keeps the no-`target/` rule from depending on how someone
  happens to write it.
- [x] `umay build` / `umay run` with no file argument build the entry point named
  by the manifest. A project created by `umay new` now builds and runs with no
  arguments at all, and its `main` returns 0 rather than whatever was in the
  return register.
- [x] `umay.lock` (format 1) and `umay lock`: path dependencies are resolved and
  recorded with a SHA-256 of their source, so a changed dependency is visible
  rather than silently adopted. Re-locking an unchanged project produces the same
  bytes.
- [ ] `upm` proper: fetching, version resolution across a dependency graph, and a
  registry. None of it can be built honestly yet -- there is no registry, so
  `umay lock` reports a dependency with no `path` and writes nothing rather than
  claiming to have pinned something it never fetched.

Exit criteria: incremental rebuild after a one-module edit does not recompile
independent modules (met: the cache key covers the whole module tree, so a
module edit misses the cache rather than reusing stale assembly); no `target/` ever appears in a project; two clean builds of
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
