//! ADR 0018: typed vtable prefix and destruction of owned `dyn Trait` values.
//!
//! Two seams are checked: the rodata each backend emits for a vtable, and the
//! MIR rewrite that turns a `Drop` of the compiler-generated owning struct into
//! an indirect call through the vtable's drop word. The MIR assertion is used
//! rather than a printed side effect: UMAY has no destructors, so the only
//! observable effect of the generated shim is the allocator call itself.

use std::path::{Path, PathBuf};
use std::process::Command;

use umayc::hir::Hir;
use umayc::mir::{MirProgram, Operand, Projection, Terminator};
use umayc::source::SourceMap;
use umayc::target::Target;
use umayc::types::{TyCtxt, TyKind};

const OWNED_DYN: &str = "
trait Draw { fn area(&self) -> i64 }
struct Square { side: i64 }
impl Draw for Square {
    fn area(&self) -> i64 { return self.side * self.side }
}
fn main() -> i32 {
    let d: dyn Draw = Square { side: 5 }
    println_i64(d.area())
    return 0
}
";

fn compiler() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_umay"))
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("umay_phase15_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

fn asm_for(dir: &Path, source: &str, target: Option<&str>) -> String {
    let file = dir.join("main.um");
    std::fs::write(&file, source).expect("write program");
    let mut cmd = Command::new(compiler());
    cmd.arg("asm");
    if let Some(target) = target {
        cmd.arg("--target").arg(target);
    }
    let out = cmd.arg(&file).current_dir(dir).output().expect("umay asm");
    assert!(
        out.status.success(),
        "umay asm failed:\n{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn vtable_prefix_is_emitted_on_x86() {
    let dir = scratch("x86_prefix");
    let asm = asm_for(&dir, OWNED_DYN, None);
    assert!(asm.contains(".p2align 3\n.Lvtable_0:\n"), "{asm}");
    assert!(asm.contains(".Lvtable_0:\n    .quad 8\n    .quad 8\n"), "{asm}");
    assert!(asm.contains("    .quad umay_user___umay_drop_Square\n"), "{asm}");
    assert!(asm.contains("    .quad umay_user___umay_dyn_Draw_Square_area\n"), "{asm}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn vtable_prefix_is_emitted_on_aarch64() {
    let dir = scratch("arm_prefix");
    let asm = asm_for(&dir, OWNED_DYN, Some("aarch64-unknown-linux-gnu"));
    assert!(asm.contains(".p2align 3\n.Lvtable_0:\n"), "{asm}");
    assert!(asm.contains(".Lvtable_0:\n    .quad 8\n    .quad 8\n"), "{asm}");
    assert!(asm.contains("    .quad umay_user___umay_drop_Square\n"), "{asm}");
    assert!(asm.contains("    .quad umay_user___umay_dyn_Draw_Square_area\n"), "{asm}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Lowers `OWNED_DYN` as far as drop elaboration, before the dyn-drop pass.
fn lowered_with_drop() -> (MirProgram, Hir, TyCtxt) {
    let mut map = SourceMap::new();
    let file = map.add("<phase15>", OWNED_DYN);
    let (tokens, lex_errors) = umayc::lexer::tokenize(OWNED_DYN, file);
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
    let mut program = umayc::lower::lower(&mut hir, &mut types).program;
    umayc::drop_elab::elaborate(&mut program, &types);
    (program, hir, types)
}

fn own_defs(hir: &Hir) -> Vec<umayc::types::DefId> {
    hir.structs
        .iter()
        .filter(|s| s.name.starts_with("__umay_dyn_own_"))
        .map(|s| s.def_id)
        .collect()
}

fn is_own_drop(program: &MirProgram, types: &TyCtxt, defs: &[umayc::types::DefId]) -> bool {
    program.bodies.iter().any(|body| {
        body.blocks.iter().any(|block| match &block.terminator {
            Terminator::Drop { place, .. } => {
                place.projection.is_empty()
                    && matches!(types.kind(body.locals[place.local.0 as usize].ty),
                        TyKind::Adt { def, .. } if defs.contains(def))
            }
            _ => false,
        })
    })
}

#[test]
fn owning_coercion_registers_a_non_copy_wrapper_and_typed_vtable() {
    let (program, hir, _types) = lowered_with_drop();
    assert_eq!(own_defs(&hir).len(), 1, "{:?}", hir.structs.iter().map(|s| &s.name).collect::<Vec<_>>());
    let s = hir.structs.iter().find(|s| s.name.starts_with("__umay_dyn_own_")).unwrap();
    assert!(!s.is_copy);
    assert_eq!(s.fields.len(), 1);
    assert_eq!(s.fields[0].name, "fat");

    let vtable = program.vtables.iter().find(|v| !v.entries.is_empty()).expect("a vtable");
    assert_eq!(vtable.size, 8, "Square is one i64");
    assert_eq!(vtable.align, 8);
    assert!(vtable.drop.is_some());
    let drop = hir.fns.iter().find(|f| Some(f.id) == vtable.drop).expect("drop shim");
    assert_eq!(drop.name, "__umay_drop_Square");
    assert_eq!(drop.params.len(), 1);
}

#[test]
fn owned_dyn_drop_is_rewritten_into_an_indirect_call() {
    let (mut program, hir, mut types) = lowered_with_drop();
    let defs = own_defs(&hir);
    assert!(is_own_drop(&program, &types, &defs), "drop elaboration must see the owning struct");

    umayc::dyn_drop::rewrite_owned_dyn_drops(&mut program, &mut types, &hir);

    assert!(!is_own_drop(&program, &types, &defs), "the owning Drop must be gone");
    let indirect = program.bodies.iter().any(|body| {
        body.blocks.iter().any(|block| match &block.terminator {
            Terminator::Call { callee, args, destination, .. } => {
                destination.is_none()
                    && args.len() == 1
                    && matches!(callee, Operand::Copy(p)
                        if matches!(p.projection.last(), Some(Projection::Deref)))
            }
            _ => false,
        })
    });
    assert!(indirect, "expected an indirect call through the vtable drop slot");

    // The shim's only statement of interest is the runtime deallocation call.
    let drop_fn = &hir.fns[hir.fns.iter().position(|f| f.name == "__umay_drop_Square").unwrap()];
    let calls_dealloc = drop_fn.body.as_ref().unwrap().stmts.iter().any(|s| {
        matches!(&s.kind, umayc::hir::HirStmtKind::Expr(e)
            if matches!(&e.kind, umayc::hir::HirExprKind::Call { callee, .. }
                if matches!(&callee.kind, umayc::hir::HirExprKind::NamedFn(n) if n == "umay_dealloc")))
    }) || drop_fn.body.as_ref().unwrap().tail.as_ref().is_some_and(|e| {
        matches!(&e.kind, umayc::hir::HirExprKind::Call { callee, .. }
            if matches!(&callee.kind, umayc::hir::HirExprKind::NamedFn(n) if n == "umay_dealloc"))
    });
    assert!(calls_dealloc, "the drop shim must free the allocation");
}

fn toolchain_available() -> bool {
    let ok = |name: &str| {
        Command::new(name).arg("--version").output().map(|o| o.status.success()).unwrap_or(false)
    };
    ok("as") && ok("cc")
}

#[test]
fn owned_dyn_still_dispatches_at_runtime() {
    if cfg!(windows) || !toolchain_available() {
        eprintln!("skipping runtime check: no `as` or `cc`");
        return;
    }
    let dir = scratch("runtime");
    let file = dir.join("main.um");
    std::fs::write(&file, OWNED_DYN).expect("write source");
    let build = Command::new(compiler()).arg("build").arg(&file).current_dir(&dir).output().expect("build");
    assert!(build.status.success(), "build failed:\n{}", String::from_utf8_lossy(&build.stderr));
    let out = Command::new(dir.join("dist/main")).output().expect("run");
    assert!(out.status.success(), "run failed: {:?}", out.status);
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim_end(), "25");
    let _ = std::fs::remove_dir_all(&dir);
}
