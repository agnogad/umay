//! Differential testing harness for the x86_64 backend.
//!
//! Generates deterministic well-typed UMAY programs, evaluates them with a
//! built-in reference interpreter (the oracle), compiles and runs each program
//! with `umay build -O0` and `umay build -O2`, and compares the outputs.

#[path = "support/progen.rs"]
mod progen;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

fn compiler() -> &'static str {
    env!("CARGO_BIN_EXE_umay")
}

fn toolchain_available() -> bool {
    let ok = |name: &str| {
        Command::new(name)
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    };
    ok("as") && ok("cc")
}

#[derive(Debug, PartialEq, Eq)]
enum ExecOutcome {
    Ok { stdout: String, status: i32 },
    BuildFailed(String),
    ExecFailed(String),
}

fn build_and_run(dir: &Path, src: &str, opt: &str) -> ExecOutcome {
    let _ = std::fs::remove_dir_all(dir);
    std::fs::create_dir_all(dir).expect("create scratch dir");
    let file = dir.join("main.um");
    std::fs::write(&file, src).expect("write main.um");

    let build = Command::new(compiler())
        .arg("build")
        .arg(opt)
        .arg(&file)
        .current_dir(dir)
        .output()
        .expect("invoke umay build");

    if !build.status.success() {
        return ExecOutcome::BuildFailed(format!(
            "exit {:?}\nstdout:\n{}\nstderr:\n{}",
            build.status.code(),
            String::from_utf8_lossy(&build.stdout),
            String::from_utf8_lossy(&build.stderr)
        ));
    }

    let exe = dir.join("dist").join("main");
    let out = match Command::new(&exe).current_dir(dir).output() {
        Ok(o) => o,
        Err(e) => return ExecOutcome::ExecFailed(format!("spawn failed: {e}")),
    };

    let status = out.status.code().unwrap_or_else(|| {
        #[cfg(unix)]
        {
            use std::os::unix::process::ExitStatusExt;
            128 + out.status.signal().unwrap_or(0)
        }
        #[cfg(not(unix))]
        {
            -1
        }
    });

    ExecOutcome::Ok {
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        status,
    }
}

fn matches_oracle(outcome: &ExecOutcome, oracle: &str) -> bool {
    // Windows writes `\r\n` to a captured pipe; the oracle is written with
    // `\n`. The line endings are the host's, not the program's answer.
    matches!(
        outcome,
        ExecOutcome::Ok { stdout, status: 0 } if stdout.replace("\r\n", "\n") == oracle
    )
}

fn run_one_seed(
    seed: u64,
    work_root: &Path,
    fail_root: &Path,
    do_shrink: bool,
) -> Result<(), String> {
    let prog = progen::gen_program(seed);
    let oracle = progen::eval_program(&prog)
        .unwrap_or_else(|e| panic!("seed {seed}: generator produced undefined program: {e:?}"));
    let src = progen::emit_program(&prog);

    let dir_o0 = work_root.join("o0");
    let dir_o2 = work_root.join("o2");
    let res_o0 = build_and_run(&dir_o0, &src, "-O0");
    let res_o2 = build_and_run(&dir_o2, &src, "-O2");

    if matches_oracle(&res_o0, &oracle) && matches_oracle(&res_o2, &oracle) {
        return Ok(());
    }

    let (final_prog, final_oracle, final_src, final_o0, final_o2) = if do_shrink {
        let shrunk = progen::shrink_program(&prog, |cand, cand_oracle| {
            let cand_src = progen::emit_program(cand);
            let o0 = build_and_run(&dir_o0, &cand_src, "-O0");
            let o2 = build_and_run(&dir_o2, &cand_src, "-O2");
            !matches_oracle(&o0, cand_oracle) || !matches_oracle(&o2, cand_oracle)
        });
        let s_oracle = progen::eval_program(&shrunk).expect("shrunk program valid");
        let s_src = progen::emit_program(&shrunk);
        let s_o0 = build_and_run(&dir_o0, &s_src, "-O0");
        let s_o2 = build_and_run(&dir_o2, &s_src, "-O2");
        (shrunk, s_oracle, s_src, s_o0, s_o2)
    } else {
        (prog, oracle, src, res_o0, res_o2)
    };

    let case_dir = fail_root.join(format!("seed_{seed}"));
    std::fs::create_dir_all(&case_dir).expect("create failure dir");
    std::fs::write(case_dir.join("main.um"), &final_src).expect("write main.um");
    std::fs::write(case_dir.join("seed.txt"), format!("{}\n", final_prog.seed)).expect("write seed");
    std::fs::write(case_dir.join("oracle.txt"), &final_oracle).expect("write oracle");
    std::fs::write(case_dir.join("o0.txt"), format!("{final_o0:?}\n")).expect("write o0");
    std::fs::write(case_dir.join("o2.txt"), format!("{final_o2:?}\n")).expect("write o2");

    Err(format!(
        "seed {seed} mismatch (saved to {}):\n  oracle={:?}\n  -O0={:?}\n  -O2={:?}",
        case_dir.display(),
        final_oracle,
        final_o0,
        final_o2
    ))
}

/// Differential fuzzing as a CI gate.
///
/// Every run compares the reference interpreter with both `-O0` and `-O2` on
/// the same generated programs. It found a real backend bug (8-bit `imul`,
/// now fixed) and later two front-end bugs (a `<` comparison mistaken for
/// generic arguments and a borrow-liveness false positive), both fixed, so
/// the generator now produces valid, well-typed UMAY for every seed checked.
/// Failures are saved under `UMAY_DIFF_OUT` or the system temp directory.
#[test]
fn backend_differential_ci() {
    // A fixed default keeps CI meaningful; the override is for a human running
    // a longer or shorter sweep locally. The ignored `long_run` test is the
    // place for a multi-thousand-seed sweep.
    let count = std::env::var("UMAY_DIFF_COUNT")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(30);
    if !toolchain_available() {
        eprintln!("skipping backend_differential_ci: `as` or `cc` is missing");
        return;
    }

    let start = Instant::now();
    let pid = std::process::id();
    let work_root = std::env::temp_dir().join(format!("umay_diff_ci_{pid}"));
    let fail_root = std::env::temp_dir().join(format!("umay_diff_ci_fail_{pid}"));
    let mut failures = Vec::new();
    let shrink = std::env::var("UMAY_DIFF_SHRINK").is_ok();
    for seed in 1..=count {
        // Shrinking is for a human investigating a saved case; in CI it adds
        // dozens of builds to a failure and hides which seed broke first.
        if let Err(msg) = run_one_seed(seed, &work_root, &fail_root, shrink) {
            failures.push(msg);
        }
    }

    let _ = std::fs::remove_dir_all(&work_root);
    eprintln!(
        "backend_differential_ci: {count} seeds checked in {:.2}s",
        start.elapsed().as_secs_f64()
    );

    assert!(
        failures.is_empty(),
        "differential failures ({}):\n{}",
        failures.len(),
        failures.join("\n\n")
    );
}

#[test]
#[ignore]
fn backend_differential_long_run() {
    if !toolchain_available() {
        eprintln!("skipping backend_differential_long_run: `as` or `cc` is missing");
        return;
    }

    let count: u64 = std::env::var("UMAY_DIFF_COUNT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(2000);
    let start_seed: u64 = std::env::var("UMAY_DIFF_SEED")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1000);
    let fail_root: PathBuf = std::env::var("UMAY_DIFF_OUT")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/tmp/umay_diff_failures"));

    let _ = std::fs::remove_dir_all(&fail_root);
    let pid = std::process::id();
    let work_root = std::env::temp_dir().join(format!("umay_diff_long_{pid}"));

    let start = Instant::now();
    let mut failures = Vec::new();

    for offset in 0..count {
        let seed = start_seed + offset;
        if let Err(msg) = run_one_seed(seed, &work_root, &fail_root, true) {
            eprintln!("[FAIL] {msg}");
            failures.push(msg);
        }
        if (offset + 1) % 100 == 0 {
            eprintln!(
                "progress: {}/{} ({:.1}s, {} failures)",
                offset + 1,
                count,
                start.elapsed().as_secs_f64(),
                failures.len()
            );
        }
    }

    let _ = std::fs::remove_dir_all(&work_root);
    eprintln!(
        "long run finished: {count} programs in {:.2}s, {} failures (saved in {})",
        start.elapsed().as_secs_f64(),
        failures.len(),
        fail_root.display()
    );

    assert!(
        failures.is_empty(),
        "found {} differential failure(s); see {}",
        failures.len(),
        fail_root.display()
    );
}

/// The generator used to emit assignments that conflicted with borrows:
/// `s.f = f(&mut s, ...)`, `s.f = s.g + 1`, and assignments to a variable that
/// is mutably borrowed elsewhere in the same body. Check the generated AST
/// directly so a regression is caught without building 2000 programs.
#[test]
fn generated_assignments_never_conflict_with_borrows() {
    fn expr_mentions(expr: &progen::Expr, name: &str) -> bool {
        match expr {
            progen::Expr::Var(v) => v == name,
            progen::Expr::Lit(_) => false,
            progen::Expr::Cast { expr, .. } | progen::Expr::Unary { expr, .. } => {
                expr_mentions(expr, name)
            }
            progen::Expr::Binary { lhs, rhs, .. } => {
                expr_mentions(lhs, name) || expr_mentions(rhs, name)
            }
            progen::Expr::FieldAccess { var, .. } => var == name,
            progen::Expr::ArrayIndex { var, index } => {
                var == name || expr_mentions(index, name)
            }
            progen::Expr::Call { args, .. } => {
                args.iter().any(|arg| expr_mentions(arg, name))
            }
            progen::Expr::MethodCall { receiver, args, .. } => {
                receiver == name || args.iter().any(|arg| expr_mentions(arg, name))
            }
            progen::Expr::StructLit { fields, .. } => {
                fields.iter().any(|field| expr_mentions(field, name))
            }
            progen::Expr::ArrayLit { init, .. } => expr_mentions(init, name),
        }
    }

    fn all_stmts<'a>(stmts: &'a [progen::Stmt], out: &mut Vec<&'a progen::Stmt>) {
        for stmt in stmts {
            out.push(stmt);
            match stmt {
                progen::Stmt::If { then_body, else_body, .. } => {
                    all_stmts(then_body, out);
                    all_stmts(else_body, out);
                }
                progen::Stmt::While { body, .. } => all_stmts(body, out),
                _ => {}
            }
        }
    }

    fn collect_expr_borrows(
        expr: &progen::Expr,
        prog: &progen::Program,
        borrowed: &mut std::collections::BTreeSet<String>,
    ) {
        match expr {
            progen::Expr::Lit(_) | progen::Expr::Var(_) | progen::Expr::FieldAccess { .. } => {}
            progen::Expr::Cast { expr, .. } | progen::Expr::Unary { expr, .. } => {
                collect_expr_borrows(expr, prog, borrowed)
            }
            progen::Expr::Binary { lhs, rhs, .. } => {
                collect_expr_borrows(lhs, prog, borrowed);
                collect_expr_borrows(rhs, prog, borrowed);
            }
            progen::Expr::ArrayIndex { index, .. } => collect_expr_borrows(index, prog, borrowed),
            progen::Expr::Call { func, args } => {
                let params = &prog.funcs[*func].params;
                for (i, arg) in args.iter().enumerate() {
                    if params[i].is_mut_ref {
                        if let progen::Expr::Var(name) = arg {
                            borrowed.insert(name.clone());
                        }
                    }
                    collect_expr_borrows(arg, prog, borrowed);
                }
            }
            progen::Expr::MethodCall { receiver, struct_id, method, args } => {
                if prog.structs[*struct_id].methods[*method].is_mut_self {
                    borrowed.insert(receiver.clone());
                }
                for arg in args {
                    collect_expr_borrows(arg, prog, borrowed);
                }
            }
            progen::Expr::StructLit { fields, .. } => {
                for field in fields {
                    collect_expr_borrows(field, prog, borrowed);
                }
            }
            progen::Expr::ArrayLit { init, .. } => collect_expr_borrows(init, prog, borrowed),
        }
    }

    fn body_has_conflict(stmts: &[progen::Stmt], prog: &progen::Program) -> bool {
        let mut flat = Vec::new();
        all_stmts(stmts, &mut flat);

        let mut borrowed = std::collections::BTreeSet::new();
        for stmt in &flat {
            match stmt {
                progen::Stmt::Let { init, .. }
                | progen::Stmt::Return(init)
                | progen::Stmt::Print(init)
                | progen::Stmt::Expr(init) => collect_expr_borrows(init, prog, &mut borrowed),
                progen::Stmt::Assign { target, value } => {
                    if let progen::LValue::Index(_, index) = target {
                        collect_expr_borrows(index, prog, &mut borrowed);
                    }
                    collect_expr_borrows(value, prog, &mut borrowed);
                }
                progen::Stmt::If { cond, .. } | progen::Stmt::While { cond, .. } => {
                    collect_expr_borrows(cond, prog, &mut borrowed)
                }
            }
        }

        flat.iter().any(|stmt| match stmt {
            progen::Stmt::Assign { target, value } => {
                let name = match target {
                    progen::LValue::Var(name)
                    | progen::LValue::Field(name, _)
                    | progen::LValue::Index(name, _) => name,
                };
                borrowed.contains(name)
                    || expr_mentions(value, name)
                    || matches!(target, progen::LValue::Index(_, index) if expr_mentions(index, name))
            }
            _ => false,
        })
    }

    for seed in 1..=2000 {
        let prog = progen::gen_program(seed);
        let conflict = prog
            .funcs
            .iter()
            .any(|f| body_has_conflict(&f.body, &prog))
            || prog.structs.iter().any(|s| {
                s.methods.iter().any(|m| body_has_conflict(&m.body, &prog))
            })
            || body_has_conflict(&prog.main_stmts, &prog);
        assert!(!conflict, "seed {seed} generated a conflicting assignment");
    }
}
