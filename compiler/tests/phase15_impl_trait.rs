//! Phase 15: return-position `impl Trait` (body-inferred hidden type).
//!
//! Uses the same two harnesses as phase14_dyn_dispatch.rs: the in-process
//! semantic-analysis harness for accept/reject checks, and the CLI build+run
//! harness to prove the concrete hidden type flows all the way to output.

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
    assert!(lex_errors.is_empty(), "lex errors: {lex_errors:?}");
    let parsed = parser::parse(tokens, Vec::new(), id);
    let parse_errors: Vec<_> = parsed
        .diagnostics
        .iter()
        .filter(|d| d.severity.is_fatal())
        .map(|d| format!("{}: {}", d.code, d.message))
        .collect();
    assert!(parse_errors.is_empty(), "parse errors: {parse_errors:?}\n---\n{src}");
    Checked { diagnostics: sema::analyze(&parsed.program, &map).diagnostics }
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
    assert!(codes.contains(&code), "expected {code}, got {codes:?}\n---\n{src}");
}

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

fn run_stdout(name: &str, source: &str) -> Option<String> {
    if !toolchain_available() {
        eprintln!("skipping {name}: no `as` or `cc`");
        return None;
    }
    let dir = std::env::temp_dir().join(format!("umay_phase15_{name}_{}", std::process::id()));
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

fn expect_stdout(name: &str, source: &str, expected: &str) {
    if let Some(out) = run_stdout(name, source) {
        assert_eq!(out.trim_end(), expected, "program output mismatch");
    }
}

const BASE: &str = "
trait Marker {}
struct S { value: i64 }
impl Marker for S {}
impl S { fn get(&self) -> i64 { self.value } }
";

#[test]
fn opaque_return_is_accepted() {
    ok(&format!(
        "{BASE}\nfn make() -> impl Marker {{ S {{ value: 1 }} }}\nfn main() {{ let v = make()\n let _ = v.get() }}"
    ));
}

#[test]
fn arg_position_impl_trait_is_e1003() {
    // Return position is the only accepted position; argument position stays a
    // hard error.
    err("fn f(x: impl Marker) {}", "E1003");
}

#[test]
fn opaque_hidden_type_missing_marker_is_e1017() {
    // Marker traits are the only bounds enforced on the hidden type.
    let src = format!(
        "{BASE}\nstruct Other {{}}\nfn bad() -> impl Marker {{ Other {{}} }}\nfn main() {{ let _ = bad() }}"
    );
    err(&src, "E1017");
}

#[test]
fn generic_opaque_return_is_accepted() {
    // The hidden type names the generic parameter `T`. Sema accepts it; the
    // bound is deferred to the call site. NOTE: a fully generic program whose
    // hidden type is *only* `T` cannot be built yet by this slice because the
    // call site sees the un-substituted `T`; that is the opaque-TyKind work that
    // is out of scope here.
    ok(&format!(
        "{BASE}\nfn wrap<T>(x: T) -> impl Marker {{ x }}\nfn main() {{ let _ = wrap(S {{ value: 1 }}) }}"
    ));
}

#[test]
fn opaque_return_with_explicit_return() {
    // A block ending in `return v` has type `!`; the hidden type is taken from
    // the returned value.
    let src = format!(
        "{BASE}\nfn make() -> impl Marker {{ return S {{ value: 8 }} }}\nfn main() {{ let v = make()\n let _ = v.get() }}"
    );
    ok(&src);
}

#[test]
fn opaque_call_and_method_print_result() {
    let src = format!(
        "{BASE}\nfn make() -> impl Marker {{ S {{ value: 42 }} }}\nfn main() -> i32 {{\n println_i64(make().get())\n return 0\n}}\n"
    );
    // Erasure is not hidden: the caller sees `S` and can call its `get` method.
    expect_stdout("opaque_call", &src, "42");
}

#[test]
fn generic_opaque_call_builds_and_runs() {
    // A generic function whose hidden type is concrete: monomorphisation still
    // rewrites its `ret` (unchanged here) and the call site sees `S`.
    let src = format!(
        "{BASE}\nfn make_with<T>(x: T) -> impl Marker {{ S {{ value: 7 }} }}\nfn main() -> i32 {{\n println_i64(make_with(0i64).get())\n return 0\n}}\n"
    );
    expect_stdout("generic_opaque", &src, "7");
}
