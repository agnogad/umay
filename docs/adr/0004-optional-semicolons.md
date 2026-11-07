# ADR 0004 — Optional semicolons with newline-aware termination

**Status:** accepted
**Date:** Phase 1

## Context

UMAY's syntax goal is minimal punctuation. Semicolon-free syntax is pleasant but
famously ambiguous: is the next line a new statement or a continuation?

## Decision

Statements terminate at a line break unless continuation is required or
intended. Termination is decided by three rules:

1. If the previous token cannot end an expression (an operator, `.`, `(`, `[`,
   `,`, `=`), the expression continues across the newline.
2. If the next token can only continue an expression (a postfix or infix
   operator, `.`, `?`), it also continues.
3. Otherwise the statement ends. Tokens that unambiguously start a statement
   (`let`, `return`, `if`, `match`, `fn`, ...) always begin a new one.

The lexer records a 1-based line number for each token in one forward pass; it
does not emit newline tokens.

## Consequences

- No newline tokens to filter, so every consumer sees a clean stream.
- `expr` followed by `(` on the next line is a **call**: rule 2 wins. This is the
  one place where the rule set requires care, and it is documented here and in
  the specification.
- Semicolons remain legal anywhere and suppress a block's tail value.
- Return and break payloads use the same predicate, so `return` on its own line
  returns unit.

## Alternatives considered

- **Mandatory semicolons**: simpler but contradicts the syntax goals.
- **Explicit newline tokens consumed by the parser**: workable, but leaks
  formatting into the token stream and complicates every match.
- **Go-style automatic semicolon insertion in the lexer**: rejected because it
  makes continuation rules invisible and produces confusing errors.
