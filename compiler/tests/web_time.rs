//! `stdlib/time.um`: clocks, HTTP dates and random bytes. Each test builds and
//! runs a whole program through `umay build`, like `web_backend.rs`.

use std::path::Path;
use std::process::Command;

fn toolchain_available() -> bool {
    let ok = |name: &str| Command::new(name).arg("--version").output().map(|o| o.status.success()).unwrap_or(false);
    ok("as") && ok("cc")
}

/// Builds `source`, runs it, and returns what it printed, or `None` without a toolchain.
fn run_stdout(name: &str, source: &str) -> Option<String> {
    if !toolchain_available() {
        return None;
    }
    let dir = Path::new("/tmp/umay_web_time").join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.um");
    std::fs::write(&file, source).unwrap();
    let build = Command::new(env!("CARGO_BIN_EXE_umay")).arg("build").arg(&file).current_dir(&dir).output().unwrap();
    assert!(build.status.success(), "build failed for {name}:\n{}", String::from_utf8_lossy(&build.stderr));
    let out = Command::new(dir.join("dist").join("main")).current_dir(&dir).output().unwrap();
    assert!(out.status.success(), "exit status {:?}\nstderr:\n{}", out.status, String::from_utf8_lossy(&out.stderr));
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

#[test]
fn http_date_formats_imf_fixdate_across_leap_years() {
    let source = r##"
fn show_date(t: i64) {
    let buf = alloc(30)
    let n = http_date(t, buf)
    unsafe { write_byte((buf as usize + n) as *mut u8, 0u8) }
    println(buf as *const u8)
    dealloc(buf)
}
fn main() -> i32 {
    show_date(0i64)
    show_date(784111777i64)
    show_date(951782400i64)
    show_date(1709210096i64)
    show_date(4102444799i64)
    return 0
}
"##;
    // Checked against Python's `datetime.fromtimestamp(t, timezone.utc)`.
    let expected = "Thu, 01 Jan 1970 00:00:00 GMT\nSun, 06 Nov 1994 08:49:37 GMT\nTue, 29 Feb 2000 00:00:00 GMT\n\
                    Thu, 29 Feb 2024 12:34:56 GMT\nThu, 31 Dec 2099 23:59:59 GMT\n";
    if let Some(out) = run_stdout("http_date", source) {
        assert_eq!(out, expected);
    }
}

#[test]
fn clocks_read_the_wall_time_and_never_go_backwards() {
    let source = r##"
fn main() -> i32 {
    println_i64(unix_now())
    let a = monotonic_ms()
    let mut i: i64 = 0
    while i < 1000000 {
        i = i + 1
    }
    let b = monotonic_ms()
    println_i64(a)
    println_i64(b)
    return 0
}
"##;
    let before = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64;
    let Some(out) = run_stdout("clocks", source) else { return };
    let after = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64;
    let v: Vec<i64> = out.lines().map(|l| l.parse().unwrap()).collect();
    assert!(v[0] >= before - 1 && v[0] <= after + 1, "unix_now {} not in [{before}, {after}]", v[0]);
    assert!(v[1] > 0 && v[2] >= v[1], "monotonic went backwards: {} then {}", v[1], v[2]);
}

#[test]
fn random_hex_gives_distinct_lowercase_tokens() {
    let source = r##"
fn main() -> i32 {
    let a = random_hex(16)
    let b = random_hex(16)
    println(a.data)
    println(b.data)
    println_i64(a.len as i64)
    if random_fill(alloc(1000), 1000) {
        println("filled")
    }
    return 0
}
"##;
    let Some(out) = run_stdout("random_hex", source) else { return };
    let lines: Vec<&str> = out.lines().collect();
    for token in &lines[..2] {
        assert_eq!(token.len(), 32, "{token}");
        assert!(token.bytes().all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c)), "{token}");
    }
    assert_ne!(lines[0], lines[1]);
    assert_eq!(&lines[2..], ["32", "filled"]);
}

#[test]
fn a_constant_wider_than_32_bits_is_not_an_instruction_immediate() {
    // Windows' FILETIME epoch offset. `subq $11644473600, %rcx` does not
    // assemble: only `movabsq` takes a 64-bit immediate.
    let source = r##"
fn shift_epoch(x: i64) -> i64 {
    if x == 4294967296i64 {
        return 0i64
    }
    return (x + 116444736000000000i64) * 3i64 - 11644473600i64
}
fn main() -> i32 {
    println_i64(shift_epoch(5i64))
    return 0
}
"##;
    if let Some(out) = run_stdout("wide_immediate", source) {
        assert_eq!(out, format!("{}\n", (5i64 + 116444736000000000) * 3 - 11644473600));
    }
}
