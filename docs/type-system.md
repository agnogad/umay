# UMAY Type System

Status: design specification for Phase 2 onward. Nothing in this document is
implemented yet except the *syntax* of type expressions.

## 1. Design position

The type system must be **sound before it is expressive**. A feature that cannot
be checked soundly is deferred rather than approximated with unsound shortcuts.
In particular:

- no implicit numeric conversions,
- no silent truncation,
- no implicit nullable types,
- no subtype polymorphism beyond the documented trait mechanism,
- no type inference that requires whole-program analysis across compilation
  units.

## 2. Types

### 2.1 Primitives

| Type | Size | Notes |
|------|------|-------|
| `bool` | 1 byte | `true` / `false` only |
| `i8` `i16` `i32` `i64` `i128` | 1/2/4/8/16 | two's complement, wrapping in release, panicking in debug |
| `u8` `u16` `u32` `u64` `u128` | 1/2/4/8/16 | |
| `usize` `isize` | pointer-sized | index and length type |
| `f32` `f64` | 4/8 | IEEE-754 |
| `char` | 4 | Unicode scalar value, not a byte |
| `str` | — | unsized UTF-8 string slice; used behind `&str` |
| `String` | 3 words | owned, growable UTF-8 string on the heap |
| `!` | 0 | never type: diverging expressions |

`()` is the unit type, represented as the empty tuple.

### 2.2 Composite

```text
(T1, T2, ...)      tuple; () is unit; (T,) is a one-element tuple
[T; N]             array of N elements, N a compile-time constant
[T]                slice, unsized
&T, &mut T         shared / exclusive reference
*const T, *mut T   raw pointer (unsafe contexts only)
Vec<T>             growable, heap allocated
Option<T>          Some(T) | None
Result<T, E>       Ok(T) | Err(E)
fn(A, B) -> C      function pointer
impl Trait         opaque return type (later phase)
dyn Trait          trait object (later phase)
```

Structs, enums and traits are nominal. Two structurally identical structs are
different types.

### 2.3 Layout rules

- Struct fields are laid out in declaration order.
- The alignment of a type is the maximum alignment of its fields.
- The size of a type is rounded up to its alignment.
- Enums are laid out as a tag plus the largest variant payload.
- `Option<&T>` and `Option<Box<T>>` use a null-pointer niche (no extra tag).
- Field reordering for padding reduction is an optimisation the compiler may
  perform only if it does not affect observable layout promises; v0.1 promises
  declaration-order layout.

## 3. Inference

Local inference only. Inference works within a single function body.

- `let` bindings may omit the type when the initialiser determines it.
- Numeric literals are polymorphic until constrained; if never constrained they
  default to `i32` for integers and `f64` for floats.
- Generic arguments may be omitted at call sites when inferable from arguments
  and the expected return type.
- Function *signatures* are always fully explicit. Inference never crosses a
  function boundary.

Unconstrained integer literal → `i32`; unconstrained float literal → `f64`;
fully unconstrained type variable in a `let` with no initialiser → error
(`let` always needs an initialiser, so this cannot silently create `!`).

## 4. Conversions

### 4.1 Implicit widening

None. Not even `i32` → `i64`. Every narrowing or widening is explicit via `as`
or a conversion function, which keeps overflow reasoning local and makes
inference more predictable.

### 4.2 `as` casts

`as` performs a *checked-value* cast with the following rules:

| From | To | Behaviour |
|------|----|-----------|
| integer | integer | truncating, two's complement (documented, deterministic) |
| integer | float | nearest representable value |
| float | integer | truncates toward zero; saturates at the type bounds; NaN → 0 |
| `char` | integer | Unicode scalar value |
| integer | `char` | **error**: use `char::from_u32(x)?` which returns `Option<char>` |
| reference | reference | identity only (no upcasting in v0.1) |
| `*T` | integer | permitted only in `unsafe` |

Narrowing integer casts are permitted because their behaviour is total and
deterministic. They are *explicit* and a lint (`truncation`) warns when a
narrowing cast can lose data.

### 4.3 Forbidden conversions

- reference → pointer without `unsafe`
- pointer → reference without `unsafe`
- numeric → `char` implicitly or silently
- anything → `bool`
- implicit `T` ↔ `Option<T>`

## 5. Generics

```um
fn first<T>(items: &[T]) -> Option<&T> { ... }

struct Pair<A, B> { first: A, second: B }

enum Result<T, E> { Ok(T), Err(E) }
```

- Generic parameters are declared in `<...>` after the name.
- Monomorphisation is the implementation strategy: each instantiation produces
  concrete code. This keeps code generation simple and performance predictable.
- Bounds use `T: Trait + OtherTrait`. Bounds are required before calling
  trait methods on a value.
- Recursive monomorphisation is detected and rejected with a dedicated code
  (instantiation depth limit), not an infinite loop.

## 6. Traits

```um
trait Display {
    fn display(&self) -> String
}
```

v0.1 rules:
- A trait declares required method signatures.
- An `impl Trait for Type` block provides them.
- Calls on generic values require a bound.
- Default methods, associated types, associated constants, supertraits and
  negative impls are deferred.
- Blanket impls are deferred.

## 7. Exhaustiveness

`match` must be exhaustive over:

- `bool` → both values,
- enums → every variant (recursively for payloads),
- integers → either every value or a final `_`,
- tuples and structs → all fields matched or `..` used,
- references and options → all variants.

`if let` and `while let` are sugar over match and are exhaustive by
construction.

## 8. Error codes (reserved for Phase 2)

| Code | Meaning |
|------|---------|
| `E1001` | type mismatch |
| `E1002` | cannot infer type |
| `E1003` | unknown type |
| `E1004` | wrong number of generic arguments |
| `E1005` | arity mismatch in call |
| `E1006` | unknown field |
| `E1007` | unknown method |
| `E1008` | non-exhaustive match |
| `E1009` | unreachable pattern |
| `E1010` | duplicate definition in scope |
| `E1011` | missing trait bound |
| `E1012` | trait not implemented |
| `E1013` | invalid `as` cast |
| `E1014` | operator not defined for type |
| `E1015` | recursion limit exceeded during monomorphisation |

These codes are reserved now so that documentation, tests and editor
integrations can refer to them stably before they are emitted.
