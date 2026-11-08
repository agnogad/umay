# 0013: `Send` and `Sync` are markers checked at the spawn boundary

- Status: accepted, enforced as of 2025-11-23
- Date: 2025-11-09
- Phase: 6

## Context

ADR 0011 chose processes for concurrency and named `Send`/`Sync` as the
successor. This records what that successor is, so that a thread can be a
language feature rather than an `extern fn` with an unchecked hand-off.

The memory model already knows ownership and borrows. What it does not know is
that a value may be *moved into another thread* or *shared by reference with
another thread*. Those are two different questions and Rust's answer is to make
them two marker traits.

## Decision

### Two empty traits, declared in the standard library

`Send` says a value may be moved to another thread. `Sync` says a `&T` may be
shared with another thread, which is the same as saying `&T` is `Send`.

Both are ordinary `trait` declarations with no methods. A marker trait needs no
machinery of its own: the existing trait machinery already answers "does this
type implement this trait", and this work reuses it.

### An `impl` is written, not inferred

Auto-derivation was rejected. An auto trait must decide, for every struct and
enum, whether its fields are `Send`, and then do the same for generic
parameters, and then explain to a user why their type is not. Each of those is a
new rule in the type checker, and a wrong guess is a memory bug the compiler
would have introduced rather than caught.

Writing `impl Send for T {}` is explicit and auditable. The standard library
writes the ones it knows are safe, and a user type says for itself. The cost is
that forgetting an `impl` is a compile error, which is the safe direction.

### The check lives at the spawn boundary, in the checker

The boundary is `thread_spawn::<A, R>(entry, value)`. The two type arguments are
written by the caller because nothing else names them: by the time the value
reaches the child it is bytes. That is the whole point of turbofish, which the
parser now accepts.

`check_call` recognises the name `thread_spawn` and takes it over. It requires
the two type arguments (`E1004` when they are missing), checks the value against
`A`, and then checks `A` with `require_send`. A type that does not carry the
marker is refused with `E1016`, whose suggestion is the `impl` line that would
fix it.

### A bound is checked where the call is made

A marker's `impl` is checked twice: directly, on the value, and through every
bound the callee declared. The second is what stops a forwarder from laundering a
value that is not `Send`:

    fn forward<A>(v: A) -> usize { thread_spawn::<A, _>(entry, v) }

`A` has no bound here, so the inner call is right to refuse it -- but only if the
bound is read from *this* function. A parameter is named `T` in every generic
function that has one, and answering from the bound list by name alone let one
declaration's `T: Send` stand in for another's bare `T`. A bound now carries the
name of the function that wrote it, and `is_send` asks only that function.

Only a marker is enforced. A trait with methods is resolved structurally in UMAY,
so a program that never wrote the `impl` still runs, and demanding it would reject
working code; a marker has no methods, so its promise is the whole of its meaning.

`is_send` reads the `impls` recorded on each trait: a parameter is `Send` when
it is bound `T: Send`, a primitive is `Send` when the library says so, an
aggregate is `Send` when it has an `impl` and -- for `impl<T: Send> Send for
Vec<T>` -- all of its arguments are `Send` too. References and raw pointers are
refused, because a borrow cannot outlive the process that made it.

The construct lowers to a call to `umay_thread_spawn`, named by its symbol
rather than by a `FnId`, because a construct has no declaration to point at.

### Two prerequisites had to be built first

Turbofish, so a generic call can name its type arguments, and a
function-pointer-to-`*const u8` cast, so an entry point can be passed at all.
Building the second turned up two backend gaps that made a function value
useless: materialising a `fn` as an operand was not implemented, and one
dereference of a `*const fn` loaded the first instruction byte and called that
as an address. Both are fixed, so a function value now behaves like an
address -- which is what the ADR said it needed.

`stdlib/thread.um` is in the prelude, so `Send`, `Sync` and `thread_spawn` are
available without an import.

## Consequences

- The markers are declared, and the standard library implements them for the
  primitives, `String`, `Bytes`, `Vec<T: Send>`, `Option<T: Send>`.
- `thread_spawn` refuses an `A` that is not `Send`, with `E1016`. A type that
  must not cross is stopped at compile time.
- The child is a `fork`ed process, not a thread: it has its own copy of memory,
  so no lock is needed and `Sync` is not yet read by anything. It stays declared
  so a scoped thread can be checked against it later without renaming anything.
- The hand-off is eight bytes in each direction. A value or answer wider than
  that is refused rather than truncated -- by `sizeof` in the library form, and
  by construction in `umay_thread_spawn`.
