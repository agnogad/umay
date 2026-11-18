//! Phase 8: declarative (`macro_rules!`-style) macro expansion.
//!
//! Two harnesses, mirroring `phase2_sema.rs` / `phase15_impl_trait.rs`: an
//! in-process frontend harness for expansion/error-code assertions, and a CLI
//! build+run harness proving a macro-using program actually compiles and runs.
//!
//! The engine itself lives in `compiler/src/macro.rs` and is wired into
//! `lib.rs::parse_file`'s lex->parse gap, so every frontend entry point (and
//! therefore the CLI) sees expanded source.

use umayc::lexer;
use umayc::parser;
use umayc::source::FileId;

fn compiler() -> &'static str {
    env!("CARGO_BIN_EXE_umay")
}

fn toolchain_available() -> bool {
    if cfg!(windows) {
        return false;
    }
    let ok = |name: &str| {
        std::process::Command::new(name)
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    };
    ok("as") && ok("cc")
}

/// Private scratch directory per test so parallel runs cannot clobber each
/// other, then `umay build` + run and collect stdout.
fn run_stdout(name: &str, source: &str) -> Option<String> {
    if !toolchain_available() {
        eprintln!("skipping {name}: no `as` or `cc`");
        return None;
    }
    let dir = std::env::temp_dir().join(format!("umay_phase23_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch");
    let file = dir.join("main.um");
    std::fs::write(&file, source).expect("write source");
    let build = std::process::Command::new(compiler())
        .arg("build")
        .arg(&file)
        .current_dir(&dir)
        .output()
        .expect("build");
    assert!(
        build.status.success(),
        "build failed:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let out = std::process::Command::new(dir.join("dist/main")).output().expect("run");
    assert!(out.status.success(), "run failed: {:?}", out.status);
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

#[track_caller]
fn expect_stdout(name: &str, source: &str, expected: &str) {
    if let Some(out) = run_stdout(name, source) {
        assert_eq!(out.trim_end(), expected, "program output mismatch");
    }
}

/// Unwrap an expected successful expansion. `Result::expect` cannot be used
/// because `SourceMap` (the error half) does not implement `Debug`.
fn expanded(name: &str, src: &str) -> umayc::ExpansionOutput {
    match umayc::expand_source(name, src) {
        Ok(out) => out,
        Err((diags, _)) => panic!("unexpected expansion error: {diags:?}"),
    }
}

/// Error codes produced by `expand_source`; an expansion error is a `Result::Err`.
fn expand_error_codes(src: &str) -> Vec<&'static str> {
    match umayc::expand_source("<test>", src) {
        Ok(_) => Vec::new(),
        Err((diags, _map)) => diags
            .items()
            .iter()
            .filter(|d| d.severity.is_fatal())
            .map(|d| d.code)
            .collect(),
    }
}

const ANSWER: &str = "macro_rules! answer { () => { 42 } }\nfn main() -> i32 {\n println_i64(answer!())\n return 0\n}\n";
const TWICE: &str = "macro_rules! twice { ($x:ident) => { $x + $x } }\nfn main() -> i32 {\n let a = 21\n let b = twice!(a)\n println_i64(b)\n return 0\n}\n";
const PLAIN: &str = "fn main() -> i32 {\n let x = 21\n println_i64(x + x)\n return 0\n}\n";

// ---- (a) zero-argument macro ------------------------------------------------

#[test]
fn a_zero_arg_macro_builds_and_runs() {
    expect_stdout("answer", ANSWER, "42");
}

#[test]
fn a_zero_arg_macro_is_removed_and_replaced() {
    let out = expanded("<test>", ANSWER);
    assert!(!out.expanded_text.contains("macro_rules"), "{}", out.expanded_text);
    assert!(out.expanded_text.contains("42"), "{}", out.expanded_text);
}

// ---- (b) ident-binding macro ------------------------------------------------

#[test]
fn an_ident_binding_macro_expands_in_a_let() {
    expect_stdout("twice", TWICE, "42");
}

#[test]
fn ident_binding_substitutes_the_capture() {
    let out = expanded("<test>", TWICE);
    assert!(out.expanded_text.contains("a + a"), "{}", out.expanded_text);
    assert!(!out.expanded_text.contains("$x"), "{}", out.expanded_text);
}

// ---- (c) malformed definitions/invocations ----------------------------------

#[test]
fn truncated_definition_is_e0125_without_panic() {
    let codes = expand_error_codes("macro_rules! broken { () => { 42");
    assert!(codes.contains(&"E0125"), "got {codes:?}");
}

#[test]
fn malformed_invocation_is_e0126_without_panic() {
    let codes = expand_error_codes("macro_rules! a { () => { 1 } }\nfn main() { let _ = a!(");
    assert!(codes.contains(&"E0126"), "got {codes:?}");
}

#[test]
fn arity_mismatch_is_e0127() {
    let codes = expand_error_codes("macro_rules! a { () => { 1 } }\nfn main() { let _ = a!(9) }");
    assert!(codes.contains(&"E0127"), "got {codes:?}");
}

#[test]
fn unbound_metavariable_is_e0128() {
    let codes = expand_error_codes("macro_rules! a { ($x:ident) => { $y } }\nfn main() { let _ = a!(q) }");
    assert!(codes.contains(&"E0128"), "got {codes:?}");
}

// ---- (d) no-macro regression ------------------------------------------------

#[test]
fn a_file_without_macros_round_trips_byte_for_byte() {
    let out = expanded("<plain>", PLAIN);
    assert_eq!(out.expanded_text, PLAIN);
    assert!(out.span_map.is_empty());
}

#[test]
fn a_file_without_macros_has_the_same_ast_as_before() {
    // `parse_source` goes through the now-expansion-aware `parse_file`; a
    // direct lex+parse is the pre-macro behaviour. They must be identical.
    let (front, map) = umayc::parse_source("<plain>", PLAIN);
    let id = FileId(0);
    let (tokens, lex_errors) = lexer::tokenize(PLAIN, id);
    let direct = parser::parse(tokens, lex_errors, id);
    assert!(front.diagnostics.is_empty(), "{:?}", front.diagnostics);
    assert_eq!(
        umayc::ast_json(&front.program, &map),
        umayc::ast_json(&direct.program, &map),
        "the non-macro path changed the AST"
    );
}

#[test]
fn a_file_without_macros_still_builds_and_runs() {
    expect_stdout("plain", PLAIN, "42");
}

// ---- (e) expansion report ---------------------------------------------------

#[test]
fn expand_source_returns_text_and_a_non_empty_span_map() {
    let out = expanded("<expanded>", TWICE);
    assert!(!out.span_map.is_empty(), "expected generated -> invocation spans");
    for (generated, invocation) in &out.span_map {
        assert_ne!(generated.file, invocation.file, "generated and invocation files must differ");
        assert!(out.map.get(generated.file).is_some(), "generated span must resolve in the map");
        assert_eq!(out.map.file(invocation.file).name(), "<expanded>");
    }
    assert!(!out.diagnostics.has_errors());
}
