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
    /// The `impl Trait for Type` blocks written for this trait.
    ///
    /// A marker trait like `Send` has no methods, so nothing else records that a
    /// type carries it: a call that demands the marker has to read this list.
    /// The arguments are the parameter names of a generic `impl`, empty for the
    /// common case, and `bounded_all` says whether every parameter must itself
    /// carry the marker (`impl<T: Send> Send for Vec<T>`).
    pub impls: Vec<TraitImplInfo>,
    /// The trait's own parameter names, in declaration order, so that a bound's
    /// arguments can be paired with the types a method mentions.
    pub generics: Vec<String>,
    pub methods: Vec<TraitMethodInfo>,
}

/// One `impl Trait for Type` block.
#[derive(Debug, Clone)]
pub struct TraitImplInfo {
    /// The implemented type's name, as written.
    pub type_name: String,
    /// The parameter names the `impl` block declared, in order.
    pub generics: Vec<String>,
    /// True when every one of those parameters carries the same trait.
    pub bounded_all: bool,
    pub span: Span,
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
    /// True for a `fn f(fixed..., ...) -> R` declared inside `extern "C"`.
    /// Trailing variadic arguments have no declared type; sema rejects the
    /// floating-point ones with E0157 while `%al` is emitted as 0.
    pub variadic: bool,
    /// Whether this is a method (`self` receiver present).
    pub has_self: bool,
    pub self_mutable: bool,
    pub self_by_ref: bool,
    /// Local table: index 0..params.len() are parameters in order.
    pub locals: Vec<LocalInfo>,
    pub body: Option<HirBlock>,
    pub is_pub: bool,
    /// Whether the declaration was written `async fn`.
    pub is_async: bool,
    /// The type a method belongs to, when it was declared in an `impl` block.
    ///
    /// Two types may each have a `read` method and a C library may export `read`
    /// itself, so a method's symbol is qualified with this name. Without it a
    /// method would take over a C function of the same name at link time.
    pub owner: Option<String>,
    /// The linker symbol for an `extern fn "symbol" name(..)`.
    ///
    /// A declaration may reach a C function without taking its name, which is what
    /// lets the prelude call `read` while a program is free to define its own.
    pub link_name: Option<String>,
    /// The type a `let` binding was declared with, when it had an annotation.
    ///
    /// Keyed by local index. A `let x: Vec<i64> = ...` names the instance an
    /// initialiser could not infer on its own, and the annotation is the only
    /// record of it once checking is done: the local's own type is whatever the
    /// initialiser answered, which for a generic call is the template.
    pub declared_local_tys: std::collections::HashMap<usize, TyId>,
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
    /// `init` is the evaluated initializer. `pattern` is what the bindings came
    /// from, so lowering can give each one its own projection instead of handing
    /// the whole value to the first.
    Let {
        bindings: Vec<LocalId>,
        init: Option<Box<HirExpr>>,
        pattern: HirPattern,
    },
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
    /// A library function named by its symbol, for a construct that lowers to a
    /// call but has no declaration to point at.
    NamedFn(String),
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
    /// `expr.await` — lowered to a poll loop in a later phase.
    Await { base: Box<HirExpr> },
    /// `expr as Ty`
    /// `sizeof(Type)` — the size of a type in bytes, known at compile time.
    Sizeof { of: TyId },
    Cast { expr: Box<HirExpr>, to: TyId },
    /// `ptr.offset(n)` — moves a raw pointer by `n` elements of its pointee.
    ///
    /// This is an intrinsic rather than a method on a type, because a raw pointer
    /// has no type of its own to hang methods on, and because the scaling by the
    /// pointee's size is a machine concern rather than a library one.
    PtrOffset { ptr: Box<HirExpr>, count: Box<HirExpr> },

    // ---- calls & projections ----
    Call { callee: Box<HirExpr>, args: Vec<HirExpr> },
    /// The address of a compiler-generated vtable static. A three-word prefix
    /// (`size`, `align`, `drop`) precedes the method shims, each of which uses
    /// the uniform `(data: *mut u8, args...) -> ret` ABI.
    VTable { size: u64, align: u64, drop: Option<FnId>, entries: Vec<FnId> },
    /// A `dyn Trait` fat pointer: the data pointer plus the vtable address.
    /// Lowers to a two-element aggregate so no backend needs a new place kind.
    DynCast { value: Box<HirExpr>, vtable: Box<HirExpr> },
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
    /// A call to a trait method on a generic parameter, e.g. `it.next()` where
    /// `T: Counter`.
    ///
    /// The receiver's type is a parameter, so no `impl` block names it yet; only
    /// the bound does. Monomorphisation knows the real type and rewrites this
    /// into an ordinary `MethodCall`, which keeps dispatch static and means no
    /// virtual table exists anywhere in the language.
    TraitMethodCall {
        receiver: Box<HirExpr>,
        trait_name: String,
        method: String,
        method_span: crate::source::Span,
        args: Vec<HirExpr>,
    },

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

/// Rewrites the types a pattern mentions, so a binding stays in step with the
/// value it came from after a substitution.
fn map_pattern_tys(pattern: &mut HirPattern, map: &std::collections::HashMap<TyId, TyId>) {
    map_ty(&mut pattern.ty, map);
    match &mut pattern.kind {
        HirPatternKind::Tuple(items) | HirPatternKind::Or(items) => {
            for p in items {
                map_pattern_tys(p, map);
            }
        }
        HirPatternKind::Struct { fields, .. } => {
            for f in fields {
                map_pattern_tys(&mut f.pattern, map);
            }
        }
        HirPatternKind::Variant { fields, .. } => match fields {
            VariantPatternFields::Tuple(items) => {
                for p in items {
                    map_pattern_tys(p, map);
                }
            }
            VariantPatternFields::Struct { fields, .. } => {
                for f in fields {
                    map_pattern_tys(&mut f.pattern, map);
                }
            }
            VariantPatternFields::Unit => {}
        },
        HirPatternKind::Slice { elems, .. } => {
            for p in elems {
                map_pattern_tys(p, map);
            }
        }
        _ => {}
    }
}

fn map_ty(ty: &mut TyId, map: &std::collections::HashMap<TyId, TyId>) {
    if let Some(t) = map.get(ty) {
        *ty = *t;
    }
}

fn rewrite_block(block: &mut HirBlock, map: &std::collections::HashMap<TyId, TyId>) {
    for stmt in &mut block.stmts {
        match &mut stmt.kind {
            HirStmtKind::Let { init, pattern, .. } => {
                if let Some(i) = init {
                    rewrite_expr(i, map);
                }
                map_pattern_tys(pattern, map);
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
        HirExprKind::Sizeof { of } => map_ty(of, map),
        HirExprKind::IntLit { .. }
        | HirExprKind::FloatLit { .. }
        | HirExprKind::StrLit(_)
        | HirExprKind::CharLit(_)
        | HirExprKind::BoolLit(_)
        | HirExprKind::Local(_)
        | HirExprKind::FnRef(_)
        | HirExprKind::NamedFn(_)
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
        HirExprKind::Await { base } => rewrite_expr(base, map),
        HirExprKind::Binary { lhs, rhs, .. }
        | HirExprKind::Assign { place: lhs, value: rhs }
        | HirExprKind::AssignOp { place: lhs, value: rhs, .. } => {
            rewrite_expr(lhs, map);
            rewrite_expr(rhs, map);
        }
        HirExprKind::Ref { place, .. } => rewrite_expr(place, map),
        HirExprKind::TraitMethodCall { receiver, args, .. } => {
            rewrite_expr(receiver, map);
            for a in args {
                rewrite_expr(a, map);
            }
        }
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
        HirExprKind::VTable { .. } => {}
        HirExprKind::DynCast { value, vtable } => {
            rewrite_expr(value, map);
            rewrite_expr(vtable, map);
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

/// Every expression directly inside `e`, in source order.
///
/// A pass that needs to look through an expression without re-implementing the
/// shape of all of them walks this. Blocks are descended into, because a `break`
/// or a call is usually a statement or a tail rather than a bare operand.
pub fn child_exprs_mut(e: &mut HirExpr) -> Vec<&mut HirExpr> {
    let mut out: Vec<&mut HirExpr> = Vec::new();
    fn push_block<'a>(b: &'a mut HirBlock, out: &mut Vec<&'a mut HirExpr>) {
        for st in b.stmts.iter_mut() {
            match &mut st.kind {
                HirStmtKind::Let { init, .. } => {
                    if let Some(i) = init {
                        out.push(i);
                    }
                }
                HirStmtKind::Expr(x) => out.push(x),
            }
        }
        if let Some(t) = b.tail.as_mut() {
            out.push(t);
        }
    }
    match &mut e.kind {
        HirExprKind::Unary { operand, .. } => out.push(operand),
        HirExprKind::Binary { lhs, rhs, .. } => {
            out.push(lhs);
            out.push(rhs);
        }
        HirExprKind::Assign { place, value } | HirExprKind::AssignOp { place, value, .. } => {
            out.push(place);
            out.push(value);
        }
        HirExprKind::Ref { place, .. } => out.push(place),
        HirExprKind::Deref(inner) | HirExprKind::Move(inner) | HirExprKind::Try(inner) => {
            out.push(inner)
        }
        HirExprKind::Await { base } => out.push(base),
        HirExprKind::Cast { expr, .. } => out.push(expr),
        HirExprKind::PtrOffset { ptr, count } => {
            out.push(ptr);
            out.push(count);
        }
        HirExprKind::Call { callee, args } => {
            out.push(callee);
            for a in args.iter_mut() {
                out.push(a);
            }
        }
        HirExprKind::DynCast { value, vtable } => {
            out.push(value);
            out.push(vtable);
        }
        HirExprKind::MethodCall { receiver, args, .. }
        | HirExprKind::TraitMethodCall { receiver, args, .. } => {
            out.push(receiver);
            for a in args.iter_mut() {
                out.push(a);
            }
        }
        HirExprKind::BuiltinMethod { receiver, args, .. } => {
            out.push(receiver);
            for a in args.iter_mut() {
                out.push(a);
            }
        }
        HirExprKind::StructLit { fields, base, .. } => {
            for f in fields.iter_mut() {
                out.push(&mut f.value);
            }
            if let Some(base) = base {
                out.push(base);
            }
        }
        HirExprKind::TupleVariant { args, .. } => {
            for a in args.iter_mut() {
                out.push(a);
            }
        }
        HirExprKind::StructVariant { fields, .. } => {
            for f in fields.iter_mut() {
                out.push(&mut f.value);
            }
        }
        HirExprKind::ArrayLit { elems } | HirExprKind::Tuple { elems } => {
            for x in elems.iter_mut() {
                out.push(x);
            }
        }
        HirExprKind::ArrayRepeat { value, .. } => out.push(value),
        HirExprKind::RangeExpr { start, end, .. } => {
            out.push(start);
            out.push(end);
        }
        HirExprKind::IterSource { source, .. } => out.push(source),
        HirExprKind::Field { base, .. } | HirExprKind::TupleIndex { base, .. } => out.push(base),
        HirExprKind::Index { base, index } => {
            out.push(base);
            out.push(index);
        }
        HirExprKind::If { cond, then_block, else_branch } => {
            out.push(cond);
            push_block(then_block, &mut out);
            if let Some(e) = else_branch {
                out.push(e);
            }
        }
        HirExprKind::Match { scrutinee, arms } => {
            out.push(scrutinee);
            for arm in arms.iter_mut() {
                if let Some(g) = arm.guard.as_mut() {
                    out.push(g);
                }
                out.push(&mut arm.body);
            }
        }
        HirExprKind::While { cond, body } => {
            out.push(cond);
            push_block(body, &mut out);
        }
        HirExprKind::Loop { body } | HirExprKind::Defer(body) | HirExprKind::Unsafe(body) => {
            push_block(body, &mut out)
        }
        HirExprKind::For { iter, body, .. } => {
            out.push(iter);
            push_block(body, &mut out);
        }
        HirExprKind::Block(b) => push_block(b, &mut out),
        HirExprKind::Return(v) | HirExprKind::Break(v) => {
            if let Some(v) = v {
                out.push(v);
            }
        }
        HirExprKind::StrInterp { parts } => {
            for p in parts.iter_mut() {
                if let HirStrPart::Value(v) = p {
                    out.push(v);
                }
            }
        }
        _ => {}
    }
    out
}

pub fn child_exprs(e: &HirExpr) -> Vec<&HirExpr> {
    let mut out: Vec<&HirExpr> = Vec::new();
    fn push_block<'a>(b: &'a HirBlock, out: &mut Vec<&'a HirExpr>) {
        for st in &b.stmts {
            match &st.kind {
                HirStmtKind::Let { init, .. } => {
                    if let Some(i) = init {
                        out.push(i);
                    }
                }
                HirStmtKind::Expr(x) => out.push(x),
            }
        }
        if let Some(t) = &b.tail {
            out.push(t);
        }
    }
    match &e.kind {
        HirExprKind::Field { base, .. }
        | HirExprKind::TupleIndex { base, .. }
        | HirExprKind::Deref(base)
        | HirExprKind::Move(base)
        | HirExprKind::Cast { expr: base, .. }
        | HirExprKind::Try(base)
        | HirExprKind::Await { base } => out.push(base),
        HirExprKind::Unary { operand, .. } => out.push(operand),
        HirExprKind::Assign { place, value } => {
            out.push(place);
            out.push(value);
        }
        HirExprKind::AssignOp { place, value, .. } => {
            out.push(place);
            out.push(value);
        }
        HirExprKind::Ref { place, .. } => out.push(place),
        HirExprKind::Index { base, index } => {
            out.push(base);
            out.push(index);
        }
        HirExprKind::Binary { lhs, rhs, .. } => {
            out.push(lhs);
            out.push(rhs);
        }
        HirExprKind::MethodCall { receiver, args, .. }
        | HirExprKind::BuiltinMethod { receiver, args, .. }
        | HirExprKind::TraitMethodCall { receiver, args, .. } => {
            out.push(receiver);
            out.extend(args.iter());
        }
        HirExprKind::Call { callee, args } => {
            out.push(callee);
            out.extend(args.iter());
        }
        HirExprKind::DynCast { value, vtable } => {
            out.push(value);
            out.push(vtable);
        }
        HirExprKind::StructLit { fields, base, .. } => {
            for f in fields {
                out.push(&f.value);
            }
            if let Some(b) = base {
                out.push(b);
            }
        }
        HirExprKind::StructVariant { fields, .. } => {
            for f in fields {
                out.push(&f.value);
            }
        }
        HirExprKind::TupleVariant { args, .. }
        | HirExprKind::ArrayLit { elems: args }
        | HirExprKind::Tuple { elems: args } => out.extend(args.iter()),
        HirExprKind::ArrayRepeat { value, .. } => out.push(value),
        HirExprKind::RangeExpr { start, end, .. } => {
            out.push(start);
            out.push(end);
        }
        HirExprKind::IterSource { source, .. } => out.push(source),
        HirExprKind::PtrOffset { ptr, count } => {
            out.push(ptr);
            out.push(count);
        }
        HirExprKind::If { cond, then_block, else_branch } => {
            out.push(cond);
            push_block(then_block, &mut out);
            if let Some(b) = else_branch {
                out.push(b);
            }
        }
        HirExprKind::Match { scrutinee, arms } => {
            out.push(scrutinee);
            for arm in arms {
                out.push(&arm.body);
            }
        }
        HirExprKind::While { cond, body } => {
            out.push(cond);
            push_block(body, &mut out);
        }
        HirExprKind::Loop { body } | HirExprKind::Block(body) | HirExprKind::Defer(body) | HirExprKind::Unsafe(body) => {
            push_block(body, &mut out);
        }
        HirExprKind::For { iter, body, .. } => {
            out.push(iter);
            push_block(body, &mut out);
        }
        HirExprKind::Return(Some(v)) | HirExprKind::Break(Some(v)) => out.push(v),
        HirExprKind::StrInterp { parts } => {
            for p in parts {
                if let HirStrPart::Value(v) = p {
                    out.push(v);
                }
            }
        }
        _ => {}
    }
    out
}
