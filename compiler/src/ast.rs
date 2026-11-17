//! Abstract syntax tree.
//!
//! The AST is a faithful, lossless-enough representation of the source: every
//! node carries its [`Span`]. It performs no name resolution and no type
//! checking; those happen in later phases (HIR).

use crate::source::Span;
use crate::token::StrPart;

/// A whole source file.
#[derive(Debug, Clone)]
pub struct Program {
    pub items: Vec<Item>,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub enum Item {
    Fn(FnDecl),
    Struct(StructDecl),
    Enum(EnumDecl),
    Trait(TraitDecl),
    Impl(ImplBlock),
    Const(ConstDecl),
    Use(UseDecl),
    Mod(ModDecl),
    TypeAlias(TypeAlias),
}

impl Item {
    pub fn span(&self) -> Span {
        match self {
            Item::Fn(d) => d.span,
            Item::Struct(d) => d.span,
            Item::Enum(d) => d.span,
            Item::Trait(d) => d.span,
            Item::Impl(d) => d.span,
            Item::Const(d) => d.span,
            Item::Use(d) => d.span,
            Item::Mod(d) => d.span,
            Item::TypeAlias(d) => d.span,
        }
    }
}

// ---------------------------------------------------------------------------
// Functions
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct FnDecl {
    pub name: String,
    pub name_span: Span,
    /// Span of the `fn` keyword, used when a fix inserts a keyword before it.
    pub fn_span: Span,
    /// Span of the `extern` keyword when present, so a fix can remove it.
    pub extern_span: Option<Span>,
    /// The linker symbol named by `extern fn "symbol" name(..)`. `None` means the
    /// symbol is the UMAY name.
    pub link_name: Option<String>,
    /// Span of that symbol, so a diagnostic can point at it.
    pub link_name_span: Option<Span>,
    pub generics: Vec<GenericParam>,
    pub params: Vec<Param>,
    /// `self` receiver, if this is a method.
    pub self_param: Option<SelfParam>,
    pub ret: Option<TypeExpr>,
    pub body: Option<Block>,
    pub attrs: Vec<Attribute>,
    /// Whether this is an `extern fn` declaration, which is a promise that the
    /// symbol exists elsewhere and is written to the C calling convention.
    pub is_extern: bool,
    /// Whether the declaration was written `pub`. A non-`pub` item is reachable
    /// only inside the module that declares it.
    pub is_pub: bool,
    pub span: Span,
}

impl FnDecl {
    /// True when the function has no body, so it can only be called.
    pub fn is_declaration(&self) -> bool {
        self.body.is_none()
    }
}

#[derive(Debug, Clone)]
pub struct SelfParam {
    pub mutable: bool,
    /// `None` means by-value `self`, `Some(false)` means `&self`,
    /// `Some(true)` means `&mut self`.
    pub by_ref: Option<bool>,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct Param {
    /// `None` when the parameter is `_` (allowed only for trait declarations).
    pub name: Option<String>,
    pub name_span: Span,
    pub ty: TypeExpr,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct GenericParam {
    pub name: String,
    pub span: Span,
    /// Trait bounds: `T: Display + Clone`.
    pub bounds: Vec<Path>,
}

// ---------------------------------------------------------------------------
// Type declarations
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct StructDecl {
    pub name: String,
    pub name_span: Span,
    pub generics: Vec<GenericParam>,
    pub fields: Vec<FieldDecl>,
    pub attrs: Vec<Attribute>,
    pub is_pub: bool,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct FieldDecl {
    pub name: String,
    pub name_span: Span,
    pub ty: TypeExpr,
    pub mutable: bool,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct EnumDecl {
    pub name: String,
    pub name_span: Span,
    pub generics: Vec<GenericParam>,
    pub variants: Vec<EnumVariant>,
    pub attrs: Vec<Attribute>,
    pub is_pub: bool,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct EnumVariant {
    pub name: String,
    pub name_span: Span,
    /// `Variant(T1, T2)`
    pub tuple_fields: Option<Vec<TypeExpr>>,
    /// `Variant { f: T }`
    pub struct_fields: Option<Vec<FieldDecl>>,
    /// Discriminant value `Variant = 3`
    pub discriminant: Option<Expr>,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct TraitDecl {
    pub name: String,
    pub name_span: Span,
    pub generics: Vec<GenericParam>,
    pub methods: Vec<FnDecl>,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct ImplBlock {
    pub generics: Vec<GenericParam>,
    /// Trait path if this is `impl Trait for Type`.
    pub trait_path: Option<Path>,
    /// The self type being implemented for.
    pub self_ty: TypeExpr,
    pub methods: Vec<FnDecl>,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct ConstDecl {
    pub name: String,
    pub name_span: Span,
    pub ty: Option<TypeExpr>,
    pub value: Expr,
    pub attrs: Vec<Attribute>,
    pub is_pub: bool,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct UseDecl {
    pub path: Path,
    /// `use a::b as c`
    pub alias: Option<String>,
    /// `use a::{b, c}`
    pub subtree: bool,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct ModDecl {
    pub name: String,
    pub name_span: Span,
    /// Inline module body, or `None` for `mod name;` (file module).
    pub items: Option<Vec<Item>>,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct TypeAlias {
    pub name: String,
    pub name_span: Span,
    pub generics: Vec<GenericParam>,
    pub ty: TypeExpr,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct Attribute {
    pub name: String,
    pub args: Vec<String>,
    pub span: Span,
}

// ---------------------------------------------------------------------------
// Paths and types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct Path {
    pub segments: Vec<PathSegment>,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct PathSegment {
    pub name: String,
    pub span: Span,
    pub args: Vec<TypeExpr>,
}

impl Path {
    pub fn is_simple(&self) -> bool {
        self.segments.len() == 1 && self.segments[0].args.is_empty()
    }

    pub fn as_string(&self) -> String {
        self.segments.iter().map(|s| s.name.clone()).collect::<Vec<_>>().join("::")
    }
}

impl std::fmt::Display for Path {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.as_string())
    }
}

#[derive(Debug, Clone)]
pub enum TypeExpr {
    /// A named type, possibly generic: `Vec<i32>`, `std::io::File`.
    Path(Path),
    /// `&T` or `&mut T`
    Ref { mutable: bool, inner: Box<TypeExpr>, span: Span },
    /// `*const T` / `*mut T` — only valid inside `unsafe` contexts.
    Ptr { mutable: bool, inner: Box<TypeExpr>, span: Span },
    /// `[T; N]`
    Array { elem: Box<TypeExpr>, len: Box<Expr>, span: Span },
    /// `[T]` (slice)
    Slice { elem: Box<TypeExpr>, span: Span },
    /// `(A, B, C)`
    Tuple { elems: Vec<TypeExpr>, span: Span },
    /// `fn(A) -> B`
    Fn { params: Vec<TypeExpr>, ret: Option<Box<TypeExpr>>, span: Span },
    /// `impl Trait`
    ImplTrait { bounds: Vec<Path>, span: Span },
    /// `dyn Trait`
    DynTrait { bounds: Vec<Path>, span: Span },
    /// Placeholder in inference position: `_`
    Infer { span: Span },
    /// `!` never type
    Never { span: Span },
}

impl TypeExpr {
    pub fn span(&self) -> Span {
        match self {
            TypeExpr::Path(p) => p.span,
            TypeExpr::Ref { span, .. }
            | TypeExpr::Ptr { span, .. }
            | TypeExpr::Array { span, .. }
            | TypeExpr::Slice { span, .. }
            | TypeExpr::Tuple { span, .. }
            | TypeExpr::Fn { span, .. }
            | TypeExpr::ImplTrait { span, .. }
            | TypeExpr::DynTrait { span, .. }
            | TypeExpr::Infer { span }
            | TypeExpr::Never { span } => *span,
        }
    }
}

// ---------------------------------------------------------------------------
// Statements and blocks
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct Block {
    pub stmts: Vec<Stmt>,
    /// Optional trailing expression (implicit return / block value).
    pub tail: Option<Box<Expr>>,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct Stmt {
    pub kind: StmtKind,
    pub span: Span,
}

#[derive(Debug, Clone)]
// Boxing `Let` and `Expr` would allocate on every statement to flatten a size
// difference the AST is short-lived enough not to care about.
#[allow(clippy::large_enum_variant)]
pub enum StmtKind {
    /// `let [mut] pattern [: Type] [= expr]`
    Let(LetStmt),
    Expr(Expr),
    /// Nested item declarations (allowed in blocks).
    Item(Box<Item>),
}

#[derive(Debug, Clone)]
pub struct LetStmt {
    pub mutable: bool,
    pub pattern: Pattern,
    pub ty: Option<TypeExpr>,
    pub init: Option<Expr>,
    /// `let ... else { ... }` (let-else, future)
    pub else_block: Option<Block>,
    pub span: Span,
}

// ---------------------------------------------------------------------------
// Expressions
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct Expr {
    pub kind: ExprKind,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub enum ExprKind {
    IntLit { value: u128, suffix: Option<crate::token::IntSuffix> },
    FloatLit { value: f64, suffix: Option<crate::token::FloatSuffix>, text: String },
    StrLit(String),
    StrInterp(Vec<StrPart>),
    CharLit(char),
    BoolLit(bool),
    Ident(String),
    Path(Path),
    /// `*expr`
    Unary { op: UnaryOp, operand: Box<Expr> },
    Binary { op: BinaryOp, lhs: Box<Expr>, rhs: Box<Expr> },
    /// `&expr` / `&mut expr`
    Ref { mutable: bool, operand: Box<Expr> },
    /// `expr as Type`
    Cast { expr: Box<Expr>, ty: TypeExpr },
    /// `sizeof(Type)` — the size of a type in bytes.
    Sizeof { ty: TypeExpr },
    /// Plain assignment: `place = value`.
    Assign { place: Box<Expr>, value: Box<Expr> },
    /// Compound assignment: `place op= value`.
    AssignOp { op: BinaryOp, place: Box<Expr>, value: Box<Expr> },
    /// `expr(args)`, `path::fn(args)`
    Call { callee: Box<Expr>, args: Vec<Expr>, trailing_comma: bool },
    /// `receiver.method(args)`
    MethodCall { receiver: Box<Expr>, method: String, method_span: Span, args: Vec<Expr> },
    /// `expr.field`
    FieldAccess { base: Box<Expr>, field: String, field_span: Span },
    /// `expr[index]`
    Index { base: Box<Expr>, index: Box<Expr> },
    /// `Type { field: value, .. }`
    StructLit { path: Path, fields: Vec<FieldInit>, base: Option<Box<Expr>> },
    /// `[a, b, c]`
    ArrayLit { elems: Vec<Expr> },
    /// `[value; count]`
    ArrayRepeat { value: Box<Expr>, count: Box<Expr> },
    /// `(a, b)` or `(a)`
    Tuple { elems: Vec<Expr> },
    /// `if cond { .. } else { .. }`
    If { cond: Box<Expr>, then_block: Block, else_branch: Option<Box<Expr>> },
    /// `match scrutinee { ... }`
    Match { scrutinee: Box<Expr>, arms: Vec<MatchArm> },
    /// `while cond { .. }`
    While { cond: Box<Expr>, body: Block },
    /// `loop { .. }`
    Loop { body: Block },
    /// `for pattern in iter { .. }`
    For { pattern: Pattern, iter: Box<Expr>, body: Block },
    /// `{ ... }` block expression
    Block(Block),
    /// `return expr?`
    Return(Option<Box<Expr>>),
    /// `break expr?`
    Break(Option<Box<Expr>>),
    /// `continue`
    Continue,
    /// `expr?` — error propagation
    Try(Box<Expr>),
    /// `defer { .. }`
    Defer(Block),
    /// `move expr` — explicit ownership transfer
    Move(Box<Expr>),
    /// `unsafe { .. }`
    Unsafe(Block),
    /// `range`: `a..b`, `a..=b`, `..b`, `a..`
    Range { start: Option<Box<Expr>>, end: Option<Box<Expr>>, inclusive: bool },
    /// Placeholder `_` in expressions (only in specific positions).
    Infer,
}

#[derive(Debug, Clone)]
pub struct FieldInit {
    pub name: String,
    pub name_span: Span,
    /// `None` means field shorthand `{ x }`.
    pub value: Option<Expr>,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct MatchArm {
    pub pattern: Pattern,
    pub guard: Option<Expr>,
    pub body: Expr,
    pub span: Span,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnaryOp {
    Neg,
    Not,
    /// `*ptr` dereference (distinct from `Neg` even though spelled differently
    /// at the call site; kept separate for clarity in later phases).
    Deref,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinaryOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    BitAnd,
    BitOr,
    BitXor,
    Shl,
    Shr,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    And,
    Or,
}

impl BinaryOp {
    /// True for `&`, `|`, `^`, `<<` and `>>`, which require integer operands.
    pub fn is_bitwise(self) -> bool {
        matches!(
            self,
            BinaryOp::BitAnd | BinaryOp::BitOr | BinaryOp::BitXor | BinaryOp::Shl | BinaryOp::Shr
        )
    }

    pub fn as_str(self) -> &'static str {
        match self {
            BinaryOp::Add => "+",
            BinaryOp::Sub => "-",
            BinaryOp::Mul => "*",
            BinaryOp::Div => "/",
            BinaryOp::Rem => "%",
            BinaryOp::BitAnd => "&",
            BinaryOp::BitOr => "|",
            BinaryOp::BitXor => "^",
            BinaryOp::Shl => "<<",
            BinaryOp::Shr => ">>",
            BinaryOp::Eq => "==",
            BinaryOp::Ne => "!=",
            BinaryOp::Lt => "<",
            BinaryOp::Le => "<=",
            BinaryOp::Gt => ">",
            BinaryOp::Ge => ">=",
            BinaryOp::And => "&&",
            BinaryOp::Or => "||",
        }
    }

    pub fn is_comparison(self) -> bool {
        matches!(self, BinaryOp::Eq | BinaryOp::Ne | BinaryOp::Lt | BinaryOp::Le | BinaryOp::Gt | BinaryOp::Ge)
    }

    pub fn is_logical(self) -> bool {
        matches!(self, BinaryOp::And | BinaryOp::Or)
    }
}

// ---------------------------------------------------------------------------
// Patterns
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct Pattern {
    pub kind: PatternKind,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub enum PatternKind {
    /// Binding, e.g. `x` or `mut x`. `name` is `_` for wildcards.
    Binding { name: String, mutable: bool, by_ref: Option<bool> },
    /// Literal pattern.
    Literal(Box<Expr>),
    /// Tuple `(a, b)`
    Tuple(Vec<Pattern>),
    /// Struct `User { id, name: n, .. }`
    Struct { path: Path, fields: Vec<PatternField>, rest: bool },
    /// Enum variant `Status::Active(x)` or `Status::Active { .. }`
    Variant { path: Path, kind: VariantPatternKind },
    /// `a | b`
    Or(Vec<Pattern>),
    /// `P if guard` is represented in MatchArm; this is a range pattern `a..=b`.
    Range { start: Box<Pattern>, end: Box<Pattern>, inclusive: bool },
    /// Slice pattern `[a, b, ..rest]`
    Slice { elems: Vec<Pattern>, rest: bool },
}

#[derive(Debug, Clone)]
pub struct PatternField {
    pub name: String,
    pub name_span: Span,
    /// `None` for shorthand patterns `{ id }`.
    pub pattern: Option<Pattern>,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub enum VariantPatternKind {
    Unit,
    Tuple(Vec<Pattern>),
    Struct { fields: Vec<PatternField>, rest: bool },
}

impl Pattern {
    pub fn is_wildcard(&self) -> bool {
        matches!(&self.kind, PatternKind::Binding { name, .. } if name == "_")
    }
}
