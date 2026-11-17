//! `umay lsp` driven over a real pipe, the way an editor drives it.
//!
//! The unit tests inside the server call the handlers directly. These tests do
//! not: they write `Content-Length` frames into the process's stdin and read the
//! frames that come back, because the failures worth catching here are framing
//! ones -- a header that miscounts bytes, a reply with no id, a notification that
//! gets answered. None of those are visible to a test that skips the wire.

use std::io::Write;
use std::process::{Command, Stdio};

fn compiler() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_BIN_EXE_umay"))
}

fn frame(body: &str) -> Vec<u8> {
    let mut out = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
    out.extend_from_slice(body.as_bytes());
    out
}

/// Sends every message, closes stdin, and returns the reply bodies.
fn exchange(messages: &[&str]) -> Vec<String> {
    let mut child = Command::new(compiler())
        .arg("lsp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start the server");
    {
        let stdin = child.stdin.as_mut().expect("stdin");
        for message in messages {
            stdin.write_all(&frame(message)).expect("write a message");
        }
    }
    let output = child.wait_with_output().expect("the server should finish");
    assert!(
        output.stderr.is_empty(),
        "the server wrote to stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    split_frames(&String::from_utf8_lossy(&output.stdout))
}

/// Splits a stream of framed messages into their bodies.
fn split_frames(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(head) = rest.find("\r\n\r\n") {
        let headers = &rest[..head];
        let length: usize = headers
            .lines()
            .find_map(|line| line.strip_prefix("Content-Length:"))
            .and_then(|n| n.trim().parse().ok())
            .unwrap_or_else(|| panic!("no Content-Length in {headers:?}"));
        let body_start = head + 4;
        let body = rest[body_start..body_start + length].to_string();
        out.push(body);
        rest = &rest[body_start + length..];
    }
    out
}

fn initialize() -> &'static str {
    r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"capabilities":{}}}"#
}

fn open(uri: &str, text: &str) -> String {
    format!(
        r#"{{"jsonrpc":"2.0","method":"textDocument/didOpen","params":{{"textDocument":{{"uri":"{uri}","languageId":"umay","version":1,"text":{}}}}}}}"#,
        json_string(text)
    )
}

/// Escapes text into a JSON string literal.
fn json_string(text: &str) -> String {
    let mut out = String::from("\"");
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Finds the reply with a given id, as raw text.
fn reply_with_id(bodies: &[String], id: i64) -> Option<&str> {
    let needle = format!(r#""id":{id}"#);
    bodies.iter().map(|b| b.as_str()).find(|b| b.contains(&needle))
}

#[test]
fn initialize_announces_every_capability_the_server_has() {
    let replies = exchange(&[initialize()]);
    let reply = reply_with_id(&replies, 1).expect("a reply to initialize");
    for capability in [
        "hoverProvider",
        "definitionProvider",
        "completionProvider",
        "documentSymbolProvider",
        "documentFormattingProvider",
    ] {
        assert!(reply.contains(capability), "missing {capability} in {reply}");
    }
    assert!(reply.contains(r#""name":"umay""#), "the server should name itself");
}

#[test]
fn an_opened_file_is_checked_and_its_diagnostics_published() {
    let text = "fn main() -> i32 {\n    return \"not an integer\"\n}\n";
    let replies = exchange(&[initialize(), &open("file:///t.um", text)]);
    let publish = replies
        .iter()
        .find(|b| b.contains("textDocument/publishDiagnostics"))
        .expect("opening a file should publish its diagnostics");
    assert!(publish.contains("E1001"), "expected the type error in {publish}");
    assert!(publish.contains(r#""severity":1"#), "an error has severity 1 in {publish}");
    assert!(publish.contains(r#""uri":"file:///t.um""#), "the report names its file in {publish}");
}

#[test]
fn fixing_the_file_clears_the_squiggle() {
    let broken = "fn main() -> i32 {\n    return \"not an integer\"\n}\n";
    let fixed = "fn main() -> i32 {\n    return 0\n}\n";
    let change = format!(
        r#"{{"jsonrpc":"2.0","method":"textDocument/didChange","params":{{"textDocument":{{"uri":"file:///t.um"}},"contentChanges":[{{"text":{}}}]}}}}"#,
        json_string(fixed)
    );
    let replies = exchange(&[initialize(), &open("file:///t.um", broken), &change]);
    let publishes: Vec<&String> =
        replies.iter().filter(|b| b.contains("publishDiagnostics")).collect();
    assert_eq!(publishes.len(), 2, "one report per edit, got {publishes:?}");
    assert!(publishes[0].contains("E1001"), "the first report has the error");
    assert!(
        publishes[1].contains(r#""diagnostics":[]"#),
        "the second report is empty, got {}",
        publishes[1]
    );
}

#[test]
fn hover_describes_the_name_under_the_cursor() {
    let text = "fn add(a: i64, b: i64) -> i64 {\n    return a + b\n}\n";
    let hover = "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"textDocument/hover\",\"params\":{\"textDocument\":{\"uri\":\"file:///t.um\"},\"position\":{\"line\":0,\"character\":4}}}";
    let replies = exchange(&[initialize(), &open("file:///t.um", text), hover]);
    let reply = reply_with_id(&replies, 2).expect("a reply to hover");
    assert!(
        reply.contains("fn add(a: i64, b: i64) -> i64"),
        "the hover should show the signature, got {reply}"
    );
}

#[test]
fn a_hover_over_nothing_answers_null_rather_than_failing() {
    let text = "fn f() -> i64 {\n    return 1i64\n}\n";
    let hover = r#"{"jsonrpc":"2.0","id":2,"method":"textDocument/hover","params":{"textDocument":{"uri":"file:///t.um"},"position":{"line":2,"character":0}}}"#;
    let replies = exchange(&[initialize(), &open("file:///t.um", text), hover]);
    let reply = reply_with_id(&replies, 2).expect("a reply to hover");
    assert!(reply.contains(r#""result":null"#), "got {reply}");
}

#[test]
fn definition_finds_the_declaration_of_a_called_function() {
    let text = "fn add(a: i64, b: i64) -> i64 {\n    return a + b\n}\n\nfn main() -> i32 {\n    return add(1i64, 2i64) as i32\n}\n";
    let definition = r#"{"jsonrpc":"2.0","id":3,"method":"textDocument/definition","params":{"textDocument":{"uri":"file:///t.um"},"position":{"line":5,"character":13}}}"#;
    let replies = exchange(&[initialize(), &open("file:///t.um", text), definition]);
    let reply = reply_with_id(&replies, 3).expect("a reply to definition");
    assert!(reply.contains("file:///t.um"), "should point into the same file: {reply}");
    assert!(reply.contains(r#""line":0"#), "should point at line 0, got {reply}");
}

#[test]
fn completion_offers_what_the_file_declares() {
    let text = "fn alpha() -> i64 {\n    return 1i64\n}\n\nstruct Beta {\n    n: i64,\n}\n";
    let completion = r#"{"jsonrpc":"2.0","id":4,"method":"textDocument/completion","params":{"textDocument":{"uri":"file:///t.um"},"position":{"line":0,"character":0}}}"#;
    let replies = exchange(&[initialize(), &open("file:///t.um", text), completion]);
    let reply = reply_with_id(&replies, 4).expect("a reply to completion");
    assert!(reply.contains(r#""label":"alpha""#), "got {reply}");
    assert!(reply.contains(r#""label":"Beta""#), "got {reply}");
    // LSP symbol kinds: 12 is a function, 22 a struct.
    assert!(reply.contains(r#""kind":12"#), "a function has kind 12, got {reply}");
    assert!(reply.contains(r#""kind":22"#), "a struct has kind 22, got {reply}");
}

#[test]
fn formatting_offers_the_formatter_s_answer() {
    // Trailing whitespace and a stray double space inside the signature: the
    // formatter rewrites the gap, so the response is a whole-document edit.
    let text = "fn f(a:  i64) -> i64 {   \n    return a\n}\n";
    let formatting = r#"{"jsonrpc":"2.0","id":5,"method":"textDocument/formatting","params":{"textDocument":{"uri":"file:///t.um"},"options":{}}}"#;
    let replies = exchange(&[initialize(), &open("file:///t.um", text), formatting]);
    let reply = reply_with_id(&replies, 5).expect("a reply to formatting");
    assert!(reply.contains("newText"), "a change should be offered, got {reply}");
    assert!(!reply.contains("(a:  i64)"), "the double space should be gone, got {reply}");
}

#[test]
fn an_already_formatted_file_offers_no_edit() {
    let text = "fn f(a: i64) -> i64 {\n    return a\n}\n";
    let formatting = r#"{"jsonrpc":"2.0","id":5,"method":"textDocument/formatting","params":{"textDocument":{"uri":"file:///t.um"},"options":{}}}"#;
    let replies = exchange(&[initialize(), &open("file:///t.um", text), formatting]);
    let reply = reply_with_id(&replies, 5).expect("a reply to formatting");
    assert!(reply.contains(r#""result":[]"#), "nothing to do, got {reply}");
}

#[test]
fn closing_a_file_clears_its_diagnostics() {
    let text = "fn main() -> i32 {\n    return \"nope\"\n}\n";
    let close = r#"{"jsonrpc":"2.0","method":"textDocument/didClose","params":{"textDocument":{"uri":"file:///t.um"}}}"#;
    let replies = exchange(&[initialize(), &open("file:///t.um", text), close]);
    let publishes: Vec<&String> =
        replies.iter().filter(|b| b.contains("publishDiagnostics")).collect();
    assert_eq!(publishes.len(), 2, "one report on open, one on close");
    assert!(
        publishes[1].contains(r#""diagnostics":[]"#),
        "closing should clear the squiggle, got {}",
        publishes[1]
    );
}

#[test]
fn a_notification_is_never_answered() {
    let text = "fn f() -> i64 {\n    return 1i64\n}\n";
    let replies = exchange(&[initialize(), &open("file:///t.um", text)]);
    // `initialize` is the only request; everything else the server sent is a
    // notification, and a notification has no id.
    for reply in &replies {
        if reply.contains("publishDiagnostics") {
            assert!(!reply.contains(r#""id""#), "a notification must not carry an id: {reply}");
        }
    }
}

#[test]
fn an_unknown_method_is_an_error_with_the_request_s_id() {
    let bogus = r#"{"jsonrpc":"2.0","id":9,"method":"textDocument/nonsense","params":{}}"#;
    let replies = exchange(&[initialize(), bogus]);
    let reply = reply_with_id(&replies, 9).expect("the error should answer the request");
    assert!(reply.contains(r#""code":-32601"#), "method not found, got {reply}");
}

#[test]
fn malformed_json_is_reported_rather_than_dropped() {
    let replies = exchange(&[initialize(), "{not json"]);
    let parse_error = replies
        .iter()
        .find(|b| b.contains("-32700"))
        .expect("a parse error should be reported");
    assert!(parse_error.contains(r#""id":null"#), "got {parse_error}");
}

#[test]
fn shutdown_answers_and_a_later_request_is_refused() {
    let shutdown = r#"{"jsonrpc":"2.0","id":2,"method":"shutdown","params":null}"#;
    let after = r#"{"jsonrpc":"2.0","id":3,"method":"textDocument/completion","params":{"textDocument":{"uri":"file:///t.um"},"position":{"line":0,"character":0}}}"#;
    let replies = exchange(&[initialize(), shutdown, after]);
    assert!(reply_with_id(&replies, 2).is_some(), "shutdown should be answered");
    let reply = reply_with_id(&replies, 3).expect("the late request should be answered with an error");
    assert!(reply.contains(r#""error""#), "a request after shutdown is refused, got {reply}");
}

#[test]
fn the_server_stops_when_stdin_closes() {
    // An editor that exits without saying goodbye closes the pipe. The server
    // must stop rather than spin.
    let replies = exchange(&[initialize()]);
    assert!(!replies.is_empty(), "initialize should have been answered");
}

#[test]
fn a_file_that_is_not_umay_is_not_checked() {
    let message = format!(
        r#"{{"jsonrpc":"2.0","method":"textDocument/didOpen","params":{{"textDocument":{{"uri":"file:///README.md","languageId":"markdown","version":1,"text":{}}}}}}}"#,
        json_string("# a heading\n\nThis is prose, not a program.\n")
    );
    let replies = exchange(&[initialize(), &message]);
    let publish = replies
        .iter()
        .find(|b| b.contains("publishDiagnostics"))
        .expect("the server still answers for a non-UMAY file");
    assert!(
        publish.contains(r#""diagnostics":[]"#),
        "prose is not a program and has no errors, got {publish}"
    );
}

#[test]
fn hover_finds_a_name_in_a_file_with_non_ascii_text() {
    // Columns in this protocol are UTF-16 units, so a line with a non-ASCII
    // character before the cursor would put the hover on the wrong word if the
    // conversion counted bytes.
    let text = "// café ☕\nfn measure() -> i64 {\n    return 1i64\n}\n";
    let hover = r#"{"jsonrpc":"2.0","id":2,"method":"textDocument/hover","params":{"textDocument":{"uri":"file:///t.um"},"position":{"line":1,"character":4}}}"#;
    let replies = exchange(&[initialize(), &open("file:///t.um", text), hover]);
    let reply = reply_with_id(&replies, 2).expect("a reply to hover");
    assert!(
        reply.contains("fn measure() -> i64"),
        "the hover should find `measure`, got {reply}"
    );
}

#[test]
fn hover_finds_a_function_that_lives_in_a_module() {
    // The editor asks the server for a document's symbols by handing it the text.
    // The text on its own does not contain a module's items, so hover answered
    // `null` for every name reached through `mod` -- the server had to be told
    // which file the text came from so the module could be read from disk.
    let dir = std::env::temp_dir().join(format!("umay_lsp_hover_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create the directory");
    std::fs::write(
        dir.join("kinds.um"),
        "pub fn weigh(k: i64) -> i64 {\n    return k\n}\n",
    )
    .expect("write the module");
    let main = dir.join("main.um");
    let text = "fn main() -> i32 {\n    println_i64(kinds::weigh(3i64))\n    return 0\n}\n\nmod kinds;\n";
    std::fs::write(&main, text).expect("write the program");

    let uri = format!("file://{}", main.display());
    // The cursor sits inside `weigh`, at the character after its first.
    let hover = format!(
        r#"{{"jsonrpc":"2.0","id":2,"method":"textDocument/hover","params":{{"textDocument":{{"uri":"{uri}"}},"position":{{"line":1,"character":24}}}}}}"#
    );
    let replies = exchange(&[initialize(), &open(&uri, text), &hover]);
    let reply = reply_with_id(&replies, 2).expect("a hover reply");
    assert!(
        reply.contains("weigh"),
        "hover in a module's function should describe it, got {reply}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn definition_jumps_into_a_module_file() {
    // `kinds::weigh` is declared in `kinds.um`. The scan used to look only at the
    // document the editor is showing, where that name appears once and declares
    // nothing, so go-to-definition answered `null`.
    let dir = std::env::temp_dir().join(format!("umay_lsp_def_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create the directory");
    std::fs::write(
        dir.join("kinds.um"),
        "pub fn weigh(k: i64) -> i64 {\n    return k\n}\n",
    )
    .expect("write the module");
    let main = dir.join("main.um");
    let text = "fn main() -> i32 {\n    println_i64(kinds::weigh(3i64))\n    return 0\n}\n\nmod kinds;\n";
    std::fs::write(&main, text).expect("write the program");

    let uri = format!("file://{}", main.display());
    let request = format!(
        r#"{{"jsonrpc":"2.0","id":2,"method":"textDocument/definition","params":{{"textDocument":{{"uri":"{uri}"}},"position":{{"line":1,"character":24}}}}}}"#
    );
    let replies = exchange(&[initialize(), &open(&uri, text), &request]);
    let reply = reply_with_id(&replies, 2).expect("a definition reply");
    assert!(
        reply.contains("kinds.um"),
        "the definition should be in the module's file, got {reply}"
    );
    assert!(
        !reply.contains("\"result\":null"),
        "go-to-definition found nothing, got {reply}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_declaration_with_a_visibility_is_still_a_definition()
{
    // `pub fn weigh` put `pub ` before the keyword, so the scan that looks for a
    // declaration did not recognise the line at all.
    let dir = std::env::temp_dir().join(format!("umay_lsp_pubdef_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create the directory");
    let main = dir.join("main.um");
    let text = "pub fn helper() -> i64 {\n    return 1i64\n}\n\nfn main() -> i32 {\n    println_i64(helper())\n    return 0\n}\n";
    std::fs::write(&main, text).expect("write the program");

    let uri = format!("file://{}", main.display());
    let request = format!(
        r#"{{"jsonrpc":"2.0","id":2,"method":"textDocument/definition","params":{{"textDocument":{{"uri":"{uri}"}},"position":{{"line":5,"character":18}}}}}}"#
    );
    let replies = exchange(&[initialize(), &open(&uri, text), &request]);
    let reply = reply_with_id(&replies, 2).expect("a definition reply");
    assert!(
        reply.contains("\"line\":0") || reply.contains("\"line\": 0"),
        "the definition is on the first line, got {reply}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
