# Changelog

All notable changes to UMAY are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project uses
[Semantic Versioning](https://semver.org/spec/v2.0.0.html). The language
specification carries its own version (`v1.0`), which moves only when the
language surface changes; the compiler's package version can move for
implementation-only changes.

## [Unreleased]

### Added

- **Static AArch64 Linux compiler.** The release workflow builds
  `aarch64-unknown-linux-musl` with `rust-lld` and publishes an
  `aarch64-linux-musl` archive next to the x86_64 ones. `install.sh` selects it
  on `aarch64`/`arm64` Linux, so the compiler runs natively on ARM servers. A CI
  step builds the ARM64 compiler and runs it under qemu.
- **AArch64 backend.** `aarch64-unknown-linux-gnu` (and
  `arm64-linux`) and `aarch64-apple-darwin` (and `arm64-macos`)
  parse to a target with the AAPCS64 ABI. The backend lowers the full MIR
  surface: scalars, strings, pointers, arrays, structs, tuples, enums, generics,
  function pointers, floats, indirect calls, casts, branches and returns.
  AAPCS64 arguments use `x0`-`x7` and `v0`-`v7`, composites up to 16 bytes in
  registers and larger ones by reference, extra arguments on the stack; returns
  use `x0`/`x1`, `d0`/`s0`, or the hidden `x8` pointer. The program and runtime
  compile for the target, and a qemu CI job links and runs a program covering
  strings, functions, structs, aggregate arguments and returns, enums, tuples,
  floats, nine arguments and runtime integer formatting.
  - Homogeneous float aggregates use the AAPCS64 `v` registers; a C helper
    returning and taking a two-double struct is exercised in CI.
  - The macOS target emits Mach-O ARM64 with `@PAGE`/`@PAGEOFF`; a `macos-15`
    job builds, links and runs a program natively on Apple Silicon. The
    `aarch64-darwin` archive is part of a tag release.
  - `aarch64-w64-mingw32` emits COFF ARM64, links with llvm-mingw and produces
    a console PE. A native `windows-11-arm` CI job runs the result, and the
    `aarch64-windows` archive is published on tags.
  - Address-free integer and float locals are allocated by CFG liveness and
    interference graph colouring to `x19`-`x28` and `d8`-`d15`; disjoint
    lifetimes share a register and locals that do not fit spill to their stack
    slot. Only callee-saved registers the body uses are saved. A twenty-local
    loop improved from 0.032 s to 0.011 s under qemu, with stack accesses
    falling from 36 to 1; a three-million iteration loop fell from 0.064 s to
    0.030 s. Stack-argument staging uses `d0`, so it cannot clobber a promoted
    parameter. Scalar assignments now write the allocated destination directly
    into the destination register (`add x19, x19, x20`, `fadd d8, d8, d9`),
    eliminating the `x0`/`x1` round trip. Constants and stack operands are
    staged in `x8`/`x9` or `d0`/`d1`; a twenty-local loop's `mov` count fell
    from 142 to 16. Signed i8/i16/i32 stack loads now sign-extend
    (`ldrsb`/`ldrsh`/`ldrsw`) so negative values compare and divide correctly.
    Aggregate copies now move eight bytes per iteration with a 4/2/1-byte
    tail; a 2000-iteration 8 KB array copy fell from 0.097 s to 0.022 s in
    qemu. Calls and aggregate construction still use fixed scratch registers;
    see [ADR 0015](docs/adr/0015-aarch64-backend.md).
- **HTTP(S) `upm` registry.** `UMAY_REGISTRY` now accepts `http://` and
  `https://` roots in a `;`-separated list alongside directories. The client
  reads `<base>/<name>/versions.txt`, downloads
  `<base>/<name>/<version>.tar` with `curl`, and extracts it with a guarded
  USTAR reader that rejects absolute paths and `..`. `scripts/registry-server.py`
  serves a directory in that shape; run it behind a TLS proxy for a public
  registry. An integration test drives a real HTTP listener.
- **Installer.** `install.sh` downloads the latest release for Linux x86_64,
  Linux ARM64, Intel macOS or Apple Silicon, verifies its SHA-256, and installs
  `umay` to `~/.local/bin` (overridable with `UMAY_INSTALL_DIR`). Windows users
  install from the zip.
- **Project policies.** `SECURITY.md`, `CODE_OF_CONDUCT.md`, issue forms for
  bugs and feature requests, and a pull-request checklist.
- **Clippy is clean and gated.** `cargo clippy --all-targets -- -D warnings`
  passes; CI and `scripts/release.sh` run it before a build.

### Changed

- **Support matrix is explicit.** README, the roadmap and the ADR now name
  Linux x86_64/AArch64, Intel macOS, Apple Silicon, Windows x86_64 and Windows
  ARM64, including which archives the installer and release job publish.

## [0.3.1] — 2026-11-17

### Added

- **macOS and Windows release artifacts.** The tag workflow now packages a
  `x86_64-darwin` tar.gz and an `x86_64-windows` zip next to the static Linux
  artifact, and a final job publishes all three with their checksums. A new
  `windows-latest` job runs the unit suite, builds the compiler and links a
  small program, so a Windows regression is visible in CI rather than only
  under Wine on one machine. Integration suites that drive the Unix assembler
  and linker skip on Windows; the native smoke covers that path.
- `scripts/package.py` owns archive creation and checksums on every host, so
  Linux, macOS and Windows packages cannot drift in contents or layout.

### Changed

- The language surface and the generated code are unchanged from 0.3.0. This
  release exists to publish the macOS and Windows artifacts and to put the
  Windows build under CI.

## [0.3.0] — 2026-11-17

### Added

- **HTTP keep-alive.** A connection now serves its requests in one child
  process instead of forking per request. HTTP/1.1 keeps the connection open
  unless the request says `close`, HTTP/1.0 closes unless it asks for
  `keep-alive`, and a malformed or oversized request answers once and closes.
  The `Date` header is formatted again only when the second changes, and an idle
  connection closes after a second so an abandoned peer does not pin a child.
  - `socket_set_read_timeout` and `socket_set_nodelay` join `stdlib/net.um`,
    with POSIX (`struct timeval`, `IPPROTO_TCP`/`TCP_NODELAY`) and Winsock
    (`DWORD`, `SO_RCVTIMEO`) implementations behind them.
  - Nagle is disabled on accepted connections. With it on, the body written
    after the head waited for the peer's delayed acknowledgement, so every
    keep-alive response cost about 41 ms.
  - A body of up to 4 KB is appended to the head buffer and written with it:
    one `write` and one segment for an ordinary JSON response.
  - `GET /api/health` through one keep-alive client: 10,064 requests/s at one
    client and 60,085 at 64, against 1,441 and 5,286 when every request paid
    fork and close. Go 1.27.0's `net/http` on the same workload and machine:
    9,408 and 65,559. Method and caveats in `docs/measurements.md`.

- **Thread detach, and a POSIX join fix.** `thread_detach` releases a started
  thread's joinable reference without waiting: POSIX calls `pthread_detach`,
  Windows closes the `CreateThread` handle, and the wrapper frees the handle slot
  on the same path that marks the thread consumed. A join after a detach returns
  `false` instead of waiting on a reference that is already gone.
  - The new raw thread tests found that `thread_join` on POSIX passed the
    handle *slot's address* to `pthread_join`, where the API expects the
    `pthread_t` value stored in it. The first raw join crashed inside libc. The
    POSIX layer now reads the value first, the same way the Windows layer does.
  - `a_detached_thread_cannot_be_joined` pins the wrapper bookkeeping,
    `a_raw_thread_starts_and_joins` covers the start/join pair it builds on, and
    `thread_detach_assembles_on_every_system` checks both platform symbols.

### Changed

- **A comparison against a literal no longer materialises the literal.** `n < 2`
  selects `cmpq $2, %rbx` instead of `movq $2, %rax` followed by a register
  compare: one instruction and one virtual register fewer per comparison, and a
  smaller binary. A value that does not fit a sign-extended 32-bit immediate
  still goes through a register, because x86 cannot encode it in `cmp`.
- Regression test `call_arguments_move_in_parallel_and_returns_survive` pins a
  two-argument function whose arguments are call results and whose return value
  must reach `%rax`. That shape returned 20 instead of 14 when a call-setup
  experiment treated the return register as dead.
- **Linux releases are static and published by CI.** `scripts/release.sh`
  builds against `x86_64-unknown-linux-musl` when that target is installed, so
  the artifact no longer requires the host's glibc (the 0.2.0 binary needed
  glibc 2.39). The tag workflow verifies the binary is not dynamically linked
  and attaches the tarball and checksum to the GitHub release instead of
  leaving both as manual steps.

## [0.2.0] — 2026-11-17

### Added

- **Cross-target compilation.** `--target` selects the system to emit code for;
  without it the compiler builds for the host.
  - `x86_64-unknown-linux-gnu` (ELF, System V)
  - `x86_64-apple-darwin` (Mach-O, System V)
  - `x86_64-pc-windows-gnu` (COFF, Microsoft x64)
- **`--emit-object-only`** on `build`: assemble and stop, linking nothing.
  Producing an object for another system was already possible but always reported
  as a failed build, because the link that followed could not succeed. A caller
  who only wants the object -- to link it elsewhere, or to inspect it -- is now
  a success. Both object paths are printed, and `run --emit-object-only` is a
  usage error rather than a build with nothing to run.
  - Regression test `emitting_an_object_alone_succeeds_for_a_target_that_cannot_be_linked`
    checks the exit status, both printed paths, that both files really are Mach-O,
    that no executable appeared, and that `run` refuses the combination.
  - `--target` was also missing from `umay --help`; both flags are documented now.
- **`asm --emit-object-only`** prints the assembly *and* leaves the object in
  `dist/`. `asm` printed text and kept nothing, so a caller who wanted an object
  had to run `build`, which links, which a cross target cannot do. Without the
  flag `asm` still writes nothing -- a command that quietly starts leaving files
  behind is a worse surprise than one needing a flag.
  - The printed assembly is byte-for-byte what `asm` prints alone; the flag only
    appends the line naming the object.
  - Regression test `asm_leaves_an_object_only_when_asked` fails if the flag is
    ignored on `asm` (`dist/main.o` missing), and it also checks that the
    unflaggged run creates no `dist/` at all.
- **`--emit-object-only` prints the command that links its output.** Leaving the
  last step to the reader only helps if the instructions are right, and the flags
  differ per system (`-no-pie`, `-mconsole`, `-lws2_32`); a wrong one is a link
  error about something else. The compiler knows them, so it says them:
  ```
  built dist/both.o
  built dist/umay_runtime.o
    link it with: x86_64-w64-mingw32-gcc -o dist/both.exe dist/both.o dist/umay_runtime.o -mconsole -lws2_32
  ```
  Where the command cannot work on this host, it says that instead of suggesting
  it silently: ``(`cc` here links ELF, not Mach-O; this needs a linker that writes
  Mach-O)``.
  - Regression test `the_printed_link_command_works_as_printed` runs the printed
    line through a shell and runs the result. It fails if the command omits a
    runtime object, or writes the executable somewhere `build` would not (both of
    which happened while writing it: `-o main` rather than `-o dist/main`). The
    "links ELF, not Mach-O" wording is checked too, so the caveat cannot go back to
    blaming a missing program when the program is present and simply wrong-format.
- **An emitted Windows object links and runs outside this compiler.** The flag's
  whole claim is that the objects are usable elsewhere, and only the end of that
  chain proves it. `an_emitted_windows_object_links_and_runs_elsewhere` links
  `dist/main.o` and `dist/umay_runtime.o` by hand with `x86_64-w64-mingw32-gcc`
  -- the command a user would type -- and runs the result under Wine, checking it
  prints its own output. Verified: `21` and `socket ok`, so sockets link and work
  through the hand-built path too.
  - Skips cleanly when MinGW or Wine is absent, rather than failing on a machine
    that cannot do the check.
  - Fails if either object is missing, which is the way this can actually break.
- **A target that cannot be linked here says so, and says where the object is.**
  `umay build --target x86_64-apple-darwin` on Linux used to hand a Mach-O object
  to the host's linker and print its complaint about file formats. It now reports
  the formats involved, names the object it produced and leaves it on disk --
  which is the half of the job that did succeed.
  - The rule is the **linker's** format, not the host's: `x86_64-w64-mingw32-gcc`
    links COFF from Linux, and an earlier version of this check refused it.
  - Regression test `an_unlinkable_target_reports_where_its_object_went` fails if
    the rule goes back to comparing against the host's format.
- **A Windows object is checked for linkability too.** Same idea as the macOS
  check, read from the real COFF symbol table: `WinMain` must be exported, every
  UMAY name one object leaves undefined must be defined by the other, and every
  remaining name must be one MinGW's CRT or Win32 actually exports.
  - Regression test `a_windows_object_leaves_only_what_mingw_provides_undefined`
    fails if a binding is misspelt (`CreateThread` written as `CreateTread`).
  - This replaced a guessed-prefix list in `windows_references_match_their_definitions`
    that could not tell a C library name from a typo.
- **A macOS object is checked for linkability without a Mac.** The symbol table is
  read back from the assembled object: `_main` must be defined and global, every
  name the program leaves undefined must be one UMAY's runtime defines, and every
  C library name the runtime leaves undefined must be one macOS actually has.
  - Regression test `a_macos_object_leaves_only_its_own_runtime_undefined` fails if
    a binding is misspelt (`write` written as `wrote`), which is the fault class
    the check exists for.
- **`umay_pipe` works on Windows.** It binds `CreatePipe` instead of POSIX
  `pipe`. Both ends are stored eight bytes wide on every system, because a POSIX
  descriptor is a small integer and a Windows handle is pointer-sized: the
  platform layer widens the narrow one, so the portable body that forks and reads
  a pipe has one shape to write against.
- **Threads work on Windows.** `thread_spawn` hands one value to another thread
  and gets one answer back; how differs -- POSIX forks and reads a pipe, Windows
  starts a `CreateThread` and asks it for its exit value. `umay_thread_spawn`
  picks, so `thread_spawn` itself holds no system-specific code and its `Send`
  check is unchanged.
- **A generic value can have its address taken.** `&value as *const A` is the only
  way to reach a value whose type is a parameter, and it is what the cross-thread
  hand-off needs. It was refused twice over: the cast had no rule, and the parser
  read `&v as T` as `&(v as T)`.
- **A Windows `--target x86_64-pc-windows-gnu` build now runs.** The standard
  library's system-specific names moved into `stdlib/platform/`, so a Windows
  object binds WinSock and `CreateThread` instead of POSIX `accept` and
  `pthread_create`, and links against `ws2_32`.
  - Verified end to end: `umay build --target x86_64-pc-windows-gnu` produces a
    real `PE32+ executable for MS Windows (console), x86-64`, and under Wine it
    prints the right answers, opens a socket and binds a port.
- **Only the prelude a program reaches is emitted.** A four-line "hello" used to
  emit 160 function bodies -- the JSON parser, the HTTP helpers, the hash map --
  because the whole prelude is compiled with every program. Walking the call graph
  from `main` (and from every function whose address is taken, since an indirect
  call cannot be resolved statically) leaves a handful. `compute` builds in
  0.063 s instead of 0.092 s and its binary is 23 KB instead of 79 KB.
- `compiler/src/target.rs`: one description of what differs between systems —
  symbol spelling, object format, calling convention, assembler and linker
  invocations, and which runtime platform layer to compile.
- **The Windows x64 calling convention** in the backend: four argument registers
  in their own order, 32 bytes of shadow space, aggregates returned through a
  hidden pointer at any size, and `%rsi`/`%rdi` preserved across calls.
- The runtime split into a portable body and per-system layers under
  `runtime/platform/`. On Windows, where `fork` does not exist, the checked
  fan-out runs on `CreateThread`; the `Send` guarantee is unchanged because the
  front end settles it before either call.
- `compiler/tests/cross_target.rs`: asserts each target's assembly is accepted by
  a real assembler for its object format and yields the right object kind with
  `_main` defined, and that the Linux output did not change.

### Added

- **GitHub Actions CI.** Every push and pull request runs `cargo test --locked`
  and a release build. Tagged `v*` releases additionally build the checksummed
  artifact with `scripts/release.sh` and keep it as a workflow artifact.
- **A `LICENSE` file.** `Cargo.toml` declared `MIT OR Apache-2.0`, but the texts
  were not in the repository or the release tarball. Both are now included.

### Fixed

- **The differential generator stopped creating conflicting assignments.**
  It emitted `s.f = f(&mut s, ...)`, `s.f = s.g + 1`, and assignments to
  variables borrowed elsewhere in the same body. Those are either genuine
  conflicts or valid programs beyond the checker's field-wise borrow model, so
  long fuzz runs reported compiler errors that were really generator faults.
  The AST regression checks 2000 generated programs; CI still builds and runs
  30 of them end to end.
- **A parenthesized comparison was read as generic arguments.** The lookahead
  accepted a `)` inside `<...>`, so `(p < q) > r` became `p<q>(...)` and failed
  with `E0330`. It now tracks groups opened inside the argument list; a `)` or
  `]` that closes an outer group ends the scan. Differential seed 1573 found
  it, and `parenthesized_comparison_is_not_generic_args` pins the fix.
- **macOS C calls now use Mach-O's leading underscore.** Foreign symbols were
  emitted bare, as ELF and Win64 require, so the Mach-O linker looked for
  `malloc` instead of `_malloc` and reported every libc function undefined.
  The assembler now prefixes foreign symbols only for Mach-O, and the object
  test rejects a bare C name. The CI smoke job also runs on `macos-15-intel`;
  an arm64 runner cannot link this x86_64 backend's objects against its libc.
- **A `<` comparison could be parsed as generic arguments.** The lookahead that
  distinguishes `f<T>()` from `x < y` scanned up to 64 tokens and ignored
  boundaries, so a `>` in a *later* statement closed a phantom argument list:
  `while (i < 6i32) ... return a > (b)` reported `E0224` at the `6i32`. The scan
  now stops at any token that cannot appear in a type. A same-line case
  (`if x < y && z > (w)`) was affected too.
- **Borrow liveness crossed sibling CFG branches.** A `&mut` borrow created in
  one arm and last used in another was treated as live in blocks on unrelated
  paths, so a shared borrow after the `if` falsely reported `E0205`. Conflict
  checks now require the earlier borrow's last use to be reachable on the CFG
  from the later borrow's creation.
- **Win64 struct returns crashed before they could be used.** The callee read
  the hidden return pointer from `%rdi` (the SysV first-argument register)
  instead of `%rcx` (Win64), writing the result through a garbage address. The
  larger `http-router` example died this way under Wine; it now runs, and a
  regression test checks a 40-byte struct round-trip.
- **8-bit multiplication emitted an instruction the assembler rejects.** `imul`
  has no two-operand byte form, so `a * b` on `i8`/`u8` produced `imulb %cl, %dl`
  and the build failed with `invalid instruction suffix`. The selector now
  multiplies at 16 bits and keeps the low byte, which is the same result modulo
  2^8 for signed and unsigned operands. A differential fuzzing harness found it;
  with the front-end bugs below fixed, that harness now runs as a CI gate over
  30 seeds (`UMAY_DIFF_COUNT` still overrides the count for local sweeps).
- **`-O2` folded `!255u8` to -256.** Unary constant folding ran in an i128
  scratch integer and never truncated back to the operand's width, so a later
  cast widened the wrong value. Bitwise not and negation now truncate to the
  value's own type.
- **The parser could spin forever on a malformed `impl` body.** Error recovery
  stops at a top-level keyword so the program loop can parse the next item; a
  body loop calling it on a stray `impl` saw the same token again forever. Body
  recovery now guarantees one token of progress.
- **The cache grew without bound.** Every source edit creates new content keys,
  so the cache had reached 1.3 GiB / 19,000 entries on an active machine. New
  key directories now trigger eviction of the oldest entries down to 512 MiB;
  `UMAY_CACHE_MAX_BYTES` overrides it and `umay doctor` reports the limit.
- **`pub` could be bypassed two ways.** A private module item was also entered
  in the global bare-name table, so `util::helper()` resolved as a bare
  `helper()` from any module; a bare reference now resolves only inside the
  declaring module and reports `E0101` with the owner named elsewhere. Methods
  declared in an `impl` were not checked at all, so `value.hidden()` reached a
  private method from outside; that now reports `E1018`. A module's own private
  items and methods still work.
- **Four emitted diagnostics had no `umay explain` entry**: `E0192`, `E0193`,
  `E0194` (module loading) and `E1017` (trait bounds). A test now scans the
  compiler source and fails if any emitted code lacks an explanation.
- **Stale documentation**: the README phase table showed phases 6–9 as partial
  or unstarted, `lib.rs` said phases 2+ were unimplemented, `Cargo.toml` pointed
  at a repository URL that is not this one, and the ROADMAP's test count was
  out of date.

- **A pipe silently became two half-handles on Windows.** `umay_pipe` was a stub
  that always failed, and the storage callers read back was four bytes per end --
  too narrow for a `HANDLE`, so even a real `CreatePipe` would have handed back a
  value that named nothing. Both ends are eight bytes now, widened in the platform
  layer on POSIX and written at full width on Windows.
  - Regression test `a_pipe_reports_two_usable_ends` builds and runs a program that
    opens a pipe and rejects any end that is zero, negative or the same as the
    other. It fails if the widening is removed.
- **`&v as T` parsed as `&(v as T)`.** `as` binds looser than every prefix
  operator, so the cast applied to the thing being pointed at rather than to the
  pointer. The same fault made `*p as usize` mean `*(p as usize)`. The parser now
  takes one operand for a prefix operator and applies casts above it.
- **A frame assumed the stack alignment it was entered with.** Both ABIs promise a
  16-byte stack pointer at a `call`, and every frame relies on it -- but a Windows
  thread starts its routine without one, so the pointer was eight bytes off and
  the first call the thread made faulted. The prologue now rounds the pointer down
  instead of trusting it.
- **A structure argument was dropped when a call mixed register and stack
  arguments.** On Win64 the stack arguments begin *above* the 32-byte shadow
  space, not inside it: taking the larger of the two sizes put the first one at
  offset zero, where the callee never looked.
- **A one-field structure crashed on Windows.** Win64 returns a structure of 1,
  2, 4 or 8 bytes in `rax`, and 16, 32 or 64 bytes in `rax` and further registers;
  only the other sizes use a hidden pointer. The compiler treated *every*
  aggregate as needing the pointer, so the caller read the field from `rax` where
  the callee had left the address it was told to write through. The rule was
  written twice -- once in `codegen` for the callee, once in `isel` for the
  caller -- and fixing one did not fix the other; it now lives in
  `codegen::aggregate_goes_through_pointer` and both ask it.
  - Regression test `windows_returns_a_small_aggregate_in_a_register` checks the
    callee and the caller in the same assembly. It fails if the Win64 arm goes
    back to `size > 0`.
- **Windows sockets crashed on the first call after opening one.** `Socket.fd` was
  an `i32`, but a Windows `SOCKET` is a 64-bit handle: narrowing it lost the top
  half, and the resulting value named nothing. The field, and the platform
  bindings, are `isize` now.
- **Windows and macOS objects could not be linked at all.** Four separate faults,
  each invisible on Linux and each reported as the same page of
  `undefined reference` errors:
  - A cross-target assembler is a *driver*: `clang -o out.o in.s` links rather
    than assembles, so the object was never produced. `-c` was missing.
  - Definitions took the target's `_` prefix but references did not, so every
    call into the runtime and every read of a string literal named a symbol that
    did not exist. Only definitions went through the mangling.
  - Symbols from the C library were given the same prefix, which asked for
    `_malloc` when MinGW exports `malloc`.
  - MinGW's startup code calls `WinMain`, not `main`, whatever subsystem the
    driver is told to use. The program object now defines a `WinMain` that jumps
    to `main`; the runtime object must not, or the two collide.
- **The Windows link driver is `x86_64-w64-mingw32-gcc`, not `clang`.** Clang's
  PE linker cannot resolve the `__ImageBase` relocations in MinGW's import
  libraries and fails on every CRT archive.
- **A store through a pointer no longer has its pointer deleted.** Optimisation
  killed the liveness of a local whenever a statement assigned to it, but
  `*p = 5` lowers to `_0.(deref) = 5`: it writes *through* `_0` and does not
  define it. The statement that produced the pointer was therefore judged dead
  and removed, so the program stored through a register that was never written.
  `umay build -O1` and above produced binaries that segfaulted, and `alloc` /
  `dealloc` benchmarks died with `double free or corruption`. Optimisation is
  only allowed to remove work, never to change what a program computes.
- **A comparison's operands keep their own type.** Folding stamped a comparison's
  destination type (`bool`) onto its operands, which retyped an integer literal
  to a byte: `i < 1000` was emitted as `movb $1000, %cl`, so the bound became
  232 and a loop ran 232 times instead of 1000. The wrong answer appeared at
  every level above `-O0`.

- `errno`, `rename` and `mkdir` no longer reach a C symbol directly from the
  portable runtime: the accessor and the replacing-rename differ per system
  (`__errno_location` / `__error` / `_errno`, and Windows' CRT `rename` refuses
  to replace an existing target where POSIX replaces it).

### Changed

- **Documentation now matches the release state.** `README.md` describes all
  completed phases and the current per-target verification; `ROADMAP.md` says
  differential fuzzing runs in CI by default; the memory model and its ADR count
  the six unenforced diagnostic codes listed in their own table. Package
  metadata moves to `0.2.0` so the release notes describe a real version.
- **The benchmark now builds UMAY as an optimised release.** `scripts/bench.py`
  compiled UMAY at its default `-O0` while every other language in the table was
  given its optimiser (`gcc -O2`, `rustc -O`, Zig `ReleaseFast`). The comparison
  measured one flag, not five languages.

### Known limitations

- A non-host target's executable cannot be linked on a machine without that
  system's SDK and CRT. `umay build --target ...` keeps the assembled objects
  and prints the link command that a machine with the SDK can run.
- Windows objects link with MinGW-w64 and run under Wine locally, including the
  larger `http-router` example. Those checks skip on a runner without Wine;
  they are not a GitHub Actions gate yet.
- macOS objects are assembled and their symbols checked statically on any host.
  A `macos-15-intel` CI job links and runs a small binary; the result of a given
  commit is visible in Actions.
- `x86_64` is the only architecture. An ARM triple is refused with an
  explanation rather than emitting a binary nothing can load.
- Borrow liveness is still field-insensitive on some paths: reading `s.f` and
  assigning `s.g` in the same body can be rejected (`E0208` remains deferred).
  The 30-seed differential gate is green; longer runs stay manual.

## [0.1.0] — 2026-11-08

The first release: a language, a compiler, a standard library and a toolchain,
all working end to end. This version is the MVP that Phase 9 of the roadmap
defines — every phase from architecture to stabilization is done.

### Language

- Statically typed, natively compiled systems language with ownership and
  borrow checking and no garbage collector.
- Modules from files (`mod name;`), with dependencies reachable as modules.
- Generics with inline trait bounds, traits and `impl` blocks, `Option`/`Result`
  as ordinary enums, exhaustive `match`, `?` error propagation.
- Explicit, checked `as` casts, no implicit numeric conversion.

### Compiler

- Lexer and recursive-descent parser with error recovery and a nesting guard.
- Semantic analysis: interning type context, three-pass checking so declaration
  order never matters, local inference, exhaustiveness checking.
- HIR and MIR with a set of optimisations.
- Custom x86_64 backend: instruction selection, linear-scan register allocation
  with honest handling of fixed registers, and an assembler printer. No LLVM,
  no Cranelift.
- Ownership and borrow checking, drop elaboration.
- Diagnostics as data: stable codes, one rendering for humans and one for
  machines, `umay explain CODE` for every code.

### Standard library and runtime

- Written in UMAY and compiled by the same backend (ADR 0010): strings, `Vec`,
  `HashMap`, JSON over counted bytes, TCP, files and processes, and a
  concurrency model with `Send`/`Sync` checked at the spawn boundary.

### Toolchain

- `umay check | build | run | asm | ast | mir | fmt | lint | explain | doctor |
  new | clean | lock | metadata | version`.
- A language server that runs the compiler rather than a copy of it: live
  diagnostics, hover, go-to-definition (including across modules), completion,
  outline and formatting over JSON-RPC.
- A deterministic formatter that rewrites the gaps between tokens and nothing
  else.
- A content-addressed build cache keyed on the compiler's own binary, the source
  bytes and the module tree, so a stale artifact is impossible and an unchanged
  rebuild recompiles neither the program nor the runtime.
- `upm`: a manifest (`umay.toml`), a lockfile (`umay.lock`), resolution of the
  whole dependency graph, and fetching from a registry named by `UMAY_REGISTRY`,
  with every fetched package pinned by SHA-256.

### Known gaps

- `where` clauses and `let`-`else` are not parsed; a program that uses them is a
  clear syntax error. Use inline bounds and `match`.
- `pub` is accepted but there is no cross-module visibility enforcement yet.
- Attributes are parsed and carried but have no semantics; an unused one is
  `W0010`.
- Zig is not part of the build-time comparison in `docs/measurements.md` because
  it was not installed on the measurement machine; it is named, not dropped.

[0.2.0]: https://github.com/agnogad/umay/releases/tag/v0.2.0
[0.1.0]: https://github.com/agnogad/umay/releases/tag/v0.1.0
