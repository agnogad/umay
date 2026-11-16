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
    matches!(outcome, ExecOutcome::Ok { stdout, status: 0 } if stdout == oracle)
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

/// A manual differential fuzzer, not a CI gate.
///
/// It found a real backend bug (8-bit `imul`, now fixed), but the generator is
/// still incomplete: some seeds emit programs the type checker rightly refuses
/// and seed 3 produces a program that hangs `umay check`. Until those are
/// resolved this test runs only when `UMAY_DIFF_COUNT` is set explicitly.
/// Failures are saved under `UMAY_DIFF_OUT` or the system temp directory.
#[test]
fn backend_differential_ci() {
    let Some(count) = std::env::var("UMAY_DIFF_COUNT")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
    else {
        eprintln!("skipping backend_differential_ci: set UMAY_DIFF_COUNT=N to run N seeds");
        return;
    };
    if !toolchain_available() {
        eprintln!("skipping backend_differential_ci: `as` or `cc` is missing");
        return;
    }

    let start = Instant::now();
    let pid = std::process::id();
    let work_root = std::env::temp_dir().join(format!("umay_diff_ci_{pid}"));
    let fail_root = std::env::temp_dir().join(format!("umay_diff_ci_fail_{pid}"));
    let mut failures = Vec::new();
    for seed in 1..=count {
        // Shrinking is for a human investigating a saved case; in CI it adds
        // dozens of builds to a failure and hides which seed broke first.
        if let Err(msg) = run_one_seed(seed, &work_root, &fail_root, false) {
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
