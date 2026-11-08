//! `umay fmt`: rewrites the whitespace between tokens, and nothing else.
//!
//! The formatter reads the token stream rather than the syntax tree. That limit
//! is what makes it safe: every token is copied exactly as it was written, so the
//! bytes of every literal, identifier and operator survive a format unchanged.
//! Only the gaps between tokens are rewritten, and a gap holding a comment is
//! re-indented but never moved, joined to code, or lost.
//!
//! It holds four rules:
//!
//! * indentation is four spaces per open brace,
//! * one space between neighbouring tokens, except inside brackets, before a
//!   comma or a closing bracket, and around `.` and `::`,
//! * no trailing whitespace, and at most one blank line between items,
//! * a line break the author wrote is kept where it was written.
//!
//! The last rule is the important one, and it is a deliberate refusal. Whether a
//! `{` begins a body or continues an expression cannot be told from tokens: in
//! `unsafe { … }` used as a statement and as an argument the tokens are the same
//! and the code is not. The same goes for `<`, which brackets a type's arguments
//! in `impl<T>` and compares two values in `a < b`. A pass that guessed at either
//! would reflow real programs wrongly, so this one does not guess: it keeps the
//! author's line breaks, and it keeps the author's spacing around `<`.
//!
//! What it guarantees instead is stronger, and it is what the tests hold it to:
//! the same tokens, in the same order, with the same text, and the same bytes for
//! every file that is already formatted. That is a fixed point on the whole
//! standard library (`compiler/tests/phase7_fmt.rs`).

use crate::lexer;
use crate::source::FileId;
use crate::token::{Keyword, Token, TokenKind};

/// What a format pass produced.
pub struct Formatted {
    pub text: String,
    /// Set when the source could not be lexed, so nothing was rewritten.
    pub errors: usize,
}

/// Formats `src`, leaving the token texts and every comment exactly as written.
pub fn format(src: &str) -> Formatted {
    let (tokens, lex_errors) = lexer::tokenize(src, FileId::DUMMY);
    if !lex_errors.is_empty() {
        // A half-formed token stream has no well-defined gaps, and guessing at
        // where they go would mean writing a file the author cannot get back.
        return Formatted {
            text: src.to_string(),
            errors: lex_errors.len(),
        };
    }
    let tokens: Vec<Token> = tokens
        .into_iter()
        .filter(|t| t.kind != TokenKind::Error && t.kind != TokenKind::Eof)
        .collect();
    Formatted {
        text: rewrite(src, &tokens),
        errors: 0,
    }
}

/// The lines being built.
struct Writer {
    out: String,
    /// The current line's text, without indentation.
    line: String,
    /// Indentation depth, in four-space units.
    depth: usize,
    /// True once any line has been written, so a blank line at the start of a
    /// file is not invented.
    started: bool,
    /// True when the `{` just written waits to see whether its block is empty.
    pending_open_brace: bool,
    /// How many argument lists are open across lines.
    open_parens: usize,
    /// How many array literals are open across lines.
    open_brackets: usize,
    /// Extra indentation for a line that continues the one above it.
    continuation: usize,
}

impl Writer {
    fn new() -> Self {
        Writer {
            out: String::new(),
            line: String::new(),
            depth: 0,
            started: false,
            pending_open_brace: false,
            open_parens: 0,
            open_brackets: 0,
            continuation: 0,
        }
    }

    /// Adds `text` to the current line.
    fn push(&mut self, text: &str) {
        self.line.push_str(text);
    }

    /// Adds one space unless the line is empty or already ends in one.
    fn space(&mut self) {
        if !self.line.is_empty() && !self.line.ends_with(' ') {
            self.line.push(' ');
        }
    }

    fn indent(&mut self) {
        self.depth += 1;
    }

    fn outdent(&mut self) {
        self.depth = self.depth.saturating_sub(1);
    }

    /// Ends the current line.
    fn newline(&mut self) {
        self.flush();
    }

    /// Keeps the blank line the author wrote.
    ///
    /// A blank line is a decision about grouping, and one the formatter has no
    /// basis to make. So a run of them collapses to one, and a single one is
    /// left exactly where it was.
    fn blank_lines(&mut self, n: usize) {
        self.flush();
        if !self.started || n == 0 {
            return;
        }
        if !self.out.ends_with("\n\n") {
            self.out.push('\n');
        }
    }

    /// Writes a comment gap, line for line, re-indented as a block.
    fn comment(&mut self, gap: &str) {
        // The break that ended the line above has already been written, so it is
        // not part of the comment's own text.
        let text = gap.trim_start_matches(|c: char| c == '\n' || c == ' ' || c == '\t');
        if text.trim().is_empty() {
            return;
        }
        let pieces: Vec<&str> = text.lines().collect();
        for (i, piece) in pieces.iter().enumerate() {
            if piece.trim().is_empty() {
                // A blank line between two runs is the author's separator, so it
                // stays. One at the very end belongs to what follows instead.
                if i + 1 < pieces.len() && !self.out.ends_with("\n\n") {
                    self.out.push('\n');
                }
                continue;
            }
            self.line.push_str(piece.trim());
            self.flush();
        }
        // A run that ends with a blank line is followed by one in the output:
        // the author put the space there to separate the comment from what comes
        // next. Only an empty line counts: the last piece is normally just the
        // indentation of the line that follows.
        if text.contains("\n\n") && pieces.last().is_some_and(|p| p.is_empty()) {
            if !self.out.ends_with("\n\n") {
                self.out.push('\n');
            }
        }
    }

    /// True when the line was already ended and nothing has been written since,
    /// so a comment can start at once.
    fn awaiting_comment_line(&self) -> bool {
        self.line.is_empty() && self.out.ends_with('\n')
    }

    /// Writes the pending line, if it holds anything but indentation.
    fn flush(&mut self) {
        let text = self.line.trim_end();
        if text.is_empty() {
            self.line.clear();
            return;
        }
        let text = text.to_string();
        self.line.clear();
        for _ in 0..self.depth {
            self.out.push_str("    ");
        }
        for _ in 0..self.continuation {
            self.out.push(' ');
        }
        self.continuation = 0;
        self.out.push_str(&text);
        self.out.push('\n');
        self.started = true;
    }

    fn finish(mut self) -> String {
        self.flush();
        // Exactly one newline at the end, whatever the input ended with.
        let mut text = self.out;
        while text.ends_with('\n') {
            text.pop();
        }
        if !text.is_empty() {
            text.push('\n');
        }
        text
    }
}

/// How much room goes before a token.
#[derive(PartialEq, Clone, Copy)]
enum Placement {
    /// Nothing: no space at all.
    Tight,
    /// One space.
    Spaced,
    /// Whatever the author wrote, which is what an undecidable case gets.
    SpaceAsWritten,
}

/// What a gap's spacing needs to know from the tokens before it.
#[derive(Default)]
struct Context {
    /// How many type argument lists `<` has opened and `>` has not closed.
    in_angles: usize,
    /// True when the last token was a `*` or `&` used as a sigil, so what follows
    /// it is its pointee or its place and reads tight.
    sigil_is_prefix: bool,
    /// True when the last token was a `{` that continues an expression.
    brace_is_expression: bool,
    /// True when a type name could start at the token being read.
    name_is_type: bool,
    /// True when the token just read was a name in a type's head, so a `<` after
    /// it opens that type's arguments.
    last_name_was_type: bool,
}

/// Decides the gap between `prev` and `next` when they share a line.
fn placement(prev: &Token, next: &Token, cx: &Context) -> Placement {
    use TokenKind as K;
    // Nothing goes before a closing bracket, a comma, a semicolon or a `:`.
    if matches!(
        next.kind,
        K::RParen | K::RBracket | K::Comma | K::Semi | K::Colon
    ) {
        return Placement::Tight;
    }
    // Nothing goes after an opening bracket, a `.`, or a `::` on either side.
    if matches!(
        prev.kind,
        K::LParen | K::LBracket | K::Dot | K::DotDot | K::DotDotEq
    ) {
        return Placement::Tight;
    }
    if matches!(next.kind, K::Dot | K::DotDot | K::DotDotEq | K::ColonColon) {
        return Placement::Tight;
    }
    if matches!(prev.kind, K::ColonColon) {
        return Placement::Tight;
    }
    // A `<` is left alone. Whether it brackets type arguments or compares two
    // values cannot be told from the token stream, and the two want opposite
    // spacing, so this pass makes no claim about either. A `>` is not ambiguous
    // in the same way and is spaced like any other token.
    if matches!(next.kind, K::Lt) || matches!(prev.kind, K::Lt) {
        return Placement::SpaceAsWritten;
    }
    // `>` is tight when it closes a type argument list, which the author wrote
    // tight. A comparison's `>` was written with a space, and that is kept.
    if matches!(next.kind, K::Gt) && matches!(prev.kind, K::Gt | K::Comma) {
        return Placement::Tight;
    }
    // `Iterator<i64>>` closes two lists at once, and the inner `>` is the one
    // before the name's own `>` in `i64`.
    // `Iterator<i64>>` lexes its closing pair as one `>>`, which closes two
    // lists at once and reads tight.
    if matches!(next.kind, K::Shr) && cx.in_angles > 0 {
        return Placement::Tight;
    }
    if matches!(next.kind, K::Gt) && matches!(prev.kind, K::Ident(_)) && cx.in_angles > 0 {
        return Placement::Tight;
    }
    if matches!(next.kind, K::Gt) {
        return Placement::SpaceAsWritten;
    }
    // `->()` reads after one space on each side.
    if matches!(prev.kind, K::Arrow) {
        return Placement::Spaced;
    }
    // A `{` that continues an expression keeps the space before it, and the
    // value inside it reads after a space of its own.
    if matches!(next.kind, K::LBrace) && matches!(prev.kind, K::Keyword(_)) {
        return Placement::Spaced;
    }
    if matches!(prev.kind, K::LBrace) && cx.brace_is_expression {
        return Placement::Spaced;
    }
    // A `(` after a name is a call and reads tight. After `if`, `while`, `match`
    // and `return` it opens a new phrase and reads with a space.
    // A `(` after an operator groups a value: `hash ^ (byte as usize)`.
    if matches!(next.kind, K::LParen) && prev_is_operator(prev) && !cx.sigil_is_prefix {
        return Placement::Spaced;
    }
    // `f::<T>(x)` closes a type's arguments and calls at once, so it is tight.
    if matches!(next.kind, K::LParen) && matches!(prev.kind, K::Gt | K::Shr) {
        return Placement::Tight;
    }
    if matches!(next.kind, K::LParen) {
        return match prev.kind {
            K::Ident(_) | K::RParen | K::RBracket | K::Gt | K::Shr => Placement::Tight,
            K::Keyword(kw) if needs_space_before_paren(kw) => Placement::Spaced,
            // `f(a, (b))`: the `(` groups, so it reads after a space.
            // A sigil before a `(` dereferences or borrows what it groups, so
            // it reads tight: `*(cell as *const R)`.
            K::Star | K::Amp | K::AmpAmp if cx.sigil_is_prefix => Placement::Tight,
            K::Comma | K::Plus | K::Minus | K::Slash | K::Percent | K::Eq => Placement::Spaced,
            _ => Placement::Tight,
        };
    }
    // A pointer or borrow sigil binds to what follows it: `*mut u8`, `&x`.
    // `a * b` is a multiply, and the two are told apart by the token before the
    // sigil, which for a type cannot end a value.
    if matches!(prev.kind, K::Star | K::Amp | K::AmpAmp) && cx.sigil_is_prefix {
        return Placement::Tight;
    }
    // `!` is always prefix: `!self.open`.
    if matches!(prev.kind, K::Bang) {
        return Placement::Tight;
    }
    // A sign binds to its number: `-1`, `fd: -1`.
    if matches!(prev.kind, K::Minus | K::Plus) && cx.sigil_is_prefix {
        return Placement::Tight;
    }

    // A `[` after a name indexes it; after anything else it starts an array.
    if matches!(next.kind, K::LBracket) {
        return match prev.kind {
            K::Ident(_) | K::RParen | K::RBracket => Placement::Tight,
            _ => Placement::Spaced,
        };
    }
    Placement::Spaced
}

/// True for the tokens that combine two values, so a `(` after one groups.
fn prev_is_operator(tok: &Token) -> bool {
    use TokenKind as K;
    matches!(
        tok.kind,
        K::Plus
            | K::Minus
            | K::Star
            | K::Slash
            | K::Percent
            | K::Caret
            | K::Amp
            | K::Pipe
            | K::Bang
            | K::Eq
            | K::EqEq
            | K::Ne
            | K::AmpAmp
            | K::PipePipe
    )
}

/// True for the keywords whose `(` opens a new phrase rather than a call.
fn needs_space_before_paren(kw: Keyword) -> bool {
    matches!(
        kw,
        Keyword::If | Keyword::While | Keyword::Match | Keyword::Return
    )
}

/// Records what the next token needs to know from this one.
fn update_context(cx: &mut Context, tok: &Token, previous: Option<TokenKind>) {
    use TokenKind as K;

    // Angle brackets nest, so they are counted rather than flagged: in
    // `Iterator<i64>>` the first `>` closes the inner list and the second the
    // outer one.
    //
    // A `<` opens a list when what precedes it is a type name. What makes a name
    // a type name is where it sits: after `fn`, `struct`, `impl`, a bound's `:`,
    // a `::`, a `,` or another `<`. It never sits after a value, which is what
    // keeps `a < b` a comparison.
    // A name is a type name when the token before it put the reader in a type's
    // head. That fact has to survive the name itself, so it is carried in `cx`
    // and read here for the `<` that may follow.
    cx.last_name_was_type = matches!(previous, Some(K::Ident(_))) && cx.name_is_type;
    let name_is_type = cx.in_angles > 0
        || cx.last_name_was_type
        || matches!(
            previous,
            Some(K::Keyword(Keyword::Fn))
                | Some(K::Keyword(Keyword::Struct))
                | Some(K::Keyword(Keyword::Enum))
                | Some(K::Keyword(Keyword::Trait))
                | Some(K::Keyword(Keyword::Impl))
                | Some(K::Keyword(Keyword::Type))
                | Some(K::Keyword(Keyword::Use))
                | Some(K::Colon)
                | Some(K::ColonColon)
                | Some(K::Comma)
                | Some(K::Lt)
                | Some(K::Gt)
                | Some(K::Arrow)
        );
    cx.in_angles = match tok.kind {
        K::Lt if name_is_type => cx.in_angles + 1,
        K::Gt => cx.in_angles.saturating_sub(1),
        K::Shr => cx.in_angles.saturating_sub(2),
        _ => cx.in_angles,
    };

    cx.name_is_type = name_is_type;

    // A sigil is prefix when nothing that could end a value precedes it. That is
    // what tells `*mut u8` and `a * b` apart without knowing the grammar. The
    // same test tells a sign from a subtraction, which is why `-1` stays whole.
    cx.sigil_is_prefix = matches!(tok.kind, K::Star | K::Amp | K::AmpAmp | K::Minus | K::Plus)
        && matches!(
            previous,
            Some(K::Colon)
                | Some(K::Comma)
                | Some(K::Eq)
                | Some(K::LParen)
                | Some(K::LBracket)
                | Some(K::LBrace)
                | Some(K::Star)
                | Some(K::Amp)
                | Some(K::AmpAmp)
                | Some(K::Arrow)
                | Some(K::Keyword(Keyword::As))
                | Some(K::Keyword(Keyword::For))
                | Some(K::Keyword(Keyword::Mut))
                | Some(K::Keyword(Keyword::Const))
                | Some(K::Keyword(Keyword::Return))
                | None
        );

    // A `{` that continues an expression, rather than opening a body.
    cx.brace_is_expression = matches!(tok.kind, K::LBrace)
        && matches!(
            previous,
            Some(K::Keyword(Keyword::Unsafe)) | Some(K::Eq) | Some(K::Comma)
        );
}

/// True when this token starts a line that carries on the one above it.
///
/// An operator at the start of a line is the clearest sign: a reader writes
/// `|| x` on its own line to say the condition continues, and that extra indent
/// is what shows the two lines are one statement.
fn continues_line(tok: &Token) -> bool {
    use TokenKind as K;
    matches!(
        tok.kind,
        K::PipePipe
            | K::AmpAmp
            | K::Plus
            | K::Minus
            | K::Star
            | K::Slash
            | K::Eq
            | K::EqEq
            | K::Ne
            | K::Lt
            | K::Le
            | K::Gt
            | K::Ge
            | K::Dot
    )
}

/// How far a continuation line was indented beyond the block it sits in.
fn continuation_width(gap: &str) -> usize {
    let after_break = match gap.rfind('\n') {
        Some(at) => &gap[at + 1..],
        None => return 0,
    };
    after_break
        .len()
        .saturating_sub(after_break.trim_start().len())
}

/// True when the author put a line break right after the token at `i`.
fn breaks_after(tokens: &[Token], i: usize, src: &str) -> bool {
    match tokens.get(i + 1) {
        None => false,
        Some(next) => {
            let gap = &src[tokens[i].span.end as usize..next.span.start as usize];
            gap.contains('\n')
        }
    }
}

/// True when the `{` at `i` ends its line, so what follows is a body.
///
/// A `{` with anything after it on the same line is part of an expression, and
/// this pass leaves the line's shape alone rather than guessing.
fn body_brace(tokens: &[Token], i: usize, src: &str) -> bool {
    match tokens.get(i + 1) {
        None => false,
        Some(next) => {
            let gap = &src[tokens[i].span.end as usize..next.span.start as usize];
            gap.contains('\n')
        }
    }
}

/// Rewrites the gaps between `tokens`.
///
/// Only two things are decided here, and both are decided by a token and its
/// neighbour: the space between them, and the indentation of a line. Nothing is
/// moved across a line break, because a break the author wrote carries meaning
/// this pass cannot recover.
fn rewrite(src: &str, tokens: &[Token]) -> String {
    let mut w = Writer::new();
    let mut last_end = 0u32;
    let mut prev: Option<&Token> = None;
    let mut cx = Context::default();
    for (index, tok) in tokens.iter().enumerate() {
        let gap = &src[last_end as usize..tok.span.start as usize];
        let has_comment = gap.contains("//") || gap.contains("/*");
        let broke = gap.contains('\n');

        if has_comment {
            // Whatever line was being built ends here: the comment starts its
            // own line, and the break the author wrote is that break.
            w.newline();
            // Only the breaks before the comment are blank lines; the ones after
            // it belong to the line that follows, and the first of them is the
            // one that ends the line the comment sits on.
            // A blank line between the comment and what came before is the
            // author's separator, so it is kept. The first break is the one that
            // ended the line above and has already been written.
            let before = &gap[..gap
                .find("//")
                .or_else(|| gap.find("/*"))
                .unwrap_or(gap.len())];
            let leading = before.len() - before.trim_start_matches('\n').len();
            w.blank_lines(count_newlines(&before[..leading]).saturating_sub(1));
            w.comment(gap);
        } else if broke {
            // The author broke the line, so the break stays. Its indentation is
            // the block's, except that a line which continues the one above it
            // keeps whatever extra indent the author gave it: that extra indent
            // is what says the two lines are one statement.
            w.newline();
            w.blank_lines(count_newlines(gap).saturating_sub(1));
            // Only a line that carries on the previous one keeps the extra
            // indent. A line after a `,` or a `{` starts something new and takes
            // the block's own indentation.
            // A line that carries on the one above it is indented one level
            // past the block, which is what shows the two are one statement.
            w.continuation = if continues_line(tok) { 4 } else { 0 };
        } else if let Some(p) = prev {
            // `{}` is one token pair, so nothing goes between them.
            let empty_block = w.pending_open_brace && matches!(tok.kind, TokenKind::RBrace);
            if !empty_block {
                match placement(p, tok, &cx) {
                    Placement::Tight => {}
                    Placement::Spaced => w.space(),
                    // Reproduce the author's own spacing: this gap is one the
                    // formatter has no basis to decide.
                    Placement::SpaceAsWritten => {
                        if gap.contains(char::is_whitespace) {
                            w.space();
                        }
                    }
                }
            }
        }

        // A closing brace is one level shallower than the body it ends, so it is
        // dedented before it is written. An empty block never raised the depth,
        // so its `}` does not lower it either.
        if matches!(tok.kind, TokenKind::RBrace) && !w.pending_open_brace {
            // Every `{` raises the depth and every `}` lowers it, whether the
            // block is a body or part of an expression, so that a line inside
            // either one is indented by the right amount.
            w.outdent();
        }
        // `{}` is written closed: an empty block has no body to put on its own
        // line, and splitting it says less with more.
        if matches!(tok.kind, TokenKind::RBrace) && w.pending_open_brace {
            w.push(token_text(src, tok));
            update_context(&mut cx, tok, prev.map(|p| p.kind.clone()));
            last_end = tok.span.end;
            prev = Some(tok);
            continue;
        }
        w.push(token_text(src, tok));
        w.pending_open_brace = false;
        // A `(` that the author broke after opens an argument list on its own
        // line, so the entries are indented one level until the `)`.
        // An array whose entries are on their own lines indents them, the way an
        // argument list does.
        if matches!(tok.kind, TokenKind::LBracket) && breaks_after(tokens, index, src) {
            w.newline();
            w.indent();
            w.open_brackets += 1;
        }
        if matches!(tok.kind, TokenKind::RBracket) && w.open_brackets > 0 {
            w.open_brackets -= 1;
            w.outdent();
        }
        if matches!(tok.kind, TokenKind::LParen) && breaks_after(tokens, index, src) {
            // The line holding the `(` ends there, so the depth rises only after
            // it has been written.
            w.newline();
            w.indent();
            w.open_parens += 1;
        }
        if matches!(tok.kind, TokenKind::RParen) && w.open_parens > 0 {
            w.open_parens -= 1;
            w.outdent();
        }
        // A `{` that opens a body ends its line and indents what follows. A `{`
        // that continues an expression (`Vec { … }` as a value, `if c { … }` on
        // one line) does not, and the two cannot be told apart from tokens.
        // Only a `{` at the end of its own line is treated as a body, which is
        // what the author's own line break already said.
        if matches!(tok.kind, TokenKind::LBrace) {
            // The line holding the `{` ends there, and the body starts one level
            // in, so the depth is raised only after that line is written. When
            // the next token is `}` the block is empty and stays closed, so that
            // is decided here rather than by the `}` having to undo this.
            if matches!(
                tokens.get(index + 1).map(|t| &t.kind),
                Some(TokenKind::RBrace)
            ) {
                // Nothing to indent and nothing to break: the `}` continues this
                // line, so the depth is left alone.
                w.pending_open_brace = true;
            } else if body_brace(tokens, index, src) {
                // A body starts on its own line, one level in.
                w.newline();
                w.indent();
            } else {
                // A `{` inside an expression keeps the line it is on, and the
                // depth still rises so that anything the author broke inside it
                // lines up.
                w.indent();
            }
        }

        update_context(&mut cx, tok, prev.map(|p| p.kind.clone()));
        last_end = tok.span.end;
        prev = Some(tok);
    }
    w.finish()
}

/// How many line breaks a gap holds.
fn count_newlines(gap: &str) -> usize {
    gap.bytes().filter(|b| *b == b'\n').count()
}

/// The exact text a token was written with, taken from the source itself.
///
/// Slicing the span rather than rebuilding the token from its parsed value is
/// what makes the pass lossless: a literal's spelling, a string's escapes and an
/// identifier's case all come out as they went in, including forms the parse
/// would normalise (a float's trailing zero, `1_000`'s separators).
fn token_text<'a>(src: &'a str, tok: &Token) -> &'a str {
    &src[tok.span.start as usize..tok.span.end as usize]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fmt(src: &str) -> String {
        let out = format(src);
        assert_eq!(out.errors, 0, "source should lex");
        out.text
    }

    #[test]
    fn a_formatted_file_is_unchanged() {
        let src = "fn main() -> i32 {\n    return 0i32\n}\n";
        assert_eq!(fmt(src), src);
    }

    #[test]
    fn indentation_is_four_spaces_per_brace() {
        let src = "fn main() -> i32 {\nreturn 0i32\n}\n";
        assert_eq!(fmt(src), "fn main() -> i32 {\n    return 0i32\n}\n");
    }

    #[test]
    fn trailing_whitespace_goes() {
        let src = "fn main() -> i32 {   \n    return 0i32\t\n}\n";
        assert_eq!(fmt(src), "fn main() -> i32 {\n    return 0i32\n}\n");
    }

    #[test]
    fn a_call_has_no_space_and_a_comma_does() {
        let src = "fn f() -> i32 {\n    return add(1i32,2i32)\n}\n";
        assert_eq!(fmt(src), "fn f() -> i32 {\n    return add(1i32, 2i32)\n}\n");
    }

    #[test]
    fn a_comparison_keeps_the_spacing_it_was_written_with() {
        // Whether `<` brackets type arguments or compares two values cannot be
        // told from the token stream, so this pass makes no claim about it and
        // leaves the author's spacing alone. Everything else on the line is
        // still normalised.
        let src = "fn f() -> bool {\n    return a<b\n}\n";
        assert_eq!(fmt(src), src);
        let spaced = "impl<T> Option<T> {}\n";
        assert_eq!(fmt(spaced), spaced);
    }

    #[test]
    fn a_comment_keeps_its_place() {
        let src = "fn f() -> i32 {\n    // why\n    return 1i32\n}\n";
        assert_eq!(fmt(src), src);
    }

    #[test]
    fn formatting_is_idempotent() {
        let src = "struct P {\nx: i32,\ny: i32,\n}\n\nfn f() -> i32 {\nlet p = P { x: 1i32, y: 2i32 }\nreturn p.x\n}\n";
        let once = fmt(src);
        let twice = fmt(&once);
        assert_eq!(once, twice);
    }
}
