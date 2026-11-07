# UMAY Memory Model

Status: design specification for Phase 4. Not yet implemented.

This document is the normative description of UMAY's memory safety model. The
borrow checker's behaviour is defined by these rules, not by an implementation
accident.

## 1. Foundations

- **No garbage collector.** Memory is reclaimed deterministically.
- **One owner.** Every value with a destructor has exactly zero or one owner at
  any program point.
- **Ownership is spatially transferred, temporally tracked.** A move transfers
  ownership; from the move point the source is invalid.
- **Stack by default.** Values are allocated inline where possible. Heap
  allocation is explicit (`Box`, `Vec`, `String`).
- **Destruction at scope exit**, in reverse declaration order (LIFO).

UMAY is "Rust-inspired" but not Rust: the syntax for lifetimes is automatic, and
the checker is designed around a control-flow-graph formulation (see §6) rather
than a region-inference formulation, because UMAY wants first-class diagnostics
naming *the two conflicting program points* rather than region names.

## 2. Ownership

### 2.1 Moves

```um
let first = String.from("Hello")
let second = first        // move: `first` becomes uninitialised

print(second)             // OK
print(first)              // E0201: use of moved value
```

A move occurs when:

- a value is assigned to a `let` binding that is not `Copy`,
- a value is passed by value to a function,
- a value is returned,
- a value is placed into a struct/enum/tuple/array that is itself moved,
- `move expr` is used explicitly.

### 2.2 Copy types

These types are copied rather than moved:

- all integer, float, `bool` and `char` types,
- tuples/arrays of `Copy` types,
- shared references `&T`,
- raw pointers.

`&mut T` is **not** `Copy` — exclusive references move.

Only structs and enums explicitly marked `#[derive(Copy, Clone)]` become `Copy`,
and only if every field is `Copy`.

### 2.3 Partial moves

Moving a field out of a struct leaves the struct partially moved. Any later use
of the whole value is an error; using the remaining fields individually is
allowed. A partially moved value cannot be dropped as a whole; the compiler
inserts per-field drops for the fields still live.

## 3. Borrowing

Two reference forms:

| Form | Aliasing | Mutation |
|------|----------|----------|
| `&T` | any number simultaneously | forbidden through the reference |
| `&mut T` | exactly one, and no `&T` may coexist with it | allowed |

Rules:

1. At any program point, for any place `p`, the set of live borrows of `p` is
   either all shared or exactly one exclusive.
2. A borrow is *live* from its creation to its last use (non-lexical lifetimes),
   not to the end of the enclosing scope.
3. A value cannot be moved while any borrow of it is live.
4. A mutation of a place is forbidden while any borrow of that place (or a
   place it overlaps) is live.
5. A reference may never be used after its referent's storage is released.

Rule 2 is important for usability: UMAY does not reject code merely because a
reference variable is still lexically in scope.

## 4. Lifetimes

### 4.1 Automatic inference

Users write no lifetime annotations in ordinary code. The checker infers:

- **Elision for functions:** a function returning a reference gets the lifetime
  of its single reference input; with multiple reference inputs and no receiver,
  returning a reference is an error requiring an explicit annotation.
- **Elision for methods:** `&self` / `&mut self` supplies the output lifetime.
- **Generic types:** `struct Ref<'a, T>` is explicit, because a struct that
  stores a reference genuinely needs a name for its lifetime.

### 4.2 Explicit annotations (limited)

Where inference cannot succeed, UMAY allows a *single* explicitly named lifetime
per declaration, using apostrophe names:

```um
fn longest<'a>(x: &'a str, y: &'a str) -> &'a str { ... }
```

This is deliberately a subset of Rust: no lifetime bounds (`'a: 'b`), no
higher-ranked lifetimes, no multiple lifetime parameters on functions in v0.1.
Anything requiring more is deferred until the model has been validated.

### 4.3 Documented limitation

UMAY will reject some programs that a more powerful checker would accept. This is
an explicit trade: simpler diagnostics and predictable compile times over maximal
expressiveness. Each such rejection produces a diagnostic with an explanation
(`E0203`) so it is clear that the limitation is intentional, not a bug.

## 5. Destruction

### 5.1 Drop order

On scope exit, values are destroyed in reverse declaration order. Struct fields
are destroyed in declaration order. Enum payloads are destroyed according to the
active variant. Tuples are destroyed front to back. Arrays are destroyed front
to back.

### 5.2 Control flow

- Early `return` destroys everything in scopes being exited.
- `?` propagates the error after destroying the current scope's values.
- `break`/`continue` exit their loop body's scope and destroy its values.
- A value that was moved is never dropped.
- Panics unwind destructors same as returns (unwinding is not allowed to skip
  destructors; `panic = "abort"` builds abort after running whatever drops are
  statically guaranteed before the panic point).

### 5.3 `defer`

```um
fn main() {
    let f = File.open("x")?
    defer { f.close() }
    // ...
}
```

`defer` runs after the destructors of values declared in the same scope, in
reverse order of the `defer` statements. `defer` is sugar for a drop guard, not a
dynamic language feature; it is fully static.

### 5.4 Destructors

`drop` is a trait in the standard library. Implementing it is allowed; running
destructors manually is not. Values with a destructor:
- cannot be `Copy`,
- are dropped exactly once, statically guaranteed.

## 6. How the checker works (MIR + CFG)

Ownership and borrow checking operate on **MIR**, a typed control-flow graph with
explicit:

- assignments, moves, borrows and drops,
- basic blocks and edges,
- temporaries and their scopes.

The analysis is a forward dataflow over the CFG:

1. **Definite initialisation** — a variable is initialised on every path to its
   use. This is why `let` requires an initialiser (`E0303` at parse time): it
   keeps this analysis simple and its errors understandable.
2. **Move analysis** — track the set of initialised places; a use of an
   uninitialised or moved place is `E0201` / `E0202`.
3. **Borrow analysis** — for each place, maintain the set of live borrows. A
   conflicting pair at a program point produces `E0204`, with both origins named.
4. **Lifetime check** — a reference's inferred lifetime must not outlive its
   referent's. Two locations are reported: where the reference is used and where
   the referent is released.
5. **Drop elaboration** — insert drop flags for conditional initialisation so
   that destruction order is correct on every path, including error paths.

Checking is done on MIR and never on source text, so results do not depend on
formatting or expression nesting.

## 7. `unsafe`

The only place raw pointers may be created, dereferenced, or converted from
integers. `unsafe` is *permissive but tracked*: every unsafe operation is
recorded in MIR so that lints and audits can report them. `unsafe` does not
disable borrow checking of references; it only opens the raw-pointer surface.

Safety requirements documented for `unsafe` code:
- pointers must be valid and aligned,
- aliasing rules must still hold,
- no use-after-free.

## 8. Threads and data races (design position)

Shared references are `Send`/`Sync`-constrained: a value may cross a thread
boundary only if its type is `Send`, and may be shared across threads only if it
is `Sync`. `&mut T` is `Send` if `T: Send` and never `Sync`. Interior mutability
without synchronisation is not available in safe code. The concrete concurrency
API is a Phase 6+ concern; the *guarantee* is that safe code cannot construct a
data race.

## 9. Error codes (reserved for Phase 4)

| Code | Meaning |
|------|---------|
| `E0201` | use of moved value |
| `E0202` | use of possibly-uninitialised value |
| `E0203` | reference may outlive its referent (limitation or genuine error) |
| `E0204` | conflicting borrows (aliasing violation) |
| `E0205` | cannot borrow as mutable while borrowed as shared |
| `E0206` | move out of a value that is borrowed |
| `E0207` | return of reference to local value |
| `E0208` | double drop / move out of a type with a destructor |
| `E0209` | cannot move out of an index or dereference |
| `E0210` | dangling reference created from a temporary |
| `E0211` | raw pointer dereference outside `unsafe` |
| `E0212` | data race: non-`Send` value across thread boundary |
| `E0213` | aliasing violation through raw pointer |

## 10. Honesty policy

UMAY does not claim "Rust-equivalent safety" until the borrow checker and its
adversarial test suite are complete and validated. See
[ROADMAP.md](../ROADMAP.md) Phase 4 exit criteria.
