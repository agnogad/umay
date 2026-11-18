//! Phase 19: syntax/HIR layer for v2 async.
//!
//! `async` and `await` are contextual keywords: they are only special in the
//! exact positions `async fn` and postfix `.await`. Everywhere else they remain
//! ordinary v1 identifiers. These tests pin that down at the AST level, using
//! the same parse/check harnesses as phase1/phase2.

use umayc::ast::{Expr, ExprKind, FnDecl, Item, PatternKind, Program, StmtKind};
use umayc::diagnostics::Severity;
use umayc::lexer;
use umayc::parser;
use umayc::sema;
use umayc::source::SourceMap;
use umayc::{parse_source, render_all};

/// Parses `src` and asserts the parse produced no diagnostics.
fn parse_clean(src: &str) -> Program {
    let (out, map) = parse_source("test.um", src);
    let rendered = render_all(&out.diagnostics, &map, false);
    assert!(out.diagnostics.items().is_empty(), "unexpected parse diagnostics:\n{rendered}");
    out.program
}

/// Finds a top-level function by name.
fn find_fn<'a>(program: &'a Program, name: &str) -> &'a FnDecl {
    program
        .items
        .iter()
        .find_map(|item| match item {
            Item::Fn(f) if f.name == name => Some(f),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no function named `{name}`"))
}

/// The one statement expression in a function body.
fn body_expr(f: &FnDecl) -> &Expr {
    let body = f.body.as_ref().expect("function has a body");
    if body.stmts.is_empty() {
        return body.tail.as_deref().expect("no expression in body");
    }
    assert_eq!(body.stmts.len(), 1, "expected exactly one statement");
    match &body.stmts[0].kind {
        StmtKind::Expr(e) => e,
        other => panic!("expected an expression statement, got {other:?}"),
    }
}

/// Runs parsing plus semantic analysis and asserts no fatal diagnostics.
fn check_ok(src: &str) {
    let mut map = SourceMap::new();
    let id = map.add("<test>", src);
    let (tokens, lex_errors) = lexer::tokenize(src, id);
    assert!(lex_errors.is_empty(), "lex errors: {lex_errors:?}");
    let parsed = parser::parse(tokens, Vec::new(), id);
    let parse_errors: Vec<_> = parsed
        .diagnostics
        .iter()
        .filter(|d| d.severity.is_fatal())
        .map(|d| format!("{}: {}", d.code, d.message))
        .collect();
    assert!(parse_errors.is_empty(), "parse errors: {parse_errors:?}");
    let out = sema::analyze(&parsed.program, &map);
    let errors: Vec<_> = out
        .diagnostics
        .items()
        .iter()
        .filter(|d| d.severity == Severity::Error)
        .map(|d| d.code)
        .collect();
    assert!(errors.is_empty(), "unexpected sema errors: {errors:?}");
}

#[test]
fn async_fn_parses_and_checks() {
    let src = "async fn f() -> i64 { return 1 }";
    let program = parse_clean(src);
    let f = find_fn(&program, "f");
    assert!(f.is_async, "`async fn` must set FnDecl::is_async");
    check_ok(src);
}

#[test]
fn plain_fn_is_not_async() {
    let program = parse_clean("fn f() {}");
    assert!(!find_fn(&program, "f").is_async);
}

#[test]
fn postfix_await_parses_as_await_expr() {
    let program = parse_clean("fn f(x: i32) { x.await }");
    let e = body_expr(find_fn(&program, "f"));
    match &e.kind {
        ExprKind::Await { base, .. } => {
            assert!(matches!(base.kind, ExprKind::Ident(ref name) if name == "x"));
        }
        other => panic!("expected ExprKind::Await, got {other:?}"),
    }
}

#[test]
fn dot_await_with_parens_is_still_a_method_call() {
    let program = parse_clean("fn f(x: i32) { x.await() }");
    let e = body_expr(find_fn(&program, "f"));
    match &e.kind {
        ExprKind::MethodCall { method, .. } => assert_eq!(method, "await"),
        other => panic!("expected ExprKind::MethodCall, got {other:?}"),
    }
}

#[test]
fn await_is_still_a_valid_identifier() {
    let program = parse_clean("fn await() { let async = 1 }");
    let f = find_fn(&program, "await");
    assert!(!f.is_async);
    let body = f.body.as_ref().unwrap();
    match &body.stmts[0].kind {
        StmtKind::Let(l) => match &l.pattern.kind {
            PatternKind::Binding { name, .. } => assert_eq!(name, "async"),
            other => panic!("expected a binding pattern, got {other:?}"),
        },
        other => panic!("expected a let statement, got {other:?}"),
    }
}

#[test]
fn standalone_async_identifier_is_not_a_modifier() {
    let program = parse_clean("fn f() -> i32 { let async = 1; return async }");
    assert!(!find_fn(&program, "f").is_async);
}

#[test]
fn truncated_async_input_does_not_panic() {
    for src in ["async", "async fn", "async fn f", "fn f() { x.", "fn f() { x.await"] {
        let _ = parse_source("test.um", src);
    }
}
