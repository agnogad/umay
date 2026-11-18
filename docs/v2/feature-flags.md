# v2 Feature Gates

Status: **policy, normative for 2.0.x**. Modeled on Rust's
`#![feature(...)]`. This document defines the exact identifiers, diagnostics,
channel behaviour and tool impact for landing unstable v2 syntax. The syntax
each gate hides is defined in [specification.md](specification.md).

## 1. Syntax and scope

```text
feature_attr ::= '#!' '[' 'feature' '(' feature_name (',' feature_name)* ','? ')' ']'
```

- A feature attribute is a crate-level **inner** attribute. It appears at the
  top of the crate root, before any item. Multiple names in one attribute and
  multiple `feature` attributes are both accepted; the enabled set is their
  union.
- Only the crate root may carry one. The same attribute in a `mod name;` file or
  an inline module is rejected with `E0155`.
- v1 parses attributes but gives them no semantics (`W0010`, unused attribute).
  v2 gives the `feature` attribute semantics; it is no longer `W0010`. All other
  unknown attributes keep `W0010`.
- There is no CLI flag, env var or manifest key that enables a feature. The
  enabled set has exactly one source — the crate root — so the editor and the
  build cannot disagree.

## 2. Feature identifiers

Identifiers are exact and case-sensitive. A name is never reused, and a removed
name stays reserved.

| Identifier | Gates | Spec | Status at `2.0.0` |
|------------|-------|------|-------------------|
| `closures` | `\|x\|` closures, `move` closures | §3 | unstable |
| `traits` | Default methods, supertraits, associated items, `where` clauses | §4 | unstable |
| `dyn_trait` | `dyn Trait`, `impl Trait` | §4 | unstable |
| `extern_abi` | `extern` blocks, named ABI strings, extern type/static | §5 | unstable |
| `async_await` | `async fn`, `.await` | §6 | unstable |
| `gui` | GUI crate syntax and backend hooks | §7 (later version) | planned, reserved |
| `mobile` | mobile target/platform syntax | §7 (later version) | planned, reserved |
| `proc_macro` | proc-macro definitions and attributes | §7 (later version) | planned, reserved |

Notes:

- `traits` does **not** gate v1 `trait`/`impl`, inline bounds or monomorphisation;
  those are stable v1.
- `dyn_trait` is separate from `traits`: an implementation may ship stable
  default methods while `dyn Trait` is still gated, and vice versa.
- `extern_abi` does **not** gate v1 `extern fn`; it gates the new block form and
  named ABIs only.
- A `planned, reserved` name is accepted in `#![feature(...)]` but currently
  gates no syntax; it exists so source does not change meaning when the feature
  lands.

## 3. Lifecycle

```text
unstable  ->  stabilized  ->  removed
```

| Phase | Gate required | Channel | Meaning |
|-------|---------------|---------|---------|
| unstable | yes | nightly only | Grammar may change; diagnostics may be refined in patch releases. |
| stabilized | no (inert) | any | Grammar is frozen. Writing the gate emits `W0011`, then is allowed to remain. |
| removed | name rejected | any | The gate name is gone (`E0155`); the syntax is stable and needs nothing. |

Timing rules:

- A feature spends at least one minor spec version as unstable before
  stabilization.
- A stabilized gate remains accepted (with `W0011`) for at least one minor spec
  version before removal, so downstream crates can delete the attribute without
  a flag day.
- Stabilization or removal is a `2.x.0` change, never a patch.
- The phase of each identifier is recorded in §2; changing it requires updating
  this table and the spec marker in the same commit.

## 4. Diagnostics

Codes are reserved here before emission and must be registered in
`compiler/src/explain.rs` with the usual uniqueness and non-empty-text test.
They sit in the syntax/structural range `E0100`–`E0399` of
[diagnostics.md](../diagnostics.md).

| Code | Trigger | Severity | Help text points at |
|------|---------|----------|---------------------|
| `E0154` | Gated syntax used without its crate-level gate | error | `#![feature(<name>)]` and the required channel |
| `E0155` | Malformed, misplaced, unknown, duplicate or removed feature name | error | the correct name or the crate root |
| `E0156` | A feature attribute or gated syntax on a stable compiler | error | the nightly channel |
| `W0011` | Gate written for a stabilized feature | warning | "gate is inert; remove it" |

Rules:

- `E0154` spans the gated syntax, not the crate root. One diagnostic per
  distinct gated construct; repeated uses of the same construct in one file each
  get their own span, following the one-range-one-report rule.
- `E0155` wins over `E0154`: an unrecognised or misplaced gate is reported first.
- Warnings never change the exit code (`diagnostics.md` §8).
- `--format json` and `umay explain` treat these codes like any other; no
  special output path.

## 5. Enforcement point

The parser recognizes all v2 grammar of the current compiler on **every**
channel. Gate enforcement is a dedicated analysis pass immediately after
parsing and before name resolution. Consequences, all deliberate:

- A stable compiler can still parse and format gated source; it refuses to
  *build* it (`E0154`/`E0156`).
- The gate error is an ordinary diagnostic with a byte span and a cause, not a
  lexer/parse failure. Diagnostics-first and machine-readable output are kept.
- No feature-dependent parse modes, so no second grammar to keep in sync.

## 6. CLI and channels

- Channel (`stable` | `nightly`) is a property of the compiler build, reported
  by `umay --version`. It is not a per-invocation flag.
- `check`, `build`, `run`, `asm` and `lsp` evaluate gates the same way: read the
  crate root, compute the enabled set, run the gate pass. There is no
  `--enable-feature`.
- On `stable`: any `#![feature(...)]` with a still-unstable name is `E0156`;
  gated syntax without a gate is `E0154` whose help asks for nightly. A gate on
  a stabilized feature is inert and emits only `W0011`.
- On `nightly`: an unstable name works only with its gate; without it, `E0154`.
- **Bootstrap** here means compiling the shipped `stdlib/` and
  `runtime/runtime.um` with the release compiler (the compiler itself is Rust;
  ADR 0001). Those sources must build on `stable` using stabilized features
  only. Nightly builds may compile additional gated stdlib modules behind their
  gates. The release channel is never blocked by an unstable gate.

## 7. LSP and formatter

- **LSP.** Per [ADR 0014](../adr/0014-lsp-runs-the-compiler.md) the server holds
  only document text and calls the same analysis library as the CLI. It passes
  the same crate-root feature set and channel into `analyze_source`. A gated
  construct without its gate therefore produces the same `E0154`, at the same
  span, as `umay check`. Because parsing precedes gate enforcement, hover and
  completion still work on gated code. Only the crate root identified for a
  buffer's path contributes gates; a `feature` attribute in another open file
  does not.
- **Formatter.** `umay fmt` is feature-agnostic: it formats whatever the current
  parser accepts, independent of channel and gates. It neither rejects nor
  reflows gated syntax because of a gate. Formatting output must be byte-identical
  before and after a feature is stabilized, covered by a golden test.

## 8. v1 programs

A v1 program uses no `#![feature(...)]` attribute and no gated syntax. It builds
on the `stable` channel with no flags, and no gate diagnostic can apply. The v1
corpus CI in [backward-compatibility.md](backward-compatibility.md) enforces
this invariant.
