//! Part 7 of docs/web-backend-plan.md: cookies, signed values, sessions and
//! password hashes. Each test builds and runs a whole program.

use std::path::Path;
use std::process::Command;

fn toolchain_available() -> bool {
    // The host execution harness uses the Unix assembler/linker and executable
    // names. Windows coverage is the native smoke step in CI.
    if cfg!(windows) {
        return false;
    }
    let ok = |name: &str| Command::new(name).arg("--version").output().map(|o| o.status.success()).unwrap_or(false);
    ok("as") && ok("cc")
}

fn expect_stdout(name: &str, body: &str, expected: &str) {
    if !toolchain_available() {
        return;
    }
    let dir = Path::new("/tmp/umay_web_cookies").join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.um");
    std::fs::write(&file, format!("{SHOW}{body}")).unwrap();
    let build = Command::new(env!("CARGO_BIN_EXE_umay")).arg("build").arg(&file).current_dir(&dir).output().unwrap();
    assert!(build.status.success(), "build failed for {name}:\n{}", String::from_utf8_lossy(&build.stderr));
    let out = Command::new(dir.join("dist").join("main")).current_dir(&dir).output().unwrap();
    assert_eq!(String::from_utf8_lossy(&out.stdout), expected, "stderr:\n{}", String::from_utf8_lossy(&out.stderr));
    assert!(out.status.success(), "exit status {:?}", out.status);
}

const SHOW: &str = r##"
fn show(b: Bytes) {
    let buf = alloc(b.len + 1)
    copy_bytes(b.data, buf, b.len)
    unsafe { write_byte((buf as usize + b.len) as *mut u8, 0u8) }
    println(buf as *const u8)
    dealloc(buf)
}
fn show_opt(o: Option<Bytes>) {
    match o {
        Option::Some(v) => show(v),
        Option::None => println("<none>"),
    }
}
fn yes(b: bool) {
    println(if b { "yes" } else { "no" })
}
// A request whose only header is `Cookie: <cookie>`.
fn with_cookie(cookie: &Bytes) -> Request {
    let buf = alloc(cookie.len + 64)
    let mut at = put_text(buf, 0, Bytes::from_str("GET / HTTP/1.1\r\nCookie: "))
    at = put_text(buf, at, Bytes::view(cookie.data, cookie.len))
    at = put_text(buf, at, Bytes::from_str("\r\n\r\n"))
    return parse_request(Bytes::view(buf as *const u8, at))
}
"##;

#[test]
fn cookies_are_read_and_set_cookie_lines_written() {
    expect_stdout(
        "cookies_read_write",
        r##"
fn main() -> i32 {
    let req = parse_request(Bytes::from_str("GET / HTTP/1.1\r\nCookie: a=1;  sid=xyz ; theme=dark\r\n\r\n"))
    show_opt(req.cookie("sid"))
    show_opt(req.cookie("theme"))
    show_opt(req.cookie("a"))
    show_opt(req.cookie("missing"))
    let bare = parse_request(Bytes::from_str("GET / HTTP/1.1\r\n\r\n"))
    show_opt(bare.cookie("sid"))
    let v = Bytes::from_str("abc")
    let r = Response::new(303).with_header("Location", Bytes::from_str("/home")).with_header("Set-Cookie", cookie_header("sid", &v, 3600i64)).with_header("Set-Cookie", cookie_header("old", &v, 0i64))
    show(r.headers)
    show(cookie_header("tmp", &v, -1i64))
    return 0
}
"##,
        "xyz\ndark\n1\n<none>\n<none>\n\
         Location: /home\r\n\
         Set-Cookie: sid=abc; Path=/; Max-Age=3600; HttpOnly; Secure; SameSite=Lax\r\n\
         Set-Cookie: old=abc; Path=/; Max-Age=0; HttpOnly; Secure; SameSite=Lax\r\n\n\
         tmp=abc; Path=/; HttpOnly; Secure; SameSite=Lax\n",
    );
}

#[test]
fn signed_values_reject_any_tampering() {
    // The tag is HMAC-SHA256("secret", "hello") in base64url, from Python.
    expect_stdout(
        "signed_values",
        r##"
fn main() -> i32 {
    let secret = Bytes::from_str("secret")
    let other = Bytes::from_str("other")
    let value = Bytes::from_str("hello")
    let signed = sign_value(&value, &secret)
    show(signed.from(0))
    show_opt(verify_signed(&signed, &secret))
    show_opt(verify_signed(&signed, &other))
    let forged = Bytes::from_str("hellp.iKqz7ejTrflNJquQ07r9SiCDBww7zOnAFO4EpEOEfAs")
    show_opt(verify_signed(&forged, &secret))
    let bad_tag = Bytes::from_str("hello.iKqz7ejTrflNJquQ07r9SiCDBww7zOnAFO4EpEOEfAt")
    show_opt(verify_signed(&bad_tag, &secret))
    let no_dot = Bytes::from_str("hello")
    show_opt(verify_signed(&no_dot, &secret))
    return 0
}
"##,
        "hello.iKqz7ejTrflNJquQ07r9SiCDBww7zOnAFO4EpEOEfAs\nhello\n<none>\n<none>\n<none>\n<none>\n",
    );
}

#[test]
fn sessions_carry_the_user_until_they_expire() {
    expect_stdout(
        "sessions",
        r##"
fn main() -> i32 {
    let secret = Bytes::from_str("k3y")
    let user = Bytes::from_str("ayse; admin=1")
    let token = session_token(&user, 60i64, &secret)
    let cookie = Bytes::from_str("sid=")
    let buf = alloc(token.len + 8)
    let mut at = put_text(buf, 0, cookie)
    at = put_text(buf, at, Bytes::view(token.data, token.len))
    let header = Bytes::view(buf as *const u8, at)
    let req = with_cookie(&header)
    show_opt(session_user(&req, "sid", &secret))
    let wrong = Bytes::from_str("nope")
    show_opt(session_user(&req, "sid", &wrong))
    show_opt(session_user(&req, "other", &secret))
    let old = session_token(&user, -5i64, &secret)
    let buf2 = alloc(old.len + 8)
    let mut at2 = put_text(buf2, 0, Bytes::from_str("sid="))
    at2 = put_text(buf2, at2, Bytes::view(old.data, old.len))
    let header2 = Bytes::view(buf2 as *const u8, at2)
    let expired = with_cookie(&header2)
    show_opt(session_user(&expired, "sid", &secret))
    return 0
}
"##,
        "ayse; admin=1\n<none>\n<none>\n<none>\n",
    );
}

#[test]
fn password_hashes_verify_and_match_python() {
    // The stored hash was made by Python's hashlib.pbkdf2_hmac with a fixed
    // salt, so this also checks the format and both base64url fields.
    expect_stdout(
        "passwords",
        r##"
fn main() -> i32 {
    let pw = Bytes::from_str("pw")
    let python = Bytes::from_str("pbkdf2-sha256$1000$MDEyMzQ1Njc4OWFiY2RlZg$g4BiOBrcHGnGTAdCcoA-WrLOFA69L2c9HA5vVpt2o7A")
    yes(password_verify(&pw, &python))
    let wrong = Bytes::from_str("pW")
    yes(password_verify(&wrong, &python))
    let stored = password_hash(&pw, 1000u32)
    yes(stored.starts_with(Bytes::from_str("pbkdf2-sha256$1000$")))
    yes(password_verify(&pw, &stored))
    yes(password_verify(&wrong, &stored))
    let again = password_hash(&pw, 1000u32)
    yes(stored.equals(&again))
    let broken = Bytes::from_str("pbkdf2-sha256$1000$MDEy")
    yes(password_verify(&pw, &broken))
    let zero = Bytes::from_str("pbkdf2-sha256$0$MDEyMzQ1Njc4OWFiY2RlZg$g4BiOBrcHGnGTAdCcoA-WrLOFA69L2c9HA5vVpt2o7A")
    yes(password_verify(&pw, &zero))
    return 0
}
"##,
        "yes\nno\nyes\nyes\nno\nno\nno\nno\n",
    );
}
