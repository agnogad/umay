//! Phase 5 tests, part 3: end-to-end code generation.
//!
//! These tests compile a UMAY program to a Linux executable, run it, and check
//! its exit status. That is the only honest way to verify a backend: the
//! interesting failures are wrong answers, not compile errors, and only executing
//! the result can catch them.
//!
//! Each test is skipped, not failed, when the assembler or linker is missing, so
//! that the suite still runs on a machine without a toolchain. The skip is
//! reported by name so it cannot pass unnoticed.

use std::process::Command;

/// The compiler binary under test.
///
/// Cargo sets `CARGO_BIN_EXE_<name>` for integration tests, which is the only
/// portable way to find it: this toolchain writes build artifacts outside the
/// crate directory. It is substituted at compile time, not set in the
/// environment of the running test.
fn compiler() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_BIN_EXE_umay"))
}

/// True when the tools needed to build and link are present.
fn have_toolchain() -> bool {
    ["as", "cc"].iter().all(|tool| {
        Command::new(tool)
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    })
}

/// Compiles `src` and returns the program's exit code, or `None` when the
/// toolchain is unavailable.
fn run_program(name: &str, src: &str) -> Option<i32> {
    if !have_toolchain() {
        eprintln!("skipping {name}: `as` or `cc` is not available");
        return None;
    }
    let dir = std::env::temp_dir().join(format!("umay_e2e_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");
    let source = dir.join("main.um");
    std::fs::write(&source, src).expect("write source");

    let output = Command::new(compiler())
        .current_dir(&dir)
        .arg("build")
        .arg(&source)
        .output()
        .expect("run umay");
    assert!(
        output.status.success(),
        "compilation failed for {name}:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let exe = dir.join("dist").join("main");
    assert!(exe.exists(), "no executable produced for {name}");
    let status = Command::new(&exe).status().expect("run program");
    let code = status.code().expect("exit code");
    let _ = std::fs::remove_dir_all(&dir);
    Some(code)
}

/// Asserts that the program exits with `expected`.
#[track_caller]
fn expect_exit(name: &str, src: &str, expected: i32) {
    if let Some(code) = run_program(name, src) {
        assert_eq!(code, expected, "{name} exited with {code}, expected {expected}");
    }
}

// ---------------------------------------------------------------------------
// Arithmetic
// ---------------------------------------------------------------------------

#[test]
fn constant_arithmetic() {
    expect_exit("const_add", "fn main() -> i32 { return 2 + 3 }", 5);
}

#[test]
fn arithmetic_is_evaluated_at_the_type_width() {
    // 200 + 100 wraps to 44 in `u8`; a build that widened silently would exit 0
    // instead of 44 because 300 & 0xFF == 44.
    expect_exit(
        "wrap_u8",
        "fn main() -> i32 {\n    let a: u8 = 200\n    let b: u8 = 100\n    let c = a + b\n    return c as i32\n}",
        44,
    );
}

#[test]
fn division_keeps_its_sign() {
    expect_exit("div_signed", "fn main() -> i32 { return -7 / 2 + 10 }", 7);
}

#[test]
fn remainder_uses_the_dividend_sign() {
    // `-7 % 3` is -1 in this language (the remainder takes the dividend's
    // sign), so adding 10 gives 9, not 8.
    expect_exit("rem_signed", "fn main() -> i32 { return -7 % 3 + 10 }", 9);
}

#[test]
fn runtime_division_is_right_at_every_width() {
    // Through parameters, so nothing is folded: the two tests above are
    // constant and never reached `idiv`. A 32-bit `idiv` after `cqto` read a
    // zero `edx` (-7 / 2 was 2147483644); an 8-bit `div` left the remainder in
    // `ah` (17u8 % 16u8 was 0); a divisor allocated to `rax` was overwritten.
    let src = "fn d32(a: i32, b: i32) -> i32 { return a / b }\n\
               fn r32(a: i32, b: i32) -> i32 { return a % b }\n\
               fn d16(a: i16, b: i16) -> i16 { return a / b }\n\
               fn r8(a: i8, b: i8) -> i8 { return a % b }\n\
               fn ru8(a: u8, b: u8) -> u8 { return a % b }\n\
               fn du8(a: u8, b: u8) -> u8 { return a / b }\n\
               fn main() -> i32 {\n\
                   if d32(-7, 2) != -3 { return 1 }\n\
                   if r32(-7, 3) != -1 { return 2 }\n\
                   if d16(-9i16, 2i16) != -4i16 { return 3 }\n\
                   if r8(-7i8, 3i8) != -1i8 { return 4 }\n\
                   if ru8(17u8, 16u8) != 1u8 { return 5 }\n\
                   if du8(200u8, 16u8) != 12u8 { return 6 }\n\
                   return 0\n\
               }";
    expect_exit("div_runtime_widths", src, 0);
}

#[test]
fn a_widening_load_ends_slot_forwarding_for_its_register() {
    // `movzbq slot, %rcx` overwrote a register the forwarding pass still
    // believed held `d1 + 1`, so `b` arrived as 36 instead of 3.
    let src = "struct S { x: usize }\n\
               impl S { fn g(&self, a: u8, b: usize) -> i64 { return b as i64 } }\n\
               fn main() -> i32 {\n\
                   let d1: usize = 2\n\
                   let s = S { x: 7 }\n\
                   return s.g('$' as u8, d1 + 1) as i32\n\
               }";
    expect_exit("widening_load_forwarding", src, 3);
}

#[test]
fn multiplication_and_subtraction() {
    expect_exit("mul_sub", "fn main() -> i32 { return 6 * 7 - 2 }", 40);
}

#[test]
fn a_constant_shift_count_becomes_an_immediate() {
    // `sar`/`shl`/`shr` take a constant count as an immediate. The count used to
    // be materialised into `%rcx` and printed at the operation's width --
    // `sarl %ecx, %ecx` -- which the assembler rejects outright, so a program
    // that merely shifted by a literal failed to assemble.
    expect_exit("shr_const", "fn main() -> i32 { return (1000 >> 8) & 0xff }", 3);
}

#[test]
fn a_variable_shift_count_lives_in_cl() {
    // A variable count must be in `%cl`, and the result register must not be
    // allocated to `%rcx` or it overwrites the count before the shift reads it.
    let src = "fn main() -> i32 {\n    let x: i32 = 1000\n    let n: i32 = 8\n    return (x >> n) & 0xff\n}";
    expect_exit("shr_var", src, 3);
}

#[test]
fn a_variable_left_shift_does_not_lose_its_count() {
    // `12 << 3` is 96. This exact shape put the count in `%cl`, then allocated the
    // result to `%rcx`, which overwrote the count with `12`, so the machine
    // shifted by `12 & 31` instead of 3 and the result was zero. The whole value
    // is returned rather than masked, so the shift amount is checked, not just
    // the low byte.
    let src = "fn main() -> i32 {\n    let a: i32 = 12\n    let b: i32 = 3\n    return a << b\n}";
    expect_exit("shl_var_collision", src, 96);
}

#[test]
fn shifts_in_both_directions_keep_their_values() {
    // Left and right shifts sharing their count, so a wrong register allocation
    // in one is visible in the other.
    let src = "fn main() -> i32 {\n    let x: i32 = 4096\n    let n: i32 = 4\n    let a: i32 = x >> n\n    let b: i32 = x << n\n    return (a + b) & 0xff\n}";
    expect_exit("shift_both", src, 0);
}

#[test]
fn an_arithmetic_shift_keeps_the_sign() {
    // A negative value shifted right is an arithmetic shift, so it stays
    // negative: `-16 >> 2` is -4, and `-4 + 10` is 6.
    expect_exit("shr_signed", "fn main() -> i32 { return (-16 >> 2) + 10 }", 6);
}

// ---------------------------------------------------------------------------
// Control flow
// ---------------------------------------------------------------------------

#[test]
fn comparison_and_branch() {
    let src = "fn main() -> i32 {\n    let a = 7\n    if a > 3 {\n        return 1\n    } else {\n        return 2\n    }\n}";
    expect_exit("branch_gt", src, 1);
}

#[test]
fn else_branch_is_taken_when_the_condition_is_false() {
    let src = "fn main() -> i32 {\n    let a = 1\n    if a > 3 {\n        return 1\n    } else {\n        return 2\n    }\n}";
    expect_exit("branch_else", src, 2);
}

#[test]
fn while_loop_accumulates() {
    let src = "fn main() -> i32 {\n    let mut i = 0\n    let mut sum = 0\n    while i < 5 {\n        sum = sum + i\n        i = i + 1\n    }\n    return sum\n}";
    expect_exit("while_sum", src, 10);
}

#[test]
fn nested_loops_keep_their_counters_separate() {
    let src = "fn main() -> i32 {\n    let mut i = 0\n    let mut total = 0\n    while i < 3 {\n        let mut j = 0\n        while j < 3 {\n            total = total + 1\n            j = j + 1\n        }\n        i = i + 1\n    }\n    return total\n}";
    expect_exit("nested_loops", src, 9);
}

// ---------------------------------------------------------------------------
// Functions and calls
// ---------------------------------------------------------------------------

#[test]
fn function_call_returns_its_value() {
    let src = "fn add(a: i32, b: i32) -> i32 {\n    return a + b\n}\nfn main() -> i32 {\n    return add(10, 20)\n}";
    expect_exit("call_add", src, 30);
}

#[test]
fn recursion_works() {
    let src = "fn fib(n: i32) -> i32 {\n    if n < 2 {\n        return n\n    }\n    return fib(n - 1) + fib(n - 2)\n}\nfn main() -> i32 {\n    return fib(10)\n}";
    expect_exit("fib", src, 55);
}

#[test]
fn more_than_six_arguments_use_the_stack() {
    // Seven parameters: the seventh goes on the stack. Getting the layout wrong
    // shifts every later argument.
    let src = "fn sum7(a: i32, b: i32, c: i32, d: i32, e: i32, f: i32, g: i32) -> i32 {\n    return a + b + c + d + e + f + g\n}\nfn main() -> i32 {\n    return sum7(1, 2, 3, 4, 5, 6, 7)\n}";
    expect_exit("stack_args", src, 28);
}

// ---------------------------------------------------------------------------
// Data
// ---------------------------------------------------------------------------

#[test]
fn struct_fields_are_read_and_written() {
    let src = "struct P { x: i32, y: i32 }\nfn main() -> i32 {\n    let p = P { x: 7, y: 35 }\n    return p.x + p.y\n}";
    expect_exit("struct_fields", src, 42);
}

#[test]
fn struct_field_assignment() {
    let src = "struct P { x: i32, y: i32 }\nfn main() -> i32 {\n    let mut p = P { x: 1, y: 2 }\n    p.y = 40\n    return p.x + p.y\n}";
    expect_exit("struct_write", src, 41);
}

#[test]
fn booleans_flow_through_conditions() {
    let src = "fn main() -> i32 {\n    let a = 5\n    let b = 5\n    let same = a == b\n    if same {\n        return 1\n    }\n    return 0\n}";
    expect_exit("bool_cond", src, 1);
}

#[test]
fn casting_narrows_and_widens() {
    let src = "fn main() -> i32 {\n    let big = 300\n    let small = big as u8\n    let wide = small as i32\n    return wide\n}";
    expect_exit("cast_round_trip", src, 44);
}

// ---------------------------------------------------------------------------
// Optimisation must not change behaviour
// ---------------------------------------------------------------------------

#[test]
fn optimisation_levels_agree_on_the_result() {
    for level in ["-O0", "-O1", "-O2", "-O3"] {
        if !have_toolchain() {
            eprintln!("skipping opt level {level}: no toolchain");
            return;
        }
        let dir = std::env::temp_dir().join(format!(
            "umay_e2e_opt{}_{}",
            level.trim_start_matches("-O"),
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        let source = dir.join("main.um");
        // A program where folding, copy propagation, DCE and control flow all
        // have something to do.
        std::fs::write(
            &source,
            "fn main() -> i32 {\n    let unused = 99\n    let a = 2 * 3\n    let b = a + 4\n    let mut i = 0\n    while i < b {\n        i = i + 1\n    }\n    return i\n}",
        )
        .expect("write source");
        let output = Command::new(compiler())
            .current_dir(&dir)
            .arg("build")
            .arg(level)
            .arg(&source)
            .output()
            .expect("run umay");
        assert!(output.status.success(), "build failed at {level}:\n{}", String::from_utf8_lossy(&output.stderr));
        let status = Command::new(dir.join("dist").join("main")).status().expect("run");
        assert_eq!(status.code(), Some(10), "wrong result at {level}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

// ---------------------------------------------------------------------------
// Diagnostics
// ---------------------------------------------------------------------------

#[test]
fn a_function_value_is_called_through_its_pointer() {
    if !have_toolchain() {
        eprintln!("skipping: no toolchain");
        return;
    }
    let dir = std::env::temp_dir().join(format!("umay_e2e_unsupported_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");
    let source = dir.join("main.um");
    // A `fn` parameter is called through an indirect call: the callee is an
    // address rather than a name, so the call goes through a register.
    std::fs::write(
        &source,
        "fn twice(x: i64) -> i64 {\n    return x * 2\n}\nfn apply(f: fn(i64) -> i64, v: i64) -> i64 {\n    return f(v)\n}\nfn main() -> i32 {\n    return apply(twice, 3) as i32\n}",
    )
    .expect("write source");
    let output = Command::new(compiler())
        .current_dir(&dir)
        .arg("build")
        .arg(&source)
        .output()
        .expect("run umay");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "should have built:\n{stderr}");
    assert!(!stderr.contains("panicked"), "the compiler must not panic:\n{stderr}");
    let program = dir.join("dist").join("main");
    let ran = Command::new(&program).output().expect("run program");
    assert_eq!(ran.status.code(), Some(6), "`apply(twice, 3)` must return 6");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn assembly_is_generated_for_a_simple_program() {
    let dir = std::env::temp_dir().join(format!("umay_e2e_asm_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");
    let source = dir.join("main.um");
    std::fs::write(&source, "fn main() -> i32 { return 1 }").expect("write source");
    let output = Command::new(compiler())
        .arg("asm")
        .arg(&source)
        .output()
        .expect("run umay asm");
    assert!(output.status.success());
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(text.contains("main:"), "no function label:\n{text}");
    assert!(text.contains("ret"), "no return:\n{text}");
    assert!(text.contains(".text"), "no text section:\n{text}");
    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------------
// Aggregate arguments and returns (SysV classification)
// ---------------------------------------------------------------------------

#[test]
fn a_large_aggregate_argument_is_passed_by_value() {
    // Twenty-four bytes is more than two eightbytes, so the value travels in the
    // caller's outgoing stack area. Reading any field wrong shows up in the sum.
    expect_exit(
        "agg_arg_stack",
        "struct B { a: i64, b: i64, c: i64 }\n\
         fn sum(x: B) -> i32 { return (x.a + x.b + x.c) as i32 }\n\
         fn main() -> i32 { let v = B { a: 10, b: 20, c: 30 } return sum(v) }",
        60,
    );
}

#[test]
fn a_sixteen_byte_aggregate_argument_uses_two_registers() {
    expect_exit(
        "agg_arg_pair",
        "struct P { a: i64, b: i64 }\n\
         fn sum(x: P) -> i32 { return (x.a + x.b) as i32 }\n\
         fn main() -> i32 { return sum(P { a: 7, b: 9 }) }",
        16,
    );
}

#[test]
fn an_eight_byte_aggregate_argument_uses_one_register() {
    expect_exit(
        "agg_arg_word",
        "struct W { a: i64 }\n\
         fn get(x: W) -> i32 { return x.a as i32 }\n\
         fn main() -> i32 { return get(W { a: 33 }) }",
        33,
    );
}

#[test]
fn an_aggregate_argument_survives_a_stack_passed_neighbour() {
    // The aggregate takes the stack area, while the scalar that follows still
    // arrives in a register. Mixing the two is where a classifier goes wrong.
    expect_exit(
        "agg_arg_mixed",
        "struct B { a: i64, b: i64, c: i64 }\n\
         fn mix(x: B, k: i64) -> i32 { return (x.a + x.b + x.c + k) as i32 }\n\
         fn main() -> i32 { let v = B { a: 1, b: 2, c: 3 } return mix(v, 44) }",
        50,
    );
}

#[test]
fn an_aggregate_argument_can_be_a_temporary() {
    expect_exit(
        "agg_arg_temp",
        "struct B { a: i64, b: i64, c: i64 }\n\
         fn sum(x: B) -> i32 { return (x.a + x.b + x.c) as i32 }\n\
         fn main() -> i32 { return sum(B { a: 4, b: 5, c: 6 }) }",
        15,
    );
}

#[test]
fn a_short_aggregate_tail_is_copied_completely() {
    // Twelve bytes leaves the last eightbyte half used; copying only whole
    // eightbytes would leave the tail holding whatever the stack held before.
    expect_exit(
        "agg_arg_twelve",
        "struct T { a: i64, b: i32 }\n\
         fn sum(x: T) -> i32 { return (x.a + x.b as i64) as i32 }\n\
         fn main() -> i32 { return sum(T { a: 30, b: 12 }) }",
        42,
    );
}

#[test]
fn an_aggregate_argument_is_not_shared_with_the_caller() {
    // Passing by value must copy: mutating the parameter inside the callee must
    // not change the caller's variable. Ownership means the caller cannot read
    // `v` after passing it, so the check reads a *different* copy that was made
    // first.
    expect_exit(
        "agg_arg_copy",
        "struct B { a: i64, b: i64, c: i64 }\n\
         fn touch(x: B) -> i32 { let mut y = x y.a = 99 return y.a as i32 }\n\
         fn main() -> i32 {\n\
             let first = B { a: 5, b: 6, c: 7 }\n\
             let second = B { a: first.a, b: first.b, c: first.c }\n\
             let changed = touch(first)\n\
             return (second.a as i32) + changed\n\
         }",
        104,
    );
}

#[test]
fn a_literal_stack_argument_keeps_its_value() {
    // A literal passed on the stack has no storage of its own. Parking it in a
    // register before the argument shuffle would let the shuffle overwrite it
    // with a neighbour's value, so it is written straight from its bytes.
    let src = "fn sum7(a: i32, b: i32, c: i32, d: i32, e: i32, f: i32, g: i32) -> i32 {\n    return a + b + c + d + e + f + g\n}\nfn main() -> i32 {\n    return sum7(1, 2, 3, 4, 5, 6, 7)\n}";
    expect_exit("literal_stack_arg", src, 28);
}

#[test]
fn two_literal_stack_arguments_keep_their_values() {
    let src = "fn sum9(a: i32, b: i32, c: i32, d: i32, e: i32, f: i32, g: i32, h: i32, i: i32) -> i32 {\n    return a + b + c + d + e + f + g + h + i\n}\nfn main() -> i32 {\n    return sum9(1, 2, 3, 4, 5, 6, 7, 8, 9)\n}";
    expect_exit("two_literal_stack_args", src, 45);
}

#[test]
fn a_value_read_through_a_pointer_does_not_share_its_base_register() {
    // `total + *p` selects a memory operand: `addq (%base), %dst`. The base is a
    // virtual register that `reads()` must report, or the allocator will hand the
    // same register to the sum's destination and every later `*p` reads whatever
    // the sum left there. The program returns 21 here; a clash turns it into a
    // crash or a wrong number.
    let src = "fn main() -> i32 {\n\
        let mut i: i64 = 0\n\
        let mut total: i64 = 0\n\
        while i < 12 {\n\
            let p = alloc(8) as *mut i64\n\
            unsafe { *p = i }\n\
            total = (total + unsafe { *p }) % 1000003\n\
            dealloc(p as *mut u8)\n\
            i = i + 1\n\
        }\n\
        return total as i32\n\
    }";
    expect_exit("pointer_base_register", src, 66);
}

#[test]
fn two_pointer_reads_in_one_expression_keep_their_bases() {
    // Two memory operands in one expression: each base must stay live across the
    // other's use. This is the shape that regressed alongside the one above.
    let src = "fn main() -> i32 {\n\
        let a = alloc(8) as *mut i64\n\
        let b = alloc(8) as *mut i64\n\
        unsafe { *a = 20 }\n\
        unsafe { *b = 22 }\n\
        let sum = unsafe { *a } + unsafe { *b }\n\
        dealloc(a as *mut u8)\n\
        dealloc(b as *mut u8)\n\
        return sum as i32\n\
    }";
    expect_exit("two_pointer_bases", src, 42);
}

#[test]
fn mut_ref_coerces_to_shared_ref_end_to_end() {
    let src = "struct Counter { val: i64 }\n\
        fn read_val(c: &Counter) -> i64 { c.val }\n\
        fn add_val(c: &mut Counter, d: i64) { c.val = c.val + d }\n\
        fn main() -> i32 {\n\
            let mut c = Counter { val: 10 }\n\
            let r = &mut c\n\
            let a = read_val(r)\n\
            add_val(r, 22)\n\
            let b = read_val(r)\n\
            return (a + b) as i32\n\
        }";
    expect_exit("mut_ref_coerces_shared", src, 42);
}

#[test]
fn computed_pointer_deref_assignment_works() {
    let src = "fn main() -> i32 {\n\
        let p = alloc(16) as *mut i64\n\
        unsafe {\n\
            *(p.offset(0)) = 19\n\
            *(p.offset(1)) = 23\n\
        }\n\
        let sum = unsafe { *(p.offset(0)) + *(p.offset(1)) }\n\
        dealloc(p as *mut u8)\n\
        return sum as i32\n\
    }";
    expect_exit("computed_ptr_assign", src, 42);
}
