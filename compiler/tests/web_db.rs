//! Part 8 of docs/web-backend-plan.md: SQLite through `stdlib/sqlite.um`.
//! Each test builds and runs a whole program in its own directory, so the
//! database file it creates is fresh.

use std::path::Path;
use std::process::Command;

fn available() -> bool {
    // The host execution harness uses the Unix assembler/linker and /dev/null.
    // Windows coverage is the native smoke step in CI.
    if cfg!(windows) {
        return false;
    }
    let ok = |name: &str| Command::new(name).arg("--version").output().map(|o| o.status.success()).unwrap_or(false);
    // The link needs the library; a machine without it skips these tests.
    let has_sqlite = Command::new("cc")
        .args(["-x", "c", "-", "-lsqlite3", "-o", "/dev/null"])
        .stdin(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut c| {
            use std::io::Write;
            c.stdin.take().unwrap().write_all(b"int sqlite3_libversion_number(void); int main(void){return sqlite3_libversion_number()==0;}")?;
            c.wait()
        })
        .map(|s| s.success())
        .unwrap_or(false);
    ok("as") && ok("cc") && has_sqlite
}

fn expect_stdout(name: &str, body: &str, expected: &str) {
    if !available() {
        return;
    }
    let dir = Path::new("/tmp/umay_web_db").join(name);
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
fn yes(b: bool) {
    println(if b { "yes" } else { "no" })
}
fn must(s: Option<Stmt>) -> Stmt {
    return match s {
        Option::Some(v) => v,
        Option::None => {
            panic("prepare failed")
            Stmt { handle: unsafe { 0 as *mut u8 } }
        }
    }
}
"##;

#[test]
fn rows_are_inserted_queried_updated_and_deleted() {
    expect_stdout(
        "crud",
        r##"
fn main() -> i32 {
    let db = match db_open("app.db") { Option::Some(d) => d, Option::None => return 1 }
    yes(db.exec("CREATE TABLE users(id INTEGER PRIMARY KEY, email TEXT UNIQUE NOT NULL, name TEXT); PRAGMA journal_mode=WAL;"))
    let ins = must(db.prepare("INSERT INTO users(email, name) VALUES (?1, ?2)"))
    let e1 = Bytes::from_str("a@x.io")
    let n1 = Bytes::from_str("O'Brien; DROP TABLE users")
    ins.bind_text(1, &e1)
    ins.bind_text(2, &n1)
    yes(ins.run())
    println_i64(db.last_insert_id())
    ins.reset()
    let e2 = Bytes::from_str("b@x.io")
    ins.bind_text(1, &e2)
    ins.bind_null(2)
    yes(ins.run())
    ins.reset()
    ins.bind_text(1, &e1)
    ins.bind_text(2, &n1)
    yes(ins.run())
    show(db.error())
    ins.finish()

    let q = must(db.prepare("SELECT id, email, name FROM users ORDER BY id"))
    println_i64(q.columns() as i64)
    while q.row() {
        println_i64(q.int(0))
        show(q.text(1))
        yes(q.is_null(2))
        show(q.text(2))
    }
    q.finish()

    let up = must(db.prepare("UPDATE users SET name = ?1 WHERE id = ?2"))
    let n2 = Bytes::from_str("Bob")
    up.bind_text(1, &n2)
    up.bind_int(2, 2i64)
    yes(up.run())
    println_i64(db.changes())
    up.finish()
    let one = must(db.prepare("SELECT name FROM users WHERE id = ?1"))
    one.bind_int(1, 2i64)
    yes(one.row())
    show(one.text(0))
    yes(one.row())
    one.finish()

    yes(db.exec("DELETE FROM users"))
    println_i64(db.changes())
    yes(db.prepare("SELEKT nonsense").is_none())
    show(db.error())
    db.close()
    return 0
}
"##,
        "yes\nyes\n1\nyes\nno\nUNIQUE constraint failed: users.email\n3\n\
         1\na@x.io\nno\nO'Brien; DROP TABLE users\n\
         2\nb@x.io\nyes\n\n\
         yes\n1\nyes\nBob\nno\n\
         yes\n2\nyes\nnear \"SELEKT\": syntax error\n",
    );
}

#[test]
fn a_json_api_reads_and_writes_the_database_through_the_router() {
    // The handlers open the database themselves, as a per-connection child
    // would, and answer JSON.
    expect_stdout(
        "json_api",
        r##"
fn open_db() -> Db {
    return match db_open("api.db") {
        Option::Some(d) => d,
        Option::None => {
            panic("open failed")
            Db { handle: unsafe { 0 as *mut u8 } }
        }
    }
}
fn create_note(req: &Request, res: &mut Response) {
    let text = match req.form("text") {
        Option::Some(t) => t,
        Option::None => {
            res.status = 422
            return
        }
    }
    let db = open_db()
    let ins = must(db.prepare("INSERT INTO notes(text) VALUES (?1)"))
    ins.bind_text(1, &text)
    let ok = ins.run()
    ins.finish()
    let j = JsonOut::new().begin_object().key("id").int(db.last_insert_id()).end_object()
    db.close()
    let answer = Response::new(if ok { 201 } else { 500 }).with_type("application/json").with_body(j.bytes())
    *res = answer
}
fn get_note(req: &Request, res: &mut Response) {
    let id = parse_decimal(req.param("id"))
    let db = open_db()
    let q = must(db.prepare("SELECT id, text FROM notes WHERE id = ?1"))
    q.bind_int(1, id)
    let mut answer = Response::new(404)
    if q.row() {
        let j = JsonOut::new().begin_object().key("id").int(q.int(0)).key("text").string(q.text(1)).end_object()
        answer = Response::new(200).with_type("application/json").with_body(j.bytes())
    }
    q.finish()
    db.close()
    *res = answer
}
fn ask(table: *const Route, raw: &str) {
    let r = dispatch(table, 2, parse_request(Bytes::from_str(raw)))
    println_i64(r.status as i64)
    show(r.body)
}
fn main() -> i32 {
    let db = open_db()
    yes(db.exec("CREATE TABLE notes(id INTEGER PRIMARY KEY, text TEXT NOT NULL)"))
    db.close()
    let routes = [route("POST", "/notes", create_note), route("GET", "/notes/:id", get_note)]
    let t = routes as *const Route
    ask(t, "POST /notes HTTP/1.1\r\nContent-Length: 22\r\n\r\ntext=hello+%22world%22")
    ask(t, "GET /notes/1 HTTP/1.1\r\n\r\n")
    ask(t, "GET /notes/9 HTTP/1.1\r\n\r\n")
    ask(t, "POST /notes HTTP/1.1\r\nContent-Length: 3\r\n\r\nx=1")
    return 0
}
"##,
        "yes\n201\n{\"id\":1}\n200\n{\"id\":1,\"text\":\"hello \\\"world\\\"\"}\n404\n\n422\n\n",
    );
}
