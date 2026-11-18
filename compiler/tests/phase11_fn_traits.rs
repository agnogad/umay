//! Phase 11: the `Fn` family of call traits.
//!
//! The traits themselves are plain UMAY trait declarations in `stdlib/fn.um`,
//! pulled into every program through the prelude. The compiler synthesizes
//! implementations for closures (wired up separately); this file covers the
//! declaration side and the registration API's footprint on method resolution.
//!
//! The first three tests use the phase2 in-process harness (lex -> parse ->
//! `sema::analyze`, no prelude), so the trait declarations are written in the
//! test source. The last two go through `analyze_source`, which prepends the
//! real prelude, to prove `stdlib/fn.um` is actually compiled into programs.

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

fn prelude_error_codes(src: &str) -> Vec<&'static str> {
    let (diagnostics, map) = umayc::analyze_source("t.um", src);
    if diagnostics.has_errors() {
        eprintln!("{}", umayc::render_all_full(&diagnostics, &map, false, false));
    }
    diagnostics
        .items()
        .iter()
        .filter(|d| d.severity.is_fatal())
        .map(|d| d.code)
        .collect()
}

// The three declarations, spelled exactly as in `stdlib/fn.um`.
const FN_TRAITS: &str = "
trait Fn<A, R> { fn call(self, a: A) -> R }
trait FnMut<A, R> { fn call_mut(self, a: A) -> R }
trait FnOnce<A, R> { fn call_once(self, a: A) -> R }
";

#[test]
fn fn_trait_declarations_parse_and_resolve() {
    // Plain generic syntax: the parser has no `Fn(A) -> B` sugar yet.
    ok(FN_TRAITS);
}

#[test]
fn a_bound_naming_each_call_trait_resolves() {
    // A bound only resolves if the trait name is declared. If any of the three
    // declarations were missing or malformed, an unknown-trait error would
    // surface here.
    ok(&format!(
        "{FN_TRAITS}
        fn by_fn<F: Fn<i64, i64>>(f: F, x: i64) -> i64 {{ return x }}
        fn by_mut<F: FnMut<i64, i64>>(f: F, x: i64) -> i64 {{ return x }}
        fn by_once<F: FnOnce<i64, i64>>(f: F, x: i64) -> i64 {{ return x }}
        "
    ));
}

#[test]
fn user_written_fn_impl_method_call_resolves() {
    // A struct can implement `Fn` with an ordinary method body. The call goes
    // through the same `methods` map a user-written `impl` always uses, so this
    // also covers the registration path `register_synthetic_impl` reuses.
    ok(&format!(
        "{FN_TRAITS}
        struct Adder {{ base: i64 }}
        impl Fn<i64, i64> for Adder {{
            fn call(self, a: i64) -> i64 {{ return self.base + a }}
        }}
        fn main() {{
            let adder = Adder {{ base: 10i64 }}
            let sum = adder.call(5i64)
            let _ = sum
        }}
        "
    ));
}

#[test]
fn fn_traits_are_in_the_prelude() {
    // `analyze_source` prepends the real prelude. A parse error in `stdlib/fn.um`
    // or a missing `include_str!` would make this (and every other compiled
    // program) fail.
    let codes = prelude_error_codes("fn main() {}");
    assert!(codes.is_empty(), "prelude failed to compile: {codes:?}");
}

#[test]
fn a_call_trait_bound_in_a_real_program_has_no_unknown_trait_error() {
    // The prelude declares `Fn`, so the bound resolves and the trait method call
    // on a generic parameter is recorded against it. Bound checking stays
    // structural: a trait with methods is not required to have an `impl`.
    let codes = prelude_error_codes(
        "fn apply<F: Fn<i64, i64>>(f: F, x: i64) -> i64 { return f.call(x) }\n\
         fn main() {}",
    );
    assert!(codes.is_empty(), "expected no errors, got {codes:?}");
    // Skipped: asserting `register_synthetic_impl` directly is impossible from
    // an integration test (it is `pub(crate)` and needs a live `Checker`). Its
    // effect is covered by the closure registration the wiring agent adds.
}
