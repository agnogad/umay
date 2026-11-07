//! Diagnostics: the AI-first error reporting layer.
//!
//! Design goals (see `docs/diagnostics.md`):
//!
//! * every diagnostic carries a **stable** error code (`E0001`, `E0201`, ...),
//! * every diagnostic carries structured fields (cause, explanation, fixes),
//! * the same diagnostic can be rendered for humans *and* serialized to the
//!   versioned machine-readable JSON schema consumed by editor and AI tooling.
//!
//! Diagnostics are pure data. Rendering lives in this module so that the
//! compiler phases never depend on presentation.

use crate::source::{FileId, LineCol, SourceMap, Span};
use std::fmt;

/// Version of the machine-readable diagnostic schema. Bump only on breaking
/// changes; additive fields do not require a bump.
pub const DIAGNOSTIC_SCHEMA_VERSION: u32 = 1;

/// Severity, ordered from least to most severe.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Default)]
pub enum Severity {
    Note,
    Help,
    #[default]
    Warning,
    Error,
}

impl Severity {
    pub fn as_str(self) -> &'static str {
        match self {
            Severity::Note => "note",
            Severity::Help => "help",
            Severity::Warning => "warning",
            Severity::Error => "error",
        }
    }

    /// Sort key used by the driver to order diagnostics deterministically.
    pub fn rank(self) -> u8 {
        match self {
            Severity::Note => 0,
            Severity::Help => 1,
            Severity::Warning => 2,
            Severity::Error => 3,
        }
    }

    /// Whether a build should abort because of this diagnostic.
    pub fn is_fatal(self) -> bool {
        self == Severity::Error
    }
}

/// Textual replacement a tool (or human) may apply to fix a diagnostic.
///
/// The compiler only emits a suggestion when the replacement is known to be
/// type-correct and memory-safe. Speculative fixes are forbidden: `E0101`
/// (unknown name) never invents a name, it only renames to a name that the
/// compiler actually resolved.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Suggestion {
    /// Short imperative description, e.g. `Borrow the value`.
    pub description: String,
    /// Span replaced by `replacement`.
    pub span: Span,
    /// Replacement text.
    pub replacement: String,
    /// Optional extra detail shown after the code diff.
    pub detail: Option<String>,
}

impl Suggestion {
    pub fn new(description: impl Into<String>, span: Span, replacement: impl Into<String>) -> Self {
        Suggestion { description: description.into(), span, replacement: replacement.into(), detail: None }
    }

    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }
}

/// A secondary location attached to a diagnostic (`note: ...` lines with a span).
#[derive(Clone, Debug)]
pub struct Label {
    pub span: Span,
    pub message: String,
}

impl Label {
    pub fn new(span: Span, message: impl Into<String>) -> Self {
        Label { span, message: message.into() }
    }
}

/// One compiler diagnostic.
#[derive(Clone, Debug)]
pub struct Diagnostic {
    pub code: &'static str,
    pub severity: Severity,
    pub message: String,
    /// Primary location. Missing only for whole-program diagnostics.
    pub span: Option<Span>,
    /// Secondary locations, rendered as `note:` / `help:` annotations.
    pub labels: Vec<Label>,
    /// Root cause in plain language ("what actually went wrong").
    pub cause: Option<String>,
    /// Longer explanation of *why* the rule exists. Used by `umay explain`.
    pub explanation: Option<String>,
    /// Machine-applicable, safe fixes.
    pub suggestions: Vec<Suggestion>,
}

impl Diagnostic {
    pub fn new(code: &'static str, severity: Severity, message: impl Into<String>) -> Self {
        Diagnostic {
            code,
            severity,
            message: message.into(),
            span: None,
            labels: Vec::new(),
            cause: None,
            explanation: None,
            suggestions: Vec::new(),
        }
    }

    pub fn error(code: &'static str, message: impl Into<String>) -> Self {
        Diagnostic::new(code, Severity::Error, message)
    }

    pub fn warning(code: &'static str, message: impl Into<String>) -> Self {
        Diagnostic::new(code, Severity::Warning, message)
    }

    pub fn at(mut self, span: Span) -> Self {
        self.span = Some(span);
        self
    }

    pub fn with_span(mut self, span: impl Into<Option<Span>>) -> Self {
        self.span = span.into();
        self
    }

    pub fn with_label(mut self, span: Span, message: impl Into<String>) -> Self {
        self.labels.push(Label::new(span, message));
        self
    }

    pub fn with_cause(mut self, cause: impl Into<String>) -> Self {
        self.cause = Some(cause.into());
        self
    }

    pub fn with_explanation(mut self, explanation: impl Into<String>) -> Self {
        self.explanation = Some(explanation.into());
        self
    }

    pub fn with_suggestion(mut self, suggestion: Suggestion) -> Self {
        self.suggestions.push(suggestion);
        self
    }

    /// Sorted file/offset key used to order diagnostics deterministically.
    fn sort_key(&self) -> (u8, FileId, u32, u32, u8, &'static str) {
        let (file, start, end) = match self.span {
            Some(s) => (s.file, s.start, s.end),
            None => (FileId::DUMMY, u32::MAX, u32::MAX),
        };
        (0, file, start, end, self.severity.rank(), self.code)
    }
}

impl fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}[{}]: {}", self.severity.as_str(), self.code, self.message)
    }
}

/// A collection of diagnostics with ordering and query helpers.
#[derive(Default, Debug)]
pub struct Diagnostics {
    items: Vec<Diagnostic>,
}

impl Diagnostics {
    pub fn new() -> Self {
        Diagnostics::default()
    }

    pub fn push(&mut self, d: Diagnostic) {
        self.items.push(d);
    }

    pub fn extend(&mut self, other: impl IntoIterator<Item = Diagnostic>) {
        self.items.extend(other);
    }

    pub fn items(&self) -> &[Diagnostic] {
        &self.items
    }

    pub fn into_items(self) -> Vec<Diagnostic> {
        self.items
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn has_errors(&self) -> bool {
        self.items.iter().any(|d| d.severity.is_fatal())
    }

    pub fn error_count(&self) -> usize {
        self.items.iter().filter(|d| d.severity == Severity::Error).count()
    }

    pub fn warning_count(&self) -> usize {
        self.items.iter().filter(|d| d.severity == Severity::Warning).count()
    }

    /// Sorts diagnostics by file, position and severity. Idempotent.
    pub fn sort(&mut self) {
        self.items.sort_by(|a, b| a.sort_key().cmp(&b.sort_key()));
    }

    /// Splits into (errors, warnings) counters for summary lines.
    pub fn summary(&self) -> (usize, usize) {
        (self.error_count(), self.warning_count())
    }
}

// ---------------------------------------------------------------------------
// Human rendering
// ---------------------------------------------------------------------------

/// Rendering options for the human-readable form.
#[derive(Clone, Copy, Debug)]
pub struct RenderOptions {
    /// Print the `cause:` line when present.
    pub show_cause: bool,
    /// Print the `help:` explanation when present.
    pub show_explanation: bool,
    /// Print candidate replacements under `help:`.
    pub show_suggestions: bool,
    /// Add ANSI colors.
    pub color: bool,
}

impl Default for RenderOptions {
    fn default() -> Self {
        RenderOptions { show_cause: true, show_explanation: false, show_suggestions: true, color: false }
    }
}

const RESET: &str = "\x1b[0m";
const RED: &str = "\x1b[1;31m";
const YELLOW: &str = "\x1b[1;33m";
const BLUE: &str = "\x1b[1;34m";
const CYAN: &str = "\x1b[1;36m";
const BOLD: &str = "\x1b[1m";

fn paint(color: bool, code: &str, text: &str) -> String {
    if color {
        format!("{code}{text}{RESET}")
    } else {
        text.to_string()
    }
}

fn severity_color(sev: Severity) -> &'static str {
    match sev {
        Severity::Error => RED,
        Severity::Warning => YELLOW,
        _ => CYAN,
    }
}

/// Renders one diagnostic in the canonical UMAY layout:
///
/// ```text
/// error[E0101]: unknown name `prnt`
///  --> src/main.um:7:5
///   |
/// 7 |     prnt("hi")
///   |     ^^^^ not found in this scope
///   |
///   = cause: no function, variable or type named `prnt` is in scope
///   = help: did you mean `print`?
/// ```
pub fn render(diag: &Diagnostic, sources: &SourceMap, opts: RenderOptions) -> String {
    let mut out = String::new();
    let sev = diag.severity.as_str();
    out.push_str(&paint(opts.color, severity_color(diag.severity), &format!("{sev}[{}]", diag.code)));
    out.push_str(": ");
    out.push_str(&diag.message);
    out.push('\n');

    let mut body: Vec<(Span, String, bool)> = Vec::new();
    if let Some(span) = diag.span {
        body.push((span, diag.labels.first().map(|l| l.message.clone()).unwrap_or_default(), true));
        body.extend(diag.labels.iter().skip(1).map(|l| (l.span, l.message.clone(), false)));
    } else {
        body.extend(diag.labels.iter().map(|l| (l.span, l.message.clone(), false)));
    }

    if let Some(span) = diag.span {
        out.push_str(&render_span_block(span, &body, sources, opts));
    }

    let note = |out: &mut String, kind: &str, text: &str, color: &str| {
        out.push_str(&format!("  {} {}\n", paint(opts.color, color, &format!("= {kind}:")), text));
    };

    if opts.show_cause {
        if let Some(cause) = &diag.cause {
            note(&mut out, "cause", cause, BLUE);
        }
    }
    if opts.show_explanation {
        if let Some(expl) = &diag.explanation {
            note(&mut out, "why", expl, BLUE);
        }
    }
    if opts.show_suggestions {
        for s in &diag.suggestions {
            let detail = s.detail.as_deref().map(|d| format!(" ({d})")).unwrap_or_default();
            note(&mut out, "help", &format!("{}{detail}", s.description), CYAN);
            if let Some(file) = sources.get(s.span.file) {
                let lc = file.line_col(s.span.start);
                let old = file.span_text(s.span);
                out.push_str(&format!(
                    "       {}:{}:{}  {} -> {}\n",
                    file.name(),
                    lc.line,
                    lc.column,
                    paint(opts.color, RED, old),
                    paint(opts.color, BOLD, &s.replacement)
                ));
            }
        }
    }
    out
}

fn render_span_block(span: Span, body: &[(Span, String, bool)], sources: &SourceMap, opts: RenderOptions) -> String {
    let Some(file) = sources.get(span.file) else { return String::new() };
    let lc = file.line_col(span.start);
    let end_lc = file.line_col(span.end);
    let mut out = String::new();
    let gutter_width = lc.line.to_string().len().max(1);

    out.push_str(&format!(" {} {}\n", " ".repeat(gutter_width), paint(opts.color, BLUE, &format!("--> {}:{}:{}", file.name(), lc.line, lc.column))));

    // Determine which lines to show: primary + up to 3 surrounding context lines.
    let mut lines: Vec<u32> = Vec::new();
    for (s, _, _) in body {
        if s.file != span.file || *s == Span::DUMMY {
            continue;
        }
        let a = file.line_col(s.start).line;
        let b = file.line_col(s.end).line;
        for l in a..=b {
            if !lines.contains(&l) {
                lines.push(l);
            }
        }
    }
    if lines.is_empty() {
        lines.push(lc.line);
    }
    lines.sort_unstable();
    // Add context above and below.
    let first = lines[0].saturating_sub(1).max(1);
    let last = (*lines.last().unwrap() + 1).min(file.line_count());
    let show: Vec<u32> = (first..=last).collect();

    // Maps a span to (start_column, caret_width) on `line`, or None when the
    // span does not actually touch this line.
    let clip = |s: Span, line: u32| -> Option<(usize, usize)> {
        if s.file != span.file || s == Span::DUMMY {
            return None;
        }
        let start_lc = file.line_col(s.start);
        let end_lc = file.line_col(s.end);
        let line_start_byte = file.line_start(line) as usize;
        let line_text = file.line_text(line);
        let line_end_byte = line_start_byte + line_text.len();
        let a = (s.start as usize).max(line_start_byte);
        let b = (s.end as usize).min(line_end_byte);
        if b < a {
            return None;
        }
        // A zero-width span still needs one caret, but only on its start line.
        if a == b && start_lc.line != line {
            return None;
        }
        if a == b && end_lc.line != line {
            return None;
        }
        let col_start = file.src()[line_start_byte..a.max(line_start_byte)].chars().count();
        let col_end = if b <= a {
            col_start + 1
        } else {
            file.src()[line_start_byte..b].chars().count()
        };
        Some((col_start, col_end.saturating_sub(col_start).max(1)))
    };

    out.push_str(&format!(" {} {}\n", " ".repeat(gutter_width), paint(opts.color, BLUE, "|")));
    let mut prev_line = 0u32;
    for line in show {
        if prev_line != 0 && line != prev_line + 1 {
            out.push_str(&format!(" {} {}\n", " ".repeat(gutter_width), paint(opts.color, BLUE, "...")));
        }
        prev_line = line;
        let text = file.line_text(line);
        out.push_str(&format!(
            " {} {} {}\n",
            paint(opts.color, BLUE, &format!("{line:>gutter_width$}")),
            paint(opts.color, BLUE, "|"),
            text
        ));
        // Render at most one caret row: the primary label if it covers this
        // line, otherwise the first secondary label that does.
        let mut caret_row: Option<(usize, usize, String, bool)> = None;
        for (s, msg, primary) in body {
            if let Some((col, width)) = clip(*s, line) {
                if caret_row.is_none() || (*primary && !caret_row.as_ref().unwrap().3) {
                    caret_row = Some((col, width, msg.clone(), *primary));
                }
            }
        }
        if let Some((col, width, msg, primary)) = caret_row {
            let color = if primary { RED } else { CYAN };
            let mut row = format!("{}{}", " ".repeat(col), paint(opts.color, color, &"^".repeat(width)));
            if !msg.is_empty() {
                row.push(' ');
                row.push_str(&paint(opts.color, color, &msg));
            }
            out.push_str(&format!(" {} {} {}\n", " ".repeat(gutter_width), paint(opts.color, BLUE, "|"), row));
        }
    }
    let _ = end_lc;
    out
}

// ---------------------------------------------------------------------------
// JSON rendering
// ---------------------------------------------------------------------------

/// Escapes a string for JSON output. Only the mandatory escapes are handled;
/// UMAY output is UTF-8 and JSON allows raw UTF-8 in strings.
pub fn json_escape(input: &str) -> String {
    let mut out = String::with_capacity(input.len() + 2);
    for ch in input.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

fn json_str(input: &str) -> String {
    format!("\"{}\"", json_escape(input))
}

fn json_opt_str(input: Option<&str>) -> String {
    match input {
        Some(s) => json_str(s),
        None => "null".to_string(),
    }
}

/// Renders a diagnostic collection as the versioned JSON schema.
///
/// The output is a single JSON object; no external JSON dependency is used so
/// that the schema stays byte-for-byte stable and reviewable.
pub fn to_json(diags: &Diagnostics, sources: &SourceMap) -> String {
    let mut out = String::new();
    out.push_str("{\"version\":");
    out.push_str(&DIAGNOSTIC_SCHEMA_VERSION.to_string());
    out.push_str(",\"diagnostics\":[");
    let mut first = true;
    for d in diags.items() {
        if !first {
            out.push(',');
        }
        first = false;
        out.push('{');
        out.push_str(&format!("\"code\":{},", json_str(d.code)));
        out.push_str(&format!("\"severity\":{},", json_str(d.severity.as_str())));
        out.push_str(&format!("\"message\":{},", json_str(&d.message)));

        match d.span.and_then(|s| sources.get(s.file).map(|f| (s, f))) {
            Some((span, file)) => {
                let lc = file.line_col(span.start);
                let end = file.line_col(span.end);
                out.push_str(&format!("\"file\":{},", json_str(file.name())));
                out.push_str(&format!("\"line\":{},", lc.line));
                out.push_str(&format!("\"column\":{},", lc.column));
                out.push_str(&format!("\"end_line\":{},", end.line));
                out.push_str(&format!("\"end_column\":{},", end.column));
                out.push_str(&format!("\"span\":{{\"start\":{},\"end\":{}}},", span.start, span.end));
            }
            None => {
                out.push_str("\"file\":null,\"line\":null,\"column\":null,");
                out.push_str("\"end_line\":null,\"end_column\":null,\"span\":null,");
            }
        }

        out.push_str(&format!("\"cause\":{},", json_opt_str(d.cause.as_deref())));
        out.push_str(&format!("\"explanation\":{},", json_opt_str(d.explanation.as_deref())));

        out.push_str("\"labels\":[");
        for (i, label) in d.labels.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            match sources.get(label.span.file) {
                Some(file) => {
                    let lc: LineCol = file.line_col(label.span.start);
                    out.push_str(&format!(
                        "{{\"file\":{},\"line\":{},\"column\":{},\"message\":{}}}",
                        json_str(file.name()),
                        lc.line,
                        lc.column,
                        json_str(&label.message)
                    ));
                }
                None => out.push_str("null"),
            }
        }
        out.push(']');

        out.push_str(",\"suggestions\":[");
        for (i, s) in d.suggestions.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            let (file, span_obj) = match sources.get(s.span.file) {
                Some(f) => (json_str(f.name()), format!("{{\"start\":{},\"end\":{},\"file\":{}}}", s.span.start, s.span.end, json_str(f.name()))),
                None => ("null".to_string(), "null".to_string()),
            };
            out.push_str(&format!(
                "{{\"description\":{},\"replacement\":{},\"file\":{},\"span\":{},\"detail\":{}}}",
                json_str(&s.description),
                json_str(&s.replacement),
                file,
                span_obj,
                json_opt_str(s.detail.as_deref())
            ));
        }
        out.push(']');
        out.push('}');
    }
    out.push_str("]}");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> (Diagnostics, SourceMap) {
        let mut map = SourceMap::new();
        let id = map.add("src/main.um", "fn main() {\n    prnt(\"hi\")\n}\n");
        // "prnt" starts at byte 15 on line 2 (column 5 is the 'p').
        let span = Span::new(id, 16, 20);
        let mut diags = Diagnostics::new();
        diags.push(
            Diagnostic::error("E0101", "unknown name `prnt`")
                .at(span)
                .with_cause("no function named `prnt` is in scope")
                .with_suggestion(Suggestion::new("did you mean `print`?", span, "print")),
        );
        (diags, map)
    }

    #[test]
    fn human_render_contains_code_span_and_help() {
        let (diags, map) = sample();
        let text = render(&diags.items()[0], &map, RenderOptions::default());
        assert!(text.contains("error[E0101]: unknown name `prnt`"), "{text}");
        assert!(text.contains("--> src/main.um:2:5"), "{text}");
        assert!(text.contains("^^^^"), "{text}");
        assert!(text.contains("cause:"), "{text}");
        assert!(text.contains("print"), "{text}");
    }

    #[test]
    fn json_render_has_stable_shape() {
        let (diags, map) = sample();
        let json = to_json(&diags, &map);
        assert!(json.starts_with("{\"version\":1,\"diagnostics\":[{"), "{json}");
        assert!(json.contains("\"code\":\"E0101\""), "{json}");
        assert!(json.contains("\"line\":2,\"column\":5"), "{json}");
        assert!(json.contains("\"replacement\":\"print\""), "{json}");
        assert!(json.ends_with("]}"), "{json}");
    }

    #[test]
    fn json_escape_handles_control_chars() {
        assert_eq!(json_escape("a\"b\\c\nd"), "a\\\"b\\\\c\\nd");
    }

    #[test]
    fn summary_counts() {
        let mut d = Diagnostics::new();
        d.push(Diagnostic::error("E1", "e"));
        d.push(Diagnostic::warning("W1", "w"));
        assert_eq!(d.summary(), (1, 1));
        assert!(d.has_errors());
    }

    #[test]
    fn sorting_is_stable_by_position() {
        let mut map = SourceMap::new();
        let id = map.add("a.um", "0123456789");
        let mut d = Diagnostics::new();
        d.push(Diagnostic::error("E2", "late").at(Span::new(id, 5, 6)));
        d.push(Diagnostic::error("E1", "early").at(Span::new(id, 1, 2)));
        d.sort();
        assert_eq!(d.items()[0].message, "early");
    }
}
