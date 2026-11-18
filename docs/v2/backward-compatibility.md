# v2 Backward Compatibility

Status: **policy, normative for 2.0.x**. The v1.0 spec is frozen. Every valid
v1.0 program must compile unchanged under the v2 compiler. This document defines
"unchanged", what counts as a break, the deprecation process, the one case where
a break is allowed, the edition question, and the CI that enforces it.

## 1. The contract

A v1.0 program compiles **unchanged** under v2 when all of the following hold:

1. **Same source.** The v1 source bytes compile with no edits and no
   `#![feature(...)]` attribute.
2. **Same observable behavior.** Program output, exit status, file and network
   side effects, and layout of v1 types are byte-for-byte what v1 produced.
3. **Same diagnostics for valid v1 code.** For a program v1 accepts, v2 emits no
   errors and no new warnings; any v1 warning is emitted with the same code,
   severity, message and span. The JSON schema (`diagnostics.md` §5) does not
   change.
4. **Same toolchain surface.** `check`, `build`, `run`, `fmt`, `lint`, `lsp`,
   `explain`, exit codes and manifest keys behave as documented for v1.

New lints that would fire on v1 source are opt-in only; they are never on by
default. Diagnostics for programs v1 *rejected* may improve — those programs are
outside the contract.

## 2. Breaking change

A change is breaking if it causes any item in §1 to fail. Concretely:

- v1 syntax is rejected or reparsed with a different meaning;
- an identifier legal in v1 (including `async`, `await`) stops being an
  identifier in the same position;
- a v1 type, expression or program that type-checked or borrow-checked no
  longer does, or checks to a different type;
- generated code produces different output, exit status, or observable layout
  (`sizeof`, field offsets, enum discriminants, string representation);
- a prelude or stdlib item available in v1 is removed or changes signature;
- a diagnostic code changes meaning, or a new warning fires on valid v1 source;
- a manifest key or CLI flag accepted in v1 is removed or changes effect.

Not breaking (additive), and therefore allowed in `2.0.x`:

- new gated syntax and new stable syntax that cannot be parsed as v1;
- new stdlib items, new diagnostic codes for new error conditions, new targets;
- contextual keywords that leave every v1 identifier position intact;
- clarifications that tighten a previously unspecified corner of v1.

## 3. Deprecation process and timeline

A feature that must eventually change follows this sequence, in order:

1. **Warn.** Introduce a `W`-code warning at the use site. The warning states the
   replacement and the first spec version in which removal is permitted.
   `umay explain <code>` and `umay lint --deprecations` expose it.
2. **Wait.** The warning ships for at least **two minor spec releases** (e.g.
   warn in `2.0.0`, may remove in `2.2.0` at the earliest), and never within a
   `2.0.x` patch series.
3. **Remove.** Only in a minor or major release, with the changelog entry naming
   the warning that preceded it.

A deprecation warning never changes the exit code (`diagnostics.md` §8), so a
deprecated v1 construct still satisfies §1 until it is actually removed — at
which point §4 applies.

## 4. When a breaking change is allowed

Breaking changes are permitted only in one of these cases:

1. **Major spec bump** (`3.0.0`), with a migration note. This is the normal path.
2. **Soundness or security fix.** The old behaviour violated the v1 spec (e.g.
   the compiler accepted a program the memory model forbids) or was a memory
   safety hole. Fixing a compiler bug is not a language break; the buggy program
   was never a valid v1 program. The fix ships with an `explain` entry and a
   regression test.
3. **A construct v1 already defined as an error or warning.** v1 §13 lists
   parsed-but-inert or rejected forms. Making one of those an error is not a
   break, because no valid v1 program relies on it.

No other justification — cleaner design, consistency with Rust, or "nobody
uses it" — permits a break in `2.x`.

## 5. Editions

The manifest already has `edition = "2026"` and the compiler knows exactly one
edition (`compiler/src/manifest.rs`, `EDITIONS`). **v2 does not introduce a new
edition.** Feature flags, not editions, are the mechanism for landing unstable
syntax (`feature-flags.md`): the same source file, the same edition, different
gates and channel.

An edition boundary would be needed only for a change that cannot be made
contextual — a change that turns a previously valid v1 identifier into a keyword
in every position, or flips a default that v1 code depends on. That is why
`async` and `await` are **contextual** keywords in
[specification.md](specification.md) §2: making them reserved would break v1
programs that used those names and would force an edition.

What would require an edition (none of it is in `2.x`):

- promoting `async`, `await` (or any new word) to fully reserved keywords;
- changing integer/float literal fallback, operator precedence or semicolon
  termination rules;
- changing the ownership default (e.g. shared-by-default bindings) or
  `match` exhaustiveness rules;
- changing the meaning of `?`, or of `extern fn`'s implicit `"C"` ABI;
- changing `&str` from NUL-terminated bytes to a counted slice, which would
  alter every FFI call in a v1 program;
- allowing deferred `let` initialisation, which v1 §5.1 forbids.

If one of these is ever required, introduce `edition = "2030"` as a separate,
opt-in boundary: both editions compile, v1 programs stay on `"2026"`, and the
crate root's edition selects the grammar. Do not smuggle an edition change into
a feature gate.

## 6. CI enforcement

The corpus is the v1 source that exists today:

- `examples/` — `http-server.um`, `http-router.um`, `web-app/`.
- `compiler/tests/` — the phase suites (e.g. `phase1_frontend.rs`,
  `phase2_sema.rs`, `phase4_ownership.rs`, `phase5_codegen.rs`,
  `phase7_diagnostics.rs`, `phase7_fmt.rs`, `phase7_lsp.rs`, `phase8_modules.rs`,
  `web_*.rs`) and `support/progen.rs`.

These files are **snapshotted** into a frozen corpus when `2.0.0` is cut, so
later edits to `examples/` cannot quietly weaken the test. The corpus is
recompiled in the v2 CI matrix on every change to the parser, checker, backends
or diagnostics:

| Axis | Values |
|------|--------|
| Channel | `stable`, `nightly` |
| Gates | none; all v2 gates enabled |
| Opt level | `-O0`, `-O3` |
| Target | `x86_64-linux`, `aarch64-linux` |

Assertions:

- every corpus program parses, checks and builds with exit status `0`;
- executable tests produce the same output and process exit status as the v1
  golden run;
- diagnostic tests match their v1 golden snapshots byte-for-byte in both human
  and `--format json` form (no new warnings, no code drift);
- `umay fmt` on corpus sources is idempotent and produces no diff.

A failure in this job blocks the change; there is no "expected break" override
other than §4, which requires the changelog and a golden-file update in the same
commit. The CI job and the compile/test budget for running the full matrix are
owned by the separate CI/budget document in the [v2 index](README.md).

## 7. What v2 adds without changing v1

- Closures, `dyn Trait` / `impl Trait`, default trait methods, extern blocks and
  named ABIs, `async fn` / `.await` — all gated by `feature-flags.md`.
- Contextual `async` / `await`; every v1 identifier keeps its meaning.
- Additive stdlib traits and types (`Future`, closure traits), new backends and
  targets, new diagnostic codes.
- GUI, mobile and proc-macro gates reserved ahead of their syntax.
- No new edition, no manifest-key changes, no flag required for v1 source.
