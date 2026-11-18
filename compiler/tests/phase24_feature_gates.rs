//! Phase 24: opt-in enforcement of crate-level `#![feature(...)]` gates.
//!
//! In-process harness in the style of `phase2_sema.rs`: lex, parse, then run the
//! gate pass directly. Sema is deliberately not involved — these tests are
//! about syntax hidden behind gates, not about type correctness.

#![cfg(feature = "v2-gates")]

use umayc::{features, lexer, parser, source::SourceMap};

fn gate_codes(src: &str) -> Vec<(&'static str, String)> {
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
    features::check(&parsed.program)
        .items()
        .iter()
        .map(|d| (d.code, d.message.clone()))
        .collect()
}

fn codes(src: &str) -> Vec<&'static str> {
    gate_codes(src).into_iter().map(|(code, _)| code).collect()
}

#[test]
fn closure_without_gate_is_e0154() {
    let found = codes("fn main() { let f = |x: i64| x + 1 }");
    assert!(found.contains(&"E0154"), "expected E0154, got {found:?}");
}

#[test]
fn closure_with_gate_is_clean() {
    let found = codes("#![feature(closures)]\nfn main() { let f: fn(i64) -> i64 = |x| x + 1 }");
    assert!(found.is_empty(), "expected no gate diagnostics, got {found:?}");
}

#[test]
fn dyn_trait_without_gate_is_e0154() {
    let found = codes("trait T { fn f(&self) }\nfn takes(x: &dyn T) { }");
    assert!(found.contains(&"E0154"), "expected E0154, got {found:?}");
}

#[test]
fn dyn_trait_with_gate_is_clean() {
    let found = codes("#![feature(dyn_trait)]\ntrait T { fn f(&self) }\nfn takes(x: &dyn T) { }");
    assert!(found.is_empty(), "expected no gate diagnostics, got {found:?}");
}

#[test]
fn unknown_feature_name_is_e0155() {
    let found = codes("#![feature(nope)]\nfn main() { }");
    assert!(found.contains(&"E0155"), "expected E0155, got {found:?}");
}

#[test]
fn duplicate_feature_name_is_e0155() {
    let found = codes("#![feature(closures, closures)]\nfn main() { }");
    assert!(found.contains(&"E0155"), "expected E0155, got {found:?}");
}

#[test]
fn reserved_gui_is_e0156() {
    let found = codes("#![feature(gui)]\nfn main() { }");
    assert!(found.contains(&"E0156"), "expected E0156, got {found:?}");
}

#[test]
fn v1_program_without_features_stays_clean() {
    let found = codes("fn add(a: i64, b: i64) -> i64 { a + b }\nfn main() { let n = add(1, 2) }");
    assert!(found.is_empty(), "expected no gate diagnostics, got {found:?}");
}

#[test]
fn v1_extern_fn_needs_no_extern_abi_gate() {
    let found = codes("extern fn puts(s: *const u8) -> i32\nfn main() { }");
    assert!(found.is_empty(), "expected no gate diagnostics, got {found:?}");
}

#[test]
fn gated_syntax_beyond_closures_is_detected() {
    assert!(codes("async fn f() { }").contains(&"E0154"));
    assert!(codes("fn main() { let x = f().await }").contains(&"E0154"));
    assert!(codes("trait T { fn f(&self) { } }").contains(&"E0154"));
    assert!(codes("extern \"C\" { fn f() }").contains(&"E0154"));
}

#[test]
fn compiler_hooks_enforce_gates() {
    let gated = "#![feature(closures)]\nfn main() { let f: fn(i64) -> i64 = |x| x + 1 }";
    let ungated = "fn main() { let f: fn(i64) -> i64 = |x| x + 1 }";
    let (diags, _) = umayc::analyze_source("t.um", ungated);
    assert!(
        diags.items().iter().any(|d| d.code == "E0154"),
        "analyze_source missed the gate error: {diags:?}"
    );
    let (diags, _) = umayc::analyze_source("t.um", gated);
    assert!(
        !diags.items().iter().any(|d| d.code == "E0154" || d.code == "E0155"),
        "analyze_source rejected a gated program: {diags:?}"
    );
    let result = umayc::codegen_source("t.um", ungated, umayc::opt::OptLevel::O0);
    let Err((diags, _)) = result else {
        panic!("codegen must refuse ungated syntax");
    };
    assert!(
        diags.items().iter().any(|d| d.code == "E0154"),
        "codegen error was not a gate error: {diags:?}"
    );
}

#[test]
fn every_diagnostic_has_an_explanation() {
    for src in [
        "fn main() { let f = |x| x }",
        "#![feature(nope)]\nfn main() { }",
        "#![feature(gui)]\nfn main() { }",
        "#![feature(closures, closures)]\nfn main() { }",
    ] {
        let mut map = SourceMap::new();
        let id = map.add("<test>", src);
        let (tokens, _) = lexer::tokenize(src, id);
        let parsed = parser::parse(tokens, Vec::new(), id);
        for d in features::check(&parsed.program).items() {
            assert!(
                umayc::explain::lookup(d.code).is_some(),
                "{} has no explain entry",
                d.code
            );
        }
    }
}
