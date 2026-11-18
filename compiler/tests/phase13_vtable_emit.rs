//! Phase 13: static vtable emission in both backends.
//!
//! The source-level `dyn Trait` lowering is being written in parallel, so this
//! file pins the backend seam directly: it builds a small MIR program, injects a
//! vtable by hand, and checks the emitted rodata. A CLI smoke test guards the
//! ordinary (no-vtable) path against regressions.

use std::path::Path;
use std::process::Command;

use umayc::hir::Hir;
use umayc::mir::MirProgram;
use umayc::source::SourceMap;
use umayc::target::Target;
use umayc::types::TyCtxt;

fn compiler() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_BIN_EXE_umay"))
}

fn asm_for(dir: &Path, source: &str, target: Option<&str>) -> String {
    let file = dir.join("main.um");
    std::fs::write(&file, source).expect("write program");
    let mut cmd = Command::new(compiler());
    cmd.arg("asm");
    if let Some(target) = target {
        cmd.arg("--target").arg(target);
    }
    let out = cmd.arg(&file).current_dir(dir).output().expect("run umay asm");
    assert!(
        out.status.success(),
        "umay asm failed:\n{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn plain_programs_still_emit_assembly_on_both_backends() {
    let dir = std::env::temp_dir().join(format!("umay_phase13_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    let source = "fn add(a: i64, b: i64) -> i64 { return a + b }\nfn main() -> i32 { return add(1, 2) as i32 }\n";

    let x86 = asm_for(&dir, source, None);
    assert!(x86.contains(".globl main"), "{x86}");
    assert!(x86.contains("umay_user_add:"), "{x86}");
    assert!(!x86.contains(".Lvtable_"), "{x86}");

    let arm = asm_for(&dir, source, Some("aarch64-unknown-linux-gnu"));
    assert!(arm.contains(".globl main"), "{arm}");
    assert!(arm.contains("umay_user_add:"), "{arm}");
    assert!(arm.contains("bl umay_user_add"), "{arm}");
    assert!(!arm.contains(".Lvtable_"), "{arm}");

    let _ = std::fs::remove_dir_all(&dir);
}

/// Lowers a tiny program and injects one vtable whose only entry is `helper`.
/// `main` never calls `helper`, so keeping its body proves the vtable root in
/// `reachable_functions` works.
fn program_with_helper_vtable() -> (MirProgram, Hir, TyCtxt) {
    let source = "fn helper() -> i64 { return 7 }\nfn main() -> i32 { return 0 }\n";
    let mut map = SourceMap::new();
    let file = map.add("<phase13>", source);
    let (tokens, lex_errors) = umayc::lexer::tokenize(source, file);
    assert!(lex_errors.is_empty(), "{lex_errors:?}");
    let parsed = umayc::parser::parse(tokens, Vec::new(), file);
    let (program, prelude_diags) =
        umayc::prelude::with_prelude(parsed.program, &mut map, Target::LINUX);
    assert!(!prelude_diags.has_errors(), "{prelude_diags:?}");

    let analyzed = umayc::sema::analyze(&program, &map);
    assert!(
        !analyzed.diagnostics.has_errors(),
        "{}",
        umayc::render_all(&analyzed.diagnostics, &map, false)
    );
    let mut hir = analyzed.hir;
    let mut types = analyzed.types;
    let mut lowered = umayc::lower::lower(&mut hir, &mut types);
    let helper = hir
        .fns
        .iter()
        .find(|f| f.name == "helper")
        .expect("helper lowered into HIR")
        .id;
    let _ = lowered.program.intern_vtable(8, 8, None, vec![helper]);
    (lowered.program, hir, types)
}

#[test]
fn x86_emits_vtable_quads_with_direct_call_spelling() {
    let (program, hir, mut types) = program_with_helper_vtable();
    let asm = umayc::codegen::generate(&program, &mut types, &hir, Target::LINUX, true)
        .expect("x86 codegen");
    assert!(asm.contains(".Lvtable_0:\n"), "{asm}");
    assert!(asm.contains("    .quad umay_user_helper\n"), "{asm}");
    // Only named by the vtable, yet still emitted.
    assert!(asm.contains("umay_user_helper:"), "{asm}");
}

#[test]
fn aarch64_emits_vtable_quads_with_direct_call_spelling() {
    let (program, hir, mut types) = program_with_helper_vtable();
    let asm = umayc::codegen::generate(&program, &mut types, &hir, Target::AARCH64_LINUX, true)
        .expect("aarch64 codegen");
    assert!(asm.contains(".Lvtable_0:\n"), "{asm}");
    assert!(asm.contains("    .quad umay_user_helper\n"), "{asm}");
    assert!(asm.contains("umay_user_helper:"), "{asm}");
}
