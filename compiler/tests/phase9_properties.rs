//! Properties that hold for every program, checked over many generated ones.
//!
//! The tests elsewhere in the suite each check one thing someone thought of. These
//! check statements that should hold whatever the numbers are -- arithmetic agrees
//! with the host, signed comparison agrees with the host, and a program that
//! type-checks builds and runs -- over a fixed, reproducible sample.
//!
//! Reproducible is the point: the generator seeds a plain integer rather than
//! reading entropy, so a failure names a case that can be re-run. A random test
//! whose failure cannot be reproduced is a rumour.

use std::path::PathBuf;
use std::process::Command;

fn compiler() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_umay"))
}

fn have_toolchain() -> bool {
    Command::new("cc")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// A small deterministic generator, so a failure can be reproduced.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn range(&mut self, low: i64, high: i64) -> i64 {
        let span = (high - low + 1) as u64;
        low + (self.next() % span) as i64
    }
}

struct Runner {
    dir: PathBuf,
}

impl Runner {
    fn new(tag: &str) -> Runner {
        let dir = std::env::temp_dir().join(format!("umay_prop_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create the directory");
        Runner { dir }
    }

    /// Builds and runs `source`, returning its exit status and output.
    fn run(&self, source: &str) -> (i32, String, String) {
        std::fs::write(self.dir.join("t.um"), source).expect("write the source");
        let output = Command::new(compiler())
            .current_dir(&self.dir)
            .env("UMAY_CACHE_DIR", self.dir.join("cache"))
            .args(["run", "t.um"])
            .output()
            .expect("run umay run");
        (
            output.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&output.stdout).into_owned(),
            String::from_utf8_lossy(&output.stderr).into_owned(),
        )
    }
}

/// A program that returns 0 if every `(a, b, op, expected)` holds.
///
/// One build per sample instead of one per case: forty programs that each check
/// twenty-four comparisons take forty seconds, which is too slow for a suite that
/// is run on every change. The cases are identical either way.
fn all_hold(cases: &[(i64, i64, &str, bool)]) -> String {
    let mut body = String::from("fn main() -> i32 {\n");
    for (a, b, op, expected) in cases {
        let truth = if *expected { "true" } else { "false" };
        body.push_str(&format!(
            "    if ({a}i64 {op} {b}i64) == {truth} {{ }} else {{ return 1 }}\n"
        ));
    }
    body.push_str("    return 0\n}\n");
    body
}

#[test]
fn signed_comparison_agrees_with_the_host() {
    if !have_toolchain() {
        eprintln!("skipping signed_comparison_agrees_with_the_host: `cc` is not available");
        return;
    }
    let runner = Runner::new("signed_cmp");
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let mut cases = Vec::new();
    for _ in 0..40 {
        // Both signs, and both sides of zero, which is where a comparison that
        // forgets about sign goes wrong.
        let a = rng.range(-2_000_000_000, 2_000_000_000);
        let b = rng.range(-2_000_000_000, 2_000_000_000);
        for (op, expected) in [
            ("<", a < b),
            (">", a > b),
            ("<=", a <= b),
            (">=", a >= b),
            ("==", a == b),
            ("!=", a != b),
        ] {
            cases.push((a, b, op, expected));
        }
    }
    assert_eq!(cases.len(), 240);
    let (status, _, stderr) = runner.run(&all_hold(&cases));
    assert_eq!(status, 0, "a comparison disagreed with the host:\n{stderr}");
}

#[test]
fn unsigned_comparison_treats_the_sign_bit_as_a_value() {
    if !have_toolchain() {
        eprintln!("skipping unsigned_comparison_treats_the_sign_bit_as_a_value: `cc` is not available");
        return;
    }
    // A value with the top bit set is larger than every small value when read
    // unsigned, and smaller when read signed. Confusing the two is the classic
    // backend bug, so it gets its own case rather than relying on random draws.
    let runner = Runner::new("unsigned_cmp");
    let program = "fn main() -> i32 {\n    let a = 18446744073709551615u64\n    let b = 1u64\n    if a > b {\n        return 0\n    }\n    return 1\n}\n";
    let (status, _, stderr) = runner.run(program);
    assert_eq!(status, 0, "2^64-1 should compare greater than 1 unsigned:\n{stderr}");
}

#[test]
fn arithmetic_agrees_with_the_host() {
    if !have_toolchain() {
        eprintln!("skipping arithmetic_agrees_with_the_host: `cc` is not available");
        return;
    }
    let runner = Runner::new("arith");
    let mut rng = Rng(0x2545_F491_4F6C_DD1D);
    let mut body = String::from("fn main() -> i32 {\n");
    for _ in 0..40 {
        let a = rng.range(-100_000, 100_000);
        let b = rng.range(-100_000, 100_000);
        for (op, expected) in [("+", a + b), ("-", a - b), ("*", a * b)] {
            body.push_str(&format!(
                "    if ({a}i64 {op} {b}i64) == {expected}i64 {{ }} else {{ return 1 }}\n"
            ));
        }
    }
    body.push_str("    return 0\n}\n");
    let (status, _, stderr) = runner.run(&body);
    assert_eq!(status, 0, "an arithmetic result disagreed with the host:\n{stderr}");
}

#[test]
fn arithmetic_through_a_function_matches_arithmetic_inline() {
    if !have_toolchain() {
        eprintln!("skipping arithmetic_through_a_function_matches_arithmetic_inline: `cc` is not available");
        return;
    }
    // Calls pass values in registers and locals live on the stack; a bug that
    // only appears across a call boundary is invisible to an inline test.
    let runner = Runner::new("call_boundary");
    let mut rng = Rng(0xDEAD_BEEF_CAFE_1234);
    for _ in 0..25 {
        let a = rng.range(-50_000, 50_000);
        let b = rng.range(-50_000, 50_000);
        let expected = a * b + a - b;
        let program = format!(
            "fn combine(a: i64, b: i64) -> i64 {{\n    return a * b + a - b\n}}\n\nfn main() -> i32 {{\n    let r = combine({a}i64, {b}i64)\n    if r == {expected}i64 {{\n        return 0\n    }}\n    return 1\n}}\n"
        );
        let (status, _, stderr) = runner.run(&program);
        assert_eq!(status, 0, "combine({a}, {b}) should be {expected}:\n{stderr}");
    }
}

#[test]
fn every_generated_program_that_type_checks_builds_and_runs() {
    if !have_toolchain() {
        eprintln!("skipping every_generated_program_that_type_checks_builds_and_runs: `cc` is not available");
        return;
    }
    // The property is not about particular numbers, it is about the pipeline: a
    // program the frontend accepts must not fail later for a reason belonging to
    // a different stage. Mixing types and operations is the cheapest way to
    // generate many shapes of legal program.
    let runner = Runner::new("pipeline");
    let types = ["i8", "i16", "i32", "i64", "u8", "u16", "u32", "u64"];
    let mut rng = Rng(0x1234_5678_9ABC_DEF0);
    for _ in 0..30 {
        let ty = types[(rng.next() % types.len() as u64) as usize];
        let value = rng.range(0, 100);
        let other = rng.range(0, 100);
        let program = format!(
            "fn main() -> i32 {{\n    let a: {ty} = {value} as {ty}\n    let b: {ty} = {other} as {ty}\n    let c = a + b\n    if c == {sum} as {ty} {{\n        return 0\n    }}\n    return 1\n}}\n",
            sum = value + other
        );
        let (status, _, stderr) = runner.run(&program);
        assert_eq!(status, 0, "a legal program failed to run ({ty}):\n{stderr}\n{program}");
    }
}
