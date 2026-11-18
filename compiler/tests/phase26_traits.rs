//! Phase 26: supertraits and default trait methods (ADR 0018 §4).
//!
//! Uses the same lex/parse/sema harness as the other phase tests plus the CLI
//! build/asm harness where execution or emitted vtable order must be checked.

use std::path::Path;
use std::process::Command;

use umayc::diagnostics::Diagnostics;
use umayc::lexer;
use umayc::parser;
use umayc::prelude;
use umayc::sema;
use umayc::source::SourceMap;
use umayc::target::Target;

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
    // The CLI injects the prelude (which declares `println_i64`); mirror that
    // here so sema-only expectations match real builds.
    let (program, prelude_diags) = prelude::with_prelude(parsed.program, &mut map, Target::LINUX);
    assert!(!prelude_diags.has_errors(), "prelude: {prelude_diags:?}");
    Checked { diagnostics: sema::analyze(&program, &map).diagnostics }
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
        Command::new(name)
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    };
    ok("as") && ok("cc")
}

fn scratch(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("umay_phase26_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch");
    dir
}

fn run_stdout(name: &str, source: &str) -> Option<String> {
    if !toolchain_available() {
        eprintln!("skipping {name}: no `as` or `cc`");
        return None;
    }
    let dir = scratch(name);
    let file = dir.join("main.um");
    std::fs::write(&file, source).expect("write source");
    let build = Command::new(compiler()).arg("build").arg(&file).current_dir(&dir).output().expect("build");
    assert!(build.status.success(), "build failed:\n{}", String::from_utf8_lossy(&build.stderr));
    let out = Command::new(dir.join("dist/main")).output().expect("run");
    assert!(out.status.success(), "run failed: {:?}", out.status);
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn expect_stdout(name: &str, source: &str, expected: &str) {
    if let Some(out) = run_stdout(name, source) {
        assert_eq!(out.trim_end(), expected, "program output mismatch");
    }
}

fn asm_for(dir: &Path, source: &str) -> String {
    let file = dir.join("main.um");
    std::fs::write(&file, source).expect("write source");
    let out = Command::new(compiler()).arg("asm").arg(&file).current_dir(dir).output().expect("umay asm");
    assert!(
        out.status.success(),
        "umay asm failed:\n{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

const SUPERTRAITS: &str = "
trait Base { fn a(&self) -> i64 }
trait Sub: Base { fn b(&self) -> i64 }
struct S { v: i64 }
impl Base for S { fn a(&self) -> i64 { return self.v } }
impl Sub for S { fn b(&self) -> i64 { return self.v + 1 } }
";

#[test]
fn supertrait_methods_are_dispatchable_on_dyn_sub() {
    ok(&format!(
        "{SUPERTRAITS}\nfn main() -> i32 {{
    let s = S {{ v: 7 }}
    let d: dyn Sub = &s
    println_i64(d.a())
    println_i64(d.b())
    return 0
}}"
    ));
    expect_stdout(
        "supertrait_slots",
        &format!(
            "{SUPERTRAITS}\nfn main() -> i32 {{
    let s = S {{ v: 7 }}
    let d: dyn Sub = &s
    println_i64(d.a())
    println_i64(d.b())
    return 0
}}"
        ),
        "7\n8",
    );
}

#[test]
fn supertrait_vtable_quads_are_base_then_sub() {
    let dir = scratch("vtable_order");
    let src = format!(
        "{SUPERTRAITS}\nfn main() -> i32 {{ let s = S {{ v: 1 }}; let d: dyn Sub = &s; let _ = d; return 0 }}"
    );
    let asm = asm_for(&dir, &src);
    let start = asm.find(".Lvtable_0:").expect("vtable emitted");
    let table = &asm[start..];
    let a = table.find("umay_user___umay_dyn_Sub_S_a").expect("base slot");
    let b = table.find("umay_user___umay_dyn_Sub_S_b").expect("sub slot");
    assert!(a < b, "ADR 0018 §4 order requires base method first:\n{table}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn unknown_supertrait_is_e1025() {
    err(
        "trait Sub: Missing { fn b(&self) -> i64 }\nstruct S { v: i64 }\nfn main() -> i32 { return 0 }",
        "E1025",
    );
}

#[test]
fn duplicate_supertrait_is_e1025() {
    err(
        "trait Base { fn a(&self) -> i64 }\ntrait Sub: Base + Base { fn b(&self) -> i64 }\nfn main() -> i32 { return 0 }",
        "E1025",
    );
}

#[test]
fn default_method_without_impl_body_runs() {
    let src = "
trait Base {
    fn a(&self) -> i64
    fn twice(&self) -> i64 { return self.a() + self.a() }
}
struct S { v: i64 }
impl Base for S { fn a(&self) -> i64 { return self.v } }
fn main() -> i32 {
    let s = S { v: 6 }
    let d: dyn Base = &s
    println_i64(d.twice())
    println_i64(s.twice())
    return 0
}
";
    ok(src);
    expect_stdout("default_method", src, "12\n12");
}

#[test]
fn multi_level_supertraits_keep_order_and_inherited_default() {
    let src = "
trait A { fn a(&self) -> i64 }
trait B: A { fn b(&self) -> i64 }
trait C: B {
    fn c(&self) -> i64
    fn extra(&self) -> i64 { return self.a() + self.b() }
}
struct S { v: i64 }
impl A for S { fn a(&self) -> i64 { return self.v } }
impl B for S { fn b(&self) -> i64 { return self.v + 1 } }
impl C for S { fn c(&self) -> i64 { return self.v + 2 } }
fn main() -> i32 {
    let s = S { v: 10 }
    let d: dyn C = &s
    println_i64(d.a())
    println_i64(d.b())
    println_i64(d.c())
    println_i64(d.extra())
    return 0
}
";
    ok(src);
    expect_stdout("supertrait_chain", src, "10\n11\n12\n21");
}

#[test]
fn self_type_in_default_body_is_e1026() {
    err(
        "trait Bad { fn f(&self) -> i64 { let x: Self = 0; return 0 } }\nfn main() -> i32 { return 0 }",
        "E1026",
    );
}

#[test]
fn inherited_object_safety_violation_is_e1023() {
    err(
        "trait Base { fn generic<T>(self, x: T) -> i64 }\n\
         trait Sub: Base { fn b(&self) -> i64 }\n\
         struct S { v: i64 }\n\
         impl Base for S { fn generic<T>(self, x: T) -> i64 { return 0 } }\n\
         impl Sub for S { fn b(&self) -> i64 { return 1 } }\n\
         fn main() -> i32 { let s = S { v: 1 }; let d: dyn Sub = &s; let _ = d; return 0 }",
        "E1023",
    );
}

#[test]
fn v1_traits_without_supertraits_are_unaffected() {
    ok("trait Draw { fn area(&self) -> i64 }\nstruct S { v: i64 }\nimpl Draw for S { fn area(&self) -> i64 { return self.v } }\nfn main() -> i32 { let s = S { v: 1 }; let d: dyn Draw = &s; println_i64(d.area()); return 0 }");
}
