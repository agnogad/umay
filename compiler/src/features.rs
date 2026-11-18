//! Opt-in enforcement of crate-level `#![feature(...)]` gates.
//!
//! The parser records gate attributes and recognises unstable syntax on every
//! channel. This pass is the enforcement half: it rejects that syntax when the
//! matching gate is missing. `lib.rs` calls it only under the `v2-gates` Cargo
//! feature, so an untouched v1 program is unaffected by default.
//!
//! Gate mapping (all names from `docs/v2/feature-flags.md` §2):
//!
//! | Gate | Syntax detected here |
//! |------|----------------------|
//! | `closures` | `ExprKind::Closure` (the `|...|` syntax itself, not capture mode) |
//! | `traits` | `TraitDecl.methods[].body.is_some()` (default method bodies only) |
//! | `dyn_trait` | `TypeExpr::DynTrait`, `TypeExpr::ImplTrait` |
//! | `extern_abi` | `FnDecl::abi().is_some()` or `FnDecl::is_variadic()` |
//! | `async_await` | `FnDecl.is_async`, `ExprKind::Await` |
//! | `gui`, `mobile`, `proc_macro` | accepted, gate no syntax (reserved for later versions) |
//!
//! Deliberate limits: `traits` cannot see where-clauses or
//! associated items yet (the AST has no fields for them), and string
//! interpolation stores raw source rather than an `Expr`, so gated syntax
//! inside `"{...}"` is not walked. Module files loaded by `mod name;` are
//! checked only when their own crate root is analysed; the hook runs on the
//! parsed entry file before module expansion. `W0011` is documented but never
//! emitted: every known name is `unstable` or `planned, reserved`, so this
//! compiler has no stabilized gate to warn about.

use std::collections::HashSet;

use crate::ast::{
    Block, Expr, ExprKind, FnDecl, Item, LetStmt, MatchArm, Pattern, PatternKind, Program,
    Stmt, StmtKind, TypeExpr, VariantPatternKind,
};
use crate::diagnostics::{Diagnostic, Diagnostics};
use crate::source::Span;

/// Canonical, case-sensitive names from `feature-flags.md` §2.
const KNOWN: &[&str] = &[
    "closures", "traits", "dyn_trait", "extern_abi", "async_await", "gui", "mobile", "proc_macro",
];

/// Names accepted for forward compatibility but implementing no syntax yet.
const RESERVED: &[&str] = &["gui", "mobile", "proc_macro"];

/// Checks one parsed crate root and returns its gate diagnostics.
///
/// Public so the integration tests can drive the same pass the compiler uses;
/// the compiler itself reaches it through the `v2-gates` hooks in `lib.rs`.
pub fn check(program: &Program) -> Diagnostics {
    let mut diags = Diagnostics::new();
    let mut enabled: HashSet<&str> = HashSet::new();
    let mut seen: HashSet<&str> = HashSet::new();

    for attr in &program.features {
        let name = attr.name.as_str();
        if !KNOWN.contains(&name) {
            diags.push(
                Diagnostic::error(
                    "E0155",
                    format!("unknown feature gate `{name}`; expected one of {}", KNOWN.join(", ")),
                )
                .at(attr.name_span)
                .with_cause("feature names are exact and case-sensitive; removed names stay reserved"),
            );
            continue;
        }
        if !seen.insert(name) {
            diags.push(
                Diagnostic::error("E0155", format!("duplicate feature gate `{name}`"))
                    .at(attr.name_span)
                    .with_cause("a gate needs to appear once at the crate root"),
            );
            continue;
        }
        if RESERVED.contains(&name) {
            diags.push(
                Diagnostic::error(
                    "E0156",
                    format!("feature gate `{name}` is reserved but not implemented"),
                )
                .at(attr.name_span)
                .with_cause("the name is reserved for a future version and gates no syntax yet"),
            );
            continue;
        }
        enabled.insert(name);
    }

    let mut visitor = Visitor { diags: &mut diags, enabled: &enabled };
    visitor.program(program);
    diags
}

struct Visitor<'a> {
    diags: &'a mut Diagnostics,
    enabled: &'a HashSet<&'a str>,
}

impl Visitor<'_> {
    /// Reports one `E0154` per gated construct, at the construct's own span.
    fn need(&mut self, gate: &'static str, span: Span) {
        if self.enabled.contains(gate) {
            return;
        }
        self.diags.push(
            Diagnostic::error(
                "E0154",
                format!("`{gate}` syntax requires `#![feature({gate})]` at the crate root on the nightly channel"),
            )
            .at(span)
            .with_cause("this construct is unstable and is hidden behind a crate-level feature gate"),
        );
    }

    fn program(&mut self, program: &Program) {
        for item in &program.items {
            self.item(item);
        }
    }

    fn item(&mut self, item: &Item) {
        match item {
            Item::Fn(d) => self.fn_decl(d),
            Item::Struct(d) => {
                for f in &d.fields {
                    self.ty(&f.ty);
                }
            }
            Item::Enum(d) => {
                for v in &d.variants {
                    if let Some(fields) = &v.tuple_fields {
                        for ty in fields {
                            self.ty(ty);
                        }
                    }
                    if let Some(fields) = &v.struct_fields {
                        for f in fields {
                            self.ty(&f.ty);
                        }
                    }
                    if let Some(discriminant) = &v.discriminant {
                        self.expr(discriminant);
                    }
                }
            }
            Item::Trait(d) => {
                for m in &d.methods {
                    // The only `traits` syntax the AST can represent today is a
                    // default method body. Where-clauses and associated items
                    // are not in the AST yet (supertraits now are).
                    if m.body.is_some() {
                        self.need("traits", m.span);
                    }
                    self.fn_decl(m);
                }
            }
            Item::Impl(d) => {
                self.ty(&d.self_ty);
                for m in &d.methods {
                    self.fn_decl(m);
                }
            }
            Item::Const(d) => {
                if let Some(ty) = &d.ty {
                    self.ty(ty);
                }
                self.expr(&d.value);
            }
            Item::Use(_) => {}
            Item::Mod(d) => {
                if let Some(items) = &d.items {
                    for item in items {
                        self.item(item);
                    }
                }
            }
            Item::TypeAlias(d) => self.ty(&d.ty),
        }
    }

    fn fn_decl(&mut self, decl: &FnDecl) {
        if decl.is_async {
            self.need("async_await", decl.fn_span);
        }
        if decl.abi().is_some() || decl.is_variadic() {
            // v1 `extern fn "symbol"` has `abi() == None` and stays ungated.
            self.need("extern_abi", decl.extern_span.unwrap_or(decl.span));
        }
        for param in &decl.params {
            self.ty(&param.ty);
        }
        if let Some(ret) = &decl.ret {
            self.ty(ret);
        }
        if let Some(body) = &decl.body {
            self.block(body);
        }
    }

    fn block(&mut self, block: &Block) {
        for stmt in &block.stmts {
            self.stmt(stmt);
        }
        if let Some(tail) = &block.tail {
            self.expr(tail);
        }
    }

    fn stmt(&mut self, stmt: &Stmt) {
        match &stmt.kind {
            StmtKind::Let(let_stmt) => self.let_stmt(let_stmt),
            StmtKind::Expr(expr) => self.expr(expr),
            StmtKind::Item(item) => self.item(item),
        }
    }

    fn let_stmt(&mut self, stmt: &LetStmt) {
        self.pat(&stmt.pattern);
        if let Some(ty) = &stmt.ty {
            self.ty(ty);
        }
        if let Some(init) = &stmt.init {
            self.expr(init);
        }
        if let Some(block) = &stmt.else_block {
            self.block(block);
        }
    }

    fn ty(&mut self, ty: &TypeExpr) {
        match ty {
            TypeExpr::Path(_) | TypeExpr::Infer { .. } | TypeExpr::Never { .. } => {}
            TypeExpr::Ref { inner, .. } | TypeExpr::Ptr { inner, .. } => self.ty(inner),
            TypeExpr::Slice { elem, .. } => self.ty(elem),
            TypeExpr::Array { elem, len, .. } => {
                self.ty(elem);
                self.expr(len);
            }
            TypeExpr::Tuple { elems, .. } => {
                for elem in elems {
                    self.ty(elem);
                }
            }
            TypeExpr::Fn { params, ret, .. } => {
                for param in params {
                    self.ty(param);
                }
                if let Some(ret) = ret {
                    self.ty(ret);
                }
            }
            TypeExpr::ImplTrait { span, .. } | TypeExpr::DynTrait { span, .. } => {
                self.need("dyn_trait", *span)
            }
        }
    }

    fn expr(&mut self, expr: &Expr) {
        match &expr.kind {
            ExprKind::IntLit { .. }
            | ExprKind::FloatLit { .. }
            | ExprKind::StrLit(_)
            | ExprKind::StrInterp(_)
            | ExprKind::CharLit(_)
            | ExprKind::BoolLit(_)
            | ExprKind::Ident(_)
            | ExprKind::Path(_)
            | ExprKind::Continue
            | ExprKind::Infer => {}
            // Interpolation parts carry raw source text (`StrPart::Expr.src`),
            // not an `Expr`, so there is nothing to walk here.
            ExprKind::Unary { operand, .. }
            | ExprKind::Ref { operand, .. }
            | ExprKind::Try(operand)
            | ExprKind::Move(operand) => self.expr(operand),
            ExprKind::Binary { lhs, rhs, .. }
            | ExprKind::Assign { place: lhs, value: rhs }
            | ExprKind::AssignOp { place: lhs, value: rhs, .. }
            | ExprKind::Index { base: lhs, index: rhs } => {
                self.expr(lhs);
                self.expr(rhs);
            }
            ExprKind::Cast { expr, ty } => {
                self.expr(expr);
                self.ty(ty);
            }
            ExprKind::Sizeof { ty } => self.ty(ty),
            ExprKind::Call { callee, args, .. } => {
                self.expr(callee);
                for arg in args {
                    self.expr(arg);
                }
            }
            ExprKind::MethodCall { receiver, args, .. } => {
                self.expr(receiver);
                for arg in args {
                    self.expr(arg);
                }
            }
            ExprKind::FieldAccess { base, .. } => self.expr(base),
            ExprKind::Await { base, span } => {
                self.need("async_await", *span);
                self.expr(base);
            }
            ExprKind::StructLit { fields, base, .. } => {
                for field in fields {
                    if let Some(value) = &field.value {
                        self.expr(value);
                    }
                }
                if let Some(base) = base {
                    self.expr(base);
                }
            }
            ExprKind::ArrayLit { elems } | ExprKind::Tuple { elems } => {
                for elem in elems {
                    self.expr(elem);
                }
            }
            ExprKind::ArrayRepeat { value, count } => {
                self.expr(value);
                self.expr(count);
            }
            ExprKind::If { cond, then_block, else_branch } => {
                self.expr(cond);
                self.block(then_block);
                if let Some(else_branch) = else_branch {
                    self.expr(else_branch);
                }
            }
            ExprKind::Match { scrutinee, arms } => {
                self.expr(scrutinee);
                for arm in arms {
                    self.match_arm(arm);
                }
            }
            ExprKind::While { cond, body } => {
                self.expr(cond);
                self.block(body);
            }
            ExprKind::Loop { body } | ExprKind::Defer(body) | ExprKind::Unsafe(body) => {
                self.block(body)
            }
            ExprKind::For { pattern, iter, body } => {
                self.pat(pattern);
                self.expr(iter);
                self.block(body);
            }
            ExprKind::Block(block) => self.block(block),
            ExprKind::Return(value) | ExprKind::Break(value) => {
                if let Some(value) = value {
                    self.expr(value);
                }
            }
            ExprKind::Range { start, end, .. } => {
                if let Some(start) = start {
                    self.expr(start);
                }
                if let Some(end) = end {
                    self.expr(end);
                }
            }
            ExprKind::Closure { params, body, pipe_span } => {
                self.need("closures", *pipe_span);
                for param in params {
                    if let Some(ty) = &param.ty {
                        self.ty(ty);
                    }
                }
                self.expr(body);
            }
        }
    }

    fn match_arm(&mut self, arm: &MatchArm) {
        self.pat(&arm.pattern);
        if let Some(guard) = &arm.guard {
            self.expr(guard);
        }
        self.expr(&arm.body);
    }

    fn pat(&mut self, pat: &Pattern) {
        match &pat.kind {
            PatternKind::Binding { .. } => {}
            PatternKind::Literal(expr) => self.expr(expr),
            PatternKind::Tuple(elems) | PatternKind::Slice { elems, .. } => {
                for elem in elems {
                    self.pat(elem);
                }
            }
            PatternKind::Struct { fields, .. } => {
                for field in fields {
                    if let Some(pattern) = &field.pattern {
                        self.pat(pattern);
                    }
                }
            }
            PatternKind::Variant { kind, .. } => match kind {
                VariantPatternKind::Unit => {}
                VariantPatternKind::Tuple(elems) => {
                    for elem in elems {
                        self.pat(elem);
                    }
                }
                VariantPatternKind::Struct { fields, .. } => {
                    for field in fields {
                        if let Some(pattern) = &field.pattern {
                            self.pat(pattern);
                        }
                    }
                }
            },
            PatternKind::Or(elems) => {
                for elem in elems {
                    self.pat(elem);
                }
            }
            PatternKind::Range { start, end, .. } => {
                self.pat(start);
                self.pat(end);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_gate_names_are_documented_constants() {
        assert_eq!(KNOWN.len(), 8);
        assert!(KNOWN.contains(&"closures"));
        assert!(KNOWN.contains(&"proc_macro"));
        assert!(RESERVED.iter().all(|name| KNOWN.contains(name)));
    }
}
