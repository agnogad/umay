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
    assert!(!asm.contains(".globl const "), "constant initialisers are not functions:\n{asm}");
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
        .args(["-c", "--target=aarch64-unknown-linux-gnu", "main.s", "-o", "main.o"])
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
        "struct Pair { a: i64, b: i64 }\n\nfn main() -> i32 {\n    let p = Pair { a: 1i64, b: 2i64 }\n    return (p.a + p.b) as i32\n}\n",
    )
    .expect_err("aggregate values are not supported yet");
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
    assert!(asm.contains("b.eq") || asm.contains("b.ne"), "the loop must branch:\n{asm}");
    assert!(asm.contains("neg x0, x0"), "the negative value must be emitted:\n{asm}");
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
        .args(["-c", "--target=aarch64-unknown-linux-gnu", "main.s", "-o", "main.o"])
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
    assert!(dir.join("dist").join("main.o").is_file(), "program object missing");
    assert!(
        dir.join("dist").join("umay_runtime.o").is_file(),
        "runtime object missing"
    );
}
