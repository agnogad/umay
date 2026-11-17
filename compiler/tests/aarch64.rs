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
    assert!(asm.contains("add x0, x0, x1"), "{asm}");
    assert!(asm.contains("sxtw x0, w0"), "{asm}");
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
        asm.contains("neg x0, x0"),
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
    assert!(asm.contains("fadd d0, d0, d1"), "{asm}");
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
