//! Phase 26: real waker invocation plus timer/yield futures.
//!
//! `async_rt.um` is deliberately not in `prelude.rs` (this slice does not own
//! that file), so each test splices its source in front of the user program.
//! UMAY resolves names across the whole compilation unit, so the spliced
//! declarations see `Future`, `Poll`, `Context`, `Waker`, `waker_wake` and
//! `monotonic_ms` from the prelude.
//!
//! `block_on` still hides its context behind `make_context`'s no-op waker. To
//! prove a wake actually ran, the counting-waker tests construct a Context
//! themselves and call the concrete `poll` directly — no `async_lower.rs`
//! overload is needed, and the default context is untouched.

use std::path::PathBuf;
use std::process::Command;

use umayc::hir::{child_exprs, Hir, HirExpr, HirExprKind};
use umayc::source::SourceMap;
use umayc::target::Target;
use umayc::types::{TyCtxt, TyKind};

const ASYNC_RT: &str = include_str!("../../stdlib/async_rt.um");

fn splice(user: &str) -> String {
    format!("{ASYNC_RT}\n{user}")
}

// (a) block_on(YieldNow) -> Pending, wake, then Ready(1).
const YIELD: &str = "
fn main() -> i32 {
    println_i64(block_on(YieldNow { done: false }))
    return 0
}
";

// (b) block_on(sleep(1)) -> 0 once the monotonic clock reaches the deadline.
const SLEEP_BLOCK_ON: &str = "
fn main() -> i32 {
    println_i64(block_on(sleep(1)))
    return 0
}
";

// (b) A direct poll with a counting waker: 100 s in the future, so the poll is
// Pending and the wake callback must fire. Prints [wakes, is_pending].
const SLEEP_WAKE: &str = "
fn main() -> i32 {
    let mut s = sleep(100000)
    let cell = alloc(8)
    write_i64(cell, 0i64)
    let cd = CountWakerData { count: cell as *mut i64 }
    let w = Waker { wake: unsafe { counting_wake as *const u8 }, data: cd.count as *mut u8 }
    let mut cx = Context { waker: w }
    let p = s.poll(&mut cx)
    println_i64(read_i64(cell as *const u8))
    match p {
        Poll::Ready(_) => println_i64(0),
        Poll::Pending => println_i64(1),
    }
    dealloc(cell)
    return 0
}
";

// (c) waker_wake on a counting waker increments exactly once.
const WAKER_WAKE: &str = "
fn main() -> i32 {
    let cell = alloc(8)
    write_i64(cell, 0i64)
    let cd = CountWakerData { count: cell as *mut i64 }
    let w = Waker { wake: unsafe { counting_wake as *const u8 }, data: cd.count as *mut u8 }
    waker_wake(&w)
    println_i64(read_i64(cell as *const u8))
    dealloc(cell)
    return 0
}
";

// (d) Existing regression: PendingOnce still resolves to 7.
const PENDING_ONCE: &str = "
fn main() -> i32 {
    println_i64(block_on(PendingOnce { done: false }))
    return 0
}
";

// (e) Sema-check both new futures through the real prelude + spliced source.
const SEMA: &str = "
fn main() -> i32 {
    let a = block_on(YieldNow { done: false })
    let b = block_on(sleep(1))
    return (a + b) as i32
}
";

fn compiler() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_umay"))
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("umay_phase26_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

/// Parse + prelude + sema, returning HIR for structural assertions.
fn analyze(source: &str) -> (Hir, TyCtxt) {
    let mut map = SourceMap::new();
    let file = map.add("<phase26>", source);
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

// (a) YieldNow wakes once, then yields Ready(1).
#[test]
fn yield_now_block_on_returns_one() {
    assert_eq!(run(&splice(YIELD), "yield"), "1");
}

// (b) sleep(1) resolves to 0 once the monotonic clock crosses the deadline.
#[test]
fn sleep_block_on_returns_zero() {
    assert_eq!(run(&splice(SLEEP_BLOCK_ON), "sleep"), "0");
}

// (b) A direct poll with a counting waker: >= 1 wake and a Pending result.
#[test]
fn sleep_pending_wakes_at_least_once() {
    let out = run(&splice(SLEEP_WAKE), "sleep_wake");
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines.len(), 2, "expected [wakes, is_pending], got {out:?}");
    let wakes: i64 = lines[0].parse().expect("wake count");
    assert!(wakes >= 1, "Sleep::poll must call the waker, got {wakes} wakes");
    assert_eq!(lines[1], "1", "a far-future sleep must return Pending");
}

// (c) waker_wake dispatches through the function pointer exactly once.
#[test]
fn waker_wake_increments_exactly_once() {
    assert_eq!(run(&splice(WAKER_WAKE), "wake"), "1");
}

// (d) The existing PendingOnce path is untouched.
#[test]
fn pending_once_regression_still_prints_seven() {
    assert_eq!(run(PENDING_ONCE, "pending_once"), "7");
}

// (e) In-process sema: spliced source type-checks and lowers both futures.
#[test]
fn spliced_async_rt_sema_and_lowering() {
    let (hir, types) = analyze(&splice(SEMA));

    for name in ["__umay_block_on_user_YieldNow", "__umay_block_on_user_Sleep"] {
        let helper = hir.fns.iter().find(|f| f.name == name).unwrap_or_else(|| panic!("missing {name}"));
        assert!(matches!(types.kind(helper.ret), TyKind::I64), "{name} must return i64");
    }

    let poll_ids: Vec<(String, umayc::hir::FnId)> = hir
        .fns
        .iter()
        .filter(|f| f.name == "poll" && f.owner.as_deref().is_some_and(|o| o == "Sleep" || o == "YieldNow"))
        .map(|f| (f.owner.clone().unwrap_or_default(), f.id))
        .collect();
    let waker_wake = hir.fns.iter().find(|f| f.name == "waker_wake").expect("waker_wake fn").id;
    let monotonic_ms = hir.fns.iter().find(|f| f.name == "monotonic_ms").expect("monotonic_ms fn").id;
    // A trait default method can register another `poll` per type, so require
    // one real impl method that dispatches the wake, not an exact fn count.
    let calls = |id: umayc::hir::FnId, target: umayc::hir::FnId| {
        let poll = hir.fns.iter().find(|f| f.id == id).expect("poll fn");
        // A trait's declared method has no body; only impl methods can wake.
        let Some(body) = poll.body.clone() else {
            return false;
        };
        let root = HirExpr {
            kind: HirExprKind::Block(body),
            ty: poll.ret,
            span: poll.span,
        };
        let mut exprs = Vec::new();
        collect_exprs(&root, &mut exprs);
        exprs.iter().any(|e| matches!(&e.kind, HirExprKind::Call { callee, .. }
            if matches!(&callee.kind, HirExprKind::FnRef(f) if *f == target)))
    };
    for owner in ["Sleep", "YieldNow"] {
        let candidates: Vec<umayc::hir::FnId> =
            poll_ids.iter().filter(|(o, _)| o == owner).map(|(_, id)| *id).collect();
        assert!(!candidates.is_empty(), "{owner}::poll must exist");
        let waking: Vec<umayc::hir::FnId> =
            candidates.into_iter().filter(|id| calls(*id, waker_wake)).collect();
        assert!(!waking.is_empty(), "{owner}::poll must call waker_wake");
        if owner == "Sleep" {
            assert!(
                waking.iter().any(|id| calls(*id, monotonic_ms)),
                "Sleep::poll must read the monotonic clock on the path that wakes"
            );
        }
    }
}

fn collect_exprs<'a>(e: &'a HirExpr, out: &mut Vec<&'a HirExpr>) {
    out.push(e);
    for child in child_exprs(e) {
        collect_exprs(child, out);
    }
}
