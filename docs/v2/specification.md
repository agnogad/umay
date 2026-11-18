# UMAY Language Specification — v2.0

Status: **working specification**. Normative for the v2 reference compiler. This
is a separate track from the frozen [v1.0 spec](../language-specification.md); it
describes additions only. Everything in v1.0 remains in force unless this
document or [backward-compatibility.md](backward-compatibility.md) says otherwise.
Where a semantic decision is owned by an ADR, this document states the decision
in one sentence and cites the ADR — it does not re-derive it.

## 1. Versioning

- The v2 spec version is `2.0.x`. `2.0.0` is the first slice (§3–§7).
- A **patch** (`2.0.x`) is a clarification or normative correction that does not
  change the set of accepted v1 or v2 programs, nor any diagnostic code.
- A **minor** (`2.1.0`) adds stabilized features. A **major** (`3.0.0`) is the
  only place a breaking change is allowed; see
  [backward-compatibility.md](backward-compatibility.md).
- A program conforms to `1.0` or to `2.0.x`. Conformance to `2.0.x` includes
  conformance to `1.0`.
- `umay --version` reports the spec version, the compiler channel, and the set
  of enabled feature gates.

Unstable grammar lands in the parser first and is marked `[unstable: <gate>]` in
this document. `[unstable: <gate>]` syntax is recognized by the parser on every
channel but is rejected in analysis unless the gate is enabled; the exact
mechanism, identifiers and diagnostics are in [feature-flags.md](feature-flags.md).
A feature is stabilized by removing its `[unstable]` marker and making its gate
inert. Grammar with no marker is stable.

## 2. Lexical additions

- `async` and `await` are **contextual keywords**: they are special only in the
  `async fn` and `.await` positions defined in §7. They remain valid identifiers
  everywhere else. This is required by the v1 freeze (v1 did not reserve them);
  see [backward-compatibility.md](backward-compatibility.md).
- `|` keeps its existing meanings (bitwise or, or-pattern separator). A closure
  is parsed where an expression is expected; the disambiguation rule is below.
- No other v1 keyword is added or redefined in `2.0.0`.

## 3. Closures — `[unstable: closures]`

```text
closure       ::= ('move')? '|' closure_params '|' (expr | block)
closure_params::= (closure_param (',' closure_param)* ','?)?
closure_param ::= identifier (':' type)?
```

- `|x| expr` has an expression body; `|x| { ... }` has a block body and its
  trailing expression is the result. `|| expr` takes no parameters.
- A `|` at the start of an expression, or immediately after `(`, `,`, `=`, `=>`
  or `:`, begins a closure. Everywhere else `|` is the existing binary/or-pattern
  token.
- Captures are per-place and least-privilege: shared borrow for reads, `&mut`
  for mutation, by value under `move` or when the use consumes the value.
  Closure kind (`Fn`/`FnMut`/`FnOnce`) selection is normative in ADR 0017.
- A closure lowers to an anonymous monomorphized struct with one field per
  captured place and a compiler-synthesized call operator (ADR 0017 §2). It
  converts to `dyn <closure trait>` where its bounds allow; a capture-less
  closure coerces to a plain `fn` pointer (ADR 0017 §6).
- Parameter types come from the expected type at the use site or from an
  annotation (`|x: i64| ...`); with neither, it is an inference error, not a
  guess (ADR 0017 §1).

## 4. Traits and trait objects — `[unstable: traits]`, `[unstable: dyn_trait]`

v1 already has `trait_decl`, `impl_block`, inline bounds and monomorphisation.
v2 adds the following; the v1 forms are unchanged.

```text
trait_decl      ::= 'trait' identifier generic_params? (':' trait_bound*)? '{' trait_item* '}'
trait_item      ::= fn_decl (';' | block) | associated_type | associated_const
impl_block      ::= 'impl' generic_params? type ('for' type)? where_clause? '{' fn_decl* '}'
where_clause    ::= 'where' type_bound (',' type_bound)*
associated_type ::= 'type' identifier ('=' type)? ';'
associated_const::= 'const' identifier ':' type ';'
```

`where` is reserved in v1 but unparsed (v1 §13); v2 gives it the grammar above
under the `traits` gate.

- A `trait_item` with a body is a default method; one without is required. A
  trait with supertraits (`trait A: B`) requires `B` of its implementors.
  Associated items are named `Type::Item` / `Type::N` and must be bound at a
  `dyn` use site (`dyn Iterator<Item = u8>`). Blanket impls are allowed, with
  overlap detection and an orphan rule, and `where` clauses are checked at the
  definition and again at each use site (ADR 0018 §1).
- `dyn Trait` is a two-word fat pointer (data pointer + vtable pointer),
  reachable only behind a reference or `Box`. Vtable layout, object-safety
  rules and per-target call convention are normative in
  [ADR 0018](../adr/0018-trait-object-abi.md). `async fn` in a trait is not
  object-safe in v2; expose a boxed-future signature (ADR 0018 §2).
- `impl Trait` in argument position is an anonymous generic parameter; in return
  position it is an opaque, monomorphized type, not a trait object (ADR 0018 §5).
- Generic associated types (`type Item<'a>;`) are out of v2 scope (ADR 0018 §6).
- OPEN: lifetime capture of return-position `impl Trait`, outside ADR 0018 §5.
  **Owner:** language owner. **Target:** `2.0.1`.

## 5. FFI — `[unstable: extern_abi]`

v1 `extern fn` (single declaration, C symbol, no body) stays valid and means
`extern "C" fn`. v2 adds grouped declarations and a named ABI:

```text
extern_block ::= 'extern' abi? '{' extern_item* '}'
abi          ::= string_literal                  // "C" when omitted
extern_item  ::= 'fn' identifier '(' param_list variadic? ')' ('->' type)? ';'
              |  'type' identifier ';'
              |  'static' 'mut'? identifier ':' type ';'
variadic     ::= ','? '...'          // terminal; requires one fixed parameter
```

- Declaration is safe; a **call** to an `extern` item is an unsafe operation and
  must appear inside an `unsafe` block, as in v1 §4.1.
- Extern types are opaque sized-by-declaration types usable only behind a
  pointer. Extern statics are never initialised by UMAY.
- The ABI string is part of the declared type: two `extern` declarations of the
  same name with different ABIs are different symbols and a duplicate is
  `E1010`.
- Variadic `extern "C"` functions accept a terminal `...` after at least one
  fixed parameter. Passing a floating-point argument to one is `E0157`
  ("variadic float arguments unsupported") until `%al` can be computed from the
  vector-register count; rejecting it beats a silent miscompile. See
  [ffi-spike.md](ffi-spike.md) §3.3.
- OPEN: the accepted ABI string set beyond `"C"`, and mangling rules per target.
  **Owner:** compiler owner. **Target:** `2.0.1`.
- Bindgen (`umay-bindgen`) is a tool, not language syntax, and is out of scope
  for `2.0.0`; its design and the per-target ABI spike are in
  [ffi-spike.md](ffi-spike.md). A capturing closure reaches C only through a
  generated `dyn Fn` trampoline (ADR 0017 §6).

## 6. Async — `[unstable: async_await]`

```text
async_fn    ::= 'async' 'fn' identifier generic_params? '(' param_list ')' ('->' type)? block
await_expr  ::= postfix_expr '.await'
```

- `async fn f(...) -> T` desugars to `fn f(...) -> impl Future<Output = T>`
  (ADR 0019 §1). The body lowers in MIR to a poll-based state machine with one
  state per `.await`; locals live across an await become fields of the generated
  type and drop through the existing drop-elaboration pass. No stack switching.
- `e.await` polls the future: `Pending` stores the `Waker` and yields;
  `Ready(v)` yields `v` and continues the state machine. `.await` is valid only
  inside an async body (ADR 0019 §2).
- The first version boxes the generated future internally at the await boundary;
  a public `Pin` and movable stack futures are deferred (ADR 0019 §1).
- `Future`, `Poll`, `Waker` and `Context` are runtime-agnostic in stdlib; the
  default executor/reactor is `umay-rt` (epoll/kqueue/IOCP), with `block_on`
  and `spawn_blocking` as the explicit bridge to blocking code (ADR 0019 §3, §8).
  The executor is not a language primitive.
- OPEN: `async` closure sugar. **Owner:** language owner. **Target:** `2.1.0`.

## 7. Not in 2.0.0

GUI, mobile and proc-macro are part of the approved v2 plan but have no grammar
in this spec version. Their decisions are owned by
[ADR 0020](../adr/0020-gui-backend-strategy.md) (GUI backend) and
[ADR 0021](../adr/0021-mobile-strategy.md) (mobile strategy); proc-macro has no
ADR yet. Their gates and identifiers are already reserved in
[feature-flags.md](feature-flags.md) so source written now does not change
meaning when they land.

## 8. Reference

| ADR | Owns |
|-----|------|
| [0017](../adr/0017-closure-semantics.md) | Closure capture and call semantics |
| [0018](../adr/0018-trait-object-abi.md) | Trait-object ABI and object safety |
| [0019](../adr/0019-async-model.md) | Async suspension and executor model |
| [0020](../adr/0020-gui-backend-strategy.md) | GUI backend |
| [0021](../adr/0021-mobile-strategy.md) | Mobile strategy |
