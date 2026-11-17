//! The server loop and the requests it answers.
//!
//! The shape is one `Document` per open file, and every request re-derives what
//! it needs from the document's current text. There is no incremental cache: a
//! keystroke replaces the text, and the next request reads the new text. At the
//! size of a UMAY file that costs less than the bookkeeping would, and it cannot
//! serve a stale answer.
//!
//! Every handler is a pure function of a document and a position, so the tests
//! call them directly instead of driving a pipe.

use std::collections::BTreeMap;
use std::io::Write;

use super::jsonrpc::{self, Value};
use super::position::{LineIndex, Position, Range};
use crate::diagnostics::{Diagnostics, Severity};
use crate::fmt;
use crate::metadata;

/// An open file, as the editor last described it.
pub struct Document {
    pub text: String,
    /// The `languageId` the client sent. Only UMAY files are checked; a client
    /// may open a README in the same session.
    pub is_umay: bool,
    /// The file on disk the document came from, when the client named one.
    ///
    /// The checker resolves `mod name;` against the directory of the file it is
    /// compiling, so an editor buffer needs its own path or every module is a
    /// missing file. `None` for a buffer with no `file:` URI, which is checked
    /// as a file with no modules.
    pub path: Option<std::path::PathBuf>,
}

impl Document {
    /// The name to hand the compiler: the file's path, or a placeholder.
    fn source_name(&self) -> String {
        match &self.path {
            Some(path) => path.to_string_lossy().into_owned(),
            None => "<lsp>".to_string(),
        }
    }
}

/// The filesystem path a `file:` URI names.
///
/// Only `file:` URIs have one; a client may invent another scheme for an
/// in-memory buffer, and there is no directory to resolve modules against.
fn uri_path(uri: &str) -> Option<std::path::PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    let decoded = percent_decode(rest);
    Some(std::path::PathBuf::from(decoded))
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok();
            if let Some(value) = hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                out.push(value);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The state the server keeps between messages.
#[derive(Default)]
pub struct Server {
    documents: BTreeMap<String, Document>,
    /// Set by `shutdown`, so a later request is refused the way the protocol
    /// says it should be.
    shutting_down: bool,
}

/// Runs the server until the client closes the pipe or sends `exit`.
pub fn serve() -> i32 {
    let stdin = std::io::stdin();
    let mut input = stdin.lock();
    let stdout = std::io::stdout();
    let mut output = stdout.lock();
    let mut server = Server::default();
    loop {
        let message = match jsonrpc::read_frame(&mut input) {
            Ok(Some(body)) => body,
            Ok(None) => return 0,
            Err(e) => {
                eprintln!("umay lsp: {}", e);
                return 1;
            }
        };
        let Ok(request) = jsonrpc::parse(&message) else {
            // A message that is not JSON cannot be answered with a result; the
            // protocol says to report a parse error with a null id.
            let error = error_response(Value::Null, -32700, "parse error");
            let _ = output.write_all(&jsonrpc::frame(&jsonrpc::to_string(&error)));
            let _ = output.flush();
            continue;
        };
        let responses = server.handle(&request);
        for response in responses {
            let framed = jsonrpc::frame(&jsonrpc::to_string(&response));
            if output.write_all(&framed).is_err() {
                return 0;
            }
        }
        if output.flush().is_err() {
            return 0;
        }
    }
}

impl Server {
    /// Handles one message, returning the responses to send.
    ///
    /// A request gets one reply or one error. A notification gets none, unless it
    /// is one of the two the server answers with work of its own (diagnostics
    /// after an edit).
    pub fn handle(&mut self, request: &Value) -> Vec<Value> {
        let id = request.get("id").cloned();
        let method = request.get("method").and_then(|m| m.as_str()).unwrap_or("");
        let params = request.get("params").cloned().unwrap_or(Value::Null);

        // A message with no id is a notification: it gets no reply, but it may
        // cause the server to push diagnostics of its own.
        let Some(id) = id else {
            return self.notify(method, &params);
        };

        if self.shutting_down && method != "exit" {
            return vec![error_response(id, -32600, "the server is shutting down")];
        }

        match method {
            "initialize" => vec![self.initialize(id)],
            "shutdown" => {
                self.shutting_down = true;
                vec![result_response(id, Value::Null)]
            }
            "textDocument/hover" => vec![self.hover(id, &params)],
            "textDocument/definition" => vec![self.definition(id, &params)],
            "textDocument/completion" => vec![self.completion(id, &params)],
            "textDocument/documentSymbol" => vec![self.document_symbol(id, &params)],
            "textDocument/formatting" => vec![self.formatting(id, &params)],
            other => vec![error_response(id, -32601, &format!("unknown method `{other}`"))],
        }
    }

    /// Handles a notification, returning any messages it causes.
    ///
    /// An edit is the whole reason the server exists, so opening or changing a
    /// file publishes that file's diagnostics. Closing one clears them: a
    /// client that keeps the squiggles of a closed file is showing stale advice.
    fn notify(&mut self, method: &str, params: &Value) -> Vec<Value> {
        match method {
            "textDocument/didOpen" => {
                let uri = self.did_open(params).unwrap_or_default();
                self.publish(&uri).into_iter().collect()
            }
            "textDocument/didChange" => {
                let uri = self.did_change(params).unwrap_or_default();
                self.publish(&uri).into_iter().collect()
            }
            "textDocument/didSave" => {
                let Some(uri) = params
                    .get("textDocument")
                    .and_then(|d| d.get("uri"))
                    .and_then(|u| u.as_str())
                else {
                    return Vec::new();
                };
                self.publish(uri).into_iter().collect()
            }
            "textDocument/didClose" => {
                let Some(uri) = params
                    .get("textDocument")
                    .and_then(|d| d.get("uri"))
                    .and_then(|u| u.as_str())
                else {
                    return Vec::new();
                };
                let closed = uri.to_string();
                self.did_close(params);
                vec![empty_publish(&closed)]
            }
            // `initialized`, `$/setTrace`, `exit` need no reply and no work.
            _ => Vec::new(),
        }
    }

    /// The diagnostics notification for one open document, if it is open.
    fn publish(&self, uri: &str) -> Option<Value> {
        let document = self.documents.get(uri)?;
        Some(Self::diagnostics_for(uri, document))
    }

    // -- lifecycle ----------------------------------------------------------

    fn initialize(&mut self, id: Value) -> Value {
        let capabilities = Value::object([
            // Incremental sync is claimed, but a full-text change is also
            // accepted: a client that sends the whole document is simpler and
            // correct, and `did_change` reads it either way.
            ("textDocumentSync", Value::int(1)),
            ("hoverProvider", Value::Bool(true)),
            ("definitionProvider", Value::Bool(true)),
            (
                "completionProvider",
                Value::object([("triggerCharacters", Value::array([Value::str(".")]))]),
            ),
            ("documentSymbolProvider", Value::Bool(true)),
            ("documentFormattingProvider", Value::Bool(true)),
        ]);
        let result = Value::object([
            ("capabilities", capabilities),
            (
                "serverInfo",
                Value::object([
                    ("name", Value::str("umay")),
                    ("version", Value::str(env!("CARGO_PKG_VERSION"))),
                ]),
            ),
        ]);
        result_response(id, result)
    }

    fn did_open(&mut self, params: &Value) -> Option<String> {
        let text_document = params.get("textDocument")?;
        let uri = text_document.get("uri").and_then(|u| u.as_str())?;
        let text = text_document
            .get("text")
            .and_then(|t| t.as_str())
            .unwrap_or_default()
            .to_string();
        let is_umay = text_document
            .get("languageId")
            .and_then(|l| l.as_str())
            .map(|id| id == "umay" || id == "UMAY")
            // A client that sends no languageId is opening the file it named, and
            // a `.um` extension is reason enough to check it.
            .unwrap_or(uri.ends_with(".um"));
        let path = uri_path(uri);
        self.documents.insert(uri.to_string(), Document { text, is_umay, path });
        Some(uri.to_string())
    }

    fn did_change(&mut self, params: &Value) -> Option<String> {
        let text_document = params.get("textDocument")?;
        let uri = text_document.get("uri").and_then(|u| u.as_str())?;
        let changes = params.get("contentChanges").and_then(|c| c.as_array())?;
        let document = self.documents.get_mut(uri)?;
        // The last change is the current state. A full-sync client sends one
        // change with the whole text; an incremental client may send several,
        // each replacing a range.
        for change in changes {
            match change.get("range") {
                None => {
                    if let Some(text) = change.get("text").and_then(|t| t.as_str()) {
                        document.text = text.to_string();
                    }
                }
                Some(range) => {
                    let Some(text) = change.get("text").and_then(|t| t.as_str()) else {
                        continue;
                    };
                    let index = LineIndex::new(&document.text);
                    let start = index.offset(
                        &document.text,
                        Position::new(
                            range
                                .get("start")
                                .and_then(|s| s.get("line"))
                                .and_then(|l| l.as_i64())
                                .unwrap_or(0) as u32,
                            range
                                .get("start")
                                .and_then(|s| s.get("character"))
                                .and_then(|c| c.as_i64())
                                .unwrap_or(0) as u32,
                        ),
                    ) as usize;
                    let end = index.offset(
                        &document.text,
                        Position::new(
                            range.get("end").and_then(|e| e.get("line")).and_then(|l| l.as_i64())
                                .unwrap_or(0) as u32,
                            range
                                .get("end")
                                .and_then(|e| e.get("character"))
                                .and_then(|c| c.as_i64())
                                .unwrap_or(0) as u32,
                        ),
                    ) as usize;
                    let (start, end) = (start.min(document.text.len()), end.min(document.text.len()));
                    let (start, end) = if start <= end { (start, end) } else { (end, start) };
                    document.text.replace_range(start..end, text);
                }
            }
        }
        Some(uri.to_string())
    }

    fn did_close(&mut self, params: &Value) {
        let Some(uri) = params
            .get("textDocument")
            .and_then(|d| d.get("uri"))
            .and_then(|u| u.as_str())
        else {
            return;
        };
        self.documents.remove(uri);
    }

    // -- requests -----------------------------------------------------------

    fn document<'a>(&'a self, params: &'a Value) -> Option<(&'a str, &'a Document)> {
        let uri = params.get("textDocument")?.get("uri")?.as_str()?;
        self.documents.get(uri).map(|d| (uri, d))
    }

    /// A position from `params`, plus the offset it names in the document.
    fn cursor(params: &Value, document: &Document) -> Option<(Position, u32)> {
        let position = params.get("position")?;
        let line = position.get("line")?.as_i64()? as u32;
        let character = position.get("character")?.as_i64()? as u32;
        let position = Position::new(line, character);
        let index = LineIndex::new(&document.text);
        Some((position, index.offset(&document.text, position)))
    }

    /// Publishes diagnostics for a document, as the `textDocument/publishDiagnostics`
    /// notification.
    pub fn diagnostics_for(uri: &str, document: &Document) -> Value {
        let diags = if document.is_umay {
            check(document)
        } else {
            Diagnostics::new()
        };
        let index = LineIndex::new(&document.text);
        let items: Vec<Value> = diags
            .items()
            .iter()
            .map(|d| {
                let range = match d.span {
                    Some(span) => {
                        // Diagnostics carry byte spans over the document text,
                        // which is the file the editor has open.
                        let line_of = |offset: u32| index.position(&document.text, offset);
                        Range::new(line_of(span.start), line_of(span.end))
                    }
                    None => Range::new(Position::new(0, 0), Position::new(0, 0)),
                };
                let mut item = BTreeMap::new();
                item.insert(
                    "range".to_string(),
                    range_value(range.start, range.end),
                );
                item.insert(
                    "severity".to_string(),
                    Value::int(match d.severity {
                        // LSP severities: 1 error, 2 warning, 3 information,
                        // 4 hint. A note is information rather than a hint: it
                        // is something the compiler has to say.
                        Severity::Error => 1,
                        Severity::Warning => 2,
                        Severity::Note | Severity::Help => 3,
                    }),
                );
                item.insert("source".to_string(), Value::str("umay"));
                item.insert("code".to_string(), Value::str(d.code));
                item.insert("message".to_string(), Value::str(render_message(d)));
                Value::Object(item)
            })
            .collect();
        let mut payload = BTreeMap::new();
        payload.insert("uri".to_string(), Value::str(uri));
        payload.insert("diagnostics".to_string(), Value::Array(items));
        Value::object([
            ("jsonrpc", Value::str("2.0")),
            ("method", Value::str("textDocument/publishDiagnostics")),
            ("params", Value::Object(payload)),
        ])
    }

    fn hover(&self, id: Value, params: &Value) -> Value {
        let Some((_, document)) = self.document(params) else {
            return result_response(id, Value::Null);
        };
        let Some((_, offset)) = Self::cursor(params, document) else {
            return result_response(id, Value::Null);
        };
        let word = match word_at(&document.text, offset) {
            Some(w) => w,
            None => return result_response(id, Value::Null),
        };
        match describe(&document.text, &document.source_name(), &word) {
            Some(text) => result_response(
                id,
                Value::object([
                    (
                        "contents",
                        Value::object([
                            ("kind", Value::str("markdown")),
                            ("value", Value::str(text)),
                        ]),
                    ),
                ]),
            ),
            None => result_response(id, Value::Null),
        }
    }

    fn definition(&self, id: Value, params: &Value) -> Value {
        let Some((uri, document)) = self.document(params) else {
            return result_response(id, Value::Null);
        };
        let Some((_, offset)) = Self::cursor(params, document) else {
            return result_response(id, Value::Null);
        };
        let Some(word) = word_at(&document.text, offset) else {
            return result_response(id, Value::Null);
        };

        // A name reached through a module is declared in the module's own file,
        // which is not the one the editor is showing. Scanning only the current
        // document answered `null` -- and `null` is also what an editor gets for
        // punctuation, so it looked like "nothing to go to" rather than "found
        // nothing because I looked in one file".
        let target = definition_offset(&document.text, &word)
            .map(|start| (uri.to_string(), document.text.clone(), start))
            .or_else(|| {
                let (file, start) = module_definition(document, &document.text, offset, &word)?;
                let text = std::fs::read_to_string(&file).ok()?;
                Some((format!("file://{}", file.display()), text, start))
            });
        let Some((target_uri, target_text, start)) = target else {
            return result_response(id, Value::Null);
        };

        let index = LineIndex::new(&target_text);
        let position = index.position(&target_text, start);
        // The whole declaration line is the target, which is what an editor
        // scrolls to.
        let end = target_text[start as usize..]
            .find('\n')
            .map(|i| start + i as u32)
            .unwrap_or(target_text.len() as u32);
        let end_position = index.position(&target_text, end);
        result_response(
            id,
            Value::object([
                ("uri", Value::str(target_uri.as_str())),
                ("range", range_value(position, end_position)),
            ]),
        )
    }

    fn completion(&self, id: Value, params: &Value) -> Value {
        let Some((_, document)) = self.document(params) else {
            return result_response(id, empty_completion());
        };
        let symbols =
            metadata::symbols_or_empty_at(&document.source_name(), &document.text);
        let items: Vec<Value> = symbols
            .iter()
            .map(|symbol| {
                let (name, kind, detail) = symbol_summary(symbol);
                Value::object([
                    ("label", Value::str(name)),
                    ("kind", Value::int(kind)),
                    ("detail", Value::str(detail)),
                ])
            })
            .collect();
        result_response(
            id,
            Value::object([("isIncomplete", Value::Bool(false)), ("items", Value::Array(items))]),
        )
    }

    fn document_symbol(&self, id: Value, params: &Value) -> Value {
        let Some((_, document)) = self.document(params) else {
            return result_response(id, Value::Array(Vec::new()));
        };
        let index = LineIndex::new(&document.text);
        let items: Vec<Value> =
            metadata::symbols_or_empty_at(&document.source_name(), &document.text)
            .iter()
            .map(|symbol| {
                let (name, kind, detail) = symbol_summary(symbol);
                // The outline entry points at the declaration it names. When the
                // declaration cannot be found -- it is generated, or the text
                // changed between the parse and this call -- the range falls
                // back to the top of the file rather than guessing.
                let offset = definition_offset(&document.text, symbol.short_name())
                    .unwrap_or(0);
                let range = index.range(&document.text, offset, offset);
                Value::object([
                    ("name", Value::str(name)),
                    ("kind", Value::int(kind)),
                    ("detail", Value::str(detail)),
                    ("range", range_value(range.start, range.end)),
                    ("selectionRange", range_value(range.start, range.end)),
                ])
            })
            .collect();
        result_response(id, Value::Array(items))
    }

    fn formatting(&self, id: Value, params: &Value) -> Value {
        let Some((_, document)) = self.document(params) else {
            return result_response(id, Value::Array(Vec::new()));
        };
        let formatted = fmt::format(&document.text);
        if formatted.errors > 0 || formatted.text == document.text {
            // Nothing to change, or nothing that can be changed safely: the
            // formatter refuses a file it cannot lex, and the editor should not
            // rewrite it on a guess.
            return result_response(id, Value::Array(Vec::new()));
        }
        let index = LineIndex::new(&document.text);
        let end = index.position(&document.text, document.text.len() as u32);
        result_response(
            id,
            Value::array([Value::object([
                (
                    "range",
                    range_value(Position::new(0, 0), end),
                ),
                ("newText", Value::str(formatted.text)),
            ])]),
        )
    }
}

/// The notification that clears a file's diagnostics.
fn empty_publish(uri: &str) -> Value {
    Value::object([
        ("jsonrpc", Value::str("2.0")),
        ("method", Value::str("textDocument/publishDiagnostics")),
        (
            "params",
            Value::object([("uri", Value::str(uri)), ("diagnostics", Value::Array(Vec::new()))]),
        ),
    ])
}

fn empty_completion() -> Value {
    Value::object([("isIncomplete", Value::Bool(false)), ("items", Value::Array(Vec::new()))])
}

/// The diagnostic message with its cause attached, which is what a person
/// reading a squiggle wants.
fn render_message(d: &crate::diagnostics::Diagnostic) -> String {
    match &d.cause {
        Some(cause) => format!("{}\n{}", d.message, cause),
        None => d.message.clone(),
    }
}

fn range_value(start: Position, end: Position) -> Value {
    let position = |p: Position| {
        Value::object([("line", Value::int(p.line)), ("character", Value::int(p.character()))])
    };
    Value::object([("start", position(start)), ("end", position(end))])
}

fn result_response(id: Value, result: Value) -> Value {
    Value::object([("jsonrpc", Value::str("2.0")), ("id", id), ("result", result)])
}

fn error_response(id: Value, code: i32, message: &str) -> Value {
    Value::object([
        ("jsonrpc", Value::str("2.0")),
        ("id", id),
        (
            "error",
            Value::object([("code", Value::int(code)), ("message", Value::str(message))]),
        ),
    ])
}

/// Runs the checker on a document.
///
/// `analyze_source` runs the whole frontend, which is what `umay check` runs:
/// parsing alone would let a type error sit in the editor with no squiggle.
fn check(document: &Document) -> Diagnostics {
    let (diags, _map) = crate::analyze_source(&document.source_name(), &document.text);
    diags
}

/// The identifier the cursor is inside, if any.
///
/// A cursor sits between two characters, so it is inside a word when either
/// neighbour is a word character. That is what makes hover work when the caret
/// is at the end of a name.
pub fn word_at(text: &str, offset: u32) -> Option<String> {
    let offset = offset.min(text.len() as u32) as usize;
    let is_word = |c: char| c.is_alphanumeric() || c == '_';
    let bytes = text.as_bytes();
    let mut start = offset;
    while start > 0 {
        // Step back over whole characters, not bytes.
        let prev = text[..start].chars().next_back()?;
        if is_word(prev) {
            start -= prev.len_utf8();
        } else {
            break;
        }
    }
    let mut end = offset;
    while end < bytes.len() {
        let next = text[end..].chars().next()?;
        if is_word(next) {
            end += next.len_utf8();
        } else {
            break;
        }
    }
    if start == end {
        None
    } else {
        Some(text[start..end].to_string())
    }
}

/// A one-line description of a symbol, for a hover box.
fn describe(text: &str, name: &str, word: &str) -> Option<String> {
    // The document's own name, so a module's items are in the list: hovering
    // `describe` in a file that only says `mod kinds;` found nothing when the
    // compiler was pointed at that one file.
    for symbol in metadata::symbols_or_empty_at(name, text) {
        if symbol.name() == word || symbol.name().ends_with(&format!("::{word}")) {
            let (_, _, detail) = symbol_summary(&symbol);
            return Some(format!("```umay\n{detail}\n```"));
        }
    }
    None
}

/// Where a name qualified by a module is declared, as a file and a byte offset.
///
/// `kinds::weigh` names a function in `kinds.um`. The module is read from disk
/// beside the document, the same way the compiler reads it, and the declaration
/// is found there by the same scan used for the document itself.
fn module_definition(
    document: &Document,
    text: &str,
    offset: u32,
    word: &str,
) -> Option<(std::path::PathBuf, u32)> {
    let path = document.path.as_ref()?;
    let dir = path.parent()?;

    // The module is whatever precedes `::` before the word on this line.
    let line_start = text[..offset.min(text.len() as u32) as usize]
        .rfind('\n')
        .map(|i| i + 1)
        .unwrap_or(0);
    // The cursor may sit anywhere inside the word, so the text before the word
    // starts -- not before the cursor -- is what precedes `::`.
    let cursor = offset.min(text.len() as u32) as usize;
    let mut word_start = cursor;
    while word_start > line_start {
        let Some(prev) = text[..word_start].chars().next_back() else { break };
        if prev.is_alphanumeric() || prev == '_' {
            word_start -= prev.len_utf8();
        } else {
            break;
        }
    }
    let before = &text[line_start..word_start];
    let trimmed = before.trim_end();
    if !trimmed.ends_with("::") {
        return None;
    }
    let head = trimmed.trim_end_matches("::");
    let module_name: String = head
        .chars()
        .rev()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect::<Vec<char>>()
        .into_iter()
        .rev()
        .collect();
    if module_name.is_empty() {
        return None;
    }

    // A module may be a dependency rather than a neighbour, so the same rule the
    // compiler uses applies here.
    let mut file = crate::modules::find_module_file(dir, &module_name);
    if file.is_none() {
        for dependency in crate::modules::dependencies_of_project(dir) {
            if dependency.name == module_name {
                file = crate::modules::find_module_file(&dependency.source_dir, &module_name);
                break;
            }
        }
    }
    let file = file?;
    let source = std::fs::read_to_string(&file).ok()?;
    let start = definition_offset(&source, word);
    Some((file, start?))
}

/// Where a name is declared, as a byte offset.
fn definition_offset(text: &str, word: &str) -> Option<u32> {
    // Every identifier occurrence, in order; the first one that starts a
    // declaration is the definition.
    let mut search_from = 0usize;
    while let Some(found) = text[search_from..].find(word) {
        let at = search_from + found;
        search_from = at + word.len();
        // Only a whole word counts: `find` would otherwise match `len` inside
        // `byte_len`.
        let before_ok = text[..at].chars().next_back().map(|c| !(c.is_alphanumeric() || c == '_')).unwrap_or(true);
        let after = at + word.len();
        let after_ok = text[after..].chars().next().map(|c| !(c.is_alphanumeric() || c == '_')).unwrap_or(true);
        if !before_ok || !after_ok {
            continue;
        }
        let line_start = text[..at].rfind('\n').map(|i| i + 1).unwrap_or(0);
        let before = text[line_start..at].trim_start();
        // A declaration is the name after `fn`, `struct`, `enum`, `trait`,
        // `const` or `type`, or a name followed by `: Type` in a field list.
        // `pub ` may precede any of them, and a method may also be `pub`.
        let before = before.strip_prefix("pub ").unwrap_or(before);
        let keywords = ["fn ", "struct ", "enum ", "trait ", "const ", "type "];
        if keywords.iter().any(|k| before.starts_with(k)) {
            return Some(at as u32);
        }
    }
    None
}

/// The LSP symbol kind, and the text shown beside a completion.
fn symbol_summary(symbol: &metadata::Symbol) -> (String, i64, String) {
    use metadata::Symbol;
    // LSP symbol kinds: 12 = Function, 22 = Struct, 10 = Enum, 11 = Interface,
    // 14 = Constant, 26 = TypeParameter. These are the ones that fit.
    match symbol {
        Symbol::Fn { name, .. } => (name.clone(), 12, text_of(symbol)),
        Symbol::Struct { name, .. } => (name.clone(), 22, text_of(symbol)),
        Symbol::Enum { name, .. } => (name.clone(), 10, text_of(symbol)),
        Symbol::Trait { name, .. } => (name.clone(), 11, text_of(symbol)),
        Symbol::Const { name, .. } => (name.clone(), 14, text_of(symbol)),
        Symbol::Alias { name, .. } => (name.clone(), 26, text_of(symbol)),
        Symbol::Impl { owner, .. } => (owner.clone(), 22, text_of(symbol)),
        Symbol::Module { name } => (name.clone(), 2, text_of(symbol)),
    }
}

fn text_of(symbol: &metadata::Symbol) -> String {
    metadata::to_text(std::slice::from_ref(symbol)).trim().to_string()
}
