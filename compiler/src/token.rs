//! Token definitions for UMAY.

use crate::source::Span;

/// A lexed token: kind + source span + optional payload.
#[derive(Clone, Debug, PartialEq)]
pub struct Token {
    pub kind: TokenKind,
    pub span: Span,
    /// 1-based source line the token starts on. Used by the parser for
    /// newline-aware statement termination.
    pub line: u32,
}

impl Token {
    pub fn new(kind: TokenKind, span: Span) -> Self {
        Token { kind, span, line: 1 }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum TokenKind {
    // ---- literals ----
    /// Integer literal. Value is stored as `u128`; whether it fits the target
    /// type is decided during type checking, not lexing.
    Int { value: u128, suffix: Option<IntSuffix> },
    /// Float literal, kept as text to guarantee exact round-tripping.
    Float { value: f64, suffix: Option<FloatSuffix>, text: String },
    /// String literal with escapes already resolved, or a raw string.
    Str(String),
    /// Character literal.
    Char(char),
    /// Interpolated string: alternating literal parts and expression sources.
    /// Represented at parse time from a plain `Str`; the lexer only produces
    /// `Str`. Kept here so the parser can build it.
    StrInterp(Vec<StrPart>),

    // ---- identifiers & keywords ----
    Ident(String),
    Keyword(Keyword),

    // ---- punctuation ----
    LParen,
    RParen,
    LBrace,
    RBrace,
    LBracket,
    RBracket,
    Comma,
    Semi,
    Colon,
    ColonColon,
    Dot,
    DotDot,
    DotDotEq,
    Arrow,        // ->
    FatArrow,     // =>
    Pipe,         // |
    PipePipe,     // ||
    Amp,          // &
    AmpAmp,       // &&
    Star,         // *
    Plus,         // +
    Minus,        // -
    Slash,        // /
    Percent,      // %
    Caret,        // ^
    Bang,         // !
    Question,     // ?
    Eq,           // =
    EqEq,         // ==
    Ne,           // !=
    Lt,           // <
    Le,           // <=
    Gt,           // >
    Ge,           // >=
    PlusEq,       // +=
    MinusEq,      // -=
    StarEq,       // *=
    SlashEq,      // /=
    PercentEq,    // %=
    AmpEq,        // &=
    PipeEq,       // |=
    CaretEq,      // ^=
    Shl,          // <<
    Shr,          // >>
    ShlEq,        // <<=
    ShrEq,        // >>=
    At,           // @
    Hash,         // #
    Underscore,   // _  (also produced as Ident("_"); see lexer)

    // ---- trivia / special ----
    Eof,
    /// Lexical error; the lexer records a diagnostic alongside it.
    Error,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IntSuffix {
    I8,
    I16,
    I32,
    I64,
    I128,
    U8,
    U16,
    U32,
    U64,
    U128,
    Usize,
    Isize,
}

impl IntSuffix {
    pub fn as_str(self) -> &'static str {
        match self {
            IntSuffix::I8 => "i8",
            IntSuffix::I16 => "i16",
            IntSuffix::I32 => "i32",
            IntSuffix::I64 => "i64",
            IntSuffix::I128 => "i128",
            IntSuffix::U8 => "u8",
            IntSuffix::U16 => "u16",
            IntSuffix::U32 => "u32",
            IntSuffix::U64 => "u64",
            IntSuffix::U128 => "u128",
            IntSuffix::Usize => "usize",
            IntSuffix::Isize => "isize",
        }
    }

    pub fn parse(s: &str) -> Option<IntSuffix> {
        Some(match s {
            "i8" => IntSuffix::I8,
            "i16" => IntSuffix::I16,
            "i32" => IntSuffix::I32,
            "i64" => IntSuffix::I64,
            "i128" => IntSuffix::I128,
            "u8" => IntSuffix::U8,
            "u16" => IntSuffix::U16,
            "u32" => IntSuffix::U32,
            "u64" => IntSuffix::U64,
            "u128" => IntSuffix::U128,
            "usize" => IntSuffix::Usize,
            "isize" => IntSuffix::Isize,
            _ => return None,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FloatSuffix {
    F32,
    F64,
}

impl FloatSuffix {
    pub fn as_str(self) -> &'static str {
        match self {
            FloatSuffix::F32 => "f32",
            FloatSuffix::F64 => "f64",
        }
    }
}

/// One piece of an interpolated string.
#[derive(Clone, Debug, PartialEq)]
pub enum StrPart {
    /// Verbatim text (escapes resolved).
    Text(String),
    /// `{expr}` source text plus the span of the inside of the braces.
    Expr { src: String, span: Span },
    /// `{{` escape, resolved to a literal `{`.
    EscapedBrace(char),
}

/// Every reserved word in UMAY. `true`/`false` are literals, not keywords.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Keyword {
    Fn,
    Let,
    Mut,
    Struct,
    Enum,
    Trait,
    Impl,
    For,
    Type,
    Const,
    Static,
    Use,
    Mod,
    Pub,
    SelfValue, // self
    SelfType,  // Self
    Return,
    If,
    Else,
    Match,
    While,
    Loop,
    In,
    Break,
    Continue,
    Unsafe,
    As,
    Where,
    True,
    False,
    Defer,
    Move,
    Ref,
    Dyn,
    Extern,
    Sizeof,
}

impl Keyword {
    pub fn as_str(self) -> &'static str {
        match self {
            Keyword::Fn => "fn",
            Keyword::Let => "let",
            Keyword::Mut => "mut",
            Keyword::Struct => "struct",
            Keyword::Enum => "enum",
            Keyword::Trait => "trait",
            Keyword::Impl => "impl",
            Keyword::For => "for",
            Keyword::Type => "type",
            Keyword::Const => "const",
            Keyword::Static => "static",
            Keyword::Use => "use",
            Keyword::Mod => "mod",
            Keyword::Pub => "pub",
            Keyword::SelfValue => "self",
            Keyword::SelfType => "Self",
            Keyword::Return => "return",
            Keyword::If => "if",
            Keyword::Else => "else",
            Keyword::Match => "match",
            Keyword::While => "while",
            Keyword::Loop => "loop",
            Keyword::In => "in",
            Keyword::Break => "break",
            Keyword::Continue => "continue",
            Keyword::Unsafe => "unsafe",
            Keyword::As => "as",
            Keyword::Where => "where",
            Keyword::True => "true",
            Keyword::False => "false",
            Keyword::Defer => "defer",
            Keyword::Move => "move",
            Keyword::Ref => "ref",
            Keyword::Dyn => "dyn",
            Keyword::Extern => "extern",
            Keyword::Sizeof => "sizeof",
        }
    }

    // The parser only needs the lookup; implementing `FromStr` would change the
    // error type for no caller.
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Option<Keyword> {
        Some(match s {
            "fn" => Keyword::Fn,
            "let" => Keyword::Let,
            "mut" => Keyword::Mut,
            "struct" => Keyword::Struct,
            "enum" => Keyword::Enum,
            "trait" => Keyword::Trait,
            "impl" => Keyword::Impl,
            "for" => Keyword::For,
            "type" => Keyword::Type,
            "const" => Keyword::Const,
            "static" => Keyword::Static,
            "use" => Keyword::Use,
            "mod" => Keyword::Mod,
            "pub" => Keyword::Pub,
            "self" => Keyword::SelfValue,
            "Self" => Keyword::SelfType,
            "return" => Keyword::Return,
            "if" => Keyword::If,
            "else" => Keyword::Else,
            "match" => Keyword::Match,
            "while" => Keyword::While,
            "loop" => Keyword::Loop,
            "in" => Keyword::In,
            "break" => Keyword::Break,
            "continue" => Keyword::Continue,
            "unsafe" => Keyword::Unsafe,
            "as" => Keyword::As,
            "where" => Keyword::Where,
            "true" => Keyword::True,
            "false" => Keyword::False,
            "defer" => Keyword::Defer,
            "move" => Keyword::Move,
            "ref" => Keyword::Ref,
            "dyn" => Keyword::Dyn,
            "extern" => Keyword::Extern,
            "sizeof" => Keyword::Sizeof,
            _ => return None,
        })
    }

    /// Keywords that may never be used as an identifier.
    pub fn is_reserved(self) -> bool {
        true
    }

    pub fn all() -> &'static [Keyword] {
        use Keyword::*;
        &[
            Fn, Let, Mut, Struct, Enum, Trait, Impl, For, Type, Const, Static, Use, Mod, Pub, SelfValue,
            SelfType, Return, If, Else, Match, While, Loop, In, Break, Continue, Unsafe, As, Where, True,
            False, Defer, Move, Ref, Dyn,
        ]
    }
}

impl TokenKind {
    /// Human-facing description used in "expected X, found Y" diagnostics.
    pub fn describe(&self) -> String {
        match self {
            TokenKind::Int { .. } => "integer literal".into(),
            TokenKind::Float { .. } => "float literal".into(),
            TokenKind::Str(_) | TokenKind::StrInterp(_) => "string literal".into(),
            TokenKind::Char(_) => "character literal".into(),
            TokenKind::Ident(name) => format!("identifier `{name}`"),
            TokenKind::Keyword(k) => format!("keyword `{}`", k.as_str()),
            TokenKind::Eof => "end of file".into(),
            TokenKind::Error => "invalid token".into(),
            other => format!("`{}`", other.symbol()),
        }
    }

    /// Concrete symbol text for punctuation tokens ("" for the rest).
    pub fn symbol(&self) -> &'static str {
        match self {
            TokenKind::LParen => "(",
            TokenKind::RParen => ")",
            TokenKind::LBrace => "{",
            TokenKind::RBrace => "}",
            TokenKind::LBracket => "[",
            TokenKind::RBracket => "]",
            TokenKind::Comma => ",",
            TokenKind::Semi => ";",
            TokenKind::Colon => ":",
            TokenKind::ColonColon => "::",
            TokenKind::Dot => ".",
            TokenKind::DotDot => "..",
            TokenKind::DotDotEq => "..=",
            TokenKind::Arrow => "->",
            TokenKind::FatArrow => "=>",
            TokenKind::Pipe => "|",
            TokenKind::PipePipe => "||",
            TokenKind::Amp => "&",
            TokenKind::AmpAmp => "&&",
            TokenKind::Star => "*",
            TokenKind::Plus => "+",
            TokenKind::Minus => "-",
            TokenKind::Slash => "/",
            TokenKind::Percent => "%",
            TokenKind::Caret => "^",
            TokenKind::Bang => "!",
            TokenKind::Question => "?",
            TokenKind::Eq => "=",
            TokenKind::EqEq => "==",
            TokenKind::Ne => "!=",
            TokenKind::Lt => "<",
            TokenKind::Le => "<=",
            TokenKind::Gt => ">",
            TokenKind::Ge => ">=",
            TokenKind::PlusEq => "+=",
            TokenKind::MinusEq => "-=",
            TokenKind::StarEq => "*=",
            TokenKind::SlashEq => "/=",
            TokenKind::PercentEq => "%=",
            TokenKind::AmpEq => "&=",
            TokenKind::PipeEq => "|=",
            TokenKind::CaretEq => "^=",
            TokenKind::Shl => "<<",
            TokenKind::Shr => ">>",
            TokenKind::ShlEq => "<<=",
            TokenKind::ShrEq => ">>=",
            TokenKind::At => "@",
            TokenKind::Hash => "#",
            TokenKind::Underscore => "_",
            _ => "",
        }
    }

    /// True if the token terminates a statement when semicolons are omitted.
    pub fn starts_statement(&self) -> bool {
        matches!(
            self,
            TokenKind::Keyword(
                Keyword::Let
                    | Keyword::Return
                    | Keyword::If
                    | Keyword::While
                    | Keyword::Loop
                    | Keyword::For
                    | Keyword::Break
                    | Keyword::Continue
                    | Keyword::Defer
            )
        )
    }
}
