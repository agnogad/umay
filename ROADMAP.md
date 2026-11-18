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

Not yet: trait bound checking. Module resolution from an entry point arrived
with Phase 6, and `pub` visibility enforcement with Phase 8 (`E1018`).

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
- An assignment through a computed pointer, `*(p.offset(n)) = v`, now works
  and is covered by `computed_pointer_deref_assignment_works`.
- `&mut T` now coerces to `&T` via reborrow without moving the mutable
  reference, covered by `phase4_ownership` and `phase5_codegen`.
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

- `push` still takes and returns the vector by value (`v = v.push(x)`) for
  compatibility, though `&mut T` → `&T` coercion is now supported.

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

## Phase 8 — Build System and Packages · **done**

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
- [x] Parallel compilation, to the extent there is work to parallelise. The
  program object and the runtime object do not read each other, so they are
  assembled at the same time. Each `as` is quiet and the caller prints, because
  two processes writing to the same stderr would make the same build say
  different things on different runs -- pinned by a test.
- [x] An unchanged rebuild links nothing: the linked executable is cached under
  a key made of the program assembly and the runtime assembly, so a warm build
  starts neither `as` nor `cc`. Warm build time on `examples/http-router.um` fell
  from about 0.30s to 0.18s. The copy restored from the cache is written with its
  execute bit -- `std::fs::write` does not set one, so the first version produced
  a build that succeeded and a `umay run` that failed with "Permission denied".
- [ ] Compiling program and runtime codegen in parallel. Measured first: a cold
  build is about 0.18s, of which the link through `cc` dominates, so splitting
  codegen across threads would add shared-state risk for a few milliseconds.
  Worth revisiting when there is a second real unit of work -- a parallel
  compiler frontend over modules, not two calls.
- [x] Reproducible builds, checked: two clean builds of the same source in two
  different directories produce byte-identical assembly, object files and
  executable (including the linker's build id). Pinned by tests, because an
  absolute path in the output would pass every other test in the suite.
- [x] A build that warned is not cached. The cache stores the assembly and
  nothing else, so a cache hit had no diagnostics with it and the second build
  of a warning program was silent. A warning a user sees once and then never
  again is worse than no warning: it teaches them a quiet build means nothing.
- [x] The cache key covers the compiler binary, not only its version string. The
  key's `compiler` field was `CARGO_PKG_VERSION`, which changes only when someone
  edits the version. A rebuild that fixed a codegen bug at the same version
  shared every key with the build before it, so the fixed compiler served the
  broken compiler's assembly from the cache -- the exact stale-artifact failure
  the field exists to prevent, and it dropped a bad `sar` operand into an
  assembler that rejected it. The key now also folds in the running executable's
  length and modification time, computed once per process: a rebuild relinks the
  binary and changes the key, a no-op build relinks nothing and keeps it.
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
- [x] Dependency graph resolution. `umay lock` walks the whole graph, not just
  the project's direct dependencies: a chain three deep is fully locked, a
  diamond locks the shared package once. A cycle is reported with the chain that
  closed it (`a -> b -> a`) rather than expanding until the stack is gone, and a
  dependency that points back at the project is caught the same way.
- [x] Version agreement. A dependency's own manifest says what it *is*; the
  parent's manifest says what was *asked for*. A disagreement is reported with
  both versions rather than resolved in either direction -- one answer builds a
  version the author did not request, the other edits a file they did not write.
  The same name at two versions through different paths is a conflict for the
  same reason: a program cannot have two `lib`s.
- [x] Locked paths are relative to the project, so a lockfile does not depend on
  the machine that wrote it.
- [x] A build warns when the lockfile no longer matches. `verify` existed and
  nothing called it, so a lockfile was documentation rather than a guarantee. The
  check now runs at the start of `build`, names the dependency that changed, and
  says to run `umay lock`. It covers transitive dependencies, which the first
  shape of the check would have missed.
- [x] A declared dependency is reachable from the program. Naming `lib` in
  `[dependencies]` makes `mod lib;` resolve to the dependency's own source, so
  `lib::answer()` compiles and runs. Before this the manifest recorded a
  dependency that nothing could reach: `umay lock` listed it and the compiler
  never saw it.
- [x] A module's constants, enums and enum variants are reachable through the
  module. Each was looked up by its bare name, and each is registered as
  `module::Name`, so `kinds::LIMIT` and `kinds::Kind::Large` were both "unknown
  name". An enum's variants were never attached to it at all, because the
  definition was found by the bare name too -- so a module's enum had no variants
  as far as the checker was concerned.
- [x] A module's type can be used in a type position, a struct literal and as the
  receiver of its own `impl`: `let p: geo::Point = geo::Point { .. }` followed by
  `p.sum()`. The receiver lookup was the last of these to fail, and it failed as
  an error type in the backend rather than at the line that named the type.
- [x] A method may be written `pub fn` inside an `impl`, and may carry
  attributes. Neither parsed: the impl body accepted only the bare `fn` keyword,
  so a module's method -- normally written `pub` because it is meant to be
  reachable from outside -- could not be written at all.
- [x] A module's items belong to it. Two dependencies that both declare `run`
  used to be a duplicate definition, and whichever was collected first answered
  for the other (`b::run()` called `a::run()`). Each module's items are now
  registered and emitted under their qualified name, so two modules may declare
  the same function and each is its own.
- [x] `pub` visibility is enforced across module boundaries. `pub` used to be
  parsed and thrown away, so a private `fn`, `struct`, `enum` or `const` in a
  module was reachable from any other module by its qualified name. A qualified
  reference to a non-`pub` item now reports `E1018`, while a module referring to
  its own private item by its full name is still allowed. Existing module
  fixtures that reached across a boundary were updated to say `pub`.
- [x] The cache key covers a dependency's source, so editing one rebuilds the
  program that uses it, rather than keeping the previous assembly.
- [x] `upm`: fetching from a registry. A dependency with no `path` is now
  fetched from the registry named by `UMAY_REGISTRY` (one or more directories)
  into the project's `deps/<name>/`, and its SHA-256 is written to the lockfile,
  so what a build verified is what it built. The graph is walked, so a fetched
  dependency's own dependencies are fetched too; an empty version asks for the
  newest, compared by number; a name that could escape `deps/` is refused. The
  registry can be a directory or an HTTP(S) service. A URL root serves
  `<name>/versions.txt` and `<name>/<version>.tar`; the client downloads with
  `curl` and extracts a guarded USTAR archive. `scripts/registry-server.py`
  serves a directory in that shape. Directory and HTTP transports share the
  version choice and content hash, and both are tested
  (`compiler/tests/phase8_upm.rs`, `compiler/tests/upm_http.rs`).

Exit criteria: incremental rebuild after a one-module edit does not recompile
independent modules (met for module granularity: the cache key covers the whole
module tree, so a module edit misses the cache rather than reusing stale
assembly; per-module reuse is still coarse -- one edit recompiles the program);
no `target/` ever appears in a project; a dependency with no `path` is fetched,
pinned by content hash, and reachable as a module; two clean builds of the same
input produce identical artifacts.

---

## Phase 9 — Stabilization · **done**

- [x] Extended test corpora and fuzzing of the parser. Random token soup, deep
  nesting, unterminated literals and multi-megabyte numbers are all diagnostics,
  never panics -- which matters because the language server checks a file on
  every keystroke and therefore sees every half-typed state.
- [x] Two problems the fuzzing found. Past the nesting limit the parser kept
  reporting every unclosed delimiter as it unwound: 5000 nested parentheses gave
  one real error followed by 256 consequences of it. And a program with no
  `main` built cleanly until `collect2` said "undefined reference to `main`",
  which names an object file rather than the author's program and arrives
  several stages too late. Both are now one clear message.
- [x] Property-based testing of the backend: signed and unsigned comparison,
  arithmetic, values across a call boundary, and "a program the frontend accepts
  builds and runs" over a fixed reproducible sample. Each sample is one build, not
  one build per case -- forty programs of twenty-four comparisons took forty
  seconds, which is too slow for a suite run on every change.

  Worth writing down: the first version of the harness compared the computed
  value to a branch rather than to the expected constant, so every correctly-false
  result was reported as a compiler bug. Twenty-four "failures" went into the
  investigation before the harness was the suspect. A test that cannot be wrong
  cheaply is worth more than one that has to be trusted.
- [x] Benchmarks of clean time, incremental time, compiler RSS and binary size,
  recorded in `docs/measurements.md` with the command that produced them. The
  numbers changed a decision: the cache saves about 0.01s of a 0.21s build
  because process startup and `cc` dominate, so the next build-system work is
  skipping `as`/`cc` when their inputs are unchanged, not compiling in parallel.
- [x] A shift's codegen, found while bringing the suite up after a pull. Two
  faults in one path: a constant count was materialised into `%rcx` and printed
  at the operation's width (`sarl %ecx, %ecx`, which the assembler rejects), and
  a variable count placed in `%cl` could be overwritten by the result register
  if the allocator handed it `%rcx`. A constant count now goes in as an
  immediate (`sarl $8, %ecx`), and a register count prints as `%cl` whatever the
  operation's width. Every program that shifted by a literal failed to assemble
  before this; four tests now pin it, including both shift directions sharing a
  count.
- [x] Comparison against Go, Rust, Zig and C, over five workloads at stated
  toolchain versions, in `docs/measurements.md`. Zig 0.17.1 was installed, so the
  `not installed` gap is closed. Every language prints the same integer in every
  workload -- the claim worth checking before any ratio. UMAY builds the
  1000-function chain in 0.34 s against rustc's 0.53 s, clang's 0.40 s, go's
  0.21 s and gcc's 4.34 s, and links 83-240 KB against rustc's 4.5 MB and go's
  2.4 MB, at 12 MB peak build RSS against 69-147 MB for the others.
- [x] **Runtime-speed comparison**, run five workloads long enough to dominate
  process start: `compute` (50M multiply/add/modulo), `alloc` (2M malloc/free),
  `fib(38)` (63M recursive calls), `str` (10 MB copy) and the call chain. UMAY
  computes the right answer everywhere but runs about 1.6x slower than gcc on
  `compute`, 1.7x on `fib` and 4x on `alloc`. The cause is register-allocation
  quality -- intermediates are stored and immediately reloaded instead of staying
  in a register -- recorded as the clearest place where effort would buy speed.
  This closes the runtime-comparison gap the previous entry left open.
- [x] The harness is `scripts/bench.py`: medians over repeated runs, peak RSS of
  the measured child via `wait4`, correctness checked by comparing the printed
  integer across languages, and a missing toolchain skipped rather than failed.
  Zig's workloads print through libc's `write` because `std.debug.print` takes
  ~24 s to compile in 0.17; the script comments that where it matters.
- [x] Benchmarks at scale, over generated 1000- and 5000-function programs. A
  fivefold input costs about seven times the frontend work and four times a full
  build -- close to linear. The numbers exist so that a future change that makes
  them a cliff is visible.
- [x] Documentation review. The specification is promoted from draft to **v1.0**
  and its feature table now says what is implemented rather than listing finished
  phases as unimplemented; the exit-code descriptions match the compiler
  (`4` is retired, not "not implemented"); `README`, `docs/diagnostics.md` and
  `docs/build-system.md` were read against the code and corrected where they
  described a finished phase as pending.
- [ ] Release engineering: versioning, release notes, binary distribution.

Exit criteria: UMAY MVP release.

**Met.** The MVP is released as `0.1.0`: the compiler, standard library and
toolchain build and run end to end, the suite passes, the specification is
`v1.0`, and `scripts/release.sh` produces a checksummed, stripped artifact from
a clean tree that passes its own tests. The runtime-speed comparison that was
previously recorded as not measured has since been run over four compute kernels
(see the item above): UMAY is correct everywhere and 1.6-4x slower than the C
compilers, and the reason -- register allocation quality -- is named rather than
hidden. Closing that gap is the first item of the next phase, not of this one.

Heading the next phase: close the register-allocation gap the benchmark
measured. Every intermediate is stored to the stack and reloaded, so a loop
body touches memory several times where the C compilers touch it once or twice.
Keeping a value in its register across a short expression -- local allocation
within a block -- would remove most of that traffic without touching the
linear-scan framework. It is a named, measurable goal: the same five workloads,
re-run, with the ratio to gcc in `docs/measurements.md` as the score.

---

## Phase 10 — Multiple systems · **in progress**

One frontend, three backends' worth of difference in three quarters that do not
touch the frontend at all.

### The target layer

- [x] `compiler/src/target.rs`: `Target` carries the triple, the object format,
      the ABI, the assembler and linker with their flags, and the runtime's
      platform layer. `--target` selects one; the host's own is the default.
- [x] The target is part of every cache key. An ELF assembly and a Mach-O one
      are different bytes from the same source, and sharing an entry between
      them would serve a Mach-O object to an ELF link.
- [x] An architecture with no backend is refused before any work is done,
      because compiling an ARM triple with an x86_64 backend produces a binary
      the loader rejects and an error message about the loader.

### What each system changes

- [x] Symbol spelling: Mach-O and COFF prefix external names with `_`.
- [x] Object dressing: Mach-O's `__TEXT,__const` section, and the ELF-only
      `.type`/`.size`/`.note.GNU-stack` metadata omitted where it is rejected.
- [x] The runtime split into a portable body (`runtime/runtime.um`) and a per
      system layer (`runtime/platform/{linux,macos,windows}.um`) holding the C
      symbols, the constant tables and the `errno` accessor.
- [x] macOS: `__error` instead of `__errno_location`, Darwin's own `O_*`
      numbering (where `O_CREAT` and `O_TRUNC` trade places against Linux), the
      Mach-O assembler reached through `clang`.

### Windows

- [x] The Microsoft x64 calling convention as a second `Abi`: four argument
      registers in a different order, 32 bytes of shadow space reserved by every
      caller, aggregates returned through a hidden pointer at any size, and
      `%rsi`/`%rdi` preserved across calls.
- [x] `regalloc` takes the callee-saved and caller-saved sets from the ABI, so a
      live range that spans a call gives up the right registers on each.
- [x] The CRT bindings: `_write`/`_read`/`_open`/`_close`/`_unlink`/`_mkdir`/
      `_errno`/`_exit`/`_lseeki64`, with the `int`/`unsigned` widths adapted to
      the portable `size_t`/`ssize_t` in one place rather than at every call.
- [x] No `fork`: the checked fan-out uses `CreateThread`, and `WaitForSingleObject`
      plus `GetExitCodeThread` where Linux uses a pipe and `waitpid`. The
      language's `Send` guarantee is unchanged, because the front end settled it
      before the call arrived.
- [x] Binary mode forced on every descriptor, so a file written byte by byte is
      not given a silent `\n` to `\r\n` translation.

### Verified, and not yet verified

- [x] Linux is unchanged: the test suite passes (754 tests at the last count), and the Linux assembly is
      asserted to carry no symbol prefix and to keep its ELF metadata.
- [x] macOS and Windows assembly is accepted by a real assembler for its object
      format, and the produced object is a Mach-O 64 / x86-64 COFF file with
      `_main` defined. Symmetry with `llvm-nm`.
- [x] **Windows: a linked, running binary is verified.** MinGW-w64 links the
      emitted COFF objects and Wine runs the result; the existing
      `an_emitted_windows_object_links_and_runs_elsewhere` test passes, and a
      program built with `build --target x86_64-pc-windows-gnu` was linked and
      run here as well. The larger `http-router` example runs under Wine too:
      its crash was a Win64 ABI bug in the prologue, which read the hidden
      struct-return pointer from `%rdi` (SysV) instead of `%rcx` (Win64).
      `win64_struct_return_through_hidden_pointer_runs` pins the fix.
- [x] **macOS: a linked, running binary is verified in CI.** A `macos-15-intel`
      job links the emitted x86_64 objects with Apple's linker and runs a small
      program; the check passed in run `44040427906`. The failures that blocked
      it were real: arm64 runners could not link x86_64 objects, Mach-O C calls
      lacked their leading underscore, and the linker needed the object
      architecture before its inputs. All three are fixed and pinned by tests.

---

## Production readiness (2026-11-16) · **done**

The compiler now builds with zero warnings and the full suite passes.

- [x] **Warnings.** Removed the remaining dead code and unused bindings across
  `sema`, `isel`, `lsp`, `lint`, `lower` and `fmt`. `cargo build` and
  `cargo test` compile without a single warning.
- [x] **`&mut T` → `&T`.** A mutable reference reborrows as a shared one at
  calls and in `if`/`let` positions without being moved, while a shared
  reference still never coerces to a mutable one and a new exclusive borrow
  still conflicts (`phase4_ownership`, `phase5_codegen`).
- [x] **Computed-pointer assignment.** `*(p.offset(n)) = v` is lowered and
  emitted, not just a write through a named pointer.
- [x] **`pub` visibility.** Enforced across module boundaries as `E1018`, with
  regressions for functions, structs, enums and constants and for the
  same-module case.
- [x] **Register allocation.** Non-address-taken scalar locals stay in virtual
  registers; spill slots no longer collide with local frames, callee-saved
  registers get their own frame slots, and redundant spill/reload pairs are
  removed. `compute` 0.717 → 0.589 s, `fib` 0.772 → 0.663 s, `alloc` 0.077 →
  0.058 s; see `docs/measurements.md`.
- [x] **Docs.** This file, `docs/language-specification.md` and
  `docs/measurements.md` record what changed and what the numbers are.
- [x] **Visibility holes closed.** Private module items no longer enter the
  global bare-name scope, and `impl` methods carry their `pub` flag and owning
  module through method resolution. Bare access to a private item is an
  unknown-name error naming the owner; a private method reached from outside
  is `E1018`.
- [x] **Explain coverage.** The four codes that had escaped the database
  (`E0192`, `E0193`, `E0194`, `E1017`) are documented, and a source-scanning
  test stops another code being emitted without an entry.
- [x] **Distribution.** A dual MIT/Apache-2.0 `LICENSE` is included in the
  release tarball, `Cargo.toml` names the real repository, and GitHub Actions
  runs the locked test suite plus a tagged release build. `scripts/release.sh`
  was run end to end and its checksum verified.
- [x] **Cache eviction.** New key directories trigger eviction down to 512 MiB,
  configurable with `UMAY_CACHE_MAX_BYTES` and reported by `umay doctor`.
- [x] **Differential fuzzing found and fixed two more compiler bugs.** Byte-width
  multiplication emitted an invalid two-operand `imulb`; -O2 constant folding of
  `!255u8` produced -256 because `!` was computed at i128 width. Both have
  regressions in `phase5_codegen`. The harness also found an infinite loop in
  parser recovery: `recover_item` stops at a top-level keyword to let the
  program loop parse the next item, but an impl-body loop calling it on a stray
  `impl` never advanced. Body recovery now forces progress, covered by
  `recovery_inside_an_impl_body_always_makes_progress`.
- [x] Differential fuzzing is a CI gate: `backend_differential_ci` checks seeds
  1..=30 by default, with `UMAY_DIFF_COUNT` for local sweeps and an ignored
  long run for thousands of seeds.
- [x] The generator no longer emits assignments that mention their target or
  assign to a variable borrowed anywhere in the same body. This covered
  `v = f(&mut v)`, `s.f = f(&mut s, ...)`, `s.f = s.g + 1` and borrows whose
  block order confused liveness. `generated_assignments_never_conflict_with_borrows`
  checks the AST for 2000 seeds; the 30-seed CI gate remains the end-to-end
  proof.
- [x] A parenthesized comparison such as `(p < q) > r` no longer trips the
  generic-argument lookahead. The fuzz generator produced it at seed 1573;
  `parenthesized_comparison_is_not_generic_args` pins the fix.
- [ ] Long fuzz runs still find generated programs that the checker rejects
  because field-wise borrow liveness is coarse (`docs/memory-model.md` E0208).
  The parser and generator faults they exposed are fixed; the remaining cases
  are a documented compiler limitation, not silent miscompilation. The long
  run stays manual.

---

## Self-hosting (after Phase 9)

Only once the specification, compiler architecture and standard library are
stable. The Rust compiler remains the reference implementation until the UMAY
compiler passes the same test corpus.

---

## Production follow-ups (after 0.3.1)

The compiler and the three x86_64 release artifacts are in place. What remains
is breadth, packaging, and the two known quality gaps.

- [x] **ARM64.** The AArch64 backend in `compiler/src/aarch64.rs` lowers
      the full MIR surface: scalars, strings, pointers, arrays, structs,
      tuples, enums, generics, function pointers, floats and indirect calls.
      AAPCS64 calls pass integer arguments in `x0`-`x7`, floats in `v0`-`v7`,
      composites in registers or by reference, and extra arguments on the
      stack; returns use `x0`/`x1`, `d0`/`s0`, or the hidden `x8` pointer. The
      program and runtime compile for `aarch64-unknown-linux-gnu`, and a CI job
      links and runs a feature-covering program under qemu. Homogeneous float
      aggregates use the AAPCS64 `v` registers. `aarch64-apple-darwin` emits
      Mach-O ARM64; the ARM64 compiler and a generated object are cross-built
      on an Intel macOS runner. A native Apple Silicon run needs a runner this
      private repository cannot obtain.
      `aarch64-w64-mingw32` emits COFF ARM64, links with llvm-mingw, and a
      native Windows ARM64 CI job runs the PE. The release workflow builds
      `aarch64-linux-musl`, `aarch64-darwin` and `aarch64-windows` artifacts.
      Address-free integer and float locals are graph-coloured into
      `x19`-`x28` and `d8`-`d15`, sharing registers across disjoint lifetimes
      and spilling the rest. A twenty-local loop's qemu time fell from 0.032 s
      to 0.011 s; intermediate values still use fixed scratch registers. See
      [ADR 0015](docs/adr/0015-aarch64-backend.md).
- [x] **Hosted `upm` registry.** `UMAY_REGISTRY` accepts `;`-separated HTTP(S)
      base URLs alongside directories. The client reads `versions.txt`,
      downloads a package tar with `curl`, and extracts it safely. A runnable
      registry server ships in `scripts/registry-server.py`; public deployments
      put it behind TLS.
- [ ] **Package-manager distribution.** Homebrew, Scoop, and an AUR package on
      top of the existing archives, with the same checksum gate.
- [ ] **Close E0208.** Field-wise borrow liveness rejects some generated
      programs that are sound. The long fuzz run stays manual until it does not.
- [~] **Register-allocation quality.** Liveness and interference graph
      colouring allocate address-free integer and float locals to callee-saved
      registers, sharing registers across disjoint lifetimes and spilling the
      rest. Scalar assignments lower directly into those registers, cutting a
      twenty-local loop's `mov` count from 142 to 16. Constants and stack
      operands are staged in scratch registers, and aggregate copies move eight
      bytes per iteration (an 8 KB copy loop improved 4.3x). A final peephole
      removes fallthrough branches, 64-bit self-moves and inverse move pairs.
- [ ] **Reproducible release check.** Rebuild an artifact and compare bytes, so
      "the tarball matches the commit" is verified rather than assumed.

---

## UMAY v2 (new specification track)

v2 is a separate specification line built on the frozen v1 language. It adds
closures, traits and dynamic dispatch, C FFI and bindgen, async/await, desktop
GUI, mobile, a game layer and proc-macros. Nothing here changes how a v1.0
program compiles; the feature-gate and migration policy is in
[`docs/v2/README.md`](docs/v2/README.md).

Work starts on branch `v2-foundation`. The baseline before any v2 change is
`cargo test --locked --manifest-path compiler/Cargo.toml`: 784 passed, 1 ignored.

**Phase 0 — framework and decisions · done**

Deliverables: the v2 spec line and feature flags, ADRs for the four core
architecture decisions, the CI matrix, the performance budget, the backward
compatibility policy, and a per-phase benchmark set.

- [x] Branch `v2-foundation` opened from `main`.
- [x] Closure semantics ADR (0017)
- [x] Trait object ABI ADR (0018)
- [x] Async model ADR (0019)
- [x] GUI backend strategy ADR (0020)
- [x] Mobile strategy ADR (0021)
- [x] v2 specification, feature-flag and compatibility policy
- [x] CI matrix, performance budget, benchmarks
- [x] FFI/bindgen, webview and native-dependency spike designs

Exit criterion: the architecture decisions are accepted as ADRs and the
engineering baseline for the later phases is written down.

**Phase 1 — closures · first slice landed**

What compiles and runs today (x86_64 host; AArch64 assembly verified, execution
needs the CI cross job):

- [x] Syntax `|x| expr`, `|x, y| expr`, `|| expr`, `|x| { ... }`, `move |x| expr`.
- [x] Capture analysis over resolved HIR locals; only referenced outer locals
      become fields of a compiler-generated environment struct.
- [x] Capture-less closures lower to plain `fn` pointers; captured closures call
      through a generated trampoline with the environment fields as arguments.
      Both reuse existing MIR, ownership checking and both backends.
- [x] Parameter types from `|x: i64|` annotations or a typed binding; `E1019`
      otherwise, with `E1020`/`E1021` reserved.
- [x] `Fn` / `FnMut` / `FnOnce` in `stdlib/fn.um`, wired into the prelude, with
      synthesized impls registered from each capturing closure through
      `Checker::register_synthetic_impl` (one-argument call form only).
- [x] Least-privilege capture modes: Copy captures become `&T` (read-only) or
      `&mut T` (mutated) references; non-Copy captures stay by value, and
      mutating one reports `E1020`. Mutating a Copy capture now changes the
      original binding.
- [x] Tests: 7 parser tests, `compiler/tests/phase10_closures.rs` (16),
      `phase11_fn_traits.rs` (5), `phase12_borrow_captures.rs` (4).
      Full suite: **831 passed, 1 ignored**; clippy `-D warnings` clean.

Still open before ADR 0017 is fully delivered:
- [ ] Borrow-capturing closures that escape the defining scope (`E0203`/`E0207`
      diagnostics from ADR 0017 §5 are not emitted yet).
- [ ] `move |x| ...` forcing by-value capture (the prefix is parsed and accepted
      but still has no extra effect).
- [ ] Capturing closures as C callbacks through a generated `dyn Fn` trampoline.
- [ ] `Fn(A, B) -> R` sugar and zero/multi-parameter call traits.
- [ ] LSP hover for inferred captures and the `Fn`* set.

**Phase 2 — traits / dyn Trait · dispatch working**

Scope of the first slice (three parallel agents, then integration):

- [x] Least-privilege closure captures: shared/mutable borrows for Copy values,
      with `E1020` for the unsupported non-Copy mutation case.
- [x] `Fn` / `FnMut` / `FnOnce` declarations in `stdlib/fn.um`, wired into the
      prelude, plus a reusable API for compiler-synthesized trait impls.
- [x] `dyn Trait` as a first-class type: `TyKind::DynTrait`, two-word layout,
      object-safety diagnostics `E1022`–`E1024`.
- [x] Integration: `E1003` now rejects only `impl Trait`; `DynTrait` resolves via
      `Checker::resolve_dyn_trait`, and capturing closures register their
      synthesized call-trait impls.

- [x] Static vtable emission: `MirProgram::vtables`, `Constant::VTable`, a
      `.Lvtable_N:` block of `.quad` entries in both backends, and vtable entries
      as reachability roots.
- [x] `dyn Trait` coercion from `&Concrete`: generated shim functions with the
      uniform `(data: *mut u8, args...) -> ret` ABI, one slot per object-safe
      method in trait declaration order.
- [x] `d.method(args)` dispatch through the vtable slot as an indirect call;
      no new MIR terminator. Fat-pointer field access works in both backends.
- [x] Tests: `compiler/tests/phase13_vtable_emit.rs` (3) and
      `phase14_dyn_dispatch.rs` (5). Full suite **841 passed, 1 ignored**;
      clippy `-D warnings` clean. x86_64 program prints 25 for a dyn `area()`
      call; AArch64 assembly generation verified (execution needs the CI cross
      job).

- [x] Vtable prefix from ADR 0018 §4: `size`, `align` and `drop` slots. Both
      backends emit `.p2align 3` plus three prefix `.quad`s before methods;
      method slots start at byte 24 and a drop shim calls `umay_dealloc`.
- [x] Owned `dyn Trait` values: a generated non-Copy owning struct plus a MIR
      pass (`dyn_drop::rewrite_owned_dyn_drops`) turns its `Drop` into an
      indirect call through the vtable drop slot. No `Box<T>` in stdlib, but
      the ownership and destruction path is real.
- [x] Associated-type bindings: `dyn Iterator<Item = u8>` and generic args now
      survive parsing, interning and display (`TraitBound`), instead of being
      dropped by `Path::as_string()`.
- [x] Return-position `impl Trait`: body-inferred hidden type, no new TyKind or
      backend work; arg-position still E1003. The hidden type is intentionally
      erased (callers can see it).
- [x] x86 indirect calls now share direct-call ABI planning: sret, 16-byte
      aggregate returns in `rax`/`rdx`, and Win64 shadow space. An indirect
      callee is restored into a non-argument register (`r10`) after the argument
      shuffle, fixing a `%rdx` collision.

Still open in Phase 2:

- [ ] `dyn` in collections and behind generics (`Vec<dyn T>`, `?Sized`) — the
      two-word fat pointer has no unsized-pointee support yet.
- [ ] Default methods, blanket impls and overlap/orphan diagnostics, and
      supertrait slot ordering. GATs stay out of ADR 0018 scope.
- [ ] Vtable ABI conformance tests on all four targets (SysV x86_64, Win64,
      AAPCS64, Windows ARM64). Win64 shadow space is correct by construction but
      not runnable on this Linux host.

**Phase 3 — C ABI / FFI / bindgen · started**

- [x] `umay bindgen --header H --out O` (or `--emit-stdout`): a deliberately
      narrow C-header reader emits one `extern "C" { ... }` block, opaque
      `type` declarations for unknown pointers, variadic functions, and
      `// umay-bindgen: skipped` comments for unmappable declarations.
- [x] Parser accepts `#![feature(name)]` crate attributes and grouped
      `extern "C" { ... }` blocks, so generated modules pass `umay check`.
- [x] FFI ABI conformance on x86_64 SysV: struct-by-value in registers and via
      hidden pointer, C callbacks through function pointers, capture-less UMAY
      closures as C callbacks (`compiler/tests/phase18_ffi_abi.rs`, 3 tests).
- [x] Windows ARM64 Microsoft aggregate CI step added to the `windows-11-arm`
      job (1/2/4/8-byte aggregates in `x0`, larger by reference). The 16-byte
      case is still a known divergence between AAPCS64 and Microsoft ABI.
- [x] Variadic float diagnostic `E0157`: the variadic flag now reaches `HirFn`,
      and each trailing float argument is reported. Integer/pointer variadic
      calls remain valid.
- [x] Native dependency manifest: `[native.<name>]`, pkg-config resolution,
      include/link flags, static selection, per-target overrides, cache keys
      (`compiler/src/native_dep.rs`, 10 tests). Build scripts are parsed as DATA
      only — execution and the security boundary are still open.
- [x] Real C library spike: `examples/v2/sqlite_spike/` binds SQLite from
      bindgen-generated `extern "C"` declarations with no hand wrapper, opens
      `:memory:`, execs SQL, prints `SQLITE_OK` (`0`) and closes. Generated file
      is byte-for-byte reproducible.
- [ ] Remaining bindgen gaps: C struct/union layout and `#[repr(C)]`, typedef
      chains, function-pointer typedefs and typed C callbacks, constants,
      `-I`/`#include` resolution, target-dependent widths, module-level
      extern visibility/symbol naming, and `-l` flags through `umay build`.
- [ ] Build-script execution with a real security boundary, UPM transitive
      native deps, `umay doctor` reporting.

Full suite: **893 passed, 1 ignored**; `cargo clippy --all-targets -D warnings`
clean. AArch64 vtable emission and Windows ARM64 aggregate CI are verified by
assembly/CI, not by a local cross-linker.

**Phase 4 — async / concurrency · first slice landed**

- [x] Contextual `async fn` modifier and postfix `.await` (no new keywords, so
      v1 identifiers named `async`/`await` still work). New AST/HIR nodes and
      their walkers.
- [x] `stdlib/future.um`: `Poll<T>`, `Waker`, `Context`, `Future<T>` (a generic
      parameter, because UMAY traits cannot declare associated types), no-op
      wake and `make_context`.
- [x] Each `async fn` synthesizes a state struct, a thunk holding the original
      body, a `poll` function returning `Poll::Ready`, and a concrete
      `block_on` helper. Parameters pass through the state struct; side effects
      happen on first poll, not at call time.
- [x] `.await` and `block_on(future)` desugar to the generated helper; a
      synthetic `impl Future<T>` is registered for the state type. Four async
      tests plus five stdlib tests pass.

Deliberately NOT real yet (all documented with `ponytail:` upgrade notes):

- [ ] No suspension: `Pending` aborts, no executor/reactor, no waker wake.
- [ ] Only parameters live in the state struct; locals across an await are not
      captured. Awaited futures must be declared before their callers.
- [ ] No `Pin`, no boxed state machine, no cancellation, `spawn`/`select`/
      `timeout`/`spawn_blocking`, no `Send` bounds, no async I/O wrappers.
- [ ] Async methods and generic async functions are not supported.

Full suite: **909 passed, 1 ignored**; `cargo clippy --all-targets -D warnings`
clean.

**Phase 5 — desktop GUI · SDL3 first slice landed**

- [x] Real SDL3 3.4.14 binding generated by `umay bindgen` from
      `examples/v2/sdl_min.h` (10 declarations, byte-exact reproducibility
      asserted). SDL structs/handles are `void*`; bool returns and `u64` flags
      match the real ABI. No C shim, no hand-written extern wrapper.
- [x] `umay build` links `-lSDL3` automatically when the program references an
      `SDL_` symbol; the SQLite rule is unchanged. Per-target flag is the same
      on all six triples; Windows/macOS/AArch64 linking is CI-only.
- [x] Pure-UMAY immediate-mode core in `examples/v2/gui_core.um`: a todo model
      (`add`/`toggle`/`delete`/`count`) plus clipped `fill_rect`/`draw` writing
      through `write_byte`. No FFI, so it stays crate-root-spliceable.
- [x] `examples/v2/p5_todo_sdl.um`: headless SDL window + event loop + surface
      fill + todo self-test. `SDL_VIDEODRIVER=dummy` run prints `GUI_OK 2`.
- [x] Tests: `phase20_gui_core` (5), `phase20_sdl_link` (3),
      `phase20_sdl_smoke` (2). Full suite **919 passed, 1 ignored**;
      clippy `-D warnings` clean.

Deferred per ADR 0020:

- [ ] `umay-gui` facade with `trait Backend`/`dyn Backend` — needs `Box`,
      owned-`dyn` collections and the 16-byte indirect-call ABI.
- [ ] Capturing closures as C event handlers; `SDL_MapRGB` (needs struct access
      and `#[repr(C)]`, which bindgen and the parser do not support yet).
- [ ] Fonts/text rendering, menus, dialogs, tray, clipboard, drag-and-drop,
      HiDPI, themes, accessibility, crash reporting, asset packaging.
- [ ] Webview backend, IPC/JS bridge, and the same todo app as an HTML UI.

**Phase 7 — 2D game layer · first slice landed**

- [x] SDL3 binding extended with `SDL_GetKeyboardState`, `SDL_GetTicks`,
      `SDL_CreateSurfaceFrom`, `SDL_DestroySurface` and `SDL_BlitSurface`
      (15 declarations total, byte-exact `umay bindgen` output; no C shim).
- [x] Pure-UMAY `examples/v2/game_core.um`: integer `Vec2`/`Sprite`, a hard-coded
      8×8 ARGB sprite, keyboard scancode input, clipped ARGB framebuffer blit,
      and a `fixed_step` accumulator. No FFI, no floats, no image decoder.
- [x] `examples/v2/p7_sprite.um`: 64×64 window, fixed-timestep loop, input →
      step → draw → `SDL_CreateSurfaceFrom`/`SDL_BlitSurface` present path.
      `SDL_VIDEODRIVER=dummy` selftest prints `SPRITE_OK 3 5`; default run
      prints `SPRITE_OK 0 8`. Both bounded and deterministic.
- [x] Tests: `phase21_game_core` (6) and `phase21_sprite_smoke` (3). Full suite
      **928 passed, 1 ignored**; clippy `-D warnings` clean.

Deferred (per the plan and ADR 0018/0020 limits):

- [ ] Audio: SDL3 audio needs struct-by-value and C function-pointer callbacks,
      which bindgen cannot express; miniaudio is not installed.
- [ ] Image/asset pipeline: no PNG decoder or zlib; sprites are hard-coded ARGB.
- [ ] SDL_GPU / Vulkan / Metal renderer, 3D demo, ECS, hot reload, frame-time
      profiler target (needs a GPU runner), packaging.
- [ ] Float/vector math module, mouse float coordinates, text/fonts, gamepad.

**Phase 6 — mobile · compile-only foundation landed**

- [x] Four mobile triples accepted as compile-only targets: `aarch64-apple-ios`,
      `aarch64-apple-ios-sim`, `aarch64-linux-android`, `x86_64-linux-android`.
      `Target::parse` no longer misclassifies them as desktop macOS/Linux.
- [x] `asm --emit-object-only --target <triple>` produces real objects (Mach-O
      `cffaedfe` / ELF `7f454c46`) with the system clang. Plain `build` on a
      mobile triple fails before invoking a linker with an explicit SDK message
      (Xcode/iOS SDK+codesign or Android NDK+Bionic) pointing at
      `--emit-object-only`.
- [x] `[mobile]` manifest section: app-id, display-name, target, permissions,
      assets, orientation, with validation; emitted only when non-default so v1
      manifests round-trip byte-identically.
- [x] `stdlib/ipc.um`: pure-UMAY JSON envelope codec for the webview bridge —
      decode request, encode reply/event, monotonic id, 1 MiB cap. Integer method
      ids and single integer args are a deliberate narrow version of the
      `webview-decision.md` envelope.
- [x] `examples/v2/mobile_core.um`: platform-independent note model plus IPC
      dispatch (method ids 1=add, 2=toggle, 3=delete, 4=count); selftest returns
      1. No SDL drawing, no webview/JNI code.
- [x] Tests: `phase22_mobile_target` (5), `phase22_mobile_manifest` (4),
      `mobile_ipc` (8), `phase22_mobile_core` (5). Full suite **950 passed,
      1 ignored**; clippy `-D warnings` clean.

Not possible on this Linux host (CI/macOS/NDK/accounts required):

- [ ] iOS: Xcode/iOS SDK sysroot, `LC_BUILD_VERSION`/load commands, codesign,
      provisioning, SPM/Xcode project, WKWebView scheme handler, simulator run.
- [ ] Android: NDK sysroot + Bionic startup, PIE, JNI bridge, Gradle AAR/APK/AAB,
      emulator run.
- [ ] Real webview shell, string-named IPC methods, native path/fd validation,
      app lifecycle, store assets, `.ipa`/`.apk` artifacts.

**Phase 8 — macros / plugin infrastructure · first slice landed**

- [x] ADR 0022 records the split: declarative token macros now; proc-macro
      dylib ABI design-only while `unsafe_code = "deny"` and zero dependencies
      remain hard rules. A separate macro-host process with a versioned wire
      protocol is the documented upgrade path.
- [x] Token-level `macro_rules!`-style expansion before parsing: one rule per
      macro, literal/`$x:ident`/`$x:tt` matchers, `name!(...)` invocation,
      deterministic textual output, no AST `Item` variant. Ceilings (no
      repetition, no recursion, call-site hygiene, literal spelling changes)
      are documented in `compiler/src/macro.rs`.
- [x] `umay expand <file>` (plus `--format json`) and `Artifact::MacroExpansion`
      / `"expand.um"` cache artifact keyed on source bytes; existing artifact
      keys and `umay doctor` derivation unchanged.
- [x] Span remap scaffolding: expanded source registered in `SourceMap`,
      generated-span -> invocation-span map exposed via `expand_source`.
      Sema/lowering errors are not yet remapped (deferred with the ADR).
- [x] `#[derive(Copy)]` now has real semantics: `TyCtxt::mark_copy` bridges the
      HIR flag into `is_copy`; unknown attributes still emit W0010.
- [x] Diagnostics E0125-E0129 documented and emitted. Tests:
      `phase23_macros` (12), `phase23_expand_cli` (4), `phase23_derive` (5).
      Full suite **975 passed, 1 ignored**; clippy `-D warnings` clean.

Deferred: proc-macro/plugin ABI and signed macro packages, user-defined
attribute macros, repetition/nested rules, full hygiene, derive(Clone/Debug),
and LSP expansion mapping.

**Hardening — compile-fail / UI snapshots (implemented).** The dependency-free
`compiler/tests/ui.rs` harness is in place: for every `compiler/tests/ui/*.um`
it runs `umay check --color never <case>.um` with the `ui/` directory as the
working directory (stable relative spans) and diffs the captured report against
`<case>.stderr`. Note that `umay check` prints rendered diagnostics to stdout
and only the summary to stderr, so the snapshots — named `.stderr` for
compatibility — capture stdout followed by stderr, with trailing whitespace
stripped per line and nothing else normalised. Regenerate with `BLESS=1` and
review the diff; a bare red snapshot blocks merge. Seeded, pinned cases cover
illegal closure captures / use-after-move into a closure (`E0201`), unannotated
closure parameters (`E1019`), object-safety violations (`E1023`), unknown `dyn`
traits (`E1022`), malformed macro definitions (`E0125`) and invocations
(`E0126`), and a v1 program that must stay diagnostic-free. CI home and the
exact command are in [`docs/v2/ci-matrix.md`](docs/v2/ci-matrix.md) §3.

**Hardening — v2 matrix (implemented 2026-11-18).**

- [x] Opt-in feature gates: `compiler/src/features.rs` checks closure, dyn/impl
      Trait, async/await, extern-ABI and default-method syntax against
      `#![feature(...)]` (E0154/E0155/E0156). Enabled only with
      `--features v2-gates`, so the v1 corpus stays green; 12 gate tests pass.
- [x] CI: `v2-features` and `v2-ui` jobs plus an AArch64 vtable-dispatch step
      added to `ci.yml`; `.github/workflows/mobile.yml` is a dispatch/nightly
      skeleton with compile-only object checks, never a PR gate.
- [x] Performance: first informational baseline recorded in
      `docs/measurements.baseline.json`; `scripts/bench.py --baseline` prints a
      ratio table with WARN/HARD-FAIL lines; no perf assertion blocks this host.
- [x] Reproducibility: `scripts/package.py` now emits byte-identical tar.gz/zip
      archives (verified by SHA-256); tag-only `release.sh` compares two package
      runs. A full compiler rebuild byte-compare remains a tag-only idea.
- [x] `docs/v2/benchmarks.md` status corrected: p1/p2/p4 are covered by Rust
      tests (no checked-in example), p8 now has `examples/v2/p8_derive.um`
      (expand + build + run print `42`).

Full suite after hardening: **977 passed, 1 ignored**; clippy `-D warnings`
clean; UI harness and gated tests pass in their respective modes.

**Open-items follow-up (implemented 2026-11-18).**

- [x] Async `Pending` + looping executor: `PendingOnce` (stdlib) returns Pending
      once then Ready(7); `block_on` on a concrete user `Future` now calls its
      concrete `poll` in a bounded busy-poll loop (1,000,000 iterations) and
      unwraps Ready. Generated async fns keep the one-poll path. No reactor,
      waker wake, spawn/select/timeout, cancellation or Send bounds yet.
- [x] Real Linux webview shell: `examples/v2/webview_gtk/` bindgen output
      (GTK3 + WebKit2GTK 4.1, 9 declarations), 18 link flags added to
      `Target::system_libraries`, Xvfb smoke run prints
      `{"id":7,"ok":true,"result":42}`. JS bridge callbacks, `file://`/asset
      scheme and macOS/Windows backends are deferred.
- [x] 3D software renderer: `examples/v2/math3d.um` (Q16.16 Mat4, integer
      projection, Bresenham lines) + `p7_3d.um`, Xvfb/dummy selftest prints
      `3D_OK 3 0`. No GPU, z-buffer, triangle fill, textures or fonts.
- [x] Audio slice (generation, not playback): `stdlib/audio_wav.um` writes
      deterministic mono 16-bit 8 kHz WAV (800 frames at 440 Hz), verified by
      Python's `wave` module and SHA-256. SDL3 device playback still needs
      struct-by-value + C callbacks in bindgen.
- [ ] Still open: proc-macro/plugin ABI (design-only per ADR 0022), real
      iOS/Android signing and app packaging (hardware/accounts), async reactor
      and waker wake, GPU rendering, audio playback and streaming.

Full suite: **996 passed, 1 ignored**; clippy `-D warnings` clean.

**Critical path**

`Phase 0 → 1 closures → 2 traits/dyn → 4 async`

Closures must be frozen before async (capture-by-reference plus async is a
lifetime trap). Traits and FFI precede dynamic dispatch. The independent lines
are `3 FFI → 5 GUI / 7 game`, `5 webview → 6 mobile`, and `8 proc-macro` after
Phase 2. See `docs/v2/benchmarks.md` for each phase's exit criteria.

---

## Rules that do not relax

- No LLVM or Cranelift in the production backend.
- No mandatory garbage collector.
- No project-local `target/` directory.
- No claimed feature without tests.
- No weakening of memory safety to simplify implementation.
