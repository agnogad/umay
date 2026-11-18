//! Declarative macro expansion — token-level, before the parser runs.
//!
//! This is the Phase 8 engine described in `docs/adr/0022-macro-expansion.md`:
//! a dependency-free, `unsafe`-free transformer that recognises one-rule
//! `macro_rules!` definitions and rewrites `name!(...)` invocations by textual
//! token substitution. There is deliberately no `ast::Item` macro variant —
//! the parser never sees a macro, only the expanded token text.
//!
//! Ceilings (all intentional, all cheap to lift later):
//!   * one rule per macro; no repetition (`$(...)*`), no nested rules;
//!   * matchers are literal tokens, `$x:ident`, `$x:tt`, or bare `$x` (= tt);
//!   * a single expansion pass — a macro emitted by a macro is NOT expanded
//!     again (that is reported as `E0129`, not silently half-expanded);
//!   * call-site hygiene only — identifiers are pasted as text, so a macro may
//!     collide with caller names. Def-site spans / `$crate` are future work;
//!   * only `ident`/`tt` fragments are bound; a `tt` is exactly one token, not
//!     a balanced token tree;
//!   * integer/float/string/char literals are rendered from their payload, not
//!     their original spelling (`0xFF` becomes `255`). Floats keep their text;
//!   * token text is normalised to single spaces, with newlines preserved from
//!     the original `Token::line`, so statement termination still works.

use crate::diagnostics::{Diagnostic, Diagnostics};
use crate::source::{FileId, SourceMap, Span};
use crate::token::{Token, TokenKind};

/// The result of one expansion pass over a token stream.
pub(crate) struct Expansion {
    /// A new source string with every macro layer removed and every invocation
    /// replaced by its (substituted) expansion body.
    pub tokens_or_text: String,
    /// `(generated_span, invocation_span)` pairs. Generated spans point into a
    /// file the caller registers under `<expanded:<name>>`; invocation spans
    /// point into the original source and are what diagnostics should use.
    pub span_map: Vec<(Span, Span)>,
}

/// Cheap pre-check so files without macros take the untouched path.
pub(crate) fn contains_macros(tokens: &[Token]) -> bool {
    tokens.windows(2).any(|w| is_macro_rules(&w[0], &w[1]))
}

fn is_macro_rules(a: &Token, b: &Token) -> bool {
    matches!(&a.kind, TokenKind::Ident(n) if n == "macro_rules") && b.kind == TokenKind::Bang
}

#[derive(Clone)]
enum BindKind {
    Ident,
    Tt,
}

#[derive(Clone)]
enum Matcher {
    Lit(TokenKind),
    Bind { name: String, kind: BindKind },
}

struct MacroDef {
    name: String,
    matcher: Vec<Matcher>,
    expansion: Vec<Token>,
}

/// Minimal cursor; every read goes through `get`, so truncated input cannot
/// index past the end and a malformed macro reports an error instead of
/// panicking.
struct Cursor<'a> {
    tokens: &'a [Token],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn cur(&self) -> Option<&'a Token> {
        self.tokens.get(self.pos)
    }

    fn kind(&self) -> Option<&TokenKind> {
        self.cur().map(|t| &t.kind)
    }

    fn at_end(&self) -> bool {
        matches!(self.kind(), None | Some(TokenKind::Eof))
    }

    fn bump(&mut self) -> Option<&'a Token> {
        let t = self.tokens.get(self.pos)?;
        self.pos += 1;
        Some(t)
    }

    fn is(&self, k: &TokenKind) -> bool {
        matches!(self.kind(), Some(cur) if cur == k)
    }

    fn eat(&mut self, k: &TokenKind) -> bool {
        if self.is(k) {
            self.pos += 1;
            true
        } else {
            false
        }
    }
}

fn err(diags: &mut Diagnostics, code: &'static str, message: impl Into<String>, span: Span) {
    diags.push(Diagnostic::error(code, message).at(span));
}

fn opener_close(k: &TokenKind) -> Option<TokenKind> {
    match k {
        TokenKind::LParen => Some(TokenKind::RParen),
        TokenKind::LBracket => Some(TokenKind::RBracket),
        TokenKind::LBrace => Some(TokenKind::RBrace),
        _ => None,
    }
}

/// Parse one `macro_rules! name { (matcher) => { expansion } }` definition.
/// Returns the macro (if well-formed) and the token index just past it.
fn parse_definition(tokens: &[Token], start: usize, diags: &mut Diagnostics) -> (Option<MacroDef>, usize) {
    let head = tokens[start].clone();
    let mut c = Cursor { tokens, pos: start };
    c.bump(); // macro_rules
    c.bump(); // !

    let name = match c.bump() {
        Some(Token { kind: TokenKind::Ident(n), .. }) => n.clone(),
        _ => {
            err(diags, "E0125", "malformed macro definition: expected a macro name", head.span);
            return (None, c.pos.max(start + 1));
        }
    };

    if !c.eat(&TokenKind::LBrace) {
        err(diags, "E0125", "malformed macro definition: expected `{` after the macro name", head.span);
        return (None, c.pos);
    }
    if !c.eat(&TokenKind::LParen) {
        err(diags, "E0125", "malformed macro definition: expected `(` matcher", head.span);
        return (None, c.pos);
    }

    let mut matcher = Vec::new();
    loop {
        if c.at_end() {
            err(diags, "E0125", "malformed macro definition: unterminated matcher", head.span);
            return (None, c.pos);
        }
        if c.eat(&TokenKind::RParen) {
            break;
        }
        if c.is(&TokenKind::Dollar) {
            let dollar = c.cur().expect("not at end").clone();
            c.bump(); // $
            let bind_name = match c.bump() {
                Some(Token { kind: TokenKind::Ident(n), .. }) => n.clone(),
                _ => {
                    err(diags, "E0125", "malformed macro definition: `$` must be followed by a metavariable name", dollar.span);
                    return (None, c.pos);
                }
            };
            let kind = if c.eat(&TokenKind::Colon) {
                match c.bump() {
                    Some(Token { kind: TokenKind::Ident(f), .. }) if f == "ident" => BindKind::Ident,
                    Some(Token { kind: TokenKind::Ident(f), .. }) if f == "tt" => BindKind::Tt,
                    Some(Token { kind: TokenKind::Ident(f), .. }) => {
                        err(diags, "E0128", format!("unsupported macro fragment specifier `{f}` (only `ident` and `tt` are supported)"), dollar.span);
                        return (None, c.pos);
                    }
                    _ => {
                        err(diags, "E0125", "malformed macro definition: expected a fragment specifier after `:`", dollar.span);
                        return (None, c.pos);
                    }
                }
            } else {
                // A bare `$x` is treated as a `tt` capture; the ADR's examples
                // use this spelling.
                BindKind::Tt
            };
            matcher.push(Matcher::Bind { name: bind_name, kind });
        } else {
            let t = c.bump().expect("not at end").clone();
            matcher.push(Matcher::Lit(t.kind));
        }
    }

    if !c.eat(&TokenKind::FatArrow) {
        err(diags, "E0125", "malformed macro definition: expected `=>` after the matcher", head.span);
        return (None, c.pos);
    }
    if !c.eat(&TokenKind::LBrace) {
        err(diags, "E0125", "malformed macro definition: expected `{` after `=>`", head.span);
        return (None, c.pos);
    }

    let mut expansion = Vec::new();
    let mut depth = 1u32;
    loop {
        if c.at_end() {
            err(diags, "E0125", "malformed macro definition: unterminated expansion", head.span);
            return (None, c.pos);
        }
        let t = c.bump().expect("not at end").clone();
        match t.kind {
            TokenKind::LBrace => {
                depth += 1;
                expansion.push(t);
            }
            TokenKind::RBrace => {
                depth -= 1;
                if depth == 0 {
                    break;
                }
                expansion.push(t);
            }
            _ => expansion.push(t),
        }
    }

    c.eat(&TokenKind::Semi); // optional trailing rule separator

    // One rule per macro. If anything other than the closing `}` of the
    // `macro_rules!` body follows, report it and skip the extra rule(s).
    if !c.is(&TokenKind::RBrace) && !c.at_end() {
        err(diags, "E0128", "only one rule per macro is supported; extra rules are ignored", head.span);
        while !c.at_end() && !c.is(&TokenKind::RBrace) {
            c.bump();
        }
    }
    c.eat(&TokenKind::RBrace);

    (Some(MacroDef { name, matcher, expansion }), c.pos)
}

/// One token in the rendered output. `invocation` is `Some` for tokens that
/// came from a macro body (literal or substituted) and `None` for tokens that
/// were already in the source.
struct OutTok {
    text: String,
    line: u32,
    invocation: Option<Span>,
}

impl OutTok {
    fn plain(t: &Token) -> Self {
        OutTok { text: render_token(&t.kind), line: t.line, invocation: None }
    }

    fn generated(text: String, line: u32, invocation: Span) -> Self {
        OutTok { text, line, invocation: Some(invocation) }
    }
}

/// Expand every definition and invocation in `tokens`.
///
/// Generated spans use `FileId(map.len())`: the id the next file added to
/// `map` will receive. `expand_source` adds the expanded text under exactly
/// that id; the plain `parse_file` path does not register it (diagnostics in
/// generated text are a documented limitation — macro errors point at the
/// invocation instead).
pub(crate) fn expand(tokens: &[Token], map: &SourceMap, diags: &mut Diagnostics) -> Expansion {
    let generated_file = FileId(map.len() as u32);

    // Pass 1 — collect definitions and remember their token ranges so the
    // render pass can drop them.
    let mut defs: Vec<MacroDef> = Vec::new();
    let mut ranges: Vec<(usize, usize)> = Vec::new();
    let mut i = 0;
    while i + 1 < tokens.len() {
        if is_macro_rules(&tokens[i], &tokens[i + 1]) {
            let (def, end) = parse_definition(tokens, i, diags);
            if let Some(def) = def {
                defs.push(def);
            }
            ranges.push((i, end.max(i + 1)));
            i = end.max(i + 1);
        } else {
            i += 1;
        }
    }

    // Pass 2 — render. Invocations are replaced by their expansion; all other
    // tokens are copied verbatim (modulo whitespace normalisation).
    let mut out: Vec<OutTok> = Vec::new();
    let mut i = 0;
    while i < tokens.len() {
        if let Some((_, end)) = ranges.iter().find(|(start, end)| i >= *start && i < *end) {
            i = *end;
            continue;
        }
        let t = &tokens[i];
        if t.kind == TokenKind::Eof {
            break;
        }
        let invoked = matches!(&t.kind, TokenKind::Ident(n) if defs.iter().any(|d| &d.name == n));
        if invoked {
            match expand_invocation(tokens, i, &defs, diags) {
                Some((generated, next)) => {
                    out.extend(generated);
                    i = next;
                    continue;
                }
                None => {
                    i += 1;
                    continue;
                }
            }
        }
        out.push(OutTok::plain(t));
        i += 1;
    }

    let (tokens_or_text, span_map) = render(&out, generated_file);
    Expansion { tokens_or_text, span_map }
}

/// Expand one `name! ...` invocation beginning at `at`. `None` means recovery
/// already emitted a diagnostic and the caller should skip the offending token.
fn expand_invocation(
    tokens: &[Token],
    at: usize,
    defs: &[MacroDef],
    diags: &mut Diagnostics,
) -> Option<(Vec<OutTok>, usize)> {
    let head = &tokens[at];
    let name = match &head.kind {
        TokenKind::Ident(n) => n.clone(),
        _ => return None,
    };
    let def = defs.iter().find(|d| d.name == name)?;

    if tokens.get(at + 1).map(|t| &t.kind) != Some(&TokenKind::Bang) {
        // A plain identifier that happens to share a macro's name is still a
        // normal token; do not error on it.
        return None;
    }
    let open = match tokens.get(at + 2).map(|t| &t.kind) {
        Some(k) if opener_close(k).is_some() => k.clone(),
        _ => {
            err(diags, "E0126", format!("malformed macro invocation of `{name}`: expected `(`, `[` or `{{` after `!`"), head.span);
            return None;
        }
    };
    let close = opener_close(&open).expect("checked above");

    // The cursor starts *after* the opening delimiter at `at + 2`.
    let mut c = Cursor { tokens, pos: at + 3 };
    let mut args: Vec<Token> = Vec::new();
    let mut depth = 1u32;
    loop {
        if c.at_end() {
            let end = tokens.last().map(|t| t.span).unwrap_or(head.span);
            err(diags, "E0126", format!("malformed macro invocation of `{name}`: missing closing delimiter"), head.span.to(end));
            return None;
        }
        let t = c.bump().expect("not at end").clone();
        if t.kind == open {
            depth += 1;
            args.push(t);
        } else if t.kind == close {
            depth -= 1;
            if depth == 0 {
                break;
            }
            args.push(t);
        } else {
            args.push(t);
        }
    }

    let last = tokens.get(c.pos.saturating_sub(1)).map(|t| t.span).unwrap_or(head.span);
    let invocation_span = head.span.to(last);

    // Defensive: a macro emitted by this macro would need another pass. Report
    // it rather than emitting a token stream that still contains a macro call.
    let nested = def.expansion.windows(2).any(|w| {
        matches!(&w[0].kind, TokenKind::Ident(n) if defs.iter().any(|other| other.name == *n))
            && w[1].kind == TokenKind::Bang
    });
    if nested {
        err(diags, "E0129", format!("recursive macro expansion of `{name}` is not supported"), invocation_span);
    }

    let Some(binds) = match_args(def, &args, diags, invocation_span) else {
        return Some((Vec::new(), c.pos));
    };

    let mut generated = Vec::new();
    let body = &def.expansion;
    let mut j = 0;
    while j < body.len() {
        if body[j].kind == TokenKind::Dollar {
            let bind_name = match body.get(j + 1).map(|t| &t.kind) {
                Some(TokenKind::Ident(n)) => n.clone(),
                _ => {
                    err(diags, "E0128", format!("malformed macro expansion of `{name}`: `$` must name a capture"), invocation_span);
                    j += 1;
                    continue;
                }
            };
            match binds.iter().find(|(n, _)| *n == bind_name) {
                Some((_, tok)) => generated.push(OutTok::generated(render_token(&tok.kind), head.line, invocation_span)),
                None => err(diags, "E0128", format!("no capture named `{bind_name}` in macro `{name}`"), invocation_span),
            }
            j += 2;
        } else {
            generated.push(OutTok::generated(render_token(&body[j].kind), head.line, invocation_span));
            j += 1;
        }
    }

    Some((generated, c.pos))
}

/// Match invocation `args` against one rule's matcher. Bound tokens are kept
/// as a vector (not a hash map) so output stays deterministic and dependency
/// free.
fn match_args(def: &MacroDef, args: &[Token], diags: &mut Diagnostics, span: Span) -> Option<Vec<(String, Token)>> {
    let mut binds: Vec<(String, Token)> = Vec::new();
    let mut ai = 0usize;
    for m in &def.matcher {
        match m {
            Matcher::Lit(k) => {
                if args.get(ai).map(|t| &t.kind) != Some(k) {
                    err(diags, "E0127", format!("macro `{}`: argument {} does not match the rule", def.name, ai + 1), span);
                    return None;
                }
                ai += 1;
            }
            Matcher::Bind { name, kind, .. } => {
                let Some(t) = args.get(ai) else {
                    err(diags, "E0127", format!("macro `{}`: too few arguments for `${name}`", def.name), span);
                    return None;
                };
                if matches!(kind, BindKind::Ident) && !matches!(t.kind, TokenKind::Ident(_)) {
                    err(diags, "E0127", format!("macro `{}`: `${name}` must be an identifier", def.name), span);
                    return None;
                }
                binds.push((name.clone(), t.clone()));
                ai += 1;
            }
        }
    }
    if ai != args.len() {
        err(diags, "E0127", format!("macro `{}`: expected {} arguments, got {}", def.name, ai, args.len()), span);
        return None;
    }
    Some(binds)
}

/// Join rendered tokens with single spaces, restoring newlines from each
/// token's `line` (the parser uses newlines to terminate statements).
fn render(out: &[OutTok], generated_file: FileId) -> (String, Vec<(Span, Span)>) {
    let mut text = String::new();
    let mut span_map = Vec::new();
    let mut line = 1u32;
    for tok in out {
        if tok.line > line {
            for _ in 0..(tok.line - line) {
                text.push('\n');
            }
            line = tok.line;
        }
        if !text.is_empty() && !text.ends_with('\n') && !text.ends_with(' ') {
            text.push(' ');
        }
        let start = text.len();
        text.push_str(&tok.text);
        let end = text.len();
        if let Some(inv) = tok.invocation {
            span_map.push((Span::new(generated_file, start as u32, end as u32), inv));
        }
    }
    (text, span_map)
}

fn render_token(k: &TokenKind) -> String {
    match k {
        TokenKind::Int { value, suffix } => format!("{}{}", value, suffix.map(|s| s.as_str()).unwrap_or("")),
        TokenKind::Float { text, suffix, .. } => format!("{}{}", text, suffix.map(|s| s.as_str()).unwrap_or("")),
        TokenKind::Str(s) => render_string(s),
        TokenKind::Char(c) => format!("'{}'", escape_char(*c)),
        TokenKind::Ident(name) => name.clone(),
        TokenKind::Keyword(kw) => kw.as_str().to_string(),
        TokenKind::StrInterp(_) | TokenKind::Eof | TokenKind::Error => String::new(),
        other => other.symbol().to_string(),
    }
}

fn render_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn escape_char(c: char) -> String {
    match c {
        '\\' => "\\\\".to_string(),
        '\'' => "\\'".to_string(),
        '\n' => "\\n".to_string(),
        '\r' => "\\r".to_string(),
        '\t' => "\\t".to_string(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::SourceMap;

    fn lex(src: &str) -> (Vec<Token>, SourceMap) {
        let mut map = SourceMap::new();
        let id = map.add("<t>", src);
        let (tokens, _) = crate::lexer::tokenize(src, id);
        (tokens, map)
    }

    #[test]
    fn dollar_token_round_trips() {
        let (tokens, _) = lex("$x");
        assert!(matches!(tokens.first().map(|t| &t.kind), Some(TokenKind::Dollar)));
    }

    #[test]
    fn a_definition_is_removed_and_an_invocation_is_replaced() {
        let (tokens, map) = lex("macro_rules! answer { () => { 42 } }\nfn main() { return answer!() }");
        let mut diags = Diagnostics::new();
        let out = expand(&tokens, &map, &mut diags);
        assert!(diags.is_empty(), "{diags:?}");
        assert!(!out.tokens_or_text.contains("macro_rules"));
        assert!(out.tokens_or_text.contains("42"));
        assert!(!out.span_map.is_empty());
        assert!(out.span_map.iter().all(|(g, i)| g.file != i.file));
    }

    #[test]
    fn a_truncated_definition_does_not_panic() {
        let (tokens, map) = lex("macro_rules! broken { () => { 42");
        let mut diags = Diagnostics::new();
        let _ = expand(&tokens, &map, &mut diags);
        assert!(diags.items().iter().any(|d| d.code == "E0125"));
    }
}
