# ADR 0019 — Async model

**Status:** accepted
**Phase:** v2 foundation
**Supersedes:** the "async is not the plan" clause of ADR 0011 only

## Context

ADR 0011 chose process-based concurrency (`fork`) and said async was not the
plan. ADR 0013 then added `Send`/`Sync` markers checked at the spawn boundary.
That decision stands for v1: `examples/http-router.um` and every process-based
server keep working unchanged. What v2 adds is a second, cooperative model for
the cases `fork` cannot serve — an event-driven desktop GUI (ADR 0020), many
concurrent network connections without one process each, and mobile I/O
(ADR 0021).

This ADR supersedes **only** the "async is not the plan" sentence of ADR 0011.
Process concurrency is not removed, not deprecated, and not rewritten as async.

## Decision

### 1. Poll-based state machines, not stackful coroutines

`async fn f(...) -> T` desugars to `fn f(...) -> impl Future<Output = T>`. The
body lowers in MIR (ADR 0007) to a state machine with one state per `.await`
suspension point; locals live across an await become fields of the generated
type, and their drops are handled by the existing drop-elaboration pass. There
is no stack switch, no segmented stack and no new runtime assembly — the custom
backends only see an ordinary struct and a `poll` call.

**First version boxes the state machine at the await boundary.** The compiler
allocates the generated future on the heap and never moves it after the first
poll, which is what makes self-referential borrows across `.await` safe. A
public `Pin` type and the associated unpin machinery are **deferred**; the
boxing is internal to the lowering and invisible to users. This is a known
ceiling: stack-allocated, moveable futures need `Pin` later.

### 2. Core types and `.await`

`stdlib/future.um` declares `Future<Output>`, `Poll::{Ready, Pending}`,
`Waker` and `Context`:

```
trait Future { type Output; fn poll(self: &mut self, cx: &mut Context) -> Poll<Self::Output>; }
```

`expr.await` polls the future. `Pending` stores the Waker from `Context` and
returns `Pending` to the caller; the reactor invokes the Waker when the event
arrives, rescheduling the task, which polls again. `Ready(v)` yields `v` and
continues the state machine. Re-polling a completed future is a programmer
error reported in debug builds.

### 3. Executor / reactor split

- **Runtime-agnostic core** (`stdlib/future.um`, `stdlib/task.um`,
  `stdlib/io.um`): the traits and types above, with no OS dependencies.
- **Default runtime** (`umay-rt`): a single-threaded executor plus a reactor
  built on `epoll` (Linux), `kqueue` (macOS/BSD) and IOCP (Windows).
- **Blocking-I/O bridge**: `spawn_blocking` and `block_on`. Legacy blocking
  stdlib calls (`stdlib/fs.um`, `stdlib/net.um`) run on a small thread pool;
  their hand-off requires `Send` (ADR 0013). This lets existing synchronous
  code work from async without blocking the reactor, and lets sync code drive
  a future at a boundary.

### 4. I/O wrappers

Non-blocking descriptors registered with the reactor back async wrappers for
**timer, TCP** (`TcpListener`/`TcpStream`), **UDP** (`UdpSocket`) and **file
I/O** (via `spawn_blocking`). **DNS** resolves through `getaddrinfo` on the
blocking pool, since no async resolver is available on all targets. All
wrappers live in an `std::async::*` module, parallel to the sync ones (§8).

### 5. Tasks, cancellation, structure

`spawn` returns a `JoinHandle`; `join` awaits it; `select!` runs several futures
and completes with the first; `timeout(dur, fut)` races a timer. Cancellation is
dropping the future — its `Drop` runs through drop elaboration (ADR 0007), so
file descriptors and buffers are released. Structured concurrency: spawned
tasks are scoped to a parent, which awaits them at scope exit; dropping the
scope cancels unfinished children. Backpressure uses bounded channels whose
`send` awaits available capacity.

### 6. `Send` bounds and the lifetime dependency

`spawn` requires `Future + Send + 'static` (ADR 0013). A future that borrows a
local cannot be spawned; it may only be awaited inline while that local lives.
Async lowering reuses ADR 0017's capture analysis verbatim: the state-machine
fields are exactly the places a closure would capture, with the same borrow,
mutable-borrow and move modes and the same last-use liveness (ADR 0008).

This is the capture-by-reference-plus-async lifetime hazard: a future borrowing
a local outlives the borrow as soon as it is stored or spawned. The rule is
concrete — borrowed captures stay inline; `move` plus owned captures is what
may be spawned; an escape whose safety cannot be proven under the current
elision-based analysis is reported as `E0203`, the reserved limitation code
(ADR 0008/0017), and a non-`Send` capture crossing `spawn` is `E1016` (ADR 0013).

**Dependency order:** ADR 0017 (closures) → ADR 0018 (traits/`dyn`) → ADR 0019
(async). Closure capture semantics and the `dyn Future` layout must be frozen
first; async work may not change them.

### 7. HTTP and error locations

An async HTTP **client and server** are built on the async TCP wrappers; the
server uses the same structured-concurrency shape as the v1 process server but
with one task per connection instead of one `fork`. An error raised across an
await records the `.await` span in its location, and a task-local await-chain is
kept so runtime errors produce an async backtrace (`UMAY_BACKTRACE=1`). Both
use the diagnostic types of ADR 0003, so `--format json` and LSP get the same
data.

### 8. Function colouring — concrete mitigation

We do **not** convert the standard library to async. Two API surfaces are
maintained: the existing synchronous functions (`std::net::TcpStream`,
`std::fs`) and a parallel async module (`std::async::net::TcpStream`). A
runtime bridge crosses between them only at boundaries: `block_on(fut)` from
sync code, `spawn_blocking(f)` from async code. Sync code is never forced to
become async, so "colour" does not propagate through the call graph; it is
contained at explicit, visible bridges.

### 9. Tests

MIR snapshots of the state machine (one state per await, correct field drops),
executor and reactor tests, cancellation/drop tests, `E0203` and `E1016`
compile-fail tests, timer/TCP/UDP round trips, and HTTP client/server
integration tests. LSP shows a future's `Output` type and the await location.

## Consequences

- High-concurrency I/O and event-driven GUIs become possible without `fork`
  per connection; v1 process programs are untouched.
- The boxed-future-first choice costs one allocation per spawned task and
  defers `Pin`; stack futures are a later, bounded upgrade.
- Two API surfaces mean some duplication in the stdlib, contained by the
  `block_on` / `spawn_blocking` bridge and the `std::async::*` namespace.
- Borrowed captures across `.await` are restricted by ADR 0017 and reported as
  `E0203` under elision; full lifetime inference is the upgrade path.

## Alternatives considered

**Stackful coroutines with their own stacks.** Rejected: they require new
backend assembly and would sit outside the ownership/borrow model entirely.

**Rewrite the stdlib as async throughout.** Rejected: it infects function
colour across every caller; two surfaces plus a bridge is more contained.

**Resume the ADR 0011 "no async" decision.** Rejected: GUI, mobile and
high-concurrency networking have no process-based answer; async is additive,
not a replacement.
