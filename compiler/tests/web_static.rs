//! Static files (docs/web-backend-plan.md, part 4): each test compiles and
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
    let dir = Path::new("/tmp/umay_web_static").join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.um");
    std::fs::write(&file, source).unwrap();
    let build = Command::new(env!("CARGO_BIN_EXE_umay")).arg("build").arg(&file).current_dir(&dir).output().unwrap();
    assert!(build.status.success(), "build failed for {name}:\n{}", String::from_utf8_lossy(&build.stderr));
    let out = Command::new(dir.join("dist").join("main")).current_dir(&dir).output().unwrap();
    assert_eq!(String::from_utf8_lossy(&out.stdout), expected, "stderr:\n{}", String::from_utf8_lossy(&out.stderr));
    assert!(out.status.success(), "exit status {:?}", out.status);
}

#[test]
fn serve_dir_answers_files_indexes_misses_and_refuses_escapes() {
    let source = r##"
fn st_show(b: Bytes) {
    let buf = alloc(b.len + 1)
    copy_bytes(b.data, buf, b.len)
    unsafe { write_byte((buf as usize + b.len) as *mut u8, 0u8) }
    println(buf as *const u8)
}
fn st_put(path: &str, text: &str) {
    let _ = write_file_bytes(path as *const u8, text as *const u8, byte_len(text as *const u8))
}
fn st_handler(req: &Request, res: &mut Response) {
    serve_dir(req, res, "/tmp/umay_web_static/files/root")
}
fn st_get(routes: *const Route, raw: &str) {
    let res = dispatch(routes, 1, parse_request(Bytes::from_str(raw)))
    println_i64(res.status() as i64)
    if res.status() == 200 {
        st_show(res.content_type)
        st_show(res.body())
    }
}
fn main() -> i32 {
    let _ = create_dir("/tmp/umay_web_static/files")
    let _ = create_dir("/tmp/umay_web_static/files/root")
    let _ = create_dir("/tmp/umay_web_static/files/root/docs")
    let _ = create_dir("/tmp/umay_web_static/files/root/empty")
    st_put("/tmp/umay_web_static/files/secret.txt", "secret")
    st_put("/tmp/umay_web_static/files/root/a.css", "body {{}}")
    st_put("/tmp/umay_web_static/files/root/a+b.JS", "x")
    st_put("/tmp/umay_web_static/files/root/docs/index.html", "<h1>docs</h1>")
    st_put("/tmp/umay_web_static/files/root/blob.xyz", "raw")
    let routes = alloc(sizeof(Route)) as *mut Route
    unsafe { *routes = route("GET", "/static/*", st_handler) }
    let r = routes as *const Route
    st_get(r, "GET /static/a.css HTTP/1.1\r\n\r\n")
    st_get(r, "GET /static/a+b.JS HTTP/1.1\r\n\r\n")
    st_get(r, "GET /static/docs HTTP/1.1\r\n\r\n")
    st_get(r, "GET /static/docs/ HTTP/1.1\r\n\r\n")
    st_get(r, "GET /static/empty HTTP/1.1\r\n\r\n")
    st_get(r, "GET /static/missing.css HTTP/1.1\r\n\r\n")
    st_get(r, "GET /static/blob.xyz HTTP/1.1\r\n\r\n")
    st_get(r, "GET /static/../secret.txt HTTP/1.1\r\n\r\n")
    st_get(r, "GET /static/%2e%2e/secret.txt HTTP/1.1\r\n\r\n")
    st_get(r, "GET /static/docs/%2E%2E%2f..%2fsecret.txt HTTP/1.1\r\n\r\n")
    st_get(r, "GET /static//tmp/umay_web_static/files/secret.txt HTTP/1.1\r\n\r\n")
    st_get(r, "GET /static/a.css%00.png HTTP/1.1\r\n\r\n")
    st_get(r, "GET /static/..%5csecret.txt HTTP/1.1\r\n\r\n")
    return 0
}
"##;
    let _ = std::fs::remove_dir_all("/tmp/umay_web_static/files");
    expect_stdout(
        "serve_dir",
        source,
        concat!(
            "200\ntext/css; charset=utf-8\nbody {}\n",
            "200\ntext/javascript; charset=utf-8\nx\n",
            "200\ntext/html; charset=utf-8\n<h1>docs</h1>\n",
            "200\ntext/html; charset=utf-8\n<h1>docs</h1>\n",
            "404\n404\n",
            "200\napplication/octet-stream\nraw\n",
            "403\n403\n403\n403\n403\n403\n",
        ),
    );
}

#[test]
fn mime_types_by_extension() {
    let source = r##"
fn st_mime(name: &str) {
    let b = Bytes::from_str(name)
    println(mime_type(&b) as *const u8)
}
fn main() -> i32 {
    st_mime("index.HTML")
    st_mime("a.htm")
    st_mime("app.mjs")
    st_mime("data.json")
    st_mime("logo.svg")
    st_mime("p.jpeg")
    st_mime("f.woff2")
    st_mime("m.wasm")
    st_mime("dir.v2/README")
    st_mime("noext")
    return 0
}
"##;
    expect_stdout(
        "mime_types",
        source,
        concat!(
            "text/html; charset=utf-8\ntext/html; charset=utf-8\ntext/javascript; charset=utf-8\n",
            "application/json; charset=utf-8\nimage/svg+xml; charset=utf-8\nimage/jpeg\nfont/woff2\n",
            "application/wasm\napplication/octet-stream\napplication/octet-stream\n",
        ),
    );
}
