//! Phase 25: the second async lowering slice — real `Pending` handling for
//! concrete user `Future` impls via a bounded busy-poll executor.
//!
//! The generated-async-fn path stays one-poll (its thunk never returns
//! `Pending`); `block_on(PendingOnce { .. })` instead calls the impl's concrete
//! `poll` in a loop until `Ready`. Both an end-to-end build+run and in-process
//! HIR assertions are used, mirroring `phase19_async.rs` / `phase15_dyn_drop.rs`.
//!
//! Ceilings (documented in `compiler/src/sema/async_lower.rs`): no waker wake,
//! no reactor, busy-poll only, no Send/Pin. `Pending` from a generated async fn
//! still aborts; the new path is only for concrete user Futures.

use std::path::PathBuf;
use std::process::Command;

use umayc::hir::{child_exprs, Hir, HirExpr, HirExprKind, HirStmtKind};
use umayc::source::SourceMap;
use umayc::target::Target;
use umayc::types::{TyCtxt, TyKind};

/// `PendingOnce` comes from the real `stdlib/future.um` via the prelude.
const PENDING_ONCE: &str = "
fn main() -> i32 {
    println_i64(block_on(PendingOnce { done: false }))
    return 0
}
";

/// Regression: the existing generated-async-fn path must stay one-poll.
const ONE: &str = "
async fn one() -> i64 { return 42 }
fn main() -> i32 {
    let v = block_on(one())
    println_i64(v)
    return 0
}
";

const NEVER_READY: &str = "
struct NeverReady {}
impl Future<i64> for NeverReady {
    fn poll(&mut self, _cx: &mut Context) -> Poll<i64> {
        return Poll::Pending
    }
}
fn main() -> i32 {
    let v = block_on(NeverReady {})
    println_i64(v)
    return 0
}
";

fn compiler() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_umay"))
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("umay_phase25_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

/// Parse + prelude + sema, returning HIR for structural assertions.
fn analyze(source: &str) -> (Hir, TyCtxt) {
    let mut map = SourceMap::new();
    let file = map.add("<phase25>", source);
    let (tokens, lex_errors) = umayc::lexer::tokenize(source, file);
    assert!(lex_errors.is_empty(), "{lex_errors:?}");
    let parsed = umayc::parser::parse(tokens, Vec::new(), file);
    let (program, prelude_diags) =
        umayc::prelude::with_prelude(parsed.program, &mut map, Target::LINUX);
    assert!(!prelude_diags.has_errors(), "{prelude_diags:?}");
    let out = umayc::sema::analyze(&program, &map);
    assert!(
        !out.diagnostics.has_errors(),
        "{}",
        umayc::render_all(&out.diagnostics, &map, false)
    );
    (out.hir, out.types)
}

fn toolchain_available() -> bool {
    let ok = |name: &str| {
        Command::new(name).arg("--version").output().map(|o| o.status.success()).unwrap_or(false)
    };
    ok("as") && ok("cc")
}

/// Builds and runs a source file, returning (exit code, stdout, stderr).
fn build_and_run(source: &str, label: &str) -> (i32, String, String) {
    if cfg!(windows) || !toolchain_available() {
        eprintln!("skipping runtime check: no `as` or `cc`");
        return (-1, String::new(), String::new());
    }
    let dir = scratch(label);
    let file = dir.join("main.um");
    std::fs::write(&file, source).expect("write source");
    let build = Command::new(compiler()).arg("build").arg(&file).current_dir(&dir).output().expect("build");
    assert!(
        build.status.success(),
        "build failed:\n{}\n{}",
        String::from_utf8_lossy(&build.stdout),
        String::from_utf8_lossy(&build.stderr)
    );
    let out = Command::new(dir.join("dist/main")).output().expect("run");
    let code = out.status.code().unwrap_or(-1);
    let stdout = String::from_utf8_lossy(&out.stdout).trim_end().to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    let _ = std::fs::remove_dir_all(&dir);
    (code, stdout, stderr)
}

fn run(source: &str, label: &str) -> String {
    let (code, stdout, _) = build_and_run(source, label);
    assert_eq!(code, 0, "program exited with {code}");
    stdout
}

// (a) Pending then resume: first poll is Pending, second returns Ready(7).
#[test]
fn pending_once_resumes_after_pending() {
    assert_eq!(run(PENDING_ONCE, "resume"), "7");
}

// (c) Regression: generated async fn still prints 42 via its one-poll helper.
#[test]
fn generated_async_fn_still_works() {
    assert_eq!(run(ONE, "regression"), "42");
}

// (b) Bounded-loop guard: a future that is always Pending exhausts the budget
// and panics with a clear message instead of spinning forever.
#[test]
fn bounded_loop_guard_panics_on_endless_pending() {
    let (code, _stdout, stderr) = build_and_run(NEVER_READY, "guard");
    if code == -1 {
        return; // toolchain unavailable
    }
    assert_ne!(code, 0, "an endlessly Pending future must not succeed");
    assert!(
        stderr.contains("polled 1000000 times"),
        "expected the bound-exhaustion message, got: {stderr}"
    );
}

// (d) In-process HIR: the helper calls the concrete poll FnId and loops.
#[test]
fn user_future_block_on_calls_concrete_poll_and_loops() {
    let (hir, _types) = analyze(PENDING_ONCE);

    let helper = hir
        .fns
        .iter()
        .find(|f| f.name == "__umay_block_on_user_PendingOnce")
        .expect("a generated user-Future block_on helper");
    assert_eq!(helper.params.len(), 1, "the helper takes the concrete state");
    assert_eq!(helper.locals[0].name, "__state");

    // The helper's HIR references the impl's concrete `poll` method directly.
    let poll_id = hir
        .fns
        .iter()
        .find(|f| f.name == "poll" && f.owner.as_deref() == Some("PendingOnce"))
        .expect("PendingOnce::poll impl method")
        .id;
    let body = helper.body.as_ref().expect("helper has a body");
    let helper_root = HirExpr { kind: HirExprKind::Block(body.clone()), ty: helper.ret, span: body.span };
    let mut all = Vec::new();
    collect_exprs(&helper_root, &mut all);

    assert!(
        all.iter().any(|e| matches!(&e.kind, HirExprKind::MethodCall { method, .. } if *method == poll_id)),
        "helper must call the concrete poll FnId, not a dyn/vtable entry"
    );
    assert!(all.iter().any(|e| matches!(&e.kind, HirExprKind::Loop { .. })), "helper must loop");
    assert!(
        all.iter().any(|e| matches!(&e.kind, HirExprKind::Break(Some(_)))),
        "Ready(v) must break with the value"
    );
    assert!(
        all.iter().any(|e| matches!(&e.kind, HirExprKind::StrLit(s) if s.contains("1000000"))),
        "the bound-exhaustion panic must name the 1,000,000 poll limit"
    );

    // The call site binds the state to a local and calls the helper; it never
    // goes through the generated-async one-poll path.
    let main = hir.fns.iter().find(|f| f.name == "main").expect("main");
    let main_root = HirExpr { kind: HirExprKind::Block(main.body.clone().unwrap()), ty: main.ret, span: main.span };
    let mut main_exprs = Vec::new();
    collect_exprs(&main_root, &mut main_exprs);
    assert!(
        main_exprs.iter().any(|e| matches!(&e.kind, HirExprKind::Call { callee, .. }
            if matches!(&callee.kind, HirExprKind::FnRef(id) if *id == helper.id))),
        "main must call the generated user-Future helper"
    );

    // Sanity: the output type the helper returns is i64.
    assert!(matches!(_types.kind(helper.ret), TyKind::I64), "{:?}", _types.kind(helper.ret));
    // Keep the HirStmtKind import honest (statements live in the helper body).
    assert!(body.stmts.iter().any(|s| matches!(s.kind, HirStmtKind::Let { .. })));
}

fn collect_exprs<'a>(e: &'a umayc::hir::HirExpr, out: &mut Vec<&'a umayc::hir::HirExpr>) {
    out.push(e);
    for child in child_exprs(e) {
        collect_exprs(child, out);
    }
}
