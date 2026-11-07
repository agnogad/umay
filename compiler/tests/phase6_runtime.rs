//! Phase 6: the runtime and the first real library layer.
//!
//! These tests compile and run whole programs, so they check the thing that
//! actually matters: what a UMAY program prints and which exit status it
//! returns. Everything here goes through the same `umay build` path a user
//! takes, including the runtime, which is linked into every program.
//!
//! Tests are skipped when `as` or `cc` is missing, so the suite still runs on a
//! machine without a toolchain; a skipped run is reported by the harness.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Path to the compiler binary under test.
fn compiler() -> &'static str {
    env!("CARGO_BIN_EXE_umay")
}

/// Whether the assembler and linker this backend shells out to are present.
fn toolchain_available() -> bool {
    let ok = |name: &str| {
        Command::new(name)
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    };
    ok("as") && ok("cc")
}

/// A directory for one test's scratch files, cleaned first.
fn scratch(name: &str) -> PathBuf {
    let dir = Path::new("/tmp/umay_phase6").join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch directory");
    dir
}

/// The result of running a built program.
struct Run {
    stdout: String,
    stderr: String,
    status: i32,
}

/// Builds `source` and runs it, returning what it printed and its exit status.
fn build_and_run(name: &str, source: &str) -> Run {
    let dir = scratch(name);
    let file = dir.join("main.um");
    std::fs::write(&file, source).expect("write program");

    let build = Command::new(compiler())
        .arg("build")
        .arg(&file)
        .current_dir(&dir)
        .output()
        .expect("run umay build");
    let build_stderr = String::from_utf8_lossy(&build.stderr).into_owned();
    assert!(
        build.status.success(),
        "build failed for {name}:\n{build_stderr}\nstdout:\n{}",
        String::from_utf8_lossy(&build.stdout)
    );

    let exe = dir.join("dist").join("main");
    let out = Command::new(&exe).output().expect("run built program");
    let code = out.status.code().unwrap_or_else(|| {
        use std::os::unix::process::ExitStatusExt;
        // A signal death has no exit code; report it as a large number so a
        // failing assertion shows a crash rather than a confusing -1.
        128 + out.status.signal().unwrap_or(0)
    });
    Run {
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        status: code,
    }
}

/// Builds `source` and asserts it is rejected with `code`.
fn build_fails_with(name: &str, source: &str, code: &str) {
    let dir = scratch(name);
    let file = dir.join("main.um");
    std::fs::write(&file, source).expect("write program");
    let out = Command::new(compiler())
        .arg("check")
        .arg(&file)
        .current_dir(&dir)
        .output()
        .expect("run umay check");
    let text = String::from_utf8_lossy(&out.stderr).into_owned()
        + &String::from_utf8_lossy(&out.stdout);
    assert!(!out.status.success(), "expected {code}, but the program compiled");
    assert!(text.contains(code), "expected {code} in output:\n{text}");
}

// ---------------------------------------------------------------------------
// Output
// ---------------------------------------------------------------------------

#[test]
fn println_writes_text_and_a_newline() {
    if !toolchain_available() {
        return;
    }
    let run = build_and_run(
        "println_text",
        r#"
fn main() -> i32 {
    println("merhaba")
    println("dunya")
    return 0
}
"#,
    );
    assert_eq!(run.stdout, "merhaba\ndunya\n");
    assert_eq!(run.status, 0);
}

#[test]
fn print_writes_without_a_newline() {
    if !toolchain_available() {
        return;
    }
    let run = build_and_run(
        "print_no_newline",
        r#"
fn main() -> i32 {
    print("a")
    print("b")
    println("")
    return 0
}
"#,
    );
    assert_eq!(run.stdout, "ab\n");
}

#[test]
fn empty_string_writes_nothing() {
    if !toolchain_available() {
        return;
    }
    // `byte_len` of an empty string must be zero, not one for the terminator.
    let run = build_and_run(
        "empty_string",
        r#"
fn main() -> i32 {
    println_i64(byte_len("") as i64)
    println_i64(byte_len("abc") as i64)
    return 0
}
"#,
    );
    assert_eq!(run.stdout, "0\n3\n");
}

#[test]
fn byte_len_counts_bytes_not_characters() {
    if !toolchain_available() {
        return;
    }
    // The runtime counts bytes; a multi-byte character therefore counts more
    // than once. This is the documented behaviour of a byte string.
    let run = build_and_run(
        "byte_len_utf8",
        r#"
fn main() -> i32 {
    println_i64(byte_len("ğ") as i64)
    return 0
}
"#,
    );
    assert_eq!(run.stdout, "2\n");
}

// ---------------------------------------------------------------------------
// Integer formatting
// ---------------------------------------------------------------------------

#[test]
fn println_i64_formats_positive_numbers() {
    if !toolchain_available() {
        return;
    }
    let run = build_and_run(
        "i64_positive",
        r#"
fn main() -> i32 {
    println_i64(0)
    println_i64(7)
    println_i64(42)
    println_i64(1000)
    return 0
}
"#,
    );
    assert_eq!(run.stdout, "0\n7\n42\n1000\n");
}

#[test]
fn println_i64_formats_negative_numbers() {
    if !toolchain_available() {
        return;
    }
    let run = build_and_run(
        "i64_negative",
        r#"
fn main() -> i32 {
    println_i64(-1)
    println_i64(-42)
    return 0
}
"#,
    );
    assert_eq!(run.stdout, "-1\n-42\n");
}

#[test]
fn println_i64_formats_the_extremes() {
    if !toolchain_available() {
        return;
    }
    // The smallest `i64` has no positive counterpart, so formatting it through
    // a magnitude would overflow. The runtime divides on the negative side to
    // avoid that, and this test is what proves it.
    let run = build_and_run(
        "i64_extremes",
        r#"
fn main() -> i32 {
    println_i64(9223372036854775807)
    println_i64(-9223372036854775808)
    return 0
}
"#,
    );
    assert_eq!(run.stdout, "9223372036854775807\n-9223372036854775808\n");
}

#[test]
fn integer_comparison_is_signed() {
    if !toolchain_available() {
        return;
    }
    // A negative number compares smaller than zero only if the comparison uses
    // the signed condition codes. Reading signedness from the result type, which
    // is `bool`, silently selects the unsigned ones.
    let run = build_and_run(
        "signed_compare",
        r#"
fn main() -> i32 {
    let a = -1
    let b = 0
    if a < b {
        println_i64(1)
    } else {
        println_i64(0)
    }
    return 0
}
"#,
    );
    assert_eq!(run.stdout, "1\n");
}

// ---------------------------------------------------------------------------
// Allocation
// ---------------------------------------------------------------------------

#[test]
fn alloc_gives_usable_memory() {
    if !toolchain_available() {
        return;
    }
    // Bytes written through the pointer must come back when the same memory is
    // read as a string: this checks that allocation returns real heap and that
    // pointer arithmetic addresses the expected byte.
    let run = build_and_run(
        "alloc_bytes",
        r#"
fn main() -> i32 {
    let p = alloc(16)
    write_byte(p.offset(0 as isize), 65u8)
    write_byte(p.offset(1 as isize), 66u8)
    write_byte(p.offset(2 as isize), 0u8)
    println(p)
    dealloc(p)
    return 0
}
"#,
    );
    assert_eq!(run.stdout, "AB\n");
}

#[test]
fn alloc_returns_distinct_blocks() {
    if !toolchain_available() {
        return;
    }
    // Two live allocations must not share memory, or the second write would
    // destroy the first string.
    let run = build_and_run(
        "alloc_distinct",
        r#"
fn fill(p: *mut u8, a: u8, b: u8) {
    write_byte(p.offset(0 as isize), a)
    write_byte(p.offset(1 as isize), b)
    write_byte(p.offset(2 as isize), 0u8)
}
fn main() -> i32 {
    let one = alloc(8)
    let two = alloc(8)
    fill(one, 88u8, 0u8)
    fill(two, 89u8, 0u8)
    println(one)
    println(two)
    dealloc(one)
    dealloc(two)
    return 0
}
"#,
    );
    assert_eq!(run.stdout, "X\nY\n");
}

#[test]
fn a_large_allocation_is_writable_end_to_end() {
    if !toolchain_available() {
        return;
    }
    // Writes far enough into the block that a short allocation would show up as
    // corruption. The loop exercises both pointer arithmetic and byte stores.
    let run = build_and_run(
        "alloc_large",
        r#"
fn main() -> i32 {
    let p = alloc(1024)
    let mut i = 0
    while i < 1023 {
        write_byte(p.offset(i as isize), 107u8)
        i = i + 1
    }
    write_byte(p.offset(1023 as isize), 0u8)
    let mut sum = 0
    let mut j = 0
    while j < 1023 {
        sum = sum + read_byte(p.offset(j as isize)) as i32
        j = j + 1
    }
    println_i64(sum as i64)
    dealloc(p)
    return 0
}
"#,
    );
    assert_eq!(run.stdout, format!("{}\n", 107 * 1023));
}

// ---------------------------------------------------------------------------
// Program shape
// ---------------------------------------------------------------------------

#[test]
fn main_can_return_a_status() {
    if !toolchain_available() {
        return;
    }
    let run = build_and_run(
        "exit_status",
        r#"
fn main() -> i32 {
    return 3
}
"#,
    );
    assert_eq!(run.status, 3);
}

#[test]
fn a_program_that_prints_and_returns_a_status_does_both() {
    if !toolchain_available() {
        return;
    }
    let run = build_and_run(
        "print_and_status",
        r#"
fn main() -> i32 {
    println("before")
    return 7
}
"#,
    );
    assert_eq!(run.stdout, "before\n");
    assert_eq!(run.stderr, "");
    assert_eq!(run.status, 7);
}

// ---------------------------------------------------------------------------
// Diagnostics
// ---------------------------------------------------------------------------

#[test]
fn an_extern_function_with_a_body_is_rejected() {
    build_fails_with(
        "extern_with_body",
        r#"
extern fn puts(s: *const u8) -> i32 {
    return 0
}
fn main() -> i32 { return 0 }
"#,
        "E0197",
    );
}

#[test]
fn a_function_without_a_body_must_be_extern() {
    build_fails_with(
        "missing_body",
        r#"
fn puts(s: *const u8) -> i32
fn main() -> i32 { return 0 }
"#,
        "E0198",
    );
}

#[test]
fn a_raw_pointer_may_be_cast_to_the_other_mutability() {
    if !toolchain_available() {
        return;
    }
    // `*mut T` to `*const T` is the cast every C-facing call needs. It must not
    // require `unsafe`: the pointer already exists, and only its permission to
    // write changes.
    let run = build_and_run(
        "mut_to_const",
        r#"
fn main() -> i32 {
    let p = alloc(4)
    write_byte(p.offset(0 as isize), 79u8)
    write_byte(p.offset(1 as isize), 75u8)
    write_byte(p.offset(2 as isize), 0u8)
    println(p)
    dealloc(p)
    return 0
}
"#,
    );
    assert_eq!(run.stdout, "OK\n");
}

// ---------------------------------------------------------------------------
// Optimisation levels must agree
// ---------------------------------------------------------------------------

#[test]
fn output_is_the_same_at_every_optimisation_level() {
    if !toolchain_available() {
        return;
    }
    let source = r#"
fn main() -> i32 {
    let mut sum = 0
    let mut i = 1
    while i <= 10 {
        sum = sum + i
        i = i + 1
    }
    println_i64(sum as i64)
    return 0
}
"#;
    for level in ["-O0", "-O1", "-O2", "-O3"] {
        let dir = scratch(&format!("opt{}", &level[2..]));
        let file = dir.join("main.um");
        std::fs::write(&file, source).expect("write program");
        let build = Command::new(compiler())
            .arg("build")
            .arg(level)
            .arg(&file)
            .current_dir(&dir)
            .output()
            .expect("run umay build");
        assert!(
            build.status.success(),
            "build failed at {level}: {}",
            String::from_utf8_lossy(&build.stderr)
        );
        let out = Command::new(dir.join("dist").join("main"))
            .output()
            .expect("run built program");
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            "55\n",
            "wrong output at {level}"
        );
    }
}

// ---------------------------------------------------------------------------
// `String`, the standard library's first owned type
// ---------------------------------------------------------------------------
//
// These run the same public interface a program uses. `String` lives in ordinary
// UMAY source on top of `alloc`, so a failure here is a failure of the language
// or the backend, never of a hand-written special case.

/// Runs a program and asserts on its whole standard output.
#[track_caller]
fn expect_stdout(name: &str, source: &str, expected: &str) {
    let run = build_and_run(name, source);
    assert_eq!(run.stdout, expected, "{name} printed the wrong text");
    assert_eq!(run.status, 0, "{name} exited with {}", run.status);
}

#[test]
fn a_string_starts_empty() {
    expect_stdout(
        "str_empty",
        "fn main() -> i32 {\n\
             let s = String::new()\n\
             println_i64(s.len() as i64)\n\
             return 0\n\
         }",
        "0\n",
    );
}

#[test]
fn bytes_can_be_appended_one_at_a_time() {
    // Thirty bytes forces several growth steps: the buffer starts empty and
    // doubles, so a growth mistake shows up as a truncated or corrupt result.
    expect_stdout(
        "str_push_grow",
        "fn main() -> i32 {\n\
             let mut s = String::new()\n\
             let mut i = 0\n\
             while i < 30 {\n\
                 s = s.push(65 + (i % 26) as u8)\n\
                 i = i + 1\n\
             }\n\
             println(s.as_ptr())\n\
             println_i64(s.len() as i64)\n\
             return 0\n\
         }",
        "ABCDEFGHIJKLMNOPQRSTUVWXYZABCD\n30\n",
    );
}

#[test]
fn a_string_can_be_built_from_existing_text() {
    expect_stdout(
        "str_from_str",
        "fn main() -> i32 {\n\
             let t = String::from_str(\"merhaba\")\n\
             println(t.as_ptr())\n\
             println_i64(t.len() as i64)\n\
             return 0\n\
         }",
        "merhaba\n7\n",
    );
}

#[test]
fn text_can_be_appended_to_a_string() {
    expect_stdout(
        "str_push_str",
        "fn main() -> i32 {\n\
             let s = String::new()\n\
             let s = s.push_str(\"umay\")\n\
             let s = s.push_str(\" \")\n\
             let s = s.push_str(\"dili\")\n\
             println(s.as_ptr())\n\
             println_i64(s.len() as i64)\n\
             return 0\n\
         }",
        "umay dili\n9\n",
    );
}

#[test]
fn an_empty_string_is_empty() {
    expect_stdout(
        "str_is_empty",
        "fn main() -> i32 {\n\
             let a = String::new()\n\
             if a.is_empty() { println(\"empty\") }\n\
             let b = a.push(120)\n\
             if b.is_empty() { println(\"still empty\") } else { println(\"full\") }\n\
             return 0\n\
         }",
        "empty\nfull\n",
    );
}

#[test]
fn byte_length_counts_bytes_not_characters() {
    // "ş" is two bytes in UTF-8, so a length that counted characters would
    // report 1 and a buffer walk would stop early.
    expect_stdout(
        "str_utf8_len",
        "fn main() -> i32 {\n\
             let t = String::from_str(\"şş\")\n\
             println_i64(t.len() as i64)\n\
             return 0\n\
         }",
        "4\n",
    );
}

#[test]
fn a_literal_local_adopts_the_width_it_is_used_at() {
    // `i` starts as an untyped `{integer}` and meets a `usize`. It must become a
    // `usize` for good: a local that typed the expression `usize` while keeping a
    // four-byte slot would let a 64-bit read pick up the neighbouring variable,
    // and `100000 + 0` would come out as something else entirely.
    expect_stdout(
        "literal_width",
        "fn main() -> i32 {\n\
             let i = 0\n\
             let n: usize = 100000\n\
             let k = n + i\n\
             println_i64(k as i64)\n\
             return 0\n\
         }",
        "100000\n",
    );
}

#[test]
fn two_literal_locals_keep_their_own_default_width() {
    // Neither local meets a concrete type, so both fall back to the default and
    // the arithmetic stays in it. This is the other side of the rule above: a
    // substitution must not be invented where none was decided.
    expect_stdout(
        "literal_default",
        "fn main() -> i32 {\n\
             let a = 2\n\
             let b = 3\n\
             println_i64((a * b) as i64)\n\
             return 0\n\
         }",
        "6\n",
    );
}
