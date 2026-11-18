# ADR 0017 — Closure semantics

**Status:** accepted
**Phase:** v2 foundation
**Supersedes:** nothing

## Context

UMAY has function values today: a `fn` can be materialised as an operand and
called through `(*f)()`, and ADR 0013 used that to pass a thread entry point.
What it does not have is a function value that *captures* its environment.
Closures are the missing primitive for three v2 features: iterator adaptors,
closure-based GUI event handlers (ADR 0020), and the state-machine captures of
`async` (ADR 0019).

Captures are a borrow-checking problem before they are a syntax problem. ADR
0008 already defines the rules — path-based places, borrows live to last use,
positions not blocks — and ADR 0007 defines drop elaboration. Closures must be
expressed in those terms, not in a second, parallel ownership model.

These semantics are **frozen before async work begins**. ADR 0019 lowers a
future's captured state using exactly the analysis fixed here; it may not
change this contract.

## Decision

### 1. Syntax

```
|x| expr              // one parameter, inferred type
|x, y| { ... }        // block body
|| expr               // zero parameters
move |x| ...          // force by-value capture
```

Parameter types come from the expected function type at the use site or from an
annotation (`|x: i64| ...`); the return type is inferred from the body, as with
ordinary body-local inference (ADR 0006). A closure with no expected type and no
annotation is an inference error, not a guess.

### 2. Closures are anonymous monomorphized structs

Each closure expression lowers to a synthetic, unnamed struct type with one
field per captured place, plus a compiler-synthesized call operator. There is
no runtime type descriptor for a closure unless it is coerced to `dyn`
(ADR 0018). The struct is monomorphized at the expression, so two syntactically
identical closures have distinct types.

**Capture analysis** scans the body for names that are neither parameters nor
locals declared in the body. For each free name we record the *place* (ADR 0008
`MovePath`), so `|x| p.x + x` captures `p.x`, not all of `p`. Each place is
captured in the least-privilege mode the body needs:

- read-only use → shared borrow `&T`;
- mutation or `&mut` use → mutable borrow `&mut T`;
- explicit `move`, or a use that consumes the value (passes it by value, moves
  it into another capture) → by value `T`.

A `move` closure captures every free variable by value. Captured by-value
values obey normal move semantics: the defining scope no longer owns them, and
they are dropped with the closure, not at the end of the defining scope.

### 3. Capture lifetimes use the ADR 0008 liveness rule

A capture's borrow is created at closure construction and lives to the closure's
last use (including its drop). The existing last-use analysis over linear
`(block, statement)` positions is reused unchanged, so:

```
let c = |x| read(&x, a)   // borrows a
let n = use(a)           // shared read — fine
let d = move |x| x + a   // move of a
```

works when `c`'s last use precedes the move in `d`, and conflicts when `c` is
called afterwards. A mutable capture conflicts with any overlapping borrow of
the same place, reported with both origins as `E0205`.

### 4. `Fn`, `FnMut`, `FnOnce`

Three declarations live in `stdlib/fn.um`, mirroring the `Send`/`Sync` pattern
of ADR 0013: the compiler synthesizes the impl for each closure, users do not
write one.

- `Fn` — call with `&self`; captures only shared borrows.
- `FnMut` — call with `&mut self`; captures include a mutable borrow.
- `FnOnce` — call with `self` by value; captures include a by-value move.

A closure implements the most permissive set consistent with its captures:
every `Fn` is also `FnMut` and `FnOnce`, every `FnMut` is also `FnOnce`.
Call syntax `f(args)` desugars to `Fn::call`, `FnMut::call_mut`, or
`FnOnce::call_once`, chosen from the binding's mutability and whether the call
consumes `f`. Calling `FnMut` through an immutable binding, or calling a
`FnOnce` twice, is a compile error.

### 5. Escape and drop

A closure may escape the function that created it, but only as far as its
captures allow:

- A by-value (`move`) closure may be returned or boxed freely; its captures are
  owned and travel with it (`Box<dyn Fn...>` per ADR 0018).
- A closure that borrows a local and is returned past that local's scope is
  `E0207` (return of reference to local) when the return is direct.
- When escape is possible but not provable with the current elision-based
  lifetime analysis, report `E0203` ("reference may outlive its referent"),
  the reserved limitation code from ADR 0008. The diagnostic must say it is a
  limitation so it is not mistaken for a compiler bug.

Dropping a closure drops its capture fields in declaration order via the
existing drop-elaboration pass (ADR 0007). Calling a `FnOnce` consumes the
closure; `Fn` and `FnMut` calls do not.

### 6. Capture-less closures coerce to function pointers

A closure with no captures coerces to a plain `fn` pointer. It carries no
environment word, so its ABI is exactly a C function pointer under SysV
x86_64, Win64, AAPCS64 and Windows ARM64 (see ADR 0018). This is the intended
path for C callbacks in FFI/bindgen and for simple GUI callbacks (ADR 0020).
A capturing closure cannot make this coercion; it must go through a `dyn Fn`
trampoline that the bindgen layer generates.

### 7. Tooling and tests

- LSP (ADR 0014) shows the inferred captures, their modes and the closure's
  `Fn`/`FnMut`/`FnOnce` set on hover; the formatter handles `|x|`, `move` and
  block bodies.
- Diagnostics reuse `E0201`, `E0204`, `E0205`, `E0206`, `E0207` and `E0203`;
  new bound errors are reserved in the `E10xx` range. Every new code gets an
  `umay explain` entry and a verified suggestion where possible (ADR 0003).
- Compile-fail tests: use after move into a closure, conflicting borrow through
  a capture, returned borrow-capturing closure, `FnMut` on an immutable
  binding, double `FnOnce` call. Run tests for coercion to `fn` pointer across
  all four ABIs.

## Consequences

- Closure captures are visible to borrowck as ordinary places, so ADR 0008
  needs no second set of rules.
- Anonymous types keep closures zero-cost; dynamic dispatch is opt-in through
  `dyn Fn` (ADR 0018).
- `E0203` means some safe escaping closures are rejected until full lifetime
  inference exists — a known, explicitly labelled ceiling.
- The generated struct shape is part of the ABI story for GUI and FFI, so this
  decision must land before ADR 0018 and ADR 0019.

## Alternatives considered

**Heap-allocate every closure behind a vtable.** Rejected: it makes the common
case (a callback used inline) cost an allocation and an indirect call.

**Give closures a named type users can write.** Rejected: opaque anonymous
types keep inference local and avoid a nominal type for an expression.

**Model captures with a fresh lifetime per closure.** Rejected: that is the
full lifetime-inference project ADR 0008 explicitly deferred; reuse last-use
liveness instead and report the residue as `E0203`.
