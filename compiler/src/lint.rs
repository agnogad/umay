//! `umay lint`: style and correctness tips the compiler has no stake in.
//!
//! A warning the compiler emits is one it had to emit: the narrowing cast that
//! can truncate, the shadowed binding, the discarded branch. Those are part of
//! checking and run on every build.
//!
//! A lint is the other kind of advice -- the code compiles and does what it says,
//! but a reader would rather it said something else. `lint` is a separate command
//! because its findings are opinions, and a project should be able to adopt them
//! at its own pace rather than have them appear in every build.
//!
//! Every rule here works on the syntax tree and the source text, never on types,
//! so a lint can be explained in one sentence and fixed without understanding the
//! program. A rule that needed type information would belong in the checker.

use crate::ast;
use crate::diagnostics::{Diagnostic, Diagnostics};
use crate::source::{SourceMap, Span};

/// One lint finding.
pub struct Finding {
    pub diagnostic: Diagnostic,
}

/// A style rule, named so that a project can talk about it.
pub struct Rule {
    pub code: &'static str,
    pub name: &'static str,
    pub summary: &'static str,
}

/// Every rule this pass implements, in reporting order.
pub const RULES: &[Rule] = &[
    Rule {
        code: "L0101",
        name: "trailing-whitespace",
        summary: "a line ends in spaces or tabs",
    },
    Rule {
        code: "L0102",
        name: "todo-marker",
        summary: "a comment marks unfinished work",
    },
    Rule {
        code: "L0103",
        name: "empty-block",
        summary: "a function body is empty where the signature promises a value",
    },
    Rule {
        code: "L0104",
        name: "redundant-else-return",
        summary: "an `else` follows a branch that already returned",
    },
    Rule {
        code: "L0105",
        name: "unused-parameter",
        summary: "a parameter is never mentioned in the body",
    },
];

/// Looks a rule up by its code.
pub fn rule(code: &str) -> Option<&'static Rule> {
    RULES.iter().find(|r| r.code.eq_ignore_ascii_case(code))
}

/// Runs every lint over one file's text and syntax tree.
///
/// The text is needed as well as the tree because two of the rules are about the
/// text itself -- trailing whitespace and marker comments -- and neither is
/// visible once the lexer has dropped the whitespace and the comments.
pub fn lint(map: &SourceMap, id: crate::source::FileId, program: &ast::Program) -> Diagnostics {
    let mut out = Diagnostics::new();
    let text = map.get(id).map(|f| f.src()).unwrap_or("");

    whitespace(text, map, id, &mut out);
    markers(text, map, id, &mut out);
    for item in &program.items {
        item_lints(item, map, &mut out);
    }
    out.sort();
    out
}

/// L0101: a line ends in spaces or tabs.
fn whitespace(text: &str, map: &SourceMap, id: crate::source::FileId, out: &mut Diagnostics) {
    for (i, line) in text.lines().enumerate() {
        let trimmed = line.trim_end();
        if trimmed.len() == line.len() {
            continue;
        }
        let start = line_offset(text, i) + trimmed.len();
        let span = Span::new(id, start as u32, (start + (line.len() - trimmed.len())) as u32);
        out.push(
            Diagnostic::warning("L0101", "trailing whitespace")
                .at(span)
                .with_cause("the line ends in spaces or tabs")
                .with_explanation(
                    "Trailing whitespace shows up in every future diff of the line and in \
                     nothing else. `umay fmt` removes it.",
                )
                .with_suggestion(crate::diagnostics::Suggestion::new(
                    "remove the trailing whitespace",
                    span,
                    trimmed,
                )),
        );
        let _ = map;
    }
}

/// Byte offset where line `n` (0-based) starts.
///
/// Line 0 starts at byte 0; line `n` starts just after the `n`-th newline.
fn line_offset(text: &str, n: usize) -> usize {
    if n == 0 {
        return 0;
    }
    text.match_indices('\n')
        .nth(n - 1)
        .map(|(i, _)| i + 1)
        .unwrap_or(text.len())
}

/// L0102: a comment marks unfinished work.
fn markers(text: &str, map: &SourceMap, id: crate::source::FileId, out: &mut Diagnostics) {
    // The words a reader scans for to find what is not done. Matched
    // case-sensitively and only in upper case, which is the convention for a
    // marker: prose that happens to use the word "todo" is not a marker.
    const MARKERS: &[&str] = &["TODO", "FIXME", "XXX", "HACK"];
    for (i, line) in text.lines().enumerate() {
        let Some(comment_at) = line.find("//") else { continue };
        let comment = &line[comment_at..];
        let Some(marker) = MARKERS.iter().find(|m| comment.contains(*m)) else {
            continue;
        };
        let start = line_offset(text, i) + comment_at;
        let span = Span::new(id, start as u32, (start + comment.len()) as u32);
        out.push(
            Diagnostic::warning("L0102", format!("unfinished work marked `{marker}`"))
                .at(span)
                .with_cause("the comment marks work left for later")
                .with_explanation(
                    "A marker in the source is a note to whoever reads the file next. Naming \
                     it here makes the list visible without reading every comment.",
                ),
        );
        let _ = map;
    }
}

/// Lints that need the syntax tree.
fn item_lints(item: &ast::Item, map: &SourceMap, out: &mut Diagnostics) {
    match item {
        ast::Item::Fn(f) => fn_lints(f, map, out),
        ast::Item::Impl(block) => {
            for f in &block.methods {
                fn_lints(f, map, out);
            }
        }
        ast::Item::Trait(t) => {
            for f in &t.methods {
                fn_lints(f, map, out);
            }
        }
        _ => {}
    }
}

fn fn_lints(f: &ast::FnDecl, map: &SourceMap, out: &mut Diagnostics) {
    // A body that is absent or empty is an `extern` declaration or a trait
    // method, and neither promises anything.
    let Some(body) = &f.body else { return };

    // L0103: a function whose signature returns a value and whose body is empty
    // cannot produce one. A block with a trailing expression is not empty: that
    // expression is the value, so `{ return x }` and `{ x }` both count.
    if body.stmts.is_empty() && body.tail.is_none() && f.ret.is_some() && !returns_unit(f) {
        out.push(
            Diagnostic::warning("L0103", format!("`{}` has an empty body but returns a value", f.name))
                .at(f.ret.as_ref().map(|t| t.span()).unwrap_or(f.name_span))
                .with_cause("the signature promises a value and the body produces none")
                .with_explanation(
                    "An empty body is a valid block, so this compiles when the return type \
                     allows it. If the function is not written yet, `unimplemented!` is clearer \
                     than an empty body that happens to typecheck.",
                ),
        );
    }

    // L0104: `if c { return a } else { b }`. The `else` is only reachable when
    // the branch did not return, so it reads better without the `else`.
    redundant_else(body, map, out);

    // L0105: a parameter the body never mentions. A leading underscore is the
    // usual way to say "kept for the signature", so it silences the rule.
    unused_params(f, body, out);
}

/// True when the function's return type is `()` or absent.
fn returns_unit(f: &ast::FnDecl) -> bool {
    match &f.ret {
        None => true,
        Some(ty) => match ty {
            ast::TypeExpr::Tuple { elems, .. } => elems.is_empty(),
            _ => false,
        },
    }
}

fn redundant_else(block: &ast::Block, map: &SourceMap, out: &mut Diagnostics) {
    // A body's last expression is not one of its statements, and an `if` is
    // normally written there, so both have to be looked at.
    for e in block.stmts.iter().filter_map(|s| match &s.kind {
        ast::StmtKind::Expr(e) => Some(e),
        _ => None,
    }).chain(block.tail.iter().map(|b| &**b)) {
        let ast::ExprKind::If { then_block, else_branch, .. } = &e.kind else {
            continue;
        };
        let Some(else_branch) = else_branch else { continue };
        // Only when the then-block is guaranteed to leave: an `else` after a
        // branch that *may* fall through is doing real work.
        if !always_diverges(then_block) {
            continue;
        }
        out.push(
            Diagnostic::warning("L0104", "`else` after a branch that always returns")
                .at(else_branch.span)
                .with_cause("the `then` branch returns, so the `else` is only reached by falling through")
                .with_explanation(
                    "Writing the second branch after the `if` takes one level of nesting out \
                     of the function body, which is what makes a long function readable.",
                ),
        );
        let _ = map;
    }
}

/// True when a block cannot reach its end.
///
/// `return` is an expression, so it can be either the last statement or the
/// block's trailing expression; both leave the block.
fn always_diverges(block: &ast::Block) -> bool {
    fn leaves(e: &ast::Expr) -> bool {
        matches!(
            e.kind,
            ast::ExprKind::Return(_) | ast::ExprKind::Break(_) | ast::ExprKind::Continue
        )
    }
    if let Some(tail) = &block.tail {
        if leaves(tail) {
            return true;
        }
    }
    match block.stmts.last().map(|s| &s.kind) {
        Some(ast::StmtKind::Expr(e)) => leaves(e),
        _ => false,
    }
}

fn unused_params(f: &ast::FnDecl, body: &ast::Block, out: &mut Diagnostics) {
    // Identifiers the body mentions, collected once rather than per parameter.
    let mut used = std::collections::HashSet::new();
    for stmt in &body.stmts {
        collect_idents_stmt(stmt, &mut used);
    }
    if let Some(tail) = &body.tail {
        collect_idents_expr(tail, &mut used);
    }
    for param in &f.params {
        let Some(name) = &param.name else { continue };
        if name.starts_with('_') || name == "self" {
            continue;
        }
        if used.contains(name.as_str()) {
            continue;
        }
        out.push(
            Diagnostic::warning(
                "L0105",
                format!("parameter `{name}` is never used"),
            )
            .at(param.name_span)
            .with_cause("the body never mentions this parameter")
            .with_explanation(
                "A parameter the body ignores is either a leftover from an earlier shape of \
                 the function or a bug. If it is kept on purpose, a leading underscore says so \
                 and silences this rule.",
            )
            .with_suggestion(crate::diagnostics::Suggestion::new(
                "rename it to show it is unused",
                param.name_span,
                format!("_{name}"),
            )),
        );
    }
}

/// A pattern's bindings are names too: `match x { Some(n) => n }` uses `n` after
/// binding it, and a parameter named `n` would otherwise look unused.
fn collect_pattern_idents(p: &ast::Pattern, used: &mut std::collections::HashSet<String>) {
    use ast::PatternKind as P;
    match &p.kind {
        P::Binding { name, .. } if name != "_" => {
            used.insert(name.clone());
        }
        P::Literal(e) => collect_idents_expr(e, used),
        P::Tuple(elems) => {
            for e in elems {
                collect_pattern_idents(e, used);
            }
        }
        P::Struct { fields, .. } => {
            for f in fields {
                match &f.pattern {
                    Some(sub) => collect_pattern_idents(sub, used),
                    // `User { id }` binds `id` by its own name.
                    None => {
                        used.insert(f.name.clone());
                    }
                }
            }
        }
        P::Variant { kind, .. } => match kind {
            ast::VariantPatternKind::Unit => {}
            ast::VariantPatternKind::Tuple(subs) => {
                for sub in subs {
                    collect_pattern_idents(sub, used);
                }
            }
            ast::VariantPatternKind::Struct { fields, .. } => {
                for f in fields {
                    match &f.pattern {
                        Some(sub) => collect_pattern_idents(sub, used),
                        None => {
                            used.insert(f.name.clone());
                        }
                    }
                }
            }
        },
        P::Or(subs) => {
            for sub in subs {
                collect_pattern_idents(sub, used);
            }
        }
        P::Range { start, end, .. } => {
            collect_pattern_idents(start, used);
            collect_pattern_idents(end, used);
        }
        _ => {}
    }
}

fn collect_idents_stmt(stmt: &ast::Stmt, used: &mut std::collections::HashSet<String>) {
    match &stmt.kind {
        ast::StmtKind::Expr(e) => collect_idents_expr(e, used),
        ast::StmtKind::Let(l) => {
            if let Some(init) = &l.init {
                collect_idents_expr(init, used);
            }
            collect_pattern_idents(&l.pattern, used);
        }
        ast::StmtKind::Item(_) => {}
    }
}

fn collect_idents_expr(e: &ast::Expr, used: &mut std::collections::HashSet<String>) {
    match &e.kind {
        ast::ExprKind::Ident(name) => {
            used.insert(name.clone());
        }
        ast::ExprKind::Path(p) => {
            used.insert(p.segments.first().map(|s| s.name.clone()).unwrap_or_default());
        }
        ast::ExprKind::Unary { operand, .. } => collect_idents_expr(operand, used),
        ast::ExprKind::Binary { lhs, rhs, .. } => {
            collect_idents_expr(lhs, used);
            collect_idents_expr(rhs, used);
        }
        ast::ExprKind::Ref { operand, .. } => collect_idents_expr(operand, used),
        ast::ExprKind::Cast { expr, .. } => collect_idents_expr(expr, used),
        ast::ExprKind::Assign { place, value } => {
            collect_idents_expr(place, used);
            collect_idents_expr(value, used);
        }
        ast::ExprKind::AssignOp { place, value, .. } => {
            collect_idents_expr(place, used);
            collect_idents_expr(value, used);
        }
        ast::ExprKind::Call { callee, args, .. } => {
            collect_idents_expr(callee, used);
            for a in args {
                collect_idents_expr(a, used);
            }
        }
        ast::ExprKind::MethodCall { receiver, args, .. } => {
            collect_idents_expr(receiver, used);
            for a in args {
                collect_idents_expr(a, used);
            }
        }
        ast::ExprKind::FieldAccess { base, .. } => collect_idents_expr(base, used),
        ast::ExprKind::Index { base, index } => {
            collect_idents_expr(base, used);
            collect_idents_expr(index, used);
        }
        ast::ExprKind::StructLit { fields, base, .. } => {
            for f in fields {
                match &f.value {
                    Some(v) => collect_idents_expr(v, used),
                    // `Bytes { data, len }` is shorthand for `data: data`, so the
                    // field's own name is the identifier being used.
                    None => {
                        used.insert(f.name.clone());
                    }
                }
            }
            if let Some(b) = base {
                collect_idents_expr(b, used);
            }
        }
        ast::ExprKind::ArrayLit { elems } => {
            for x in elems {
                collect_idents_expr(x, used);
            }
        }
        ast::ExprKind::ArrayRepeat { value, count } => {
            collect_idents_expr(value, used);
            collect_idents_expr(count, used);
        }
        ast::ExprKind::Tuple { elems } => {
            for x in elems {
                collect_idents_expr(x, used);
            }
        }
        ast::ExprKind::If { cond, then_block, else_branch } => {
            collect_idents_expr(cond, used);
            for s in &then_block.stmts {
                collect_idents_stmt(s, used);
            }
            if let Some(tail) = &then_block.tail {
                collect_idents_expr(tail, used);
            }
            if let Some(e) = else_branch {
                collect_idents_expr(e, used);
            }
        }
        // A block's last expression is its value, and is not one of its
        // statements, so both have to be walked: `unsafe { write_byte(p) }` puts
        // the call in the tail.
        ast::ExprKind::Match { scrutinee, arms } => {
            collect_idents_expr(scrutinee, used);
            for arm in arms {
                if let Some(g) = &arm.guard {
                    collect_idents_expr(g, used);
                }
                collect_idents_expr(&arm.body, used);
                collect_pattern_idents(&arm.pattern, used);
            }
        }
        ast::ExprKind::While { cond, body } => {
            collect_idents_expr(cond, used);
            for s in &body.stmts {
                collect_idents_stmt(s, used);
            }
            if let Some(tail) = &body.tail {
                collect_idents_expr(tail, used);
            }
        }
        ast::ExprKind::For { pattern, iter, body } => {
            collect_idents_expr(iter, used);
            collect_pattern_idents(pattern, used);
            for s in &body.stmts {
                collect_idents_stmt(s, used);
            }
            if let Some(tail) = &body.tail {
                collect_idents_expr(tail, used);
            }
        }
        ast::ExprKind::Loop { body } => {
            for s in &body.stmts {
                collect_idents_stmt(s, used);
            }
            if let Some(tail) = &body.tail {
                collect_idents_expr(tail, used);
            }
        }
        ast::ExprKind::Return(v) | ast::ExprKind::Break(v) => {
            if let Some(v) = v {
                collect_idents_expr(v, used);
            }
        }
        ast::ExprKind::Try(inner) | ast::ExprKind::Move(inner) => collect_idents_expr(inner, used),
        ast::ExprKind::Defer(b) => {
            for s in &b.stmts {
                collect_idents_stmt(s, used);
            }
        }
        ast::ExprKind::Block(b) | ast::ExprKind::Unsafe(b) => {
            for s in &b.stmts {
                collect_idents_stmt(s, used);
            }
            if let Some(tail) = &b.tail {
                collect_idents_expr(tail, used);
            }
        }
        _ => {}
    }
}

/// The rule list, for `umay lint --rules` and the documentation. Kept here so a
/// new rule cannot be added without a line in the table.
pub fn describe_rules() -> String {
    let mut out = String::new();
    for rule in RULES {
        out.push_str(&format!("{:<6} {:<22} {}\n", rule.code, rule.name, rule.summary));
    }
    out
}

#[cfg(test)]
mod tests {
    fn findings(src: &str) -> Vec<String> {
        let (diags, _map) = crate::lint_source("t.um", src);
        let mut codes: Vec<String> = diags
            .items()
            .iter()
            .map(|d| d.code.to_string())
            .collect();
        codes.sort();
        codes
    }

    #[test]
    fn each_rule_fires_on_its_own_case() {
        assert!(findings("fn f() -> i64 {\n}\n").contains(&"L0103".to_string()));
        assert!(findings("// TODO later\nfn f() -> i64 {\n    return 1i64\n}\n").contains(&"L0102".to_string()));
        assert!(findings("fn f(a: i64) -> i64 {\n    return 0i64\n}\n").contains(&"L0105".to_string()));
        assert!(findings("fn f() -> i64 {\n    return 1i64   \n}\n").contains(&"L0101".to_string()));
        assert!(findings(
            "fn f(c: bool) -> i64 {\n    if c {\n        return 1i64\n    } else {\n        return 2i64\n    }\n}\n"
        )
        .contains(&"L0104".to_string()));
    }
}
