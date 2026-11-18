# ADR 0018 — Trait system and `dyn Trait` ABI

**Status:** accepted
**Phase:** v2 foundation
**Supersedes:** nothing

## Context

Trait bounds work today, but only statically. `stdlib/iter.um` defines
`Iterator<T>` and monomorphisation resolves a method call on a bounded
parameter to that type's `impl`; there is no vtable and no way to store two
different implementors in one collection (ROADMAP, ADR 0007). A missing bound
is `E3001`, never silently dropped.

v2 needs dynamic dispatch: a GUI backend selected at startup behind a
`Backend` trait (ADR 0020), heterogeneous `Vec<Box<dyn Renderer>>` scenes, and
boxing for async futures (ADR 0019). It also needs the trait language complete
enough that the static path is not a toy: where clauses, default methods,
associated items, supertraits, blanket impls and `impl Trait` returns.

Dispatch is ABI work because UMAY has its own x86_64 and AArch64 backends
(ADR 0002, ADR 0009, ADR 0015). A vtable call is an indirect call with a
specific `self` register and aggregate rules, per target.

## Decision

### 1. Trait declarations

- **Where clauses** are checked at the definition and again at each use site;
  a satisfied bound at definition does not excuse a call site.
- **Default methods** are emitted once and placed in the vtable; an `impl`
  may override them. A default method may call other trait methods only through
  `Self`, dispatching dynamically on `dyn`.
- **Associated types and consts** are named `Type::Item` / `Type::N`. For a
  trait object they must be bound at the use site (`dyn Iterator<Item = u8>`);
  an unbound associated type on a `dyn` is an error naming the missing binding.
- **Supertraits** mean `dyn Sub` carries the supertrait's methods in its
  vtable; a `Sub: Base` bound is required to call a `Base` method on `dyn Sub`.
- **Blanket impls** (`impl<T: Display> Debug for T`) are allowed, with overlap
  detection at definition and an orphan rule (trait or self type local to the
  crate). A bound with no `impl` remains `E3001`.
- **Method resolution** tries inherent methods first, then in-scope trait
  methods. Two applicable traits produce an ambiguity diagnostic listing both
  candidates and the trait names, not a silent pick.

### 2. Object safety

A trait is `dyn`-compatible when every method is object-safe. A method is
object-safe when it has no generic type parameters, does not return `Self`
unless `where Self: Sized` is present, and has no `Self: Sized` requirement that
would exclude it from dispatch. Associated consts must be specified and
associated types bound. `async fn` in a trait is **not** object-safe in v2;
expose a boxed-future signature instead (ADR 0019). A violation names the
offending method and the rule it breaks.

### 3. `dyn Trait` is a two-word fat pointer

```
struct FatPtr { data: *u8, vtable: *VTable }
```

`dyn Trait` is unsized; it is only reachable behind `&dyn T`, `&mut dyn T` or
`Box<dyn T>` (all two words, hence `Sized`). Generic parameters are `Sized` by
default; accepting a trait object in a generic function requires an explicit
`?Sized` relaxation.

### 4. Vtable layout and call convention

The vtable prefix is `{ size: usize, align: usize, drop: fn(*u8) }`, followed by
one function pointer per object-safe method, ordered supertrait methods first
then declaration order. The drop pointer is how `Box<dyn T>` and containers
destroy their contents; it routes through drop elaboration (ADR 0007). No
downcasting or `Any` in v2.

The first argument of every vtable method is the data pointer (`self`). The
compiler loads the vtable pointer from the second word of the fat pointer and
emits an indirect call through the slot:

- **SysV x86_64** (ADR 0009): `self` in `rdi`, further integers in
  `rsi, rdx, rcx, r8, r9`, floats in `xmm0`–`xmm7`, return in `rax`/`xmm0`. A
  fat pointer passed by value occupies two integer registers.
- **Win64**: `self` in `rcx`, further integers in `rdx, r8, r9`, 32 bytes of
  caller-allocated shadow space, floats in `xmm0`–`xmm3`, return in `rax`.
- **AAPCS64** (ADR 0015): `self` in `x0`, a by-value fat pointer in `x0`/`x1`,
  integers in `x0`–`x7`, floats in `v0`–`v7`, HFA rules, returns in `x0`/`x1`
  or `v0`–`v3`, larger returns via hidden pointer in `x8`. Vtable calls that
  carry float aggregates must pass the "Check HFA calls against C" step in
  `.github/workflows/ci.yml`.
- **Windows ARM64**: vtable calls follow Microsoft's composite/aggregate rules
  (ADR 0015, "Known limitations"), not AAPCS64 HFA. UMAY-to-UMAY calls stay
  internally consistent; runtime external calls pass scalars, so startup, file
  and console calls are unaffected.

All four paths are emitted by the existing backends
(`compiler/src/isel.rs`, `compiler/src/codegen.rs`, `compiler/src/aarch64.rs`);
there is no new machine-IR layer.

### 5. `impl Trait` in return position

`-> impl Iterator<Item = u8>` is an opaque, monomorphized type, not a trait
object. Callers see only the declared bounds and cannot name the concrete type;
it does not allocate and has no vtable. It is the static counterpart of
`Box<dyn Iterator<Item = u8>>`.

### 6. GAT is out of v2 scope

Generic associated types (`type Item<'a>;`) are **not** included. They require
lifetime-indexed associated-type constructors and higher-ranked bounds — the
same lifetime-inference machinery ADR 0008 and ADR 0017 explicitly deferred
(`E0203`). Adding them now would force a generic/lifetime redesign mid-v2.
Upgrade path: once lifetime inference lands, extend associated types to carry
lifetime parameters, keep the existing `dyn` layout unchanged (GATs are
naturally excluded from object safety), and add `dyn` tests for the new form.
Until then, return boxed iterators or parameterise the trait by a lifetime.

### 7. Tests

Object-safety rejection, unbound-associated-type error, ambiguity diagnostic,
`?Sized` generic acceptance, drop-through-trait-object counting, and vtable
call tests across SysV x86_64, Win64, AAPCS64 and Windows ARM64, including the
HFA CI extension. LSP (ADR 0014) shows the resolved method and its trait.

## Consequences

- Heterogeneous collections and runtime-selected backends become possible
  without changing the static monomorphisation path.
- Every vtable addition is a backend change on four ABIs; the CI HFA test is
  the shared correctness gate.
- `dyn` cannot carry generic methods or GATs in v2; those cases use `impl
  Trait` or boxed futures.
- Ambiguity and object-safety errors are new `E10xx` codes with `umay explain`
  entries and LSP spans (ADR 0003), never silent fallbacks.

## Alternatives considered

**Make every trait call dynamic.** Rejected: it gives up monomorphisation,
which is how the existing iterator and numeric code stay allocation-free.

**A single "fat pointer" convention hand-written per backend.** Rejected: it
would duplicate the aggregate rules four times; route through the existing
backend call lowering instead.

**Include GATs in v2.** Rejected: they depend on the deferred lifetime
inference (ADR 0008/0017) and would couple two large features.
