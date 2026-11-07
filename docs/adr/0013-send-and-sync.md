# 0013: `Send` and `Sync` are markers checked at the spawn boundary

- Status: accepted
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

### The check has no place to stand yet

The intended boundary was `thread_spawn_with(f, arg)` with `A: Send`. It cannot
be written: by the time the value reaches the thread it is a `*mut u8`, and the
type is gone. A checked form must be generic over the value, and naming its
result type needs turbofish (`thread_spawn::<Job, i64>(..)`), which the parser
does not accept.

So the markers exist and the standard library implements them for the types it
knows, and **nothing checks them**. `Send` and `Sync` are therefore a named
intention, not an enforced rule, and this ADR says so rather than letting an
`impl` list imply otherwise. `stdlib/thread.um` declares them and a
`pthread_create` wrapper; the wrapper is not in the prelude, because it cannot
be called until a function pointer can be cast to `*const u8`.

## Consequences

- The markers are declared, and the standard library implements them for the
  primitives, `String`, `Bytes`, `Vec<T: Send>`, `Option<T: Send>`.
- No stage reads them yet. A type that must not cross is not stopped.
- Two prerequisites are named and unmet: turbofish, so a generic call can name
  its type arguments, and a function-pointer-to-`*const u8` cast, so an entry
  point can be passed at all.
- `Sync` is declared for the same reason: so that a scoped thread can be checked
  against it later without renaming anything.
