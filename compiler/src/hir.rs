//! HIR: the semantically resolved program representation.
//!
//! HIR is what remains after name resolution and desugaring. It differs from the
//! AST in three important ways:
//!
//! * every name is resolved to a [`Res`] — no lookup by string happens later,
//! * sugar is removed (`for` becomes `loop` + `next`, `?` becomes a readiness
//!   test, string interpolation becomes concatenation of a formatted list),
//! * every type expression is resolved to a [`TyId`].
//!
//! Phase 2 produces HIR together with types; Phase 3 lowers HIR to MIR.

use crate::source::Span;
pub use crate::types::DefId;
use crate::types::TyId;

// ---------------------------------------------------------------------------
// Resolutions
// ---------------------------------------------------------------------------

/// What a name resolved to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Res {
    /// A local variable binding (function parameter or `let`).
    Local(LocalId),
    /// A function or method.
    Fn(FnId),
    /// A struct or enum declaration.
    Type(DefId),
    /// An enum variant: which enum, and which variant index.
    Variant { def: DefId, index: u32 },
    /// A constant.
    Const(ConstId),
}

/// Local variable slot inside a function body.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash, PartialOrd, Ord)]
pub struct LocalId(pub u32);

/// Function slot inside the program.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash, PartialOrd, Ord)]
pub struct FnId(pub u32);

/// Constant slot inside the program.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash, PartialOrd, Ord)]
pub struct ConstId(pub u32);

// ---------------------------------------------------------------------------
// Program
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub struct Hir {
    pub defs: Vec<DefInfo>,
    pub fns: Vec<HirFn>,
    pub consts: Vec<HirConst>,
    /// Struct/enum definitions by `DefId`.
    pub structs: Vec<StructInfo>,
    pub enums: Vec<EnumInfo>,
    pub traits: Vec<TraitInfo>,
}

#[derive(Debug, Clone)]
pub struct DefInfo {
    pub name: String,
    pub span: Span,
    pub def_id: DefId,
    pub kind: DefKind,
    pub is_pub: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DefKind {
    Struct,
    Enum,
    Trait,
}

#[derive(Debug, Clone)]
pub struct StructInfo {
    pub name: String,
    pub span: Span,
    pub def_id: DefId,
    pub generics: Vec<String>,
    pub fields: Vec<FieldInfo>,
    /// Whether `#[derive(Copy)]` was present (checked in Phase 4).
    pub is_copy: bool,
}

#[derive(Debug, Clone)]
pub struct FieldInfo {
    pub name: String,
    pub span: Span,
    pub ty: TyId,
    pub ty_span: Span,
}

#[derive(Debug, Clone)]
pub struct EnumInfo {
    pub name: String,
    pub span: Span,
    pub def_id: DefId,
    pub generics: Vec<String>,
    pub variants: Vec<VariantInfo>,
}

#[derive(Debug, Clone)]
pub struct VariantInfo {
    pub name: String,
    pub span: Span,
    pub fields: VariantFields,
}

#[derive(Debug, Clone)]
pub enum VariantFields {
    /// `Variant`
    Unit,
    /// `Variant(A, B)`
    Tuple(Vec<TyId>),
    /// `Variant { a: A }`
    Struct(Vec<FieldInfo>),
}

impl VariantFields {
    pub fn arity(&self) -> usize {
        match self {
            VariantFields::Unit => 0,
            VariantFields::Tuple(tys) => tys.len(),
            VariantFields::Struct(fields) => fields.len(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct TraitInfo {
    pub name: String,
    pub span: Span,
    pub methods: Vec<TraitMethodInfo>,
}

#[derive(Debug, Clone)]
pub struct TraitMethodInfo {
    pub name: String,
    pub span: Span,
    pub params: Vec<TyId>,
    pub ret: TyId,
}

#[derive(Debug, Clone)]
pub struct HirConst {
    pub name: String,
    pub span: Span,
    pub ty: TyId,
    pub value: HirExpr,
}

#[derive(Debug, Clone)]
pub struct HirFn {
    pub id: FnId,
    pub name: String,
    pub span: Span,
    pub generics: Vec<String>,
    /// Declared parameter types after resolution.
    pub params: Vec<TyId>,
    /// Resolved return type. `()` when omitted.
    pub ret: TyId,
    /// Whether this is a method (`self` receiver present).
    pub has_self: bool,
    pub self_mutable: bool,
    pub self_by_ref: bool,
    /// Local table: index 0..params.len() are parameters in order.
    pub locals: Vec<LocalInfo>,
    pub body: Option<HirBlock>,
    pub is_pub: bool,
}

#[derive(Debug, Clone)]
pub struct LocalInfo {
    pub name: String,
    pub ty: TyId,
    pub mutable: bool,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct HirBlock {
    pub stmts: Vec<HirStmt>,
    pub tail: Option<Box<HirExpr>>,
    pub span: Span,
    /// Number of locals introduced directly by this block, so that the
    /// ownership phase knows the block's drop scope.
    pub locals_introduced: u32,
}

#[derive(Debug, Clone)]
pub struct HirStmt {
    pub kind: HirStmtKind,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub enum HirStmtKind {
    /// One or more bindings introduced by `let`. A destructuring `let` binds
    /// several locals at once.
    ///
    /// `init` is the evaluated initializer. It is `None` only when a
    /// destructuring `let` already consumed it into the pattern bindings.
    Let { bindings: Vec<LocalId>, init: Option<Box<HirExpr>> },
    Expr(HirExpr),
}

#[derive(Debug, Clone)]
pub struct HirExpr {
    pub kind: HirExprKind,
    /// The type this expression was checked against. `Error` on failure.
    pub ty: TyId,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub enum HirExprKind {
    // ---- literals ----
    IntLit { value: u128, suffix: Option<crate::token::IntSuffix> },
    FloatLit { value: f64, suffix: Option<crate::token::FloatSuffix> },
    StrLit(String),
    /// Interpolated string, already split into literal segments and value
    /// expressions. Each value expression has been checked for `Display`.
    StrInterp { parts: Vec<HirStrPart> },
    CharLit(char),
    BoolLit(bool),

    // ---- names ----
    Local(LocalId),
    FnRef(FnId),
    ConstRef(ConstId),
    /// Unit variant used as a value: `Status::Active`
    UnitVariant { def: DefId, index: u32 },

    // ---- operators ----
    Unary { op: HirUnaryOp, operand: Box<HirExpr> },
    Binary { op: HirBinaryOp, lhs: Box<HirExpr>, rhs: Box<HirExpr> },
    /// Assignment to a place.
    Assign { place: Box<HirExpr>, value: Box<HirExpr> },
    /// Compound assignment, desugared later in MIR (`x += y` → `x = x + y`).
    AssignOp { op: HirBinaryOp, place: Box<HirExpr>, value: Box<HirExpr> },
    /// `&place` / `&mut place`
    Ref { mutable: bool, place: Box<HirExpr> },
    /// `*expr`
    Deref(Box<HirExpr>),
    /// `move expr`
    Move(Box<HirExpr>),
    /// `expr as Ty`
    Cast { expr: Box<HirExpr>, to: TyId },
    /// `ptr.offset(n)` — moves a raw pointer by `n` elements of its pointee.
    ///
    /// This is an intrinsic rather than a method on a type, because a raw pointer
    /// has no type of its own to hang methods on, and because the scaling by the
    /// pointee's size is a machine concern rather than a library one.
    PtrOffset { ptr: Box<HirExpr>, count: Box<HirExpr> },

    // ---- calls & projections ----
    Call { callee: Box<HirExpr>, args: Vec<HirExpr> },
    /// Struct literal, with generic args already substituted.
    StructLit { def: DefId, fields: Vec<HirFieldInit>, base: Option<Box<HirExpr>> },
    /// Tuple variant construction: `Some(1)`.
    TupleVariant { def: DefId, index: u32, args: Vec<HirExpr> },
    /// Struct variant construction: `Shape::Rect { w: 1, h: 2 }`.
    StructVariant { def: DefId, index: u32, fields: Vec<HirFieldInit> },
    /// Array literal `[a, b, c]`.
    ArrayLit { elems: Vec<HirExpr> },
    /// Repeated array `[v; n]`.
    ArrayRepeat { value: Box<HirExpr>, count: u64 },
    /// Tuple construction `(a, b)`.
    Tuple { elems: Vec<HirExpr> },
    /// An iterable produced for a `for` loop: an integer range or a collection.
    RangeExpr { start: Box<HirExpr>, end: Box<HirExpr>, inclusive: bool, elem: TyId },
    /// A collection being iterated by `for`.
    IterSource { source: Box<HirExpr>, elem: TyId },
    /// Field access by resolved index (after flattening tuple fields).
    Field { base: Box<HirExpr>, index: u32, name: String },
    /// Tuple index access, resolved at check time.
    TupleIndex { base: Box<HirExpr>, index: u32 },
    Index { base: Box<HirExpr>, index: Box<HirExpr> },
    /// `receiver.method(args)` — resolved to the declaring function.
    MethodCall { receiver: Box<HirExpr>, method: FnId, args: Vec<HirExpr> },
    /// A method provided by the language on a built-in type, e.g. `str::len`.
    /// Lowered by the backend rather than declared in an `impl` block.
    BuiltinMethod { name: String, receiver: Box<HirExpr>, args: Vec<HirExpr> },

    // ---- control flow ----
    /// `if` with a `then` block and optional `else` expression.
    If { cond: Box<HirExpr>, then_block: HirBlock, else_branch: Option<Box<HirExpr>> },
    Match { scrutinee: Box<HirExpr>, arms: Vec<HirMatchArm> },
    While { cond: Box<HirExpr>, body: HirBlock },
    Loop { body: HirBlock },
    /// Desugared `for`: the iterator is consumed by explicit calls.
    For { pattern_locals: Vec<LocalId>, iter: Box<HirExpr>, body: HirBlock },
    Block(HirBlock),
    Return(Option<Box<HirExpr>>),
    Break(Option<Box<HirExpr>>),
    Continue,
    /// `?` on a `Result`/`Option`: returns early when not `Ok`/`Some`.
    Try(Box<HirExpr>),
    /// Runs a block when the enclosing scope exits.
    Defer(HirBlock),
    /// `unsafe { .. }`
    Unsafe(HirBlock),

    /// Placeholder after an error; type is `Error` so nothing cascades.
    Error,
}

#[derive(Debug, Clone)]
pub struct HirFieldInit {
    pub name: String,
    pub span: Span,
    /// Resolved field index in the struct/variant.
    pub index: u32,
    pub value: HirExpr,
}

#[derive(Debug, Clone)]
pub enum HirStrPart {
    Text(String),
    Value(HirExpr),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HirUnaryOp {
    Neg,
    Not,
    Deref,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HirBinaryOp {
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

impl HirBinaryOp {
    pub fn as_str(self) -> &'static str {
        match self {
            HirBinaryOp::Add => "+",
            HirBinaryOp::Sub => "-",
            HirBinaryOp::Mul => "*",
            HirBinaryOp::Div => "/",
            HirBinaryOp::Rem => "%",
            HirBinaryOp::BitAnd => "&",
            HirBinaryOp::BitOr => "|",
            HirBinaryOp::BitXor => "^",
            HirBinaryOp::Shl => "<<",
            HirBinaryOp::Shr => ">>",
            HirBinaryOp::Eq => "==",
            HirBinaryOp::Ne => "!=",
            HirBinaryOp::Lt => "<",
            HirBinaryOp::Le => "<=",
            HirBinaryOp::Gt => ">",
            HirBinaryOp::Ge => ">=",
            HirBinaryOp::And => "&&",
            HirBinaryOp::Or => "||",
        }
    }

    pub fn is_comparison(self) -> bool {
        matches!(self, Self::Eq | Self::Ne | Self::Lt | Self::Le | Self::Gt | Self::Ge)
    }

    pub fn is_bitwise(self) -> bool {
        matches!(self, Self::BitAnd | Self::BitOr | Self::BitXor | Self::Shl | Self::Shr)
    }
}

#[derive(Debug, Clone)]
pub struct HirMatchArm {
    pub pattern: HirPattern,
    pub guard: Option<HirExpr>,
    pub body: HirExpr,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct HirPattern {
    pub kind: HirPatternKind,
    pub ty: TyId,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub enum HirPatternKind {
    /// `_`
    Wildcard,
    /// Binding to a local.
    Binding { local: LocalId, mutable: bool },
    BoolLit(bool),
    IntLit { value: u128, negative: bool },
    CharLit(char),
    StrLit(String),
    Tuple(Vec<HirPattern>),
    /// Struct pattern with resolved field indices.
    Struct { def: DefId, fields: Vec<HirPatternField>, rest: bool },
    /// Enum variant pattern.
    Variant { def: DefId, index: u32, fields: VariantPatternFields },
    /// `a | b`
    Or(Vec<HirPattern>),
    /// `start..=end` inclusive integer range.
    Range { start: i128, end: i128 },
    /// `start..end` exclusive integer range.
    RangeExclusive { start: i128, end: i128 },
    Slice { elems: Vec<HirPattern>, rest: bool },
    /// Placeholder after an error.
    Error,
}

#[derive(Debug, Clone)]
pub struct HirPatternField {
    pub index: u32,
    pub pattern: HirPattern,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub enum VariantPatternFields {
    Unit,
    Tuple(Vec<HirPattern>),
    Struct { fields: Vec<HirPatternField>, rest: bool },
}

impl Hir {
    pub fn struct_info(&self, def: DefId) -> &StructInfo {
        &self.structs[def.0 as usize]
    }

    pub fn enum_info(&self, def: DefId) -> &EnumInfo {
        &self.enums[def.0 as usize]
    }

    pub fn fn_info(&self, id: FnId) -> &HirFn {
        &self.fns[id.0 as usize]
    }

    /// True if `def` refers to a struct (as opposed to an enum).
    pub fn is_struct(&self, def: DefId) -> bool {
        self.structs.iter().any(|s| s.def_id == def)
    }

    /// The declared name of a struct or enum, for diagnostics.
    pub fn def_name(&self, def: DefId) -> String {
        if let Some(s) = self.structs.iter().find(|s| s.def_id == def) {
            return s.name.clone();
        }
        if let Some(e) = self.enums.iter().find(|e| e.def_id == def) {
            return e.name.clone();
        }
        "<unknown>".to_string()
    }
}

/// Rewrites every `TyId` in the tree according to `map`.
///
/// Used by the checker to replace literal inference variables (`{integer}`,
/// `{float}`) with their defaults once checking is done. Lowering never has to
/// know that inference happened.
/// Rewrites literal defaults into every place a type is stored in the HIR,
/// including struct-literal field types and local declarations.
pub fn rewrite_types(block: &mut HirBlock, map: &std::collections::HashMap<TyId, TyId>) {
    if map.is_empty() {
        return;
    }
    rewrite_block(block, map);
}

fn map_ty(ty: &mut TyId, map: &std::collections::HashMap<TyId, TyId>) {
    if let Some(t) = map.get(ty) {
        *ty = *t;
    }
}

fn rewrite_block(block: &mut HirBlock, map: &std::collections::HashMap<TyId, TyId>) {
    for stmt in &mut block.stmts {
        match &mut stmt.kind {
            HirStmtKind::Let { init, .. } => {
                if let Some(i) = init {
                    rewrite_expr(i, map);
                }
            }
            HirStmtKind::Expr(e) => rewrite_expr(e, map),
        }
    }
    if let Some(t) = &mut block.tail {
        rewrite_expr(t, map);
    }
}

fn rewrite_expr(e: &mut HirExpr, map: &std::collections::HashMap<TyId, TyId>) {
    map_ty(&mut e.ty, map);
    match &mut e.kind {
        HirExprKind::IntLit { .. }
        | HirExprKind::FloatLit { .. }
        | HirExprKind::StrLit(_)
        | HirExprKind::CharLit(_)
        | HirExprKind::BoolLit(_)
        | HirExprKind::Local(_)
        | HirExprKind::FnRef(_)
        | HirExprKind::ConstRef(_)
        | HirExprKind::UnitVariant { .. }
        | HirExprKind::Continue
        | HirExprKind::Error => {}
        HirExprKind::StrInterp { parts } => {
            for p in parts {
                if let HirStrPart::Value(v) = p {
                    rewrite_expr(v, map);
                }
            }
        }
        HirExprKind::Unary { operand, .. }
        | HirExprKind::Deref(operand)
        | HirExprKind::Move(operand)
        | HirExprKind::Try(operand) => rewrite_expr(operand, map),
        HirExprKind::Binary { lhs, rhs, .. }
        | HirExprKind::Assign { place: lhs, value: rhs }
        | HirExprKind::AssignOp { place: lhs, value: rhs, .. } => {
            rewrite_expr(lhs, map);
            rewrite_expr(rhs, map);
        }
        HirExprKind::Ref { place, .. } => rewrite_expr(place, map),
        HirExprKind::Cast { expr, to } => {
            rewrite_expr(expr, map);
            map_ty(to, map);
        }
        HirExprKind::PtrOffset { ptr, count } => {
            rewrite_expr(ptr, map);
            rewrite_expr(count, map);
        }
        HirExprKind::Call { callee, args } => {
            rewrite_expr(callee, map);
            for a in args {
                rewrite_expr(a, map);
            }
        }
        HirExprKind::StructLit { fields, base, .. } => {
            for f in fields {
                rewrite_expr(&mut f.value, map);
            }
            if let Some(b) = base {
                rewrite_expr(b, map);
            }
        }
        HirExprKind::TupleVariant { args, .. } => {
            for a in args {
                rewrite_expr(a, map);
            }
        }
        HirExprKind::StructVariant { fields, .. } => {
            for f in fields {
                rewrite_expr(&mut f.value, map);
            }
        }
        HirExprKind::ArrayLit { elems } => {
            for x in elems {
                rewrite_expr(x, map);
            }
        }
        HirExprKind::ArrayRepeat { value, .. } => rewrite_expr(value, map),
        HirExprKind::Tuple { elems } => {
            for x in elems {
                rewrite_expr(x, map);
            }
        }
        HirExprKind::RangeExpr { start, end, elem, .. } => {
            rewrite_expr(start, map);
            rewrite_expr(end, map);
            map_ty(elem, map);
        }
        HirExprKind::IterSource { source, elem } => {
            rewrite_expr(source, map);
            map_ty(elem, map);
        }
        HirExprKind::Field { base, .. } | HirExprKind::TupleIndex { base, .. } => {
            rewrite_expr(base, map)
        }
        HirExprKind::Index { base, index } => {
            rewrite_expr(base, map);
            rewrite_expr(index, map);
        }
        HirExprKind::MethodCall { receiver, args, .. }
        | HirExprKind::BuiltinMethod { receiver, args, .. } => {
            rewrite_expr(receiver, map);
            for a in args {
                rewrite_expr(a, map);
            }
        }
        HirExprKind::If { cond, then_block, else_branch } => {
            rewrite_expr(cond, map);
            rewrite_block(then_block, map);
            if let Some(e) = else_branch {
                rewrite_expr(e, map);
            }
        }
        HirExprKind::Match { scrutinee, arms } => {
            rewrite_expr(scrutinee, map);
            for arm in arms {
                rewrite_expr(&mut arm.body, map);
                if let Some(g) = &mut arm.guard {
                    rewrite_expr(g, map);
                }
            }
        }
        HirExprKind::While { cond, body } => {
            rewrite_expr(cond, map);
            rewrite_block(body, map);
        }
        HirExprKind::Loop { body } => rewrite_block(body, map),
        HirExprKind::For { iter, body, .. } => {
            rewrite_expr(iter, map);
            rewrite_block(body, map);
        }
        HirExprKind::Block(b) => rewrite_block(b, map),
        HirExprKind::Return(v) | HirExprKind::Break(v) => {
            if let Some(x) = v {
                rewrite_expr(x, map);
            }
        }
        HirExprKind::Defer(b) | HirExprKind::Unsafe(b) => rewrite_block(b, map),
    }
}
