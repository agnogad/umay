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

## Phase 6 — Runtime and Standard Library · planned

- Allocation (`alloc`/`dealloc`), `String`, `Vec<T>`, `HashMap`.
- `Option`, `Result`, `Iterator` basics.
- I/O: stdout/stderr/`print`/`println`, buffered readers.
- `fs`: read, write, open, metadata.
- `process`: exit codes, arguments.
- `net`: TCP basics (only after the memory model is stable).

Exit criteria: a non-trivial CLI program can be written and built with only the
standard library.

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
