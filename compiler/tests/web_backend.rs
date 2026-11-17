//! The web backend library (docs/web-backend-plan.md): each test compiles and
//! runs a whole program through `umay build`, like `phase6_runtime.rs`.

use std::path::Path;
use std::process::Command;

fn toolchain_available() -> bool {
    let ok = |name: &str| Command::new(name).arg("--version").output().map(|o| o.status.success()).unwrap_or(false);
    ok("as") && ok("cc")
}

/// Builds `source`, runs it, and asserts what it printed.
fn expect_stdout(name: &str, source: &str, expected: &str) {
    if !toolchain_available() {
        return;
    }
    let dir = Path::new("/tmp/umay_web").join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.um");
    std::fs::write(&file, source).unwrap();
    let build = Command::new(env!("CARGO_BIN_EXE_umay")).arg("build").arg(&file).current_dir(&dir).output().unwrap();
    assert!(build.status.success(), "build failed for {name}:\n{}", String::from_utf8_lossy(&build.stderr));
    let out = Command::new(dir.join("dist").join("main")).current_dir(&dir).output().unwrap();
    let printed = String::from_utf8_lossy(&out.stdout);
    assert_eq!(printed.matches("HTTP/1.1 ").count(), printed.matches("\r\nDate: ").count(), "every response carries a Date");
    assert_eq!(without_date(&printed), expected, "stderr:\n{}", String::from_utf8_lossy(&out.stderr));
    assert!(out.status.success(), "exit status {:?}", out.status);
}

/// `text` with each response's `\r\nDate: <29-byte IMF-fixdate>` removed, since
/// the date changes from run to run.
fn without_date(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(at) = rest.find("\r\nDate: ") {
        let date = &rest[at + 8..at + 8 + 29];
        assert!(date.ends_with(" GMT") && date.as_bytes()[3] == b',', "malformed Date: {date:?}");
        out.push_str(&rest[..at]);
        rest = &rest[at + 8 + 29..];
    }
    out.push_str(rest);
    out
}

#[test]
fn headers_path_query_and_percent_decoding() {
    let source = r##"
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
fn main() -> i32 {
    let raw = Bytes::from_str("GET /search?q=a%20b+c&flag&x=%zz HTTP/1.1\r\nHost: example\r\nX-Token:  abc \r\n\r\n")
    let req = parse_request(raw)
    show(req.path())
    show(req.query())
    show_opt(req.header("host"))
    show_opt(req.header("X-TOKEN"))
    show_opt(req.header("missing"))
    show_opt(query_param(req.query(), "flag"))
    show_opt(query_param(req.query(), "nope"))
    let q = match query_param(req.query(), "q") { Option::Some(v) => v, Option::None => Bytes::from_str("") }
    let out = alloc(q.len)
    show(Bytes::view(out as *const u8, percent_decode(q, out)))
    let x = match query_param(req.query(), "x") { Option::Some(v) => v, Option::None => Bytes::from_str("") }
    show(Bytes::view(out as *const u8, percent_decode(x, out)))
    let plain = parse_request(Bytes::from_str("GET /plain HTTP/1.1\r\n\r\n"))
    show(plain.path())
    println_i64(plain.query().len as i64)
    return 0
}
"##;
    expect_stdout(
        "http_core_parse",
        source,
        "/search\nq=a%20b+c&flag&x=%zz\nexample\nabc\n<none>\n\n<none>\na b c\n%zz\n/plain\n0\n",
    );
}

#[test]
fn a_request_split_across_reads_is_read_whole_and_answered() {
    // The client sends the head first and the body later, so the server's first
    // read cannot hold the whole request: `read_request` has to keep reading
    // until the `Content-Length` body is in.
    let source = r##"
extern fn "connect" sys_connect(fd: isize, addr: *const u8, len: i32) -> i32
extern fn "getpid" umay_c_getpid() -> i32
fn main() -> i32 {
    let port = 20000u16 + (umay_c_getpid() % 20000i32) as u16
    let server = tcp_socket()
    let addr = address(port)
    if !socket_bind(&server, &addr) { return 1 }
    socket_listen(&server, 4)
    let client = tcp_socket()
    sys_connect(client.fd, addr.bytes as *const u8, address_size() as i32)
    let peer = socket_accept(&server)
    let head = "POST /items HTTP/1.1\r\nContent-Length: 5\r\n\r\nhe"
    socket_write_all(&client, head as *const u8, byte_len(head as *const u8))
    let pid = spawn()
    if is_child(pid) {
        let buf = alloc(256)
        let got = read_request(&peer, buf, 256usize)
        let req = parse_request(Bytes::view(buf as *const u8, got as usize))
        println_i64(req.body().len as i64)
        let r = Response::new(201).with_type("text/plain").with_headers(Bytes::from_str("X-A: 1\r\n")).with_body(Bytes::from_str("ok"))
        let date = alloc(40)
        let n = http_date(unix_now(), date)
        write_response(&peer, &r, Bytes::view(date as *const u8, n), false)
        socket_close(&peer)
        exit(0)
    }
    socket_close(&peer)
    socket_write_all(&client, "llo" as *const u8, 3usize)
    wait()
    let buf = alloc(256)
    let mut got: i64 = 0
    while true {
        let n = socket_read(&client, unsafe { (buf as usize + got as usize) as *mut u8 }, 255usize - got as usize)
        if n <= 0i64 { break }
        got = got + n
    }
    unsafe { write_byte((buf as usize + got as usize) as *mut u8, 0u8) }
    print(buf as *const u8)
    println("")
    let tiny = alloc(8)
    let pid2 = spawn()
    if is_child(pid2) {
        let c2 = tcp_socket()
        sys_connect(c2.fd, addr.bytes as *const u8, address_size() as i32)
        socket_write_all(&c2, head as *const u8, byte_len(head as *const u8))
        socket_close(&c2)
        exit(0)
    }
    let p2 = socket_accept(&server)
    println_i64(read_request(&p2, tiny, 8usize))
    wait()
    return 0
}
"##;
    expect_stdout(
        "http_core_roundtrip",
        source,
        "5\nHTTP/1.1 201 Created\r\nContent-Type: text/plain\r\nContent-Length: 2\r\nX-A: 1\r\nConnection: close\r\n\r\nok\n-2\n",
    );
}

const ROUTER_APP: &str = r##"
fn home(req: &Request, res: &mut Response) {
    *res = Response::new(200).with_type("text/plain").with_body(Bytes::from_str("home"))
}
fn user(req: &Request, res: &mut Response) {
    *res = Response::new(200).with_body(req.param("id"))
}
fn create(req: &Request, res: &mut Response) {
    *res = Response::new(201).with_body(req.body())
}
fn asset(req: &Request, res: &mut Response) {
    *res = Response::new(200).with_body(req.param("*"))
}
fn show(r: Response) {
    println_i64(r.status as i64)
    let b = r.body
    let buf = alloc(b.len + 1)
    copy_bytes(b.data, buf, b.len)
    unsafe { write_byte((buf as usize + b.len) as *mut u8, 0u8) }
    println(buf as *const u8)
    dealloc(buf)
}
fn ask(table: *const Route, raw: &str) {
    show(dispatch(table, 4, parse_request(Bytes::from_str(raw))))
}
"##;

#[test]
fn routes_dispatch_by_method_and_pattern() {
    let source = format!(
        "{ROUTER_APP}{}",
        r##"
fn main() -> i32 {
    let routes = [
        route("GET", "/", home),
        route("GET", "/users/:id", user),
        route("POST", "/users", create),
        route("GET", "/static/*", asset),
    ]
    let t = routes as *const Route
    ask(t, "GET / HTTP/1.1\r\n\r\n")
    ask(t, "GET /users/42?x=1 HTTP/1.1\r\n\r\n")
    ask(t, "GET /users/42/x HTTP/1.1\r\n\r\n")
    ask(t, "GET /users/ HTTP/1.1\r\n\r\n")
    ask(t, "DELETE /users/42 HTTP/1.1\r\n\r\n")
    ask(t, "POST /users HTTP/1.1\r\nContent-Length: 3\r\n\r\nabc")
    ask(t, "GET /static/css/a.css HTTP/1.1\r\n\r\n")
    ask(t, "GET /nope HTTP/1.1\r\n\r\n")
    return 0
}
"##
    );
    expect_stdout(
        "router_dispatch",
        &source,
        "200\nhome\n200\n42\n404\n\n404\n\n405\n\n201\nabc\n200\ncss/a.css\n404\n\n",
    );
}

#[test]
fn handle_connection_answers_over_a_socket() {
    let source = format!(
        "{ROUTER_APP}{}",
        r##"
extern fn "connect" sys_connect(fd: isize, addr: *const u8, len: i32) -> i32
extern fn "getpid" umay_c_getpid() -> i32
fn main() -> i32 {
    let routes = [route("GET", "/", home), route("GET", "/users/:id", user), route("POST", "/users", create), route("GET", "/static/*", asset)]
    let port = 20000u16 + (umay_c_getpid() % 20000i32) as u16
    let server = tcp_socket()
    let addr = address(port)
    if !socket_bind(&server, &addr) { return 1 }
    socket_listen(&server, 4)
    let client = tcp_socket()
    sys_connect(client.fd, addr.bytes as *const u8, address_size() as i32)
    let req = "GET /users/7 HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n"
    socket_write_all(&client, req as *const u8, byte_len(req as *const u8))
    let peer = socket_accept(&server)
    handle_connection(&peer, routes as *const Route, 4)
    socket_close(&peer)
    let buf = alloc(256)
    let mut got: i64 = 0
    while true {
        let n = socket_read(&client, unsafe { (buf as usize + got as usize) as *mut u8 }, 255usize - got as usize)
        if n <= 0i64 { break }
        got = got + n
    }
    unsafe { write_byte((buf as usize + got as usize) as *mut u8, 0u8) }
    print(buf as *const u8)
    return 0
}
"##
    );
    expect_stdout(
        "router_socket",
        &source,
        "HTTP/1.1 200 OK\r\nContent-Length: 1\r\nConnection: close\r\n\r\n7",
    );
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
"##;

#[test]
fn json_out_builds_nested_documents_with_commas_and_escapes() {
    let source = format!(
        "{SHOW}{}",
        r##"
fn main() -> i32 {
    let nested = JsonOut::new().begin_object().key("k").null().end_object()
    let j = JsonOut::new().begin_object()
        .key("id").int(-42i64)
        .key("name").string(Bytes::from_str("a \"q\" \\ b\n\t"))
        .key("ok").boolean(true)
        .key("tags").begin_array().string(Bytes::from_str("x")).int(0i64).begin_array().end_array().begin_object().end_object().end_array()
        .key("inner").json(nested.bytes())
        .end_object()
    show(j.bytes())
    let ctl = alloc(1)
    unsafe { write_byte(ctl, 1u8) }
    show(JsonOut::new().string(Bytes::view(ctl as *const u8, 1)).bytes())
    let doc = j.bytes()
    println_i64(json_i64(json_field(&doc, "id")))
    println_i64(if json_bool(json_field(&doc, "ok")) { 1 } else { 0 })
    nested.free()
    j.free()
    return 0
}
"##
    );
    expect_stdout(
        "json_out",
        &source,
        "{\"id\":-42,\"name\":\"a \\\"q\\\" \\\\ b\\n\\t\",\"ok\":true,\"tags\":[\"x\",0,[],{}],\"inner\":{\"k\":null}}\n\"\\u0001\"\n-42\n1\n",
    );
}

#[test]
fn form_and_query_values_are_decoded() {
    let source = format!(
        "{SHOW}{}",
        r##"
fn main() -> i32 {
    let req = parse_request(Bytes::from_str("POST /signup?ref=a%2Fb HTTP/1.1\r\nContent-Type: application/x-www-form-urlencoded\r\nContent-Length: 34\r\n\r\nname=Ay%C5%9Fe+Y&email=a%40b.c&x="))
    show_opt(req.form("name"))
    show_opt(req.form("email"))
    show_opt(req.form("x"))
    show_opt(req.form("missing"))
    show_opt(req.query_param("ref"))
    show_opt(req.query_param("name"))
    return 0
}
"##
    );
    expect_stdout("form_values", &source, "Ayşe Y\na@b.c\n\n<none>\na/b\n<none>\n");
}
