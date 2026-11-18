//! Phase 18: variadic extern calls (v2 spec §5, ffi-spike §3.3).
//!
//! Trailing integer/pointer arguments are accepted; a floating-point one is
//! E0157 because the backend's unconditional `%al = 0` would miscompile it.

use umayc::diagnostics::Diagnostics;
use umayc::lexer;
use umayc::parser;
use umayc::sema;
use umayc::source::SourceMap;

fn analyze(src: &str) -> Diagnostics {
    let mut map = SourceMap::new();
    let id = map.add("<test>", src);
    let (tokens, lex_errors) = lexer::tokenize(src, id);
    assert!(lex_errors.is_empty(), "test source failed to lex: {lex_errors:?}\n---\n{src}");
    let parsed = parser::parse(tokens, Vec::new(), id);
    let parse_errors: Vec<_> = parsed
        .diagnostics
        .iter()
        .filter(|d| d.severity.is_fatal())
        .map(|d| format!("{}: {}", d.code, d.message))
        .collect();
    assert!(parse_errors.is_empty(), "test source failed to parse: {parse_errors:?}\n---\n{src}");
    sema::analyze(&parsed.program, &map).diagnostics
}

fn error_codes(src: &str) -> Vec<&'static str> {
    analyze(src)
        .items()
        .iter()
        .filter(|d| d.severity.is_fatal())
        .map(|d| d.code)
        .collect()
}

#[test]
fn variadic_call_with_only_integer_trailing_args_is_accepted() {
    let codes = error_codes(
        r#"
extern "C" {
    fn log(tag: i32, ...) -> i32;
}

fn main() {
    let r = log(1, 2, 3)
    let _ = r
}
"#,
    );
    assert!(codes.is_empty(), "expected no errors, got {codes:?}");
}

#[test]
fn variadic_call_with_float_trailing_arg_reports_e0157() {
    let codes = error_codes(
        r#"
extern "C" {
    fn log(tag: i32, ...) -> i32;
}

fn main() {
    let x: f64 = 1.5
    let r = log(1, x)
    let _ = r
}
"#,
    );
    assert!(codes.contains(&"E0157"), "expected E0157, got {codes:?}");
}

#[test]
fn non_variadic_extern_call_with_float_arg_does_not_report_e0157() {
    let codes = error_codes(
        r#"
extern "C" {
    fn fixed(x: f64) -> i32;
}

fn main() {
    let x: f64 = 1.5
    let r = fixed(x)
    let _ = r
}
"#,
    );
    assert!(codes.is_empty(), "expected no errors, got {codes:?}");
    assert!(!codes.contains(&"E0157"), "non-variadic call must not report E0157: {codes:?}");
}

#[test]
fn every_float_trailing_arg_is_reported() {
    let codes = error_codes(
        r#"
extern "C" {
    fn log(tag: i32, ...) -> i32;
}

fn main() {
    let x: f32 = 1.5
    let y: f64 = 2.5
    let r = log(1, x, 7, y)
    let _ = r
}
"#,
    );
    assert_eq!(
        codes.iter().filter(|c| **c == "E0157").count(),
        2,
        "both floats must be diagnosed, got {codes:?}"
    );
}
