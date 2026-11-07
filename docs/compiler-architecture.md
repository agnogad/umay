# UMAY Compiler Architecture

Status: normative for the reference implementation. Phase 1 is implemented.

## 1. Pipeline

```text
Source text
    │
    ▼
SourceMap  ── file ids, byte spans, line/column mapping        [implemented]
    │
    ▼
Lexer      ── tokens + lexical diagnostics                     [implemented]
    │
    ▼
Parser     ── AST + syntax diagnostics (error recovery)        [implemented]
    │
    ▼
HIR        ── names resolved, desugared, types attached        [Phase 2/3]
    │
    ▼
Type check ── inference, trait resolution, exhaustiveness      [Phase 2]
    │
    ▼
MIR lowering ── expressions become temporaries, control flow      [Phase 3]
    │          becomes explicit branches
    ▼
Drop elaboration ── every exit path drops what it still owns      [Phase 3]
    │
    ▼
MIR        ── typed CFG: moves, borrows, drops, blocks         [Phase 3]
    │
    ▼
Ownership checking ── definite init and moves, per place       [Phase 4]
    │
    ▼
Borrow checking ── conflicts, escapes, both origins named       [Phase 4]
    │
    ▼
MIR optimisation (folding, DCE, copy prop, unreachable)         [Phase 5]
    │
    ▼
Layout ── sizes, alignments, SysV eightbyte classes             [Phase 5]
    │
    ▼
Frames ── one stack slot per MIR local                          [Phase 5]
    │
    ▼
Instruction selection ── machine IR, virtual registers          [Phase 5]
    │
    ▼
Register allocation ── linear scan with spilling                [Phase 5]
    │
    ▼
Assembly text (AT&T) ── one shared epilogue per function         [Phase 5]
    │
    ▼
`as` then `cc -no-pie` ── the system tools, not UMAY's own       [Phase 5]
    │
    ▼
Native executable in dist/
```

Each arrow is a module boundary with a narrow, explicit interface. No stage
reads source text except the lexer. No stage after HIR depends on syntax shape.

## 2. Module responsibilities

| Module | Owns | Must not |
|--------|------|----------|
| `source` | Source text, `FileId`, `Span`, `LineCol` | know about tokens |
| `token` | Token kinds, keyword table | know about syntax |
| `lexer` | Character → token conversion | know about the AST |
| `ast` | Syntax tree shapes | resolve names or types |
| `parser` | Tokens → AST, error recovery | resolve names or types |
| `diagnostics` | Diagnostic data, human + JSON rendering | depend on compiler phases |
| `explain` | Static code → explanation table | be optional or incomplete |
| `cache` | External cache location, AST JSON | write to the project dir |
| `hir` (Phase 2) | Named, desugared trees | know about machine types |
| `types` (Phase 2) | Type representations, inference | generate code |
| `mir` (Phase 3) | CFG, places, moves, drops | know about syntax |
| `lower` (Phase 3) | HIR → MIR, one block per linear region | know about ownership |
| `drop_elab` (Phase 3) | Where values are dropped | decide ownership |
| `mir_print` / `mir_parse` (Phase 3) | Text ⇄ MIR for tests and `umay mir` | be used by codegen |
| `ownership` (Phase 4) | Move/init dataflow over the CFG | rewrite MIR semantics |
| `borrowck` (Phase 4) | Borrow conflicts and escaped references | depend on source text |
| `opt` (Phase 5) | MIR → MIR transformations at `-O0`..`-O3` | change observable behaviour |
| `layout` (Phase 5) | Sizes, alignments, SysV eightbyte classes | know about instructions |
| `frame` (Phase 5) | One stack slot per MIR local | allocate registers |
| `machine` (Phase 5) | The machine instruction model | be a model of all of x86_64 |
| `isel` (Phase 5) | MIR → machine IR into virtual registers | know about physical registers |
| `regalloc` (Phase 5) | Intervals → registers and spills | know about MIR |
| `asm` (Phase 5) | Machine IR → AT&T text | resolve relocations |
| `codegen` (Phase 5) | Driving the backend, one function at a time | be LLVM or Cranelift |
| `linker` | Assembly → executable via `as` and `cc` | implement a linker front-end |
| `build` | Incremental driver, cache keys | create project-local targets |

Dependency direction is strictly downward in this table. `diagnostics` is a leaf
so that every phase can emit diagnostics without depending on higher phases.

## 3. Key data structures

### 3.1 Spans

```rust
struct Span { file: FileId, start: u32, end: u32 }   // half-open byte range
```

Spans are small, `Copy`, and carry the file so that multi-file diagnostics need
no context argument. All user-facing positions are derived via
`SourceMap::resolve`, which converts byte offsets to 1-based line/column while
counting *characters*, not bytes.

### 3.2 Tokens

Tokens carry a `line` field computed once by a single forward pass over the
source. The parser uses it for newline-aware statement termination, so the lexer
never needs to emit explicit "newline" tokens (which would either pollute every
consumer or be filtered out anyway).

### 3.3 AST

The AST is a faithful syntax representation: every node has a span, no name is
resolved, no sugar is desugared. `PatternKind::Literal` holds a boxed `Expr`
(the box breaks the `Expr`↔`Pattern` recursion and keeps node sizes sane).

Recursive descent with:

- precedence climbing for binary operators,
- a nesting-depth guard (`MAX_DEPTH = 256`, diagnostic `E0052`) so adversarial
  input cannot overflow the stack,
- statement- and item-level error recovery that always consumes at least one
  token, guaranteeing termination.

## 4. Error recovery strategy

The parser never bails out on the first error. Guidelines:

1. **Report, then resynchronise.** On a malformed item, skip to the next
   item-starting keyword or matched brace. On a malformed statement, skip to the
   next `;` or statement keyword at the same brace depth.
2. **Never emit two diagnostics for one byte range.** If the lexer already
   produced an error token, the parser stays silent about that token.
3. **Prefer one root-cause diagnostic over cascades.** A missing `)` reports the
   missing `)` and, if it can be avoided, not the three follow-on errors.
4. **Always make progress.** Every recovery path consumes at least one token or
   the token stream is exhausted.

Two invariants are enforced by tests:

- the parser never panics on *any* truncated prefix of a valid program,
- the parser never panics on mutated input (a deterministic fuzz loop).

## 5. Diagnostics architecture

Diagnostics are pure data (`Diagnostic`), rendered by two independent backends:

- a human renderer producing the canonical layout with ANSI colours optional,
- a JSON emitter producing the versioned schema (see
  [diagnostics.md](diagnostics.md)).

Both live in `diagnostics`. Compiler phases never format output themselves,
which is what keeps the JSON schema stable across phases.

Every code is registered in `explain::EXPLAIN`. A test asserts that every code
the parser can emit has an entry, so `umay explain <code>` is never a dead end.

## 6. Custom backend commitment

The production backend is written in this repository.

- x86_64 Linux first: ELF64 objects, System V AMD64 ABI.
- No LLVM, no Cranelift, no external IR as the production path.
- The system linker (`cc`/`ld`) is acceptable initially for the final link step.
- The backend is target-abstracted: instruction selection consumes a target
  description, so adding aarch64 later does not touch the frontend.

Correctness before optimisation. Optimisation levels are explicit
(`--opt-level 0..3`) and must never change program semantics.

## 7. Naming and code conventions

- Rust edition 2021; no `unsafe` in the compiler (`unsafe_code = "deny"` at the
  crate level).
- Diagnostic codes are string constants at the emission site, and every one is
  registered in `explain.rs`.
- Public interfaces between phases take data, not callbacks.
- Parser submodules (`items`, `expr`, `stmt`) are plain `impl Parser` blocks in
  separate files to keep each file reviewable; they share no state beyond
  `Parser`.

## 8. Testing strategy per phase

| Phase | Required tests |
|-------|----------------|
| Lexer | unit tests per token class, malformed literal tests |
| Parser | accept/reject tests per construct, error recovery tests, panic-freedom invariants |
| Semantics | accept/reject program pairs per rule |
| Borrow check | adversarial memory-safety fixtures, one per violation kind |
| Codegen | executable tests asserting program output and exit codes |
| Diagnostics | snapshot tests per code, JSON schema stability tests |

A bug fix must ship with a regression test. No exceptions.

## 9. Reproducibility

- Diagnostics are sorted deterministically (file, start, end, severity, code).
- No diagnostic depends on hash-map iteration order.
- Builds use content-addressed cache keys (see
  [build-system.md](build-system.md)) so identical inputs produce identical
  outputs.
