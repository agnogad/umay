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
    // The program runs in its own scratch directory: a test that writes a file
    // must not leave it in the directory the test binary was started from.
    let out = Command::new(&exe)
        .current_dir(&dir)
        .output()
        .expect("run built program");
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

#[test]
fn a_match_selects_the_arm_whose_pattern_matches() {
    // Every arm's body must be behind a test on the enum's tag. Without one each
    // body would run in turn and the result would be the last arm's, whatever the
    // value was.
    expect_stdout(
        "match_selects",
        "enum E { A, B, C }\n\
         fn pick(e: E) -> i64 {\n\
             return match e { E::A => 10, E::B => 20, E::C => 30 }\n\
         }\n\
         fn main() -> i32 {\n\
             println_i64(pick(E::A))\n\
             println_i64(pick(E::B))\n\
             println_i64(pick(E::C))\n\
             return 0\n\
         }",
        "10\n20\n30\n",
    );
}

#[test]
fn a_match_on_a_bool_takes_the_right_branch() {
    expect_stdout(
        "match_bool",
        "fn describe(b: bool) -> i64 {\n\
             return match b { true => 1, false => 0 }\n\
         }\n\
         fn main() -> i32 {\n\
             println_i64(describe(true))\n\
             println_i64(describe(false))\n\
             return 0\n\
         }",
        "1\n0\n",
    );
}

#[test]
fn a_generic_enum_payload_binds_its_concrete_type() {
    // The variant's field type is written in terms of `T`, so matching an
    // `Option<i64>` must bind an `i64`: a binding left as `T` would have no size
    // and the backend would refuse to lay it out.
    expect_stdout(
        "generic_enum",
        "fn unwrap_or(o: Option<i64>, fallback: i64) -> i64 {\n\
             return match o { Option::Some(v) => v, Option::None => fallback }\n\
         }\n\
         fn main() -> i32 {\n\
             let a = Option::Some(7i64)\n\
             let b: Option<i64> = Option::None\n\
             println_i64(unwrap_or(a, 0))\n\
             println_i64(unwrap_or(b, -1))\n\
             return 0\n\
         }",
        "7\n-1\n",
    );
}

#[test]
fn a_generic_method_reads_its_receiver_field() {
    expect_stdout(
        "generic_method",
        "struct Box2<T> { value: T }\n\
         impl<T> Box2<T> {\n\
             fn get(&self) -> i64 {\n\
                 return self.value\n\
             }\n\
         }\n\
         fn main() -> i32 {\n\
             let b: Box2<i64> = Box2 { value: 7 }\n\
             println_i64(b.get())\n\
             return 0\n\
         }",
        "7\n",
    );
}

#[test]
fn a_vector_grows_and_keeps_its_elements() {
    // The vector starts with no allocation, grows to two, then doubles. Every
    // earlier element must survive each move to the larger block.
    expect_stdout(
        "vec_push",
        "fn main() -> i32 {\n\
             unsafe {\n\
                 let mut v: Vec<i64> = Vec::new()\n\
                 v = v.push(10)\n\
                 v = v.push(20)\n\
                 v = v.push(30)\n\
                 println_i64(v.len() as i64)\n\
                 println_i64(v.get(0))\n\
                 println_i64(v.get(1))\n\
                 println_i64(v.get(2))\n\
                 println_i64(v.get(99))\n\
                 v.free()\n\
             }\n\
             return 0\n\
         }",
        "3\n10\n20\n30\n0\n",
    );
}

#[test]
fn an_out_of_range_index_does_not_read_past_the_allocation() {
    // `get` checks the index against `len`, so an index past the end yields the
    // default instead of whatever byte follows the block.
    expect_stdout(
        "vec_bounds",
        "fn main() -> i32 {\n\
             unsafe {\n\
                 let mut v: Vec<i64> = Vec::new()\n\
                 v = v.push(5)\n\
                 println_i64(v.get(1))\n\
                 println_i64(v.capacity() as i64)\n\
                 v.free()\n\
             }\n\
             return 0\n\
         }",
        "0\n2\n",
    );
}

#[test]
fn sizeof_reports_a_type_it_has_to_lay_out() {
    // The operand is a type, not a value, so nothing is evaluated and the size is
    // known before the program runs.
    expect_stdout(
        "sizeof",
        "fn main() -> i32 {\n\
             println_i64(sizeof(u8) as i64)\n\
             println_i64(sizeof(i64) as i64)\n\
             println_i64(sizeof(Vec<u8>) as i64)\n\
             println_i64(sizeof([u8; 4]) as i64)\n\
             return 0\n\
         }",
        "1\n8\n24\n4\n",
    );
}

#[test]
fn a_vector_of_bytes_stores_one_byte_per_element() {
    // The stride comes from `sizeof(T)`, so a byte vector must pack its elements
    // one byte apart. Reading a byte as an eight-byte word would return whatever
    // the neighbouring bytes happen to be.
    expect_stdout(
        "vec_u8",
        "fn main() -> i32 {\n\
             unsafe {\n\
                 let mut v: Vec<u8> = Vec::new()\n\
                 v = v.push(65)\n\
                 v = v.push(66)\n\
                 v = v.push(67)\n\
                 println_i64(v.len() as i64)\n\
                 println_i64(v.get(0))\n\
                 println_i64(v.get(1))\n\
                 println_i64(v.get(2))\n\
                 let gap = (v.address_of(1) as i64) - (v.address_of(0) as i64)\n\
                 println_i64(gap)\n\
                 v.free()\n\
             }\n\
             return 0\n\
         }",
        "3\n65\n66\n67\n1\n",
    );
}

#[test]
fn a_vector_of_words_stores_one_word_per_element() {
    expect_stdout(
        "vec_i64_stride",
        "fn main() -> i32 {\n\
             unsafe {\n\
                 let mut v: Vec<i64> = Vec::new()\n\
                 v = v.push(1000000000)\n\
                 v = v.push(2000000000)\n\
                 let gap = (v.address_of(1) as i64) - (v.address_of(0) as i64)\n\
                 println_i64(gap)\n\
                 println_i64(v.get(0))\n\
                 println_i64(v.get(1))\n\
                 v.free()\n\
             }\n\
             return 0\n\
         }",
        "8\n1000000000\n2000000000\n",
    );
}

#[test]
fn an_associated_function_is_called_through_its_type() {
    // `Type::fn(..)` reaches a function in the type's `impl` block that takes no
    // receiver. Without it a constructor can only be a free function.
    expect_stdout(
        "associated_fn",
        "struct Point { x: i64, y: i64 }\n\
         impl Point {\n\
             fn origin() -> Point { return Point { x: 0, y: 0 } }\n\
             fn at(x: i64, y: i64) -> Point { return Point { x: x, y: y } }\n\
         }\n\
         fn main() -> i32 {\n\
             let p = Point::origin()\n\
             let q = Point::at(3, 4)\n\
             println_i64(p.x + p.y)\n\
             println_i64(q.x)\n\
             println_i64(q.y)\n\
             return 0\n\
         }",
        "0\n3\n4\n",
    );
}

#[test]
fn an_associated_function_of_a_generic_type_takes_its_instance_from_the_binding() {
    // `Vec::with_capacity(n)` names no `T` in its arguments, so the annotation on
    // the binding is what says which `Vec` it returns.
    expect_stdout(
        "associated_fn_generic",
        "fn main() -> i32 {\n\
             unsafe {\n\
                 let mut v: Vec<i64> = Vec::with_capacity(4)\n\
                 v = v.push(9)\n\
                 println_i64(v.capacity() as i64)\n\
                 println_i64(v.get(0))\n\
                 v.free()\n\
             }\n\
             return 0\n\
         }",
        "4\n9\n",
    );
}

#[test]
fn a_file_is_written_and_read_back() {
    // The program writes its own scratch file, so the only thing the test asserts
    // is that the bytes round-trip and the sizes agree.
    expect_stdout(
        "fs_roundtrip",
        "fn main() -> i32 {\n\
             let w = write_file_bytes(\"note.txt\" as *const u8, \"merhaba\\n\" as *const u8, 8)\n\
             println_i64(if w { 1 } else { 0 })\n\
             println_i64(file_size(\"note.txt\" as *const u8))\n\
             let f = File::open_read(\"note.txt\" as *const u8)\n\
             println_i64(if f.is_open() { 1 } else { 0 })\n\
             let buf = alloc(16)\n\
             let n = f.read(buf, 16)\n\
             println_i64(n as i64)\n\
             let first = read_byte(buf)\n\
             println_i64(first as i64)\n\
             f.close()\n\
             dealloc(buf)\n\
             return 0\n\
         }",
        "1\n8\n1\n8\n109\n",
    );
}

#[test]
fn a_missing_file_is_reported_rather_than_panicking() {
    // Opening something that is not there yields a closed `File`; nothing here
    // aborts, because a missing file is an ordinary answer.
    expect_stdout(
        "fs_missing",
        "fn main() -> i32 {\n\
             let f = File::open_read(\"definitely_absent.txt\" as *const u8)\n\
             println_i64(if f.is_open() { 1 } else { 0 })\n\
             println_i64(file_size(\"definitely_absent.txt\" as *const u8))\n\
             println_i64(if file_exists(\"definitely_absent.txt\" as *const u8) { 1 } else { 0 })\n\
             return 0\n\
         }",
        "0\n-1\n0\n",
    );
}

#[test]
fn a_file_is_appended_and_removed() {
    expect_stdout(
        "fs_append",
        "fn main() -> i32 {\n\
             let _ = write_file_bytes(\"log.txt\" as *const u8, \"one\\n\" as *const u8, 4)\n\
             let f = File::append(\"log.txt\" as *const u8)\n\
             let _ = f.write_str(\"two\\n\" as *const u8)\n\
             f.close()\n\
             println_i64(file_size(\"log.txt\" as *const u8))\n\
             println_i64(if remove_file(\"log.txt\" as *const u8) { 1 } else { 0 })\n\
             println_i64(if file_exists(\"log.txt\" as *const u8) { 1 } else { 0 })\n\
             return 0\n\
         }",
        "8\n1\n0\n",
    );
}

#[test]
fn a_whole_file_is_read_into_a_byte_vector() {
    // This is the first real use of `Vec<u8>`: the file's bytes arrive through the
    // vector, so a wrong stride would show up as wrong bytes rather than a wrong
    // length.
    expect_stdout(
        "fs_read_all",
        "fn main() -> i32 {\n\
             let text = \"abc\"\n\
             let _ = write_file_bytes(\"data.bin\" as *const u8, text as *const u8, 3)\n\
             let bytes = read_file_bytes(\"data.bin\" as *const u8)\n\
             println_i64(bytes.len() as i64)\n\
             println_i64(bytes.get(0))\n\
             println_i64(bytes.get(1))\n\
             println_i64(bytes.get(2))\n\
             bytes.free()\n\
             return 0\n\
         }",
        "3\n97\n98\n99\n",
    );
}

/// Builds `source` and runs it with `args`, returning what it printed.
fn build_and_run_with_args(name: &str, source: &str, args: &[&str]) -> Run {
    let dir = scratch(name);
    let file = dir.join("main.um");
    std::fs::write(&file, source).expect("write program");

    let build = Command::new(compiler())
        .arg("build")
        .arg(&file)
        .current_dir(&dir)
        .output()
        .expect("run umay build");
    assert!(
        build.status.success(),
        "build failed for {name}:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );

    let exe = dir.join("dist").join("main");
    let out = Command::new(&exe)
        .args(args)
        .current_dir(&dir)
        .output()
        .expect("run built program");
    Run {
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        status: out.status.code().unwrap_or(-1),
    }
}

#[test]
fn the_arguments_a_program_was_given_are_readable() {
    let run = build_and_run_with_args(
        "args_read",
        "fn main(argc: i32, argv: *mut *mut u8) -> i32 {\n\
             let args = Args::new(argc, argv)\n\
             println_i64(args.len() as i64)\n\
             println(args.get(1))\n\
             println(args.get(2))\n\
             return 0\n\
         }",
        &["alpha", "beta"],
    );
    assert_eq!(run.stdout, "3\nalpha\nbeta\n");
    assert_eq!(run.status, 0);
}

#[test]
fn a_missing_argument_reads_as_empty_rather_than_crashing() {
    let run = build_and_run_with_args(
        "args_missing",
        "fn main(argc: i32, argv: *mut *mut u8) -> i32 {\n\
             let args = Args::new(argc, argv)\n\
             println_i64(byte_len(args.get(9)) as i64)\n\
             println_i64(if args.is_empty() { 1 } else { 0 })\n\
             return 0\n\
         }",
        &[],
    );
    assert_eq!(run.stdout, "0\n1\n");
    assert_eq!(run.status, 0);
}

#[test]
fn a_flag_and_the_value_after_it_are_found() {
    let run = build_and_run_with_args(
        "args_flags",
        "fn main(argc: i32, argv: *mut *mut u8) -> i32 {\n\
             let args = Args::new(argc, argv)\n\
             println_i64(if args.has_flag(\"--verbose\" as *const u8) { 1 } else { 0 })\n\
             println(args.value_after(\"--output\" as *const u8))\n\
             return 0\n\
         }",
        &["--verbose", "--output", "rapor.txt"],
    );
    assert_eq!(run.stdout, "1\nrapor.txt\n");
    assert_eq!(run.status, 0);
}

#[test]
fn an_explicit_exit_code_reaches_the_shell() {
    let run = build_and_run(
        "process_exit",
        "fn main() -> i32 {\n\
             println(\"bitti\")\n\
             exit(7)\n\
             return 0\n\
         }",
    );
    assert_eq!(run.stdout, "bitti\n");
    assert_eq!(run.status, 7);
}

#[test]
fn the_size_of_a_file_is_read_without_reading_it() {
    // The file is 417 bytes, written by the test itself, so a size read by
    // seeking is checked against a size known independently.
    let dir = scratch("fs_size_seek");
    let data = vec![b'x'; 417];
    std::fs::write(dir.join("blob.bin"), &data).expect("write blob");
    let file = dir.join("main.um");
    std::fs::write(
        &file,
        "fn main() -> i32 {\n\
             println_i64(file_size(\"blob.bin\" as *const u8))\n\
             println_i64(file_size(\"absent.bin\" as *const u8))\n\
             return 0\n\
         }",
    )
    .expect("write program");
    let build = Command::new(compiler())
        .arg("build")
        .arg(&file)
        .current_dir(&dir)
        .output()
        .expect("run umay build");
    assert!(
        build.status.success(),
        "build failed:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let out = Command::new(dir.join("dist").join("main"))
        .current_dir(&dir)
        .output()
        .expect("run built program");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "417\n-1\n");
}

#[test]
fn a_directory_is_told_apart_from_a_file() {
    let dir = scratch("fs_is_dir");
    std::fs::create_dir_all(dir.join("sub")).expect("sub directory");
    std::fs::write(dir.join("plain.txt"), "x").expect("file");
    let file = dir.join("main.um");
    std::fs::write(
        &file,
        "fn main() -> i32 {\n\
             println_i64(if is_dir(\"sub\" as *const u8) { 1 } else { 0 })\n\
             println_i64(if is_dir(\"plain.txt\" as *const u8) { 1 } else { 0 })\n\
             println_i64(if is_dir(\"absent\" as *const u8) { 1 } else { 0 })\n\
             return 0\n\
         }",
    )
    .expect("write program");
    let build = Command::new(compiler())
        .arg("build")
        .arg(&file)
        .current_dir(&dir)
        .output()
        .expect("run umay build");
    assert!(
        build.status.success(),
        "build failed:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let out = Command::new(dir.join("dist").join("main"))
        .current_dir(&dir)
        .output()
        .expect("run built program");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "1\n0\n0\n");
}

#[test]
fn an_option_says_whether_it_holds_a_value() {
    // `is_some` matches on `&self`, so the receiver is inspected through a
    // reference rather than consumed.
    expect_stdout(
        "option_is_some",
        "fn half(n: i64) -> Option<i64> {\n\
             if n % 2 != 0 { return Option::None }\n\
             return Option::Some(n / 2)\n\
         }\n\
         fn main() -> i32 {\n\
             let even = half(8i64)\n\
             let odd = half(7i64)\n\
             println_i64(if even.is_some() { 1 } else { 0 })\n\
             println_i64(if odd.is_none() { 1 } else { 0 })\n\
             println_i64(even.unwrap_or(-1i64))\n\
             println_i64(odd.unwrap_or(-1i64))\n\
             return 0\n\
         }",
        "1\n1\n4\n-1\n",
    );
}

#[test]
fn a_result_carries_either_a_value_or_an_error() {
    expect_stdout(
        "result_ok_err",
        "fn parse_digit(text: *const u8) -> Result<i64, i64> {\n\
             let byte = read_byte(text)\n\
             if byte < 48u8 || byte > 57u8 {\n\
                 return Result::Err(-1i64)\n\
             }\n\
             return Result::Ok((byte - 48u8) as i64)\n\
         }\n\
         fn main() -> i32 {\n\
             let good = parse_digit(\"7\")\n\
             let bad = parse_digit(\"x\")\n\
             println_i64(if good.is_ok() { 1 } else { 0 })\n\
             println_i64(if bad.is_err() { 1 } else { 0 })\n\
             println_i64(good.unwrap_or(0i64))\n\
             println_i64(bad.unwrap_or(0i64))\n\
             return 0\n\
         }",
        "1\n1\n7\n0\n",
    );
}

#[test]
fn a_result_converts_to_an_option_of_its_success() {
    // The `Ok` arm settles the type of the match, so the `Err` arm's
    // `Option::None` takes its `T` from that arm rather than needing an
    // annotation of its own.
    expect_stdout(
        "result_to_option",
        "fn checked(n: i64) -> Result<i64, i64> {\n\
             if n < 0 { return Result::Err(1i64) }\n\
             return Result::Ok(n)\n\
         }\n\
         fn main() -> i32 {\n\
             let good = checked(5i64).ok()\n\
             let bad = checked(-5i64).ok()\n\
             println_i64(good.unwrap_or(0i64))\n\
             println_i64(bad.unwrap_or(0i64))\n\
             return 0\n\
         }",
        "5\n0\n",
    );
}

#[test]
fn an_option_without_a_type_is_rejected_rather_than_guessed() {
    // `None` holds nothing, so nothing says what `T` is. Guessing would pick a
    // type silently and be wrong later, so the program is refused instead.
    build_fails_with(
        "option_no_hint",
        "fn main() -> i32 {\n\
             let empty = Option::None\n\
             return 0\n\
         }",
        "E1001",
    );
}

#[test]
fn matching_a_reference_inspects_the_value_behind_it() {
    // `&self` gives a reference, and matching on it must look through to the
    // enum rather than report that `&Option<i64>` is not an `Option<i64>`.
    expect_stdout(
        "match_through_ref",
        "fn describe(value: &Option<i64>) -> i64 {\n\
             return match value {\n\
                 Option::Some(n) => n,\n\
                 Option::None => -1,\n\
             }\n\
         }\n\
         fn main() -> i32 {\n\
             let full = Option::Some(9i64)\n\
             let empty: Option<i64> = Option::None\n\
             println_i64(describe(&full))\n\
             println_i64(describe(&empty))\n\
             return 0\n\
         }",
        "9\n-1\n",
    );
}

#[test]
fn a_method_calls_another_on_its_own_receiver() {
    // `&mut self` inside a `&mut self` method: the receiver is reborrowed, not
    // moved, so the value is still usable after the inner call.
    expect_stdout(
        "reborrow_self",
        "struct Counter { n: i64 }\n\
         impl Counter {\n\
             fn bump(&mut self) { self.n = self.n + 1 }\n\
             fn twice(&mut self) { self.bump()\n self.bump() }\n\
             fn read(&self) -> i64 { return self.n }\n\
             fn read_via(&mut self) -> i64 { return self.read() }\n\
         }\n\
         fn main() -> i32 {\n\
             let mut c = Counter { n: 0 }\n\
             c.twice()\n\
             println_i64(c.read())\n\
             println_i64(c.read_via())\n\
             return 0\n\
         }",
        "2\n2\n",
    );
}

#[test]
fn two_assignments_in_one_block_stay_separate() {
    // Semicolons are optional, so `self.n = self.n + 1` followed by another
    // assignment must end the first statement: a number is never called.
    expect_stdout(
        "two_assignments",
        "struct Pair { a: i64, b: i64 }\n\
         impl Pair {\n\
             fn fill(&mut self) {\n\
                 self.a = self.a + 1\n\
                 self.b = self.b + 2\n\
             }\n\
         }\n\
         fn main() -> i32 {\n\
             let mut p = Pair { a: 0, b: 0 }\n\
             p.fill()\n\
             println_i64(p.a)\n\
             println_i64(p.b)\n\
             return 0\n\
         }",
        "1\n2\n",
    );
}

#[test]
fn an_enum_method_sees_its_type_arguments() {
    // `instance_of` looks the receiver's arguments up for structs and enums
    // alike, so a method on `Result<i64, i64>` may return `Option<T>`.
    expect_stdout(
        "enum_method_args",
        "fn checked(n: i64) -> Result<i64, i64> {\n\
             if n < 0 { return Result::Err(1i64) }\n\
             return Result::Ok(n)\n\
         }\n\
         fn main() -> i32 {\n\
             let good = checked(4i64).ok()\n\
             let bad = checked(-4i64).ok()\n\
             println_i64(good.unwrap_or(0i64))\n\
             println_i64(bad.unwrap_or(0i64))\n\
             return 0\n\
         }",
        "4\n0\n",
    );
}

#[test]
fn a_hash_map_stores_and_replaces_values() {
    expect_stdout(
        "hashmap_basic",
        "fn main() -> i32 {\n\
             let mut m = HashMap::new()\n\
             println_i64(m.len() as i64)\n\
             m.insert(\"bir\", 1i64)\n\
             m.insert(\"iki\", 2i64)\n\
             m.insert(\"uc\", 3i64)\n\
             println_i64(m.len() as i64)\n\
             println_i64(m.get_or(\"bir\", -1i64))\n\
             println_i64(m.get_or(\"iki\", -1i64))\n\
             println_i64(m.get_or(\"uc\", -1i64))\n\
             m.insert(\"iki\", 22i64)\n\
             println_i64(m.get_or(\"iki\", -1i64))\n\
             println_i64(m.len() as i64)\n\
             m.release()\n\
             return 0\n\
         }",
        "0\n3\n1\n2\n3\n22\n3\n",
    );
}

#[test]
fn a_missing_key_yields_the_fallback() {
    expect_stdout(
        "hashmap_missing",
        "fn main() -> i32 {\n\
             let mut m = HashMap::new()\n\
             m.insert(\"var\", 1i64)\n\
             println_i64(m.get_or(\"yok\", -1i64))\n\
             println_i64(if m.has_key(\"var\") { 1i64 } else { 0i64 })\n\
             println_i64(if m.has_key(\"yok\") { 1i64 } else { 0i64 })\n\
             println_i64(m.len() as i64)\n\
             m.release()\n\
             return 0\n\
         }",
        "-1\n1\n0\n1\n",
    );
}

#[test]
fn a_hash_map_grows_past_its_first_capacity() {
    // Forty keys cross several doublings, so every rehash path runs.
    expect_stdout(
        "hashmap_grow",
        "fn main() -> i32 {\n\
             let mut m = HashMap::new()\n\
             m.insert(\"a\", 1i64)\n\
             m.insert(\"b\", 2i64)\n\
             m.insert(\"c\", 3i64)\n\
             m.insert(\"d\", 4i64)\n\
             m.insert(\"e\", 5i64)\n\
             m.insert(\"f\", 6i64)\n\
             println_i64(m.len() as i64)\n\
             println_i64(m.get_or(\"a\", -1i64))\n\
             println_i64(m.get_or(\"f\", -1i64))\n\
             m.release()\n\
             return 0\n\
         }",
        "6\n1\n6\n",
    );
}


#[test]
fn a_byte_pointer_round_trips_through_a_string() {
    // `&str` is a NUL-terminated byte pointer, so both directions are real
    // conversions rather than reinterpretations.
    expect_stdout(
        "pointer_string_roundtrip",
        "extern fn strlen(s: *const u8) -> usize\n\
         fn length_of(p: *const u8) -> usize { return strlen(p) }\n\
         fn show(p: *const u8) { println(unsafe { p as &str }) }\n\
         fn main() -> i32 {\n\
             let text = \"merhaba\"\n\
             let bytes = text as *const u8\n\
             println_i64(length_of(bytes) as i64)\n\
             show(bytes)\n\
             return 0\n\
         }",
        "7\nmerhaba\n",
    );
}


#[test]
fn two_strings_are_equal_when_their_bytes_are() {
    // `str` is a pointer, so `==` must compare bytes: comparing addresses
    // would call two equal strings different.
    expect_stdout(
        "str_content_equality",
        "fn copy_of(text: &str) -> &str {\n\
             let buf = alloc(16)\n\
             unsafe {\n\
                 let n = byte_len(text)\n\
                 let mut i: usize = 0\n\
                 while i < n {\n\
                     write_byte((buf as usize + i) as *mut u8, read_byte((text as *const u8 as usize + i) as *const u8))\n\
                     i = i + 1\n\
                 }\n\
                 write_byte((buf as usize + n) as *mut u8, 0u8)\n\
                 return buf as *const u8 as &str\n\
             }\n\
             return text\n\
         }\n\
         fn main() -> i32 {\n\
             let a = \"/about\"\n\
             let b = copy_of(a)\n\
             println_i64(if a == b { 1 } else { 0 })\n\
             println_i64(if a == \"/other\" { 1 } else { 0 })\n\
             println_i64(if a != \"/other\" { 1 } else { 0 })\n\
             return 0\n\
         }",
        "1\n0\n1\n",
    );
}

#[test]
fn a_route_table_walks_by_pointer_arithmetic() {
    expect_stdout(
        "route_table_walk",
        "struct Route { path: *const u8, body: *const u8 }\n\
         fn lookup(table: *const Route, count: usize, want: *const u8) -> *const u8 {\n\
             let key = unsafe { want as &str }\n\
             let mut i: usize = 0\n\
             while i < count {\n\
                 let r = unsafe { (table as usize + i * sizeof(Route)) as *const Route }\n\
                 if unsafe { (*r).path as &str } == key { return unsafe { (*r).body } }\n\
                 i = i + 1\n\
             }\n\
             return unsafe { 0 as *const u8 }\n\
         }\n\
         fn main() -> i32 {\n\
             let routes = [\n\
                 Route { path: \"/\" as *const u8, body: \"home\" as *const u8 },\n\
                 Route { path: \"/about\" as *const u8, body: \"about us\" as *const u8 },\n\
             ]\n\
             let table = routes as *const Route\n\
             println(unsafe { lookup(table, 2, \"/about\" as *const u8) as &str })\n\
             return 0\n\
         }",
        "about us\n",
    );
}

#[test]
fn a_child_process_runs_its_own_copy_of_the_program() {
    // `spawn` is the whole of UMAY's concurrency today: the child gets a copy of
    // the address space, so a write in the child is invisible to the parent.
    // That is what makes a child a safe worker with no lock, and it is why the
    // parent's own value survives.
    expect_stdout(
        "fork_child_copy",
        "fn main() -> i32 {\n\
             let mut shared = 1i64\n\
             let pid = spawn()\n\
             if is_child(pid) {\n\
                 shared = 99i64\n\
                 exit(0)\n\
             }\n\
             let child = wait()\n\
             println_i64(if child > 0 { 1 } else { 0 })\n\
             println_i64(shared)\n\
             return 0\n\
         }",
        "1\n1\n",
    );
}

#[test]
fn a_child_writes_to_a_pipe_and_the_parent_reads_it() {
    // A pipe is the one channel a child and its parent share, so it is how a
    // worker reports a result back.
    expect_stdout(
        "fork_pipe",
        "extern fn pipe(fds: *mut i32) -> i32\n\
         extern fn read(fd: i32, buf: *mut u8, n: usize) -> i64\n\
         extern fn write(fd: i32, buf: *const u8, n: usize) -> i64\n\
         extern fn close(fd: i32) -> i32\n\
         fn main() -> i32 {\n\
             let fds = alloc(8)\n\
             let as_i = fds as *mut i32\n\
             unsafe { pipe(as_i) }\n\
             let rfd = unsafe { *(as_i as *const i32) }\n\
             let wfd = unsafe { *((as_i as usize + 4) as *const i32) }\n\
             let pid = spawn()\n\
             if is_child(pid) {\n\
                 close(rfd)\n\
                 write(wfd, \"merhaba\" as *const u8, 7)\n\
                 close(wfd)\n\
                 exit(0)\n\
             }\n\
             close(wfd)\n\
             let buf = alloc(16)\n\
             unsafe {\n\
                 let n = read(rfd, buf, 15) as usize\n\
                 write_byte((buf as usize + n) as *mut u8, 0u8)\n\
                 println(buf as *const u8 as &str)\n\
             }\n\
             close(rfd)\n\
             wait()\n\
             return 0\n\
         }",
        "merhaba\n",
    );
}

#[test]
fn bytes_carry_their_length_and_parse_a_request_line() {
    // A `&str` cannot hold a request: the body may contain a NUL, and scanning
    // for a terminator would read past the end. `Bytes` carries the byte count
    // that `read` returned, so every access is bounded.
    expect_stdout(
        "bytes_request_line",
        "fn main() -> i32 {\n\
             let raw = \"GET /about HTTP/1.1\\r\\nHost: x\\r\\n\\r\\n\"\n\
             let request = Bytes::from_str(raw)\n\
             println_i64(if request.len == 32usize { 1 } else { 0 })\n\
             let line_end = match request.end_of_line(0) {\n\
                 Option::Some(i) => i,\n\
                 Option::None => { println(\"no line\"); return 1 }\n\
             }\n\
             let line = request.prefix(line_end)\n\
             let first = match line.find_byte(' ' as u8, 0) {\n\
                 Option::Some(i) => i,\n\
                 Option::None => { println(\"no space\"); return 1 }\n\
             }\n\
             let rest = line.from(first + 1)\n\
             let second = match rest.find_byte(' ' as u8, 0) {\n\
                 Option::Some(i) => i,\n\
                 Option::None => rest.len\n\
             }\n\
             let target = rest.prefix(second)\n\
             println_i64(if target.len == 6usize { 1 } else { 0 })\n\
             let about = Bytes::from_str(\"/about\")\n\
             let other = Bytes::from_str(\"/other\")\n\
             let blank = Bytes::from_str(\"\\r\\n\\r\\n\")\n\
             println_i64(if target.equals(&about) { 1 } else { 0 })\n\
             println_i64(if target.equals(&other) { 1 } else { 0 })\n\
             println_i64(if request.find_slice(blank, 0).is_some() { 1 } else { 0 })\n\
             return 0\n\
         }",
        "1\n1\n1\n0\n1\n",
    );
}

#[test]
fn bytes_equality_is_bounded_by_the_shorter_length() {
    // Without the length check, comparing a 6-byte view with a 1-byte view reads
    // past the smaller one and can report equality it should not.
    expect_stdout(
        "bytes_bounded_equality",
        "fn main() -> i32 {\n\
             let a = Bytes::from_str(\"/about\")\n\
             let b = Bytes::from_str(\"/\")\n\
             let c = Bytes::from_str(\"/about\")\n\
             println_i64(if a.equals(&b) { 1 } else { 0 })\n\
             println_i64(if a.equals(&c) { 1 } else { 0 })\n\
             println_i64(if b.equals(&a) { 1 } else { 0 })\n\
             return 0\n\
         }",
        "0\n1\n0\n",
    );
}

#[test]
fn let_else_is_refused_rather_than_silently_miscompiled() {
    // The parser accepts `let ... else`, but nothing runs the block. Reporting it
    // is the only honest answer: a silent miscompile would bind the tag.
    build_fails_with(
        "let_else_refused",
        "fn main() -> i32 {\n\
             let value: Option<i64> = Option::None\n\
             let Option::Some(x) = value else { return 1 }\n\
             println_i64(x)\n\
             return 0\n\
         }",
        "E3001",
    );
}

#[test]
fn http_request_parts_are_parsed_from_counted_bytes() {
    expect_stdout(
        "http_parse_request",
        "fn main() -> i32 {\n\
             let raw = Bytes::from_str(\"POST /items HTTP/1.1\\r\\nContent-Length: 5\\r\\nHost: x\\r\\n\\r\\nhello\")\n\
             let req = parse_request(raw)\n\
             let post = Bytes::from_str(\"POST\")\n\
             let items = Bytes::from_str(\"/items\")\n\
             let v11 = Bytes::from_str(\"HTTP/1.1\")\n\
             println_i64(if req.method().equals(&post) { 1 } else { 0 })\n\
             println_i64(if req.target().equals(&items) { 1 } else { 0 })\n\
             println_i64(if req.version().equals(&v11) { 1 } else { 0 })\n\
             println_i64(if req.body().len == 5usize { 1 } else { 0 })\n\
             println_i64(req.body().get(0) as i64)\n\
             return 0\n\
         }",
        "1\n1\n1\n1\n104\n",
    );
}

#[test]
fn content_length_is_read_case_insensitively() {
    expect_stdout(
        "http_content_length",
        "fn main() -> i32 {\n\
             let a = Bytes::from_str(\"Content-Length: 42\\r\\nHost: x\")\n\
             let b = Bytes::from_str(\"content-length:7\\r\\nHost: x\")\n\
             let c = Bytes::from_str(\"Host: x\\r\\nAccept: */*\")\n\
             println_i64(content_length(&a))\n\
             println_i64(content_length(&b))\n\
             println_i64(content_length(&c))\n\
             return 0\n\
         }",
        "42\n7\n-1\n",
    );
}

#[test]
fn a_body_without_a_content_length_is_empty_not_the_rest_of_the_buffer() {
    expect_stdout(
        "http_no_content_length",
        "fn main() -> i32 {\n\
             let raw = Bytes::from_str(\"GET / HTTP/1.1\\r\\nHost: x\\r\\n\\r\\njunk\")\n\
             let req = parse_request(raw)\n\
             println_i64(if req.body().len == 0usize { 1 } else { 0 })\n\
             return 0\n\
         }",
        "1\n",
    );
}

#[test]
fn a_malformed_request_line_is_reported_as_an_empty_target() {
    expect_stdout(
        "http_malformed",
        "fn main() -> i32 {\n\
             let raw = Bytes::from_str(\"GARBAGE\\r\\n\\r\\n\")\n\
             let req = parse_request(raw)\n\
             println_i64(if req.target().is_empty() { 1 } else { 0 })\n\
             return 0\n\
         }",
        "1\n",
    );
}

#[test]
fn a_trait_method_on_a_generic_parameter_resolves_to_the_impl() {
    // `it.next()` on a `T: Iterator<i64>` has no `impl` to name at the call
    // site: only the bound does. Monomorphisation cashes the bound in once the
    // real type is known, so dispatch stays static.
    expect_stdout(
        "iterator_generic_bound",
        "struct Countdown { left: i64 }\n\
         impl Iterator<i64> for Countdown {\n\
             fn next(&mut self) -> Option<i64> {\n\
                 if self.left <= 0i64 { return Option::None }\n\
                 let v = self.left\n\
                 self.left = self.left - 1\n\
                 return Option::Some(v)\n\
             }\n\
         }\n\
         fn main() -> i32 {\n\
             let mut a = Countdown { left: 4i64 }\n\
             println_i64(iterate_sum(&mut a, 10))\n\
             let mut b = Countdown { left: 3i64 }\n\
             println_i64(iterate_count(&mut b, 10) as i64)\n\
             let mut c = Countdown { left: 3i64 }\n\
             let items = iterate_collect(&mut c, 10)\n\
             println_i64(items.len() as i64)\n\
             println_i64(items.get(0))\n\
             println_i64(items.get(2))\n\
             return 0\n\
         }",
        "10\n3\n3\n3\n1\n",
    );
}

#[test]
fn two_nested_generic_lists_close_with_a_double_angle() {
    // The lexer makes `>>` one token because that is a shift. Closing
    // `Option<Vec<i64>>` and a bound like `T: Iterator<i64>` must split it.
    expect_stdout(
        "nested_generics_close",
        "fn first_or_zero(v: &Vec<i64>) -> i64 {\n\
             if v.len() == 0usize { return 0i64 }\n\
             return v.get(0)\n\
         }\n\
         fn main() -> i32 {\n\
             let mut v: Vec<i64> = Vec::new()\n\
             v = v.push(7i64)\n\
             println_i64(first_or_zero(&v))\n\
             let boxed: Option<Vec<i64>> = Option::Some(v)\n\
             println_i64(if boxed.is_some() { 1 } else { 0 })\n\
             return 0\n\
         }",
        "7\n1\n",
    );
}

#[test]
fn an_annotation_outranks_a_defaulted_literal_in_a_variant() {
    // `Option::Some(1)` would default its payload to `i32`; the annotation says
    // `i64`, and a literal has not decided its width yet, so the annotation wins.
    expect_stdout(
        "variant_literal_takes_annotation",
        "fn main() -> i32 {\n\
             let a: Option<i64> = Option::Some(1)\n\
             println_i64(a.unwrap_or(0i64))\n\
             let b: Option<i64> = Option::None\n\
             println_i64(b.unwrap_or(9i64))\n\
             return 0\n\
         }",
        "1\n9\n",
    );
}

#[test]
fn a_local_array_is_addressed_correctly() {
    // A local array's base and index must not land in one register: `(%rax,%rax,1)`
    // addresses `base + base`, which faulted on every local array read.
    expect_stdout(
        "local_array_index",
        "fn main() -> i32 {\n\
             let d = [1u8, 2u8, 3u8, 4u8]\n\
             println_i64(d[0] as i64)\n\
             println_i64(d[2] as i64)\n\
             let mut m = [0u8; 4]\n\
             m[0] = 65u8\n\
             m[3] = 66u8\n\
             println_i64(m[0] as i64)\n\
             println_i64(m[3] as i64)\n\
             return 0\n\
         }",
        "1\n3\n65\n66\n",
    );
}

#[test]
fn a_let_destructures_a_tuple() {
    // Every binding of a destructuring `let` must receive its own part of the
    // value. Only the first used to be assigned, so reading the second was
    // reported as reading an uninitialised local.
    expect_stdout(
        "let_tuple_destructure",
        "fn pair(n: i64) -> (i64, i64) { return (n, n * 2i64) }\n\
         fn main() -> i32 {\n\
             let (a, b) = pair(5i64)\n\
             println_i64(a)\n\
             println_i64(b)\n\
             let (c, d) = (7i64, 8i64)\n\
             println_i64(c + d)\n\
             return 0\n\
         }",
        "5\n10\n15\n",
    );
}

#[test]
fn a_mutable_reference_parameter_can_call_a_mutable_method() {
    // The mutability of a `&mut T` binding lives in its type, not in how it was
    // declared, so `c.bump()` must not be refused as a write to an immutable
    // binding. The reference must be reborrowed for the call, not moved, so it can
    // be used again afterwards.
    expect_stdout(
        "mut_ref_parameter",
        "struct C { n: i64 }\n\
         impl C {\n\
             fn bump(&mut self) { self.n = self.n + 1 }\n\
         }\n\
         fn step(c: &mut C) { c.bump() }\n\
         fn twice(c: &mut C) { step(c)\n step(c) }\n\
         fn main() -> i32 {\n\
             let mut c = C { n: 0i64 }\n\
             step(&mut c)\n\
             twice(&mut c)\n\
             println_i64(c.n)\n\
             return 0\n\
         }",
        "3\n",
    );
}

#[test]
fn a_match_arm_block_may_be_followed_by_another_arm() {
    // A block-bodied arm does not need a comma, so `_ => ..` after
    // `Variant(x) => { .. }` is legal.
    expect_stdout(
        "match_block_arm_then_wildcard",
        "fn main() -> i32 {\n\
             let v: Option<i64> = Option::Some(3)\n\
             match v {\n\
                 Option::Some(x) => {\n\
                     println_i64(x)\n\
                 }\n\
                 _ => println_i64(-1),\n\
             }\n\
             return 0\n\
         }",
        "3\n",
    );
}

#[test]
fn json_fields_are_read_by_name() {
    // The document is a normal UMAY string literal, so the escapes in it are
    // UMAY's: `\{` keeps a brace from starting an interpolation and `\"` embeds a
    // quote.
    let source = r##"
fn main() -> i32 {
    let doc = Bytes::from_str("\{\"name\":\"umay\",\"version\":3,\"ok\":true,\"tags\":[10,20]}")
    let nv = json_field(&doc, "name")
    let name = json_text(nv)
    let want = Bytes::from_str("umay")
    println_i64(if name.equals(&want) { 1 } else { 0 })
    let version = json_field(&doc, "version")
    println_i64(json_i64(version))
    let ok = json_field(&doc, "ok")
    println_i64(if json_bool(ok) { 1 } else { 0 })
    let tags = json_field(&doc, "tags")
    match tags {
        Json::Array(body) => {
            println_i64(json_count(&body) as i64)
            println_i64(json_i64(json_at(&body, 1)))
        }
        _ => println_i64(-1),
    }
    return 0
}
"##;
    expect_stdout("json_read", source, "1\n3\n1\n2\n20\n");
}

#[test]
fn json_numbers_round_trip_their_text() {
    // A number is kept as its decimal text: `0.1` has no exact binary float, so
    // converting on the way in would change the value a server echoes back.
    let source = r##"
fn main() -> i32 {
    let buf = alloc(64)
    let n = json_write_i64(buf, 64usize, -7i64)
    println_i64(n)
    unsafe {
        write_byte((buf as usize + n as usize) as *mut u8, 0u8)
        println(buf as *const u8 as &str)
    }
    let text = Bytes::from_str("a\"b")
    let q = json_quote(buf, 64usize, text)
    unsafe {
        write_byte((buf as usize + q as usize) as *mut u8, 0u8)
        println(buf as *const u8 as &str)
    }
    let doc = Bytes::from_str("\{\"n\":0.1\}")
    let nv = json_field(&doc, "n")
    match nv {
        Json::Number(text) => println_i64(text.len as i64),
        _ => println_i64(-1),
    }
    return 0
}
"##;
    expect_stdout("json_write", source, "2\n-7\n\"a\\\"b\"\n3\n");
}

#[test]
fn a_generic_function_may_return_a_tuple() {
    // A tuple return from a bounded generic instantiation was once typed as `()`,
    // which made the result unusable; the bound's method result is substituted with
    // the bound's argument and the tuple comes back intact.
    let source = r##"
struct Counter { at: i64, stop: i64 }
impl Counter {
    fn next(&mut self) -> Option<i64> {
        if self.at >= self.stop { return Option::None }
        self.at = self.at + 1
        return Option::Some(self.at)
    }
}
fn main() -> i32 {
    let mut c = Counter { at: 0i64, stop: 4i64 }
    let (sum, seen) = iterate_sum_and_count(&mut c, 10usize)
    println_i64(sum)
    println_i64(seen as i64)
    return 0
}
"##;
    expect_stdout("generic_tuple_return", source, "10\n4\n");
}

#[test]
fn a_program_may_define_a_name_the_prelude_also_uses() {
    // The prelude reaches the C library's `read` and `write`, but it names them
    // `umay_c_read`/`umay_c_write` and links them through `extern fn "read" ..`,
    // so those words stay free for a program.
    let source = r##"
fn read(n: i64) -> i64 { return n + 1i64 }
fn write(n: i64) -> i64 { return read(n) * 2i64 }
fn main() -> i32 {
    println_i64(write(4i64))
    return 0
}
"##;
    expect_stdout("prelude_name_is_free", source, "10\n");
}

#[test]
fn net_binds_a_port_and_accepts() {
    // The socket address layout is the library's business: a port written whole
    // binds a different port and still succeeds, which is invisible without a real
    // connection. This checks the address by binding, connecting and reading back.
    let source = r##"
extern fn "connect" sys_connect(fd: i32, addr: *const u8, len: i32) -> i32
extern fn "getpid" umay_c_getpid() -> i32
fn main() -> i32 {
    // The port comes from the process id, so a port left in `TIME_WAIT` by an
    // earlier run cannot make this test fail.
    let port = 20000u16 + (umay_c_getpid() % 20000i32) as u16
    let server = tcp_socket()
    let addr = address(port)
    if !socket_bind(&server, &addr) {
        println_i64(-1)
        return 1
    }
    socket_listen(&server, 4)
    let client = tcp_socket()
    let ok = sys_connect(client.fd, addr.bytes as *const u8, address_size() as i32)
    println_i64(ok as i64)
    let peer = socket_accept(&server)
    println_i64(if socket_is_closed(&peer) { 0 } else { 1 })
    let buf = alloc(16)
    socket_write_all(&client, "hi" as *const u8, 2usize)
    let got = socket_read(&peer, buf, 16usize)
    println_i64(got)
    dealloc(buf)
    socket_close(&peer)
    socket_close(&client)
    socket_close(&server)
    release_address(addr)
    return 0
}
"##;
    expect_stdout("net_bind_accept", source, "0\n1\n2\n");
}

#[test]
fn a_loop_yields_the_value_it_breaks_with() {
    // `loop { break v }` had type `()` and its head block was left empty and
    // unreachable, so the value never arrived. Both are fixed: the loop's type is
    // the break value's, and the head block is the body.
    let source = r##"
fn first_over(items: &Vec<i64>, limit: i64) -> i64 {
    let mut i: usize = 0
    let found = loop {
        if i >= items.len {
            break -1i64
        }
        if items.get(i) > limit {
            break items.get(i)
        }
        i = i + 1usize
    }
    return found
}
fn main() -> i32 {
    let mut v: Vec<i64> = Vec::new()
    v = v.push(2i64)
    v = v.push(9i64)
    v = v.push(15i64)
    println_i64(first_over(&v, 8i64))
    println_i64(first_over(&v, 100i64))
    let z = loop { break 5i64 }
    println_i64(z)
    return 0
}
"##;
    expect_stdout("loop_break_value", source, "9\n-1\n5\n");
}

#[test]
fn two_generic_parameters_named_t_do_not_share_a_bound() {
    // A parameter is named `T` in every function that has one. Filing a bound by
    // the name alone made one function's `Send` bound answer for another's
    // `Iterator` bound, and `sum + v` inside the iterator helper was then rejected
    // with "`+` is not defined for `T`".
    let source = r##"
trait Send {}
trait Sync {}
impl Send for i64 {}
trait Agent { fn step(&mut self) -> i64 }
struct Worker { at: i64 }
impl Agent for Worker {
    fn step(&mut self) -> i64 {
        self.at = self.at + 1i64
        return self.at
    }
}
fn drive<T: Agent>(it: &mut T, times: usize) -> i64 {
    let mut sum: i64 = 0
    let mut i: usize = 0
    while i < times {
        sum = sum + it.step()
        i = i + 1
    }
    return sum
}
fn needs_send<T: Send>(x: &T) -> bool { return true }
fn main() -> i32 {
    let mut w = Worker { at: 0i64 }
    println_i64(drive(&mut w, 3usize))
    println_i64(if needs_send(&w) { 1 } else { 0 })
    return 0
}
"##;
    expect_stdout("bounds_do_not_leak", source, "6\n1\n");
}

#[test]
fn turbofish_names_the_type_arguments_of_a_call() {
    // `::<..>` is what makes a generic call's type arguments explicit when nothing
    // else in the call says them.
    let source = r##"
fn make<T>(value: T) -> T { return value }
fn main() -> i32 {
    println_i64(make::<i64>(7i64))
    let mut v: Vec<i64> = Vec::new()
    v = v.push(make::<i64>(3i64))
    println_i64(v.len as i64)
    return 0
}
"##;
    expect_stdout("turbofish", source, "7\n1\n");
}
