//! Phase 2: `dyn Trait` coercion and vtable dispatch.
//!
//! Uses the same two harnesses as phase10_closures.rs.

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
    assert!(parse_errors.is_empty(), "parse errors: {parse_errors:?}");
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
    let dir = std::env::temp_dir().join(format!("umay_phase14_{name}_{}", std::process::id()));
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

const SQUARE: &str = "
trait Draw { fn area(&self) -> i64 }
struct Square { side: i64 }
impl Draw for Square { fn area(&self) -> i64 { return self.side * self.side } }
";

#[test]
fn dyn_binding_checks() {
    ok(&format!(
        "{SQUARE}\nfn main() {{ let s = Square {{ side: 5 }}\n let d: dyn Draw = &s\n let _ = d }}"
    ));
}

#[test]
fn dyn_method_call_prints_result() {
    let src = format!(
        "{SQUARE}\nfn main() -> i32 {{
    let s = Square {{ side: 5 }}
    let d: dyn Draw = &s
    println_i64(d.area())
    return 0
}}
"
    );
    expect_stdout("dyn_area", &src, "25");
}

#[test]
fn dyn_unknown_trait_is_e1022() {
    err(
        "struct S { v: i64 }\nfn main() { let s = S { v: 1 }\n let d: dyn Missing = &s\n let _ = d }",
        "E1022",
    );
}

#[test]
fn dyn_object_safety_violation_is_e1023() {
    err(
        "trait Bad { fn generic<T>(self, x: T) -> i64 }\nstruct S { v: i64 }\nfn main() { let s = S { v: 1 }\n let x: dyn Bad = &s\n let _ = x }",
        "E1023",
    );
}

#[test]
fn dyn_two_principal_traits_is_e1024() {
    err(
        "trait A { fn a(self) }\ntrait B { fn b(self) }\nstruct S { v: i64 }\nfn main() { let s = S { v: 1 }\n let x: dyn A + B = &s\n let _ = x }",
        "E1024",
    );
}
