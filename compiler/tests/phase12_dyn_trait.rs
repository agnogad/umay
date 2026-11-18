//! Phase 2 foundation: `dyn Trait` resolution and object-safety diagnostics.
//!
//! Uses the same in-process harness as `phase2_sema.rs`.
//!
//! The `ok`/auto-trait tests assume `resolve_dyn_trait` builds `TraitBound`s
//! (including generic args and associated bindings) before interning.

use umayc::diagnostics::Diagnostics;
use umayc::lexer;
use umayc::parser;
use umayc::sema;
use umayc::source::SourceMap;

struct Checked {
    diagnostics: Diagnostics,
}

fn analyze(src: &str) -> Checked {
    let mut map = SourceMap::new();
    let id = map.add("<test>", src);
    let (tokens, lex_errors) = lexer::tokenize(src, id);
    assert!(lex_errors.is_empty(), "test source failed to lex: {:?}\n---\n{src}", lex_errors);
    let parsed = parser::parse(tokens, Vec::new(), id);
    let parse_errors: Vec<_> = parsed
        .diagnostics
        .iter()
        .filter(|d| d.severity.is_fatal())
        .map(|d| format!("{}: {}", d.code, d.message))
        .collect();
    assert!(parse_errors.is_empty(), "test source failed to parse: {parse_errors:?}\n---\n{src}");
    let out = sema::analyze(&parsed.program, &map);
    Checked { diagnostics: out.diagnostics }
}

fn error_codes(src: &str) -> Vec<&'static str> {
    analyze(src)
        .diagnostics
        .items()
        .iter()
        .filter(|d| d.severity.is_fatal())
        .map(|d| d.code)
        .collect()
}

#[track_caller]
fn ok(src: &str) {
    let codes = error_codes(src);
    assert!(codes.is_empty(), "expected no errors, got {codes:?}\n---\n{src}");
}

#[track_caller]
fn err(src: &str, code: &str) {
    let codes = error_codes(src);
    assert!(codes.contains(&code), "expected error {code}, got {codes:?}\n---\n{src}");
}

// ---------------------------------------------------------------------------
// Accepted programs
// ---------------------------------------------------------------------------

#[test]
fn dyn_trait_binding_is_accepted() {
    ok("
trait Shape {
    fn area(&self) -> i64
}

fn draw(s: &dyn Shape) { }
");
}

#[test]
fn dyn_with_generic_args_reaches_sema() {
    ok("
trait Iterator {
    fn next(&self) -> i64
}

fn f(it: &dyn Iterator<i64>) { }
");
}

#[test]
fn auto_trait_does_not_count_as_second_trait() {
    ok("
trait Shape {
    fn area(&self) -> i64
}

fn draw(s: &dyn Shape + Send) { }
");
}

// ---------------------------------------------------------------------------
// E1022 — unknown trait
// ---------------------------------------------------------------------------

#[test]
fn unknown_dyn_trait_reports_e1022() {
    err("
fn f(s: &dyn NoSuchTrait) { }
", "E1022");
}

// ---------------------------------------------------------------------------
// E1023 — object-safety violations
// ---------------------------------------------------------------------------

#[test]
fn generic_method_reports_e1023() {
    err("
trait Generic {
    fn get<T>(&self, x: T) -> T
}

fn f(s: &dyn Generic) { }
", "E1023");
}

#[test]
fn self_return_reports_e1023() {
    err("
trait Cloner {
    fn duplicate(&self) -> Self
}

fn f(s: &dyn Cloner) { }
", "E1023");
}

#[test]
fn object_safe_trait_has_no_e1023() {
    let codes = error_codes("
trait Shape {
    fn area(&self) -> i64
}

fn draw(s: &dyn Shape) { }
");
    assert!(!codes.contains(&"E1023"), "unexpected E1023: {codes:?}");
}

// ---------------------------------------------------------------------------
// E1024 — more than one principal trait
// ---------------------------------------------------------------------------

#[test]
fn two_traits_reports_e1024() {
    err("
trait A { fn a(&self) }
trait B { fn b(&self) }

fn f(s: &dyn A + B) { }
", "E1024");
}

// ---------------------------------------------------------------------------
// Generic arguments and associated-type bindings survive into the type
// ---------------------------------------------------------------------------

use umayc::ast;
use umayc::hir::HirFn;
use umayc::types::{TyCtxt, TyId, TyKind};

/// Collect the display text of every `dyn …` reachable from a type.
fn dyn_texts(cx: &TyCtxt, ty: TyId, out: &mut Vec<String>) {
    match cx.kind(ty) {
        TyKind::DynTrait { .. } => out.push(cx.display(ty)),
        TyKind::Ref(inner)
        | TyKind::RefMut(inner)
        | TyKind::RawPtr(inner)
        | TyKind::RawPtrMut(inner)
        | TyKind::Slice(inner) => dyn_texts(cx, *inner, out),
        TyKind::Array { elem, .. } => dyn_texts(cx, *elem, out),
        TyKind::Tuple(items) => {
            for &item in items {
                dyn_texts(cx, item, out);
            }
        }
        TyKind::FnPtr { params, ret } => {
            for &p in params {
                dyn_texts(cx, p, out);
            }
            dyn_texts(cx, *ret, out);
        }
        _ => {}
    }
}

fn dyn_types(src: &str) -> Vec<String> {
    let mut map = SourceMap::new();
    let id = map.add("<test>", src);
    let (tokens, _) = lexer::tokenize(src, id);
    let parsed = parser::parse(tokens, Vec::new(), id);
    let out = sema::analyze(&parsed.program, &map);
    let mut found = Vec::new();
    let scan = |f: &HirFn, found: &mut Vec<String>| {
        for &p in &f.params {
            dyn_texts(&out.types, p, found);
        }
    };
    for f in &out.hir.fns {
        scan(f, &mut found);
    }
    found
}

#[test]
fn generic_argument_survives_into_dyn_display() {
    let texts = dyn_types("
trait Iterator { fn next(&self) -> i64 }
fn f(it: &dyn Iterator<i64>) { }
");
    assert!(
        texts.iter().any(|t| t.contains("i64")),
        "i64 was dropped from dyn display: {texts:?}"
    );
}

#[test]
fn assoc_binding_parses_and_renders_in_path() {
    let src = "fn f(x: Iterator<i64, Item = u8>) { }";
    let mut map = SourceMap::new();
    let id = map.add("<test>", src);
    let (tokens, lex_errors) = lexer::tokenize(src, id);
    assert!(lex_errors.is_empty(), "lex errors: {lex_errors:?}");
    let parsed = parser::parse(tokens, Vec::new(), id);
    let fatal: Vec<_> = parsed
        .diagnostics
        .iter()
        .filter(|d| d.severity.is_fatal())
        .collect();
    assert!(fatal.is_empty(), "parse errors: {fatal:?}");
    let ast::Item::Fn(f) = &parsed.program.items[0] else {
        panic!("expected a function item");
    };
    let ast::TypeExpr::Path(p) = &f.params[0].ty else {
        panic!("expected a path type");
    };
    assert_eq!(p.as_string(), "Iterator<i64, Item = u8>");
    let seg = &p.segments[0];
    assert_eq!(seg.args.len(), 1);
    assert_eq!(seg.bindings.len(), 1);
    assert_eq!(seg.bindings[0].name, "Item");
}

#[test]
fn assoc_binding_in_let_annotation_type_checks() {
    ok("
trait Iterator { fn next(&self) -> i64 }

fn f(it: &dyn Iterator<Item = u8>) {
    let x: &dyn Iterator<Item = u8> = it;
}
");
    let texts = dyn_types("
trait Iterator { fn next(&self) -> i64 }
fn f(it: &dyn Iterator<Item = u8>) { }
");
    assert!(
        texts.iter().any(|t| t.contains("Item = u8")),
        "assoc binding was dropped from dyn display: {texts:?}"
    );
}

#[test]
fn dyn_types_never_emit_spurious_e1003() {
    let codes = error_codes("
trait Iterator { fn next(&self) -> i64 }
fn f(it: &dyn Iterator<i64, Item = u8>) { }
");
    assert!(!codes.contains(&"E1003"), "unexpected E1003: {codes:?}");
}
