# Contributing to UMAY

Thank you for working on UMAY. This document describes the rules that keep the
project trustworthy.

## Non-negotiable rules

1. **Read before writing.** Read the relevant specification in `docs/` and the
   existing code in `compiler/src/` before changing behaviour.
2. **No untested features.** A feature is not implemented until tests verify it.
3. **No placeholder code presented as complete.** A stub must be visibly a stub:
   it returns "not implemented", not a plausible-looking wrong answer.
4. **No weakening of memory safety** to make an implementation easier.
5. **No `target/` directories in UMAY projects.** Compiler caches go to the OS
   user cache directory.
6. **No LLVM or Cranelift** in the production backend.
7. **Every fixed bug gets a regression test.**
8. **Every new diagnostic code gets an `umay explain` entry.** A test enforces
   this.
9. **Update `ROADMAP.md`** when a milestone advances.

## Workflow

```bash
cargo build
cargo test
cargo clippy --all-targets
```

All three must be clean before a change is proposed.

### Adding a language feature

1. Update `docs/language-specification.md` (syntax) or the relevant design doc
   (semantics) **first**. The spec is the contract.
2. Add the AST nodes and parser support.
3. Add accept tests (valid programs) and reject tests (each diagnostic code it
   can emit).
4. If the feature is semantic, add the diagnostics and explain entries.
5. Record any long-term architectural consequence as an ADR in `docs/adr/`.

### Adding a diagnostic

1. Reserve the code in the appropriate range (see `docs/diagnostics.md`).
2. Add an entry to `compiler/src/explain.rs` with a summary, an explanation and
   ideally an example.
3. Emit it with a `cause`. If the phase does not supply one, the frontend
   attaches the explain entry's summary automatically.
4. Add a test asserting the code appears for the triggering program.
5. Only add a `Suggestion` if the replacement is verified type-correct and
   memory-safe. Speculative fixes are forbidden.

### Parser changes

The parser must maintain two invariants, both covered by tests:

- it never panics on any truncated prefix of a valid program,
- it never panics on mutated input.

Error recovery must always consume at least one token. Never emit two
diagnostics for the same byte range.

## Code style

- Rust edition 2021, no `unsafe` in the compiler crate.
- Keep modules focused; prefer clear code over clever abstractions.
- Comments explain *why*, not *what*.
- Files should stay reviewable; split large modules rather than growing one.

## Testing expectations by area

| Area | Required |
|------|----------|
| Lexer | a unit test per token class, plus malformed-literal tests |
| Parser | accept test per construct, reject test per error, recovery test |
| Diagnostics | render test per code, JSON stability test |
| Memory safety | an adversarial fixture per violation kind |
| Codegen | executable test asserting output and exit code |

## Reporting a bug

Include: the minimal triggering source, the command run, the actual output and
the expected output. If the compiler panics, that is always a bug worth fixing
regardless of the input's validity.

## Architecture Decision Records

Changes with long-term consequences (representation choices, ownership model
details, backend strategy, caching) require an ADR. Keep ADRs short: context,
decision, consequences, alternatives considered.

## Commit messages

Use Conventional Commits, compressed to intent:

```text
feat(parser): support compound assignment operators
fix(lexer): keep `<<=` as a single token
docs(spec): freeze v0.1 expression precedence
test(borrowck): add dangling reference fixtures
```
