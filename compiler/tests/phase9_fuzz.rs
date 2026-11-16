//! The parser against input written to break it.
//!
//! The parser's own contract is that it never panics: every malformed program is
//! a diagnostic and a recovery, never a crash. That contract is what makes the
//! language server safe -- an editor checks a file on every keystroke, so it sees
//! every half-typed state a file can be in, including the ones no reasonable
//! author would save.
//!
//! These cases were found by feeding random token soup and pathological nesting
//! to `umay check`. They are kept as fixed inputs so the failures come back as
//! tests rather than as a fuzzer someone has to remember to run.

use std::path::PathBuf;
use std::process::Command;

fn compiler() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_umay"))
}

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("umay_fuzz_{tag}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create the directory");
    dir
}

struct Result_ {
    status: i32,
    stdout: String,
    stderr: String,
}

impl Result_ {
    fn report(&self) -> String {
        format!("{}{}", self.stdout, self.stderr)
    }
}

fn check(tag: &str, source: &str) -> Result_ {
    let dir = scratch(tag);
    std::fs::write(dir.join("t.um"), source).expect("write the source");
    let output = Command::new(compiler())
        .current_dir(&dir)
        .env("UMAY_CACHE_DIR", dir.join("cache"))
        .args(["check", "t.um"])
        .output()
        .expect("run umay check");
    Result_ {
        status: output.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    }
}

fn must_not_panic(tag: &str, source: &str) -> Result_ {
    let result = check(tag, source);
    let report = result.report();
    assert!(
        !report.contains("panicked"),
        "the compiler panicked on `{tag}`:\n{report}"
    );
    // 0 is a clean program and 1 is a reported error; anything else is the
    // compiler failing at something other than its job.
    assert!(
        result.status == 0 || result.status == 1,
        "`{tag}` exited with {} rather than a diagnostic:\n{report}",
        result.status
    );
    result
}

#[test]
fn deeply_nested_parentheses_do_not_overflow_the_stack() {
    let source = format!(
        "fn main() -> i32 {{\n    let x = {}1i64{}\n    return 0\n}}\n",
        "(".repeat(5000),
        ")".repeat(5000)
    );
    let result = must_not_panic("deep_parens", &source);
    let report = result.report();
    assert!(report.contains("E0052"), "the depth limit should be named:\n{report}");
}

#[test]
fn the_depth_limit_is_reported_once_not_once_per_unclosed_level() {
    // Recovery kept parsing the 4700 parentheses left after the limit and
    // reported "expected `)`" for each: one real complaint followed by 256
    // consequences of it, burying the line the author needed to read.
    let source = format!(
        "fn main() -> i32 {{\n    let x = {}1i64{}\n    return 0\n}}\n",
        "(".repeat(5000),
        ")".repeat(5000)
    );
    let report = must_not_panic("depth_once", &source).report();
    let errors = report.matches("error[").count();
    assert_eq!(
        errors, 1,
        "expected one error about the nesting limit, found {errors}:\n{report}"
    );
}

#[test]
fn deeply_nested_blocks_do_not_overflow_the_stack() {
    let source = format!(
        "fn main() -> i32 {{\n    let x = {}1i64{}\n    return 0\n}}\n",
        "{".repeat(2000),
        "}".repeat(2000)
    );
    must_not_panic("deep_braces", &source);
}

#[test]
fn deeply_nested_generic_arguments_are_bounded() {
    let source = format!(
        "fn main() -> i32 {{\n    let x: {}i64{}\n    return 0\n}}\n",
        "Vec<".repeat(500),
        ">".repeat(500)
    );
    must_not_panic("deep_generics", &source);
}

#[test]
fn an_unterminated_string_is_a_diagnostic() {
    must_not_panic("unterminated_string", "fn main() -> i32 {\n    let s = \"abc\n    return 0\n}\n");
}

#[test]
fn an_unterminated_comment_is_a_diagnostic() {
    must_not_panic(
        "unterminated_comment",
        "fn main() -> i32 {\n    /* never closed\n    return 0\n}\n",
    );
}

#[test]
fn a_number_too_large_for_any_type_is_a_diagnostic() {
    let source = format!(
        "fn main() -> i32 {{\n    let x = {}i64\n    return 0\n}}\n",
        "9".repeat(10000)
    );
    must_not_panic("huge_number", &source);
}

#[test]
fn a_byte_order_mark_at_the_start_does_not_stop_the_parser() {
    // An editor may save a BOM. It is not part of the language, but refusing the
    // file outright would be a worse answer than ignoring it.
    must_not_panic("bom", "\u{feff}fn main() -> i32 {\n    return 0\n}\n");
}

#[test]
fn non_ascii_identifiers_are_either_accepted_or_reported() {
    // The language allows some Unicode in identifiers. Whatever the rule, a
    // source full of them must not crash the parser.
    must_not_panic(
        "unicode",
        "fn main() -> i32 {\n    let \u{e9}l\u{e9}ment = 1i64\n    return 0\n}\n",
    );
}

#[test]
fn random_token_soup_never_panics() {
    // A deterministic sample of the shapes a fuzzer produces. Not exhaustive by
    // design: the value is that these specific inputs are cheap to keep.
    let pieces = [
        "fn ", "let ", "mut ", "if ", "else ", "{", "}", "(", ")", "[", "]", "\n", ";", "::",
        "->", "<", ">", "&", "*", "1i64", "\"s\"", "match", "while", "struct", "impl", "trait",
        "mod", "use", "defer", "unsafe", "0x", "1e999", "//", "/*", "'a'", "@", "#", "$", "i32",
        "=>", "|", "!", "..", "\t",
    ];
    // A fixed pattern rather than a random one: the same inputs every run, so a
    // failure can be reproduced by reading this file.
    let mut source = String::new();
    let mut state: u64 = 0x2545_F491_4F6C_DD1D;
    for _ in 0..400 {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        source.push_str(pieces[(state % pieces.len() as u64) as usize]);
    }
    must_not_panic("soup", &source);
}

#[test]
fn an_empty_file_parses_to_nothing_without_crashing() {
    let result = must_not_panic("empty_file", "");
    // `check` has nothing to complain about: an empty file is a valid program
    // text. Whether it can be *built* is a different question, answered by the
    // linker unless the build asks first.
    assert_eq!(result.status, 0, "an empty file has no syntax error:\n{}", result.report());
}

#[test]
fn a_file_of_only_whitespace_parses_without_crashing() {
    must_not_panic("whitespace", "\n\n   \t\n\n");
}

#[test]
fn building_a_program_with_no_main_is_reported_not_left_to_the_linker() {
    // The linker says "undefined reference to `main`" through `collect2`, after
    // the assembler has already run. That names an object file rather than the
    // author's program, and arrives several stages too late.
    if !Command::new("cc")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
    {
        eprintln!("skipping building_a_program_with_no_main_is_reported_not_left_to_the_linker: `cc` is not available");
        return;
    }
    for (tag, source) in [
        ("empty", ""),
        ("no_main", "fn not_main() -> i32 {\n    return 0\n}\n"),
        (
            "main_in_a_module",
            "fn not_main() -> i32 {\n    return 0\n}\n\nmod helper;\n",
        ),
    ] {
        let dir = scratch(tag);
        std::fs::write(dir.join("t.um"), source).expect("write the source");
        if tag == "main_in_a_module" {
            // `main` in a module is not the program's entry point.
            std::fs::write(dir.join("helper.um"), "fn main() -> i32 {\n    return 0\n}\n")
                .expect("write the module");
        }
        let output = Command::new(compiler())
            .current_dir(&dir)
            .env("UMAY_CACHE_DIR", dir.join("cache"))
            .args(["build", "t.um"])
            .output()
            .expect("run umay build");
        let report = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert_ne!(output.status.code(), Some(0), "`{tag}` should not build:\n{report}");
        assert!(
            report.contains("has no `main` function"),
            "`{tag}` should be told about `main`, not left to the linker:\n{report}"
        );
        assert!(
            !report.contains("collect2") && !report.contains("undefined reference"),
            "`{tag}` reached the linker:\n{report}"
        );
    }
}
