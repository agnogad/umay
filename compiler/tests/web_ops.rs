//! Part 10 of docs/web-backend-plan.md: server configuration from the
//! environment, port reuse (`SO_REUSEADDR`), per-request stderr logging, and
//! graceful shutdown on SIGTERM / SIGINT.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

fn toolchain_available() -> bool {
    let ok = |name: &str| {
        Command::new(name)
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    };
    ok("as") && ok("cc")
}

/// Ensures a spawned server process is killed and reaped even if a test panics.
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

    fn stdout(&mut self) -> Option<std::process::ChildStdout> {
        self.child.as_mut().and_then(|c| c.stdout.take())
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

fn build_program(name: &str, source: &str) -> PathBuf {
    let dir = Path::new("/tmp/umay_web_ops").join(format!("{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.um");
    std::fs::write(&file, source).unwrap();
    let build = Command::new(env!("CARGO_BIN_EXE_umay"))
        .arg("build")
        .arg(&file)
        .current_dir(&dir)
        .output()
        .unwrap();
    assert!(
        build.status.success(),
        "build failed for {name}:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );
    dir.join("dist").join("main")
}

fn test_port(slot: u16) -> u16 {
    20000u16 + ((std::process::id() % 5000) as u16) * 6 + slot
}

fn connect_with_retry(port: u16) -> TcpStream {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        match TcpStream::connect(("127.0.0.1", port)) {
            Ok(stream) => return stream,
            Err(e) if Instant::now() < deadline => {
                let _ = e;
                std::thread::sleep(Duration::from_millis(15));
            }
            Err(e) => panic!("failed to connect to 127.0.0.1:{port}: {e}"),
        }
    }
}

fn send_http(port: u16, raw: &[u8]) -> String {
    let mut stream = connect_with_retry(port);
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    stream.write_all(raw).unwrap();
    stream.flush().unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    response
}

fn send_signal(pid: u32, sig: &str) {
    let status = Command::new("kill")
        .arg(sig)
        .arg(pid.to_string())
        .status()
        .expect("run kill");
    assert!(status.success(), "kill {sig} {pid} failed: {status:?}");
}

#[test]
fn env_or_and_env_port_read_and_validate_env() {
    if !toolchain_available() {
        return;
    }
    let bin = build_program(
        "env_config",
        r##"
fn show(b: Bytes) {
    let buf = alloc(b.len + 1)
    copy_bytes(b.data, buf, b.len)
    unsafe { write_byte((buf as usize + b.len) as *mut u8, 0u8) }
    println(buf as *const u8)
    dealloc(buf)
}

fn main() -> i32 {
    show(env_or("UMAY_TEST_HOST", "0.0.0.0"))
    show(env_or("UMAY_TEST_UNSET", "0.0.0.0"))
    show(env_or("UMAY_TEST_EMPTY", "fallback"))
    println_i64(env_port("UMAY_PORT_OK", 3000u16) as i64)
    println_i64(env_port("UMAY_PORT_ONE", 3000u16) as i64)
    println_i64(env_port("UMAY_PORT_MAX", 3000u16) as i64)
    println_i64(env_port("UMAY_PORT_UNSET", 3000u16) as i64)
    println_i64(env_port("UMAY_PORT_EMPTY", 3000u16) as i64)
    println_i64(env_port("UMAY_PORT_ALPHA", 3000u16) as i64)
    println_i64(env_port("UMAY_PORT_MIX", 3000u16) as i64)
    println_i64(env_port("UMAY_PORT_NEG", 3000u16) as i64)
    println_i64(env_port("UMAY_PORT_ZERO", 3000u16) as i64)
    println_i64(env_port("UMAY_PORT_HIGH", 3000u16) as i64)
    println_i64(env_port("UMAY_PORT_HUGE", 3000u16) as i64)
    return 0
}
"##,
    );

    let out = Command::new(&bin)
        .env("UMAY_TEST_HOST", "127.0.0.1")
        .env_remove("UMAY_TEST_UNSET")
        .env("UMAY_TEST_EMPTY", "")
        .env("UMAY_PORT_OK", "8080")
        .env("UMAY_PORT_ONE", "1")
        .env("UMAY_PORT_MAX", "65535")
        .env_remove("UMAY_PORT_UNSET")
        .env("UMAY_PORT_EMPTY", "")
        .env("UMAY_PORT_ALPHA", "abc")
        .env("UMAY_PORT_MIX", "80a")
        .env("UMAY_PORT_NEG", "-1")
        .env("UMAY_PORT_ZERO", "0")
        .env("UMAY_PORT_HIGH", "65536")
        .env("UMAY_PORT_HUGE", "99999999999999999999999")
        .output()
        .unwrap();
    assert!(out.status.success(), "exit status {:?}", out.status);
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "127.0.0.1\n0.0.0.0\nfallback\n8080\n1\n65535\n3000\n3000\n3000\n3000\n3000\n3000\n3000\n3000\n"
    );
}

#[test]
fn port_reuse_allows_immediate_restart_after_sigterm() {
    if !toolchain_available() {
        return;
    }
    let bin = build_program(
        "port_reuse",
        r##"
fn ping(_req: &Request, res: &mut Response) {
    *res = Response::new(200).with_type("text/plain").with_body(Bytes::from_str("pong"))
}

fn main() -> i32 {
    let port = env_port("UMAY_TEST_PORT", 0u16)
    let routes = [route("GET", "/ping", ping)]
    return serve(port, routes as *const Route, 1usize)
}
"##,
    );

    let port = test_port(0);

    let child1 = Command::new(&bin)
        .env("UMAY_TEST_PORT", port.to_string())
        .env("UMAY_LOG", "off")
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut server1 = ServerGuard::new(child1);

    let resp1 = send_http(port, b"GET /ping HTTP/1.1\r\nHost: localhost\r\n\r\n");
    assert!(resp1.starts_with("HTTP/1.1 200 OK\r\n"), "resp1={resp1:?}");
    assert!(resp1.ends_with("\r\n\r\npong"), "resp1={resp1:?}");

    send_signal(server1.pid(), "-TERM");
    let (status1, err1) = server1.wait_exit(Duration::from_secs(5));
    assert!(status1.success(), "server1 failed: {status1:?}\n{err1}");

    // Immediately rebind the same port without sleeping.
    let child2 = Command::new(&bin)
        .env("UMAY_TEST_PORT", port.to_string())
        .env("UMAY_LOG", "off")
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut server2 = ServerGuard::new(child2);

    let resp2 = send_http(port, b"GET /ping HTTP/1.1\r\nHost: localhost\r\n\r\n");
    assert!(resp2.starts_with("HTTP/1.1 200 OK\r\n"), "resp2={resp2:?}");
    assert!(resp2.ends_with("\r\n\r\npong"), "resp2={resp2:?}");

    send_signal(server2.pid(), "-INT");
    let (status2, err2) = server2.wait_exit(Duration::from_secs(5));
    assert!(status2.success(), "server2 failed: {status2:?}\n{err2}");
}

#[test]
fn request_log_writes_to_stderr_and_honors_umay_log_off() {
    if !toolchain_available() {
        return;
    }
    let bin = build_program(
        "request_log",
        r##"
fn hello(_req: &Request, res: &mut Response) {
    *res = Response::new(200).with_type("text/plain").with_body(Bytes::from_str("hi"))
}

fn main() -> i32 {
    let port = env_port("UMAY_TEST_PORT", 0u16)
    let routes = [route("GET", "/hello", hello)]
    return serve(port, routes as *const Route, 1usize)
}
"##,
    );

    let port1 = test_port(1);
    let child1 = Command::new(&bin)
        .env("UMAY_TEST_PORT", port1.to_string())
        .env_remove("UMAY_LOG")
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut server1 = ServerGuard::new(child1);

    let r_ok = send_http(port1, b"GET /hello?q=1 HTTP/1.1\r\nHost: localhost\r\n\r\n");
    assert!(r_ok.starts_with("HTTP/1.1 200 OK\r\n"), "r_ok={r_ok:?}");

    let r_404 = send_http(port1, b"GET /nope HTTP/1.1\r\nHost: localhost\r\n\r\n");
    assert!(
        r_404.starts_with("HTTP/1.1 404 Not Found\r\n"),
        "r_404={r_404:?}"
    );

    let r_400 = send_http(port1, b"MALFORMED\r\n\r\n");
    assert!(
        r_400.starts_with("HTTP/1.1 400 Bad Request\r\n"),
        "r_400={r_400:?}"
    );

    let r_413 = send_http(port1, b"POST /big HTTP/1.1\r\nContent-Length: 70000\r\n\r\n");
    assert!(
        r_413.starts_with("HTTP/1.1 413 Content Too Large\r\n"),
        "r_413={r_413:?}"
    );

    send_signal(server1.pid(), "-TERM");
    let (status1, stderr1) = server1.wait_exit(Duration::from_secs(5));
    assert!(status1.success(), "status1={status1:?}\n{stderr1}");
    assert!(
        stderr1.contains("GET /hello 200 "),
        "expected `GET /hello 200 ` in stderr:\n{stderr1}"
    );
    assert!(
        !stderr1.contains("?q=1"),
        "query string should not appear in log:\n{stderr1}"
    );
    assert!(
        stderr1.contains("GET /nope 404 "),
        "expected `GET /nope 404 ` in stderr:\n{stderr1}"
    );
    assert!(
        stderr1.contains("- - 400 "),
        "expected `- - 400 ` in stderr:\n{stderr1}"
    );
    assert!(
        stderr1.contains("- - 413 "),
        "expected `- - 413 ` in stderr:\n{stderr1}"
    );
    for line in stderr1.lines() {
        assert!(line.ends_with("ms"), "expected `...ms` suffix on {line:?}");
    }

    let port2 = test_port(2);
    let child2 = Command::new(&bin)
        .env("UMAY_TEST_PORT", port2.to_string())
        .env("UMAY_LOG", "off")
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut server2 = ServerGuard::new(child2);

    let r_off = send_http(port2, b"GET /hello HTTP/1.1\r\nHost: localhost\r\n\r\n");
    assert!(r_off.starts_with("HTTP/1.1 200 OK\r\n"), "r_off={r_off:?}");

    send_signal(server2.pid(), "-TERM");
    let (status2, stderr2) = server2.wait_exit(Duration::from_secs(5));
    assert!(status2.success(), "status2={status2:?}\n{stderr2}");
    assert!(
        stderr2.is_empty(),
        "expected no log lines with UMAY_LOG=off, got:\n{stderr2}"
    );
}

#[test]
fn graceful_shutdown_finishes_inflight_slow_request_and_exits_zero() {
    if !toolchain_available() {
        return;
    }
    let bin = build_program(
        "graceful_shutdown",
        r##"
fn slow(_req: &Request, res: &mut Response) {
    println("started")
    let deadline = monotonic_ms() + 350i64
    while monotonic_ms() < deadline {
        let _ = monotonic_ms()
    }
    let out = Response::new(200).with_type("text/plain").with_body(Bytes::from_str("slow-ok"))
    unsafe { *res = out }
}

fn main() -> i32 {
    let port = env_port("UMAY_TEST_PORT", 0u16)
    let routes = [route("GET", "/slow", slow)]
    return serve(port, routes as *const Route, 1usize)
}
"##,
    );

    let port = test_port(3);
    let child = Command::new(&bin)
        .env("UMAY_TEST_PORT", port.to_string())
        .env("UMAY_LOG", "off")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut server = ServerGuard::new(child);
    let stdout = server.stdout().expect("piped stdout");

    let mut stream = connect_with_retry(port);
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    stream
        .write_all(b"GET /slow HTTP/1.1\r\nHost: localhost\r\n\r\n")
        .unwrap();
    stream.flush().unwrap();

    // Wait until the forked worker confirms it has entered the slow handler.
    let mut reader = BufReader::new(stdout);
    let mut marker = String::new();
    reader.read_line(&mut marker).unwrap();
    assert_eq!(marker, "started\n");

    // Send SIGTERM while the 350 ms handler is still in flight.
    send_signal(server.pid(), "-TERM");

    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    assert!(
        response.starts_with("HTTP/1.1 200 OK\r\n"),
        "expected full 200 response, got: {response:?}"
    );
    assert!(
        response.ends_with("\r\n\r\nslow-ok"),
        "expected full body, got: {response:?}"
    );

    let (status, stderr) = server.wait_exit(Duration::from_secs(5));
    assert!(
        status.success(),
        "expected clean exit status 0, got {status:?}\n{stderr}"
    );
    assert_eq!(status.code(), Some(0));
}
