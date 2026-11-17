//! Capstone integration test for `examples/web-app`: accounts, signed
//! cookie sessions, SQLite-backed notes API, HTML templates, and static files.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

fn available() -> bool {
    let ok = |name: &str| {
        Command::new(name)
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    };
    let has_sqlite = Command::new("cc")
        .args(["-x", "c", "-", "-lsqlite3", "-o", "/dev/null"])
        .stdin(Stdio::piped())
        .spawn()
        .and_then(|mut c| {
            c.stdin.take().unwrap().write_all(
                b"int sqlite3_libversion_number(void); int main(void){return sqlite3_libversion_number()==0;}",
            )?;
            c.wait()
        })
        .map(|s| s.success())
        .unwrap_or(false);
    ok("as") && ok("cc") && has_sqlite
}

struct ServerGuard {
    child: Option<Child>,
}

impl ServerGuard {
    fn new(child: Child) -> Self {
        Self { child: Some(child) }
    }

    fn pid(&self) -> u32 {
        self.child.as_ref().expect("server still owned").id()
    }

    fn wait_exit(&mut self, timeout: Duration) -> (ExitStatus, String) {
        let mut child = self.child.take().expect("server already waited");
        let deadline = Instant::now() + timeout;
        let status = loop {
            match child.try_wait().expect("try_wait server") {
                Some(status) => break status,
                None if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(20));
                }
                None => {
                    let _ = child.kill();
                    let _ = child.wait();
                    panic!("server did not exit within {timeout:?}");
                }
            }
        };
        let mut stderr = String::new();
        if let Some(mut err) = child.stderr.take() {
            let _ = err.read_to_string(&mut stderr);
        }
        (status, stderr)
    }
}

impl Drop for ServerGuard {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("compiler crate sits inside the repository")
        .to_path_buf()
}

fn copy_dir_all(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).unwrap();
    for entry in std::fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        if src_path.is_dir() {
            copy_dir_all(&src_path, &dst_path);
        } else {
            std::fs::copy(&src_path, &dst_path).unwrap();
        }
    }
}

fn prepare_app(name: &str) -> (PathBuf, PathBuf) {
    let dir = Path::new("/tmp/umay_web_app").join(format!("{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    let app_src = repo_root().join("examples").join("web-app");
    copy_dir_all(&app_src.join("views"), &dir.join("views"));
    copy_dir_all(&app_src.join("public"), &dir.join("public"));

    let main_um = app_src.join("main.um");
    let build = Command::new(env!("CARGO_BIN_EXE_umay"))
        .arg("build")
        .arg(&main_um)
        .current_dir(&dir)
        .output()
        .unwrap();
    assert!(
        build.status.success(),
        "build failed for web-app:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let bin = dir.join("dist").join("main");
    (dir, bin)
}

fn test_port(slot: u16) -> u16 {
    26000u16 + ((std::process::id() % 4000) as u16) * 4 + slot
}

fn connect_with_retry(port: u16) -> TcpStream {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        match TcpStream::connect(("127.0.0.1", port)) {
            Ok(stream) => return stream,
            Err(_) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(15));
            }
            Err(e) => panic!("failed to connect to 127.0.0.1:{port}: {e}"),
        }
    }
}

struct HttpResp {
    status: u16,
    headers: String,
    body: String,
}

fn send_http(port: u16, method: &str, path: &str, headers: &[(&str, &str)], body: &str) -> HttpResp {
    let mut raw = format!("{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n");
    for (k, v) in headers {
        raw.push_str(&format!("{k}: {v}\r\n"));
    }
    if !body.is_empty() || method == "POST" {
        raw.push_str(&format!("Content-Length: {}\r\n", body.len()));
    }
    // The helper reads to EOF, so ask for a close rather than leave the
    // connection open under HTTP/1.1 keep-alive. The header goes before the
    // blank line that ends the head, not after it.
    raw.push_str("Connection: close\r\n");
    raw.push_str("\r\n");
    raw.push_str(body);

    let mut stream = connect_with_retry(port);
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    stream.write_all(raw.as_bytes()).unwrap();
    stream.flush().unwrap();

    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();

    let (head, resp_body) = response
        .split_once("\r\n\r\n")
        .unwrap_or_else(|| panic!("malformed HTTP response: {response:?}"));
    let status_line = head.lines().next().unwrap_or("");
    let status: u16 = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or_else(|| panic!("bad status line: {status_line:?}"));

    HttpResp {
        status,
        headers: head.to_string(),
        body: resp_body.to_string(),
    }
}

fn header_value(resp: &HttpResp, name: &str) -> Option<String> {
    let want = name.to_ascii_lowercase();
    for line in resp.headers.lines().skip(1) {
        if let Some((k, v)) = line.split_once(':') {
            if k.trim().to_ascii_lowercase() == want {
                return Some(v.trim().to_string());
            }
        }
    }
    None
}

fn extract_cookie_pair(resp: &HttpResp) -> String {
    let set_cookie = header_value(resp, "Set-Cookie")
        .unwrap_or_else(|| panic!("expected Set-Cookie header in:\n{}", resp.headers));
    set_cookie
        .split(';')
        .next()
        .unwrap()
        .trim()
        .to_string()
}

fn send_sigterm(pid: u32) {
    let status = Command::new("kill")
        .arg("-TERM")
        .arg(pid.to_string())
        .status()
        .expect("run kill");
    assert!(status.success(), "kill -TERM {pid} failed: {status:?}");
}

#[test]
fn accounts_notes_api_templates_and_static_files() {
    if !available() {
        return;
    }
    let (dir, bin) = prepare_app("main_flow");
    let db_path = dir.join("test.db");
    let port = test_port(0);

    let child = Command::new(&bin)
        .current_dir(&dir)
        .env("PORT", port.to_string())
        .env("APP_SECRET", "test-secret-key-0123456789abcdef")
        .env("APP_PBKDF2_ITERATIONS", "1000")
        .env("APP_DB", &db_path)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut server = ServerGuard::new(child);

    // Unauthenticated GET / shows sign up and log in links.
    let guest_home = send_http(port, "GET", "/", &[], "");
    assert_eq!(guest_home.status, 200);
    assert!(guest_home.body.contains("/signup"), "body={}", guest_home.body);
    assert!(guest_home.body.contains("/login"), "body={}", guest_home.body);

    // Unauthenticated API access -> 401 JSON.
    let unauth_api = send_http(port, "GET", "/api/notes", &[], "");
    assert_eq!(unauth_api.status, 401);
    assert_eq!(unauth_api.body, r#"{"error":"login required"}"#);

    // Sign up user 1 with HTML-special characters in email to verify escaping.
    let signup1 = send_http(
        port,
        "POST",
        "/signup",
        &[("Content-Type", "application/x-www-form-urlencoded")],
        "email=%3Cb%3Ex%40y.z&password=secretpassword1",
    );
    assert_eq!(signup1.status, 303, "signup1 body={}", signup1.body);
    assert_eq!(header_value(&signup1, "Location").as_deref(), Some("/"));
    let cookie1 = extract_cookie_pair(&signup1);
    assert!(cookie1.starts_with("sid="), "cookie1={cookie1}");

    // Duplicate signup -> 409.
    let dup = send_http(
        port,
        "POST",
        "/signup",
        &[("Content-Type", "application/x-www-form-urlencoded")],
        "email=%3Cb%3Ex%40y.z&password=secretpassword1",
    );
    assert_eq!(dup.status, 409, "dup body={}", dup.body);

    // GET / with user 1 cookie shows the email escaped.
    let home1 = send_http(port, "GET", "/", &[("Cookie", &cookie1)], "");
    assert_eq!(home1.status, 200);
    assert!(
        home1.body.contains("&lt;b&gt;x@y.z"),
        "expected escaped email in:\n{}",
        home1.body
    );
    assert!(
        !home1.body.contains("<b>x@y.z"),
        "raw HTML must not appear in:\n{}",
        home1.body
    );

    // POST /api/notes with invalid empty body -> 422.
    let bad_note = send_http(
        port,
        "POST",
        "/api/notes",
        &[
            ("Cookie", &cookie1),
            ("Content-Type", "application/x-www-form-urlencoded"),
        ],
        "text=",
    );
    assert_eq!(bad_note.status, 422, "bad_note={}", bad_note.body);

    // POST /api/notes -> 201 {"id":1}.
    let create1 = send_http(
        port,
        "POST",
        "/api/notes",
        &[
            ("Cookie", &cookie1),
            ("Content-Type", "application/x-www-form-urlencoded"),
        ],
        "text=first+%3Cnote%3E",
    );
    assert_eq!(create1.status, 201, "create1={}", create1.body);
    assert_eq!(create1.body, r#"{"id":1}"#);

    // GET /api/notes lists user 1's note.
    let list1 = send_http(port, "GET", "/api/notes", &[("Cookie", &cookie1)], "");
    assert_eq!(list1.status, 200);
    assert!(list1.body.contains(r#""id":1"#), "list1={}", list1.body);
    assert!(
        list1.body.contains(r#""text":"first <note>""#),
        "list1={}",
        list1.body
    );

    // GET / also renders the note escaped.
    let home1_notes = send_http(port, "GET", "/", &[("Cookie", &cookie1)], "");
    assert_eq!(home1_notes.status, 200);
    assert!(
        home1_notes.body.contains("first &lt;note&gt;"),
        "home1_notes={}",
        home1_notes.body
    );

    // Sign up user 2.
    let signup2 = send_http(
        port,
        "POST",
        "/signup",
        &[("Content-Type", "application/x-www-form-urlencoded")],
        "email=two%40example.com&password=anothersecret9",
    );
    assert_eq!(signup2.status, 303);
    let cookie2 = extract_cookie_pair(&signup2);

    // User 2 cannot see user 1's note.
    let list2 = send_http(port, "GET", "/api/notes", &[("Cookie", &cookie2)], "");
    assert_eq!(list2.status, 200);
    assert_eq!(list2.body, "[]");

    // User 2 cannot delete user 1's note -> 404.
    let del_other = send_http(port, "DELETE", "/api/notes/1", &[("Cookie", &cookie2)], "");
    assert_eq!(del_other.status, 404);

    // User 1 deletes their own note -> 204, then second delete -> 404.
    let del_own = send_http(port, "DELETE", "/api/notes/1", &[("Cookie", &cookie1)], "");
    assert_eq!(del_own.status, 204);
    let list1_after = send_http(port, "GET", "/api/notes", &[("Cookie", &cookie1)], "");
    assert_eq!(list1_after.status, 200);
    assert_eq!(list1_after.body, "[]");

    // Logout clears the cookie (Max-Age=0).
    let logout = send_http(port, "POST", "/logout", &[("Cookie", &cookie1)], "");
    assert_eq!(logout.status, 303);
    let cleared = header_value(&logout, "Set-Cookie").unwrap();
    assert!(cleared.contains("Max-Age=0"), "Set-Cookie={cleared}");

    // Wrong password and unknown email both return 401 with the same message.
    let wrong_pw = send_http(
        port,
        "POST",
        "/login",
        &[("Content-Type", "application/x-www-form-urlencoded")],
        "email=two%40example.com&password=wrongpassword",
    );
    assert_eq!(wrong_pw.status, 401);
    let unknown_email = send_http(
        port,
        "POST",
        "/login",
        &[("Content-Type", "application/x-www-form-urlencoded")],
        "email=nobody%40example.com&password=wrongpassword",
    );
    assert_eq!(unknown_email.status, 401);
    assert!(wrong_pw.body.contains("Invalid email or password."));
    assert!(unknown_email.body.contains("Invalid email or password."));

    // Valid login succeeds and sets a session cookie.
    let login_ok = send_http(
        port,
        "POST",
        "/login",
        &[("Content-Type", "application/x-www-form-urlencoded")],
        "email=two%40example.com&password=anothersecret9",
    );
    assert_eq!(login_ok.status, 303);
    let cookie2_login = extract_cookie_pair(&login_ok);
    let home2 = send_http(port, "GET", "/", &[("Cookie", &cookie2_login)], "");
    assert_eq!(home2.status, 200);
    assert!(home2.body.contains("two@example.com"));

    // Static files: style.css -> 200 text/css, path traversal -> 403.
    let css = send_http(port, "GET", "/static/style.css", &[], "");
    assert_eq!(css.status, 200);
    let ct = header_value(&css, "Content-Type").unwrap_or_default();
    assert!(ct.starts_with("text/css"), "Content-Type={ct:?}");

    let escape = send_http(port, "GET", "/static/../main.um", &[], "");
    assert_eq!(escape.status, 403);

    // Graceful shutdown on SIGTERM.
    send_sigterm(server.pid());
    let (status, stderr) = server.wait_exit(Duration::from_secs(5));
    assert!(status.success(), "server failed: {status:?}\n{stderr}");
    assert_eq!(status.code(), Some(0));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn generated_ephemeral_secret_works_across_forked_requests() {
    if !available() {
        return;
    }
    let (dir, bin) = prepare_app("ephemeral_secret");
    let db_path = dir.join("ephemeral.db");
    let port = test_port(1);

    let child = Command::new(&bin)
        .current_dir(&dir)
        .env("PORT", port.to_string())
        .env_remove("APP_SECRET")
        .env("APP_PBKDF2_ITERATIONS", "1000")
        .env("APP_DB", &db_path)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut server = ServerGuard::new(child);

    let signup = send_http(
        port,
        "POST",
        "/signup",
        &[("Content-Type", "application/x-www-form-urlencoded")],
        "email=ephem%40example.com&password=secretpassword1",
    );
    assert_eq!(signup.status, 303);
    let cookie = extract_cookie_pair(&signup);

    // Subsequent forked child must verify the cookie signed with the parent's generated secret.
    let home = send_http(port, "GET", "/", &[("Cookie", &cookie)], "");
    assert_eq!(home.status, 200);
    assert!(home.body.contains("ephem@example.com"));

    send_sigterm(server.pid());
    let (status, stderr) = server.wait_exit(Duration::from_secs(5));
    assert!(status.success(), "server failed: {status:?}\n{stderr}");
    assert!(
        stderr.contains("APP_SECRET"),
        "expected APP_SECRET warning on stderr, got:\n{stderr}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
