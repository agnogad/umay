//! The first slice of the AArch64 backend.
//!
//! This does not claim the backend is complete. It pins the part that exists:
//! a scalar program is lowered to AArch64 assembly, a real assembler accepts it,
//! and the object names the AArch64 machine. Linking and running need an
//! AArch64 C library, which the Linux CI host does not have; the cross-target
//! tests cover object format, and a native ARM runner is the next step.

use std::path::{Path, PathBuf};
use std::process::Command;

fn compiler() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_umay"))
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("umay_aarch64_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch directory");
    dir
}

fn asm_for(dir: &Path, source: &str) -> Result<String, String> {
    let file = dir.join("main.um");
    std::fs::write(&file, source).expect("write program");
    let output = Command::new(compiler())
        .arg("asm")
        .arg("--target")
        .arg("aarch64-unknown-linux-gnu")
        .arg(&file)
        .current_dir(dir)
        .output()
        .expect("run umay asm");
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).into_owned())
    }
}

fn assemble(dir: &Path, asm: &str) {
    let probe = Command::new("clang")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !probe {
        return;
    }
    std::fs::write(dir.join("main.s"), asm).expect("write assembly");
    let assembled = Command::new("clang")
        .args([
            "-c",
            "--target=aarch64-unknown-linux-gnu",
            "main.s",
            "-o",
            "main.o",
        ])
        .current_dir(dir)
        .output()
        .expect("run clang");
    assert!(
        assembled.status.success(),
        "assembler rejected the output:\n{}",
        String::from_utf8_lossy(&assembled.stderr)
    );
}

#[test]
fn a_scalar_program_lowers_to_aarch64_instructions() {
    let dir = scratch("scalar");
    let source = "fn add(a: i64, b: i64) -> i64 {\n    return a + b\n}\n\nfn main() -> i32 {\n    return add(20i64, 22i64) as i32\n}\n";
    let asm = asm_for(&dir, source).expect("AArch64 assembly");
    assert!(asm.contains(".globl main"), "{asm}");
    assert!(asm.contains("bl umay_user_add"), "{asm}");
    assert!(asm.contains("add x19, x19, x20"), "{asm}");
    assert!(asm.contains("sxtw x19, w19"), "{asm}");
    assert!(asm.contains("ret"), "{asm}");
    assert!(
        !asm.contains(".globl const "),
        "constant initialisers are not functions:\n{asm}"
    );
}

#[test]
fn the_assembly_is_accepted_by_an_aarch64_assembler() {
    let probe = Command::new("clang")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !probe {
        eprintln!("skipping: clang is not available");
        return;
    }
    let dir = scratch("object");
    let asm = asm_for(&dir, "fn main() -> i32 {\n    return 42\n}\n").expect("assembly");
    std::fs::write(dir.join("main.s"), &asm).expect("write assembly");
    let assembled = Command::new("clang")
        .args([
            "-c",
            "--target=aarch64-unknown-linux-gnu",
            "main.s",
            "-o",
            "main.o",
        ])
        .current_dir(&dir)
        .output()
        .expect("run clang");
    assert!(
        assembled.status.success(),
        "assembler rejected the output:\n{}",
        String::from_utf8_lossy(&assembled.stderr)
    );
    let bytes = std::fs::read(dir.join("main.o")).expect("read object");
    assert_eq!(&bytes[..4], b"\x7fELF", "not an ELF object");
    // `e_machine` is a little-endian halfword at offset 18; 183 is EM_AARCH64.
    let machine = u16::from_le_bytes([bytes[18], bytes[19]]);
    assert_eq!(machine, 183, "the object is not AArch64");
}

#[test]
fn an_unsupported_construct_names_the_backend_instead_of_miscompiling() {
    let dir = scratch("unsupported");
    let error = asm_for(
        &dir,
        "fn main() -> i32 {\n    let x: i128 = 1i128\n    return x as i32\n}\n",
    )
    .expect_err("128-bit integers are not supported yet");
    assert!(
        error.contains("AArch64 backend"),
        "the error must name the backend: {error}"
    );
}

#[test]
fn control_flow_and_a_negative_constant_assemble() {
    let dir = scratch("control");
    let source = "fn main() -> i32 {\n    let mut i = 0i64\n    while i < 3i64 {\n        i = i + 1i64\n    }\n    let n = -i\n    return n as i32\n}\n";
    let asm = asm_for(&dir, source).expect("assembly");
    assert!(
        asm.contains("b.eq") || asm.contains("b.ne"),
        "the loop must branch:\n{asm}"
    );
    assert!(
        asm.contains("neg x19, x19"),
        "the negative value must be emitted:\n{asm}"
    );
    let probe = Command::new("clang")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !probe {
        return;
    }
    std::fs::write(dir.join("main.s"), &asm).expect("write assembly");
    let assembled = Command::new("clang")
        .args([
            "-c",
            "--target=aarch64-unknown-linux-gnu",
            "main.s",
            "-o",
            "main.o",
        ])
        .current_dir(&dir)
        .output()
        .expect("run clang");
    assert!(
        assembled.status.success(),
        "assembler rejected the loop:\n{}",
        String::from_utf8_lossy(&assembled.stderr)
    );
}

#[test]
fn the_runtime_compiles_for_aarch64() {
    let dir = scratch("runtime");
    let source = "fn main() -> i32 {\n    println(\"hello\")\n    return 0\n}\n";
    let file = dir.join("main.um");
    std::fs::write(&file, source).expect("write program");
    let output = Command::new(compiler())
        .arg("build")
        .arg("--target")
        .arg("aarch64-unknown-linux-gnu")
        .arg("--emit-object-only")
        .arg(&file)
        .current_dir(&dir)
        .output()
        .expect("run umay build");
    assert!(
        output.status.success(),
        "the runtime must compile for AArch64:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        dir.join("dist").join("main.o").is_file(),
        "program object missing"
    );
    assert!(
        dir.join("dist").join("umay_runtime.o").is_file(),
        "runtime object missing"
    );
}

#[test]
fn struct_fields_are_addressed_and_assembled() {
    let dir = scratch("struct");
    let source = "struct Pair {\n    a: i64,\n    b: i64,\n}\n\nfn main() -> i32 {\n    let p = Pair { a: 40i64, b: 2i64 }\n    return (p.a + p.b) as i32\n}\n";
    let asm = asm_for(&dir, source).expect("struct assembly");
    assert!(
        asm.contains("add x11, x11, #8"),
        "the second field is at offset 8:\n{asm}"
    );
    let probe = Command::new("clang")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !probe {
        return;
    }
    std::fs::write(dir.join("main.s"), &asm).expect("write assembly");
    let assembled = Command::new("clang")
        .args([
            "-c",
            "--target=aarch64-unknown-linux-gnu",
            "main.s",
            "-o",
            "main.o",
        ])
        .current_dir(&dir)
        .output()
        .expect("run clang");
    assert!(
        assembled.status.success(),
        "assembler rejected the struct program:\n{}",
        String::from_utf8_lossy(&assembled.stderr)
    );
}

#[test]
fn aggregate_arguments_and_returns_assemble() {
    let dir = scratch("aggregate");
    let source = "struct Pair { a: i64, b: i64 }\nfn sum(p: Pair) -> i64 { return p.a + p.b }\nfn make() -> Pair { return Pair { a: 40i64, b: 2i64 } }\nfn main() -> i32 { let p = make(); return sum(p) as i32 }\n";
    let asm = asm_for(&dir, source).expect("aggregate assembly");
    assert!(asm.contains("bl umay_user_sum"), "{asm}");
    assert!(asm.contains("bl umay_user_make"), "{asm}");
    assemble(&dir, &asm);
}

#[test]
fn enums_tuples_and_floats_assemble() {
    let dir = scratch("mixed");
    let source = "enum Kind { A(i64), B }\nfn main() -> i32 { let k = Kind::A(40i64); let t = (1.0f64, 2.0f64); let n = match k { Kind::A(x) => x, Kind::B => 0i64 }; return (n as f64 + t.0 + t.1) as i32 }\n";
    let asm = asm_for(&dir, source).expect("mixed assembly");
    assert!(asm.contains("fadd d8, d8, d9"), "{asm}");
    assert!(
        asm.contains("ldrb w0, [x11]"),
        "enum tag load missing:\n{asm}"
    );
    assemble(&dir, &asm);
}

#[test]
fn a_nine_argument_call_uses_the_stack() {
    let dir = scratch("nine");
    let source = "fn sum(a: i64, b: i64, c: i64, d: i64, e: i64, f: i64, g: i64, h: i64, i: i64) -> i64 { return a+b+c+d+e+f+g+h+i }\nfn main() -> i32 { return sum(1i64,2i64,3i64,4i64,5i64,6i64,7i64,8i64,6i64) as i32 }\n";
    let asm = asm_for(&dir, source).expect("nine-argument assembly");
    assert!(
        asm.contains("str x12, [sp, #0]"),
        "the ninth argument must be on the stack:\n{asm}"
    );
    assemble(&dir, &asm);
}

#[test]
fn float_locals_are_promoted_to_callee_saved_registers() {
    let dir = scratch("float_promote");
    let source = "fn main() -> i32 { let mut x: f64 = 0.0; let mut i: i64 = 0; while i < 3 { x = x + 0.5; i = i + 1 }; return x as i32 }\n";
    let asm = asm_for(&dir, source).expect("float loop assembly");
    assert!(
        asm.contains("stp d8, d9") || asm.contains("str d8, [sp, #-16]"),
        "the prologue must save d8:\n{asm}"
    );
    assert!(
        asm.contains("fadd d8, d8"),
        "the float local must stay in d8 instead of a stack slot:\n{asm}"
    );
    assemble(&dir, &asm);
}

#[test]
fn a_ninth_float_argument_does_not_clobber_a_promoted_parameter() {
    let dir = scratch("nine_floats");
    let source = "fn sum(a: f64, b: f64, c: f64, d: f64, e: f64, f: f64, g: f64, h: f64, i: f64) -> f64 { return a+b+c+d+e+f+g+h+i }\nfn main() -> i32 { return sum(1.0f64,2.0f64,3.0f64,4.0f64,5.0f64,6.0f64,7.0f64,8.0f64,6.0f64) as i32 }\n";
    let asm = asm_for(&dir, source).expect("nine-float assembly");
    assert!(
        asm.contains("ldr d0, [x29, #16]"),
        "the stack argument must use d0, not a promoted parameter register:\n{asm}"
    );
    assert!(
        !asm.contains("ldr d12, [x29, #16]"),
        "d12 may hold parameter e and must not be the stack scratch:\n{asm}"
    );
    assemble(&dir, &asm);
}

#[test]
fn float_values_reach_aggregate_fields_from_v_registers() {
    let dir = scratch("float_fields");
    let source = "struct S { a: f64, b: f64 }\nenum E { V(f64), N }\nfn main() -> i32 { let x = 1.5f64; let t = (x, 2.5f64); let s = S { a: x, b: 2.5f64 }; let e = E::V(x); let a = [x; 2]; return (t.0 + t.1 + s.a + s.b + a[0]) as i32 }\n";
    let asm = asm_for(&dir, source).expect("float aggregate assembly");
    assert!(
        asm.contains("str d0, [x11]"),
        "float fields must be stored from a `v` register:\n{asm}"
    );
    assemble(&dir, &asm);
}

#[test]
fn disjoint_scalar_locals_share_a_register() {
    let dir = scratch("allocator_reuse");
    let mut source = String::from("fn main() -> i32 {\n    let mut sum: i64 = 0\n");
    for index in 0..20 {
        source.push_str(&format!("    let v{index}: i64 = {index}\n    sum = sum + v{index}\n"));
    }
    source.push_str("    return sum as i32\n}\n");
    let asm = asm_for(&dir, &source).expect("register reuse assembly");
    assert!(asm.contains("stp x19"), "the allocator must use x19:\n{asm}");
    assert!(
        !asm.contains("stp x20"),
        "disjoint locals must share x19 instead of consuming more registers:\n{asm}"
    );
    assert!(
        !asm.contains("sub x11, x29"),
        "no local in this body needs a stack slot:\n{asm}"
    );
    assemble(&dir, &asm);
}

#[test]
fn more_than_ten_live_values_spill_to_the_stack() {
    let dir = scratch("allocator_spill");
    let mut source = String::from("fn main() -> i32 {\n");
    for index in 0..12 {
        source.push_str(&format!("    let v{index}: i64 = {index}\n"));
    }
    let sum = (0..12)
        .map(|index| format!("v{index}"))
        .collect::<Vec<_>>()
        .join(" + ");
    source.push_str(&format!("    return ({sum}) as i32\n}}\n"));
    let asm = asm_for(&dir, &source).expect("spill assembly");
    assert!(
        asm.contains("stp x27, x28"),
        "ten integer locals can live in x19-x28:\n{asm}"
    );
    assert!(
        asm.contains("sub x11, x29"),
        "the two locals that do not fit must stay in stack slots:\n{asm}"
    );
    assemble(&dir, &asm);
}

#[test]
fn scalar_arithmetic_targets_allocated_registers() {
    let dir = scratch("direct_int");
    let source = "fn add(a: i64, b: i64) -> i64 { return a + b }\nfn main() -> i32 { return add(40i64, 2i64) as i32 }\n";
    let asm = asm_for(&dir, source).expect("direct integer assembly");
    assert!(
        asm.contains("add x19, x19, x20"),
        "the addition must write the allocated destination directly:\n{asm}"
    );
    assert!(
        !asm.contains("add x0, x0, x1"),
        "the scratch-register round trip must be gone:\n{asm}"
    );
    assemble(&dir, &asm);
}

#[test]
fn float_arithmetic_targets_allocated_registers() {
    let dir = scratch("direct_float");
    let source = "fn add(a: f64, b: f64) -> f64 { return a + b }\nfn main() -> i32 { return add(40.0f64, 2.0f64) as i32 }\n";
    let asm = asm_for(&dir, source).expect("direct float assembly");
    assert!(
        asm.contains("fadd d8, d8, d9"),
        "the float addition must write the allocated destination directly:\n{asm}"
    );
    assemble(&dir, &asm);
}

#[test]
fn signed_stack_loads_extend_to_the_full_register() {
    let dir = scratch("signed_loads");
    let source = "struct S { a: i8, b: i16, c: i32 }\nfn main() -> i32 { let s = S { a: -1i8, b: -2i16, c: -3i32 }; return (s.a as i32) + (s.b as i32) + (s.c as i32) }\n";
    let asm = asm_for(&dir, source).expect("signed load assembly");
    assert!(asm.contains("ldrsb"), "i8 must sign-extend:\n{asm}");
    assert!(asm.contains("ldrsh"), "i16 must sign-extend:\n{asm}");
    assert!(asm.contains("ldrsw"), "i32 must sign-extend:\n{asm}");
    assemble(&dir, &asm);
}

#[test]
fn aggregate_copies_move_words_not_bytes() {
    let dir = scratch("copy_words");
    let source = "fn main() -> i32 { let a = [1i64; 100]; let b = a; return b[99] as i32 }\n";
    let asm = asm_for(&dir, source).expect("aggregate copy assembly");
    assert!(
        asm.contains("ldr x12, [x13, x15]"),
        "the copy loop must move eight bytes at a time:\n{asm}"
    );
    assert!(
        !asm.contains("add x15, x15, #1"),
        "the byte-at-a-time copy loop must be gone:\n{asm}"
    );
    assemble(&dir, &asm);
}

#[test]
fn windows_assembly_is_a_coff_arm64_object() {
    let dir = scratch("winarm");
    let source = "fn main() -> i32 { println(\"win-arm\"); return 42 }\n";
    let file = dir.join("main.um");
    std::fs::write(&file, source).expect("write program");
    let output = Command::new(compiler())
        .arg("asm")
        .arg("--target")
        .arg("aarch64-w64-mingw32")
        .arg(&file)
        .current_dir(&dir)
        .output()
        .expect("run umay asm");
    assert!(
        output.status.success(),
        "Windows ARM64 assembly failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let asm = String::from_utf8_lossy(&output.stdout);
    assert!(asm.contains(".weak WinMain"), "{asm}");
    assert!(
        asm.contains("b _main"),
        "the startup shim must branch to _main:\n{asm}"
    );
    assert!(
        !asm.contains(".type main,"),
        "COFF has no ELF `.type`:\n{asm}"
    );

    let probe = Command::new("clang")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !probe {
        return;
    }
    std::fs::write(dir.join("main.s"), asm.as_bytes()).expect("write assembly");
    let assembled = Command::new("clang")
        .args([
            "-c",
            "--target=aarch64-w64-mingw32",
            "main.s",
            "-o",
            "main.o",
        ])
        .current_dir(&dir)
        .output()
        .expect("run clang");
    assert!(
        assembled.status.success(),
        "assembler rejected the COFF output:\n{}",
        String::from_utf8_lossy(&assembled.stderr)
    );
    let bytes = std::fs::read(dir.join("main.o")).expect("read object");
    assert_eq!(&bytes[..2], &[0x64, 0xaa], "the object is not ARM64 COFF");
}

#[test]
fn macos_assembly_is_a_mach_o_arm64_object() {
    let dir = scratch("macos");
    let source = "struct Pair { a: i64, b: i64 }\nfn main() -> i32 { let p = Pair { a: 40i64, b: 2i64 }; println(\"mac\"); return (p.a + p.b) as i32 }\n";
    let file = dir.join("main.um");
    std::fs::write(&file, source).expect("write program");
    let output = Command::new(compiler())
        .arg("asm")
        .arg("--target")
        .arg("aarch64-apple-darwin")
        .arg(&file)
        .current_dir(&dir)
        .output()
        .expect("run umay asm");
    assert!(
        output.status.success(),
        "macOS assembly failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let asm = String::from_utf8_lossy(&output.stdout);
    assert!(asm.contains("@PAGE") && asm.contains("@PAGEOFF"), "{asm}");
    assert!(
        !asm.contains(".type main,"),
        "Mach-O has no ELF `.type`:\n{asm}"
    );
    let probe = Command::new("clang")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !probe {
        return;
    }
    std::fs::write(dir.join("main.s"), asm.as_bytes()).expect("write assembly");
    let assembled = Command::new("clang")
        .args([
            "-c",
            "--target=arm64-apple-darwin",
            "main.s",
            "-o",
            "main.o",
        ])
        .current_dir(&dir)
        .output()
        .expect("run clang");
    assert!(
        assembled.status.success(),
        "assembler rejected the Mach-O output:\n{}",
        String::from_utf8_lossy(&assembled.stderr)
    );
    let bytes = std::fs::read(dir.join("main.o")).expect("read object");
    assert_eq!(
        &bytes[..4],
        b"\xcf\xfa\xed\xfe",
        "not a 64-bit Mach-O object"
    );
    let cpu = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
    assert_eq!(cpu, 0x0100_000c, "the object is not ARM64");
}
