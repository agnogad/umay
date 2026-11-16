//! Part 9 of docs/web-backend-plan.md: HTML templates with escaping.
//! Each test builds and runs a whole program.

use std::path::Path;
use std::process::Command;

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

fn expect_stdout(name: &str, body: &str, expected: &str) {
    if !toolchain_available() {
        return;
    }
    let dir = Path::new("/tmp/umay_web_template").join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.um");
    std::fs::write(&file, format!("{SHOW}{body}")).unwrap();
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
    let out = Command::new(dir.join("dist").join("main"))
        .current_dir(&dir)
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        expected,
        "stderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
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
"##;

#[test]
fn html_escape_escapes_all_five_chars_and_script_injection() {
    expect_stdout(
        "html_escape",
        r##"
fn main() -> i32 {
    let five = Bytes::from_str("&<>\"'")
    show(html_escape(&five))
    let empty = Bytes::from_str("")
    show(html_escape(&empty))
    let plain = Bytes::from_str("hello world")
    show(html_escape(&plain))
    let tmpl = Bytes::from_str("<p>{{{{msg}}}}</p>")
    let vars = TemplateVars::new()
        .text("msg", Bytes::from_str("<script>alert('xss' & \"1\")</script>"))
    show(render(&tmpl, &vars))
    vars.free()
    return 0
}
"##,
        "&amp;&lt;&gt;&quot;&#39;\n\
         \n\
         hello world\n\
         <p>&lt;script&gt;alert(&#39;xss&#39; &amp; &quot;1&quot;)&lt;/script&gt;</p>\n",
    );
}

#[test]
fn render_handles_html_int_spaced_missing_unterminated_and_growth() {
    expect_stdout(
        "render_features",
        r##"
fn main() -> i32 {
    let tmpl = Bytes::from_str("<h1>{{{{ title }}}}</h1><div>{{{{raw}}}}</div><span>{{{{n}}}},{{{{z}}}},{{{{neg}}}}</span>[{{{{missing}}}}]{{{{ \tspaced\t }}}}<style>body {{ color: red; }}</style>tail {{{{open")
    let vars = TemplateVars::new()
        .text("title", Bytes::from_str("Old"))
        .text("title", Bytes::from_str("A & B"))
        .html("raw", Bytes::from_str("<b>bold</b>"))
        .int("n", 42i64)
        .int("z", 0i64)
        .int("neg", -7i64)
        .text("spaced", Bytes::from_str("ok"))
        .text("extra1", Bytes::from_str("1"))
        .text("extra2", Bytes::from_str("2"))
    show(render(&tmpl, &vars))
    vars.free()
    return 0
}
"##,
        "<h1>A &amp; B</h1><div><b>bold</b></div><span>42,0,-7</span>[]ok<style>body { color: red; }</style>tail {{open\n",
    );
}

#[test]
fn render_list_by_joining_row_templates() {
    expect_stdout(
        "render_list",
        r##"
fn main() -> i32 {
    let row_tmpl = Bytes::from_str("<li>{{{{id}}}}: {{{{name}}}}</li>")
    let page_tmpl = Bytes::from_str("<ul>{{{{items}}}}</ul>")

    let v1 = TemplateVars::new().int("id", 1i64).text("name", Bytes::from_str("A & B"))
    let v2 = TemplateVars::new().int("id", 2i64).text("name", Bytes::from_str("<C>"))
    let v3 = TemplateVars::new().int("id", 3i64).text("name", Bytes::from_str("D"))
    let rows = [
        render(&row_tmpl, &v1),
        render(&row_tmpl, &v2),
        render(&row_tmpl, &v3),
    ]
    let joined = template_join(rows as *const Bytes, 3usize)
    let page_vars = TemplateVars::new().html("items", joined)
    show(render(&page_tmpl, &page_vars))

    let mut acc = String::new()
    let mut i: i64 = 1i64
    while i <= 2i64 {
        let rv = TemplateVars::new().int("id", i).text("name", Bytes::from_str("row"))
        acc = template_append(acc, render(&row_tmpl, &rv))
        rv.free()
        i = i + 1i64
    }
    let page_vars2 = TemplateVars::new().html("items", Bytes::view(acc.as_ptr(), acc.len()))
    show(render(&page_tmpl, &page_vars2))

    v1.free()
    v2.free()
    v3.free()
    page_vars.free()
    page_vars2.free()
    return 0
}
"##,
        "<ul><li>1: A &amp; B</li><li>2: &lt;C&gt;</li><li>3: D</li></ul>\n\
         <ul><li>1: row</li><li>2: row</li></ul>\n",
    );
}

#[test]
fn render_file_renders_existing_and_returns_none_for_missing() {
    expect_stdout(
        "render_file",
        r##"
fn main() -> i32 {
    let content = Bytes::from_str("<h1>{{{{ title }}}}</h1><p>{{{{body}}}}</p>")
    let _ = write_file_bytes("page.html", content.data, content.len)
    let _ = write_file_bytes("empty.html", "" as *const u8, 0usize)

    let vars = TemplateVars::new()
        .text("title", Bytes::from_str("<Home>"))
        .html("body", Bytes::from_str("<em>ok</em>"))

    show_opt(render_file("page.html", &vars))
    show_opt(render_file("empty.html", &vars))
    show_opt(render_file("does_not_exist.html", &vars))

    vars.free()
    return 0
}
"##,
        "<h1>&lt;Home&gt;</h1><p><em>ok</em></p>\n\
         \n\
         <none>\n",
    );
}
