# ADR 0011: Concurrency stays process-based for now

- Status: accepted
- Date: 2025-11-08
- Phase: 6

## Context

A web server and a GUI both need concurrency, and today neither has it. A
server can handle one connection at a time and a GUI cannot repaint while a
handler runs. `fork` works — the socket probe and the HTTP example proved it —
so the capability is not zero, but it is process-shaped, not thread-shaped.

The central question is what concurrency *means* in a language whose memory
model is ownership and borrows. Rust answers with `Send`/`Sync`: a thread may
hold a value only when its type says the value is safe to move across the
thread boundary, and a borrow may be shared only when its type says the
borrowed value is safe to share. That answer costs a marker system, auto-trait
derivation and an affine type check that follows moves between threads.

The backend is UMAY's own, so nothing stops us adding the same machinery. The
question is whether it is the least work that solves the two concrete needs.

## Decision

### Threads do not exist as a language feature yet

`pthread_create` works today through an `extern fn`; what does not exist is a
language-level thread with a checked ownership hand-off. Until `Send`/`Sync`
are specified, a program that wants threads writes them with raw pointers and
owns the ordering itself. That is the same contract `fork` already has, and it
is honest: the compiler promises nothing about cross-thread data flow, so it
must not pretend to check it.

### `fork` is the documented way to run things in parallel now

- The address space is copied, so there are no shared mutable cells to race on
  — the process model sidesteps the ownership question entirely.
- `accept()` on a listening socket survives a fork, so one process can accept
  and hand the connection to a child. The HTTP probe used exactly this shape.
- The price is copying the address space and the absence of shared memory
  (except pipes, sockets and files), which is precisely the shape an HTTP
  worker needs: stateless per-connection work.

### `Send`/`Sync` are the intended successor, not `async`

Rust's async model is about suspending and resuming *tasks* on a few threads;
UMAY has no coroutine machinery, no stack switching and no executor, so async
would be three new subsystems before the first line of application code. A
marker-based thread model reuses the type checker, which already knows ownership
and borrows; the additions are a per-type marker, a check at the thread-spawn
boundary, and a runtime `pthread_create` wrapper. It can be layered on top of
what `fork` gives today without changing the memory model.

## Consequences

- A concurrency bug today is a program bug, not a compiler promise broken.
- `examples/http-router.um` documents the process model: one process accepts,
  children handle.
- The `Send`/`Sync` work is tracked as the first item of the concurrency phase,
  with the boundary check and the runtime wrapper listed together so it lands as
  one testable unit.
