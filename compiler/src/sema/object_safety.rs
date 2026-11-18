//! Object-safety checking for `dyn Trait` bounds (ADR 0018 §2).
//!
//! A `dyn` bound is valid only when each named trait can be dispatched through
//! a vtable. v2 keeps this deliberately small: one principal trait per object,
//! no generic methods, no `Self` returns, no associated items (GATs are out of
//! scope per ADR 0018 §6). The checker reports and moves on so one bad bound
//! does not hide others.
use std::collections::HashMap;

use crate::ast;
use crate::diagnostics::{Diagnostic, Diagnostics};
use crate::source::Span;

/// Marker traits that do not add vtable methods and so do not count toward the
/// one-principal-trait limit.
// ponytail: hard-coded list; replace with a real `auto trait` declaration if
// auto traits ever become first-class.
const AUTO_TRAITS: &[&str] = &["Send", "Sync"];

fn is_auto_trait(name: &str) -> bool {
    AUTO_TRAITS.contains(&name)
}

/// The name of a path's final segment: `Iterator` from `core::Iterator<i64>`.
fn path_name(path: &ast::Path) -> &str {
    path.segments.last().map(|s| s.name.as_str()).unwrap_or("")
}

/// True when a type expression mentions `Self` anywhere in its structure.
fn mentions_self(ty: &ast::TypeExpr) -> bool {
    match ty {
        ast::TypeExpr::Path(p) => path_name(p) == "Self",
        ast::TypeExpr::Ref { inner, .. } | ast::TypeExpr::Ptr { inner, .. } => mentions_self(inner),
        ast::TypeExpr::Slice { elem, .. } | ast::TypeExpr::Array { elem, .. } => mentions_self(elem),
        ast::TypeExpr::Tuple { elems, .. } => elems.iter().any(mentions_self),
        ast::TypeExpr::Fn { params, ret, .. } => {
            params.iter().any(mentions_self)
                || ret.as_ref().is_some_and(|r| mentions_self(r))
        }
        ast::TypeExpr::ImplTrait { bounds, .. } | ast::TypeExpr::DynTrait { bounds, .. } => {
            bounds.iter().any(|b| path_name(b) == "Self")
        }
        ast::TypeExpr::Infer { .. } | ast::TypeExpr::Never { .. } => false,
    }
}

fn violation(diags: &mut Diagnostics, span: Span, method: &str, rule: &str) {
    diags.push(
        Diagnostic::error("E1023", format!("method `{method}` is not object-safe: {rule}"))
            .at(span)
            .with_cause("a `dyn` vtable can only hold methods with a fixed signature")
            .with_explanation(
                "A trait object calls its methods through a vtable: one function pointer per \
                 method. A generic method would need a separate slot for every instantiation, \
                 and a method returning `Self` would need to name the (unknown) concrete type. \
                 Both are rejected. Remove the method's type parameters or return a boxed value \
                 instead. Generic associated types are out of v2 scope (ADR 0018 §6).",
            ),
    );
}

fn check_trait(decl: &ast::TraitDecl, span: Span, diags: &mut Diagnostics) {
    for method in &decl.methods {
        if !method.generics.is_empty() {
            violation(diags, span, &method.name, "it has generic type parameters");
        }
        if method.ret.as_ref().is_some_and(mentions_self) {
            violation(diags, span, &method.name, "it returns `Self`");
        }
    }
}

/// Checks every bound of a `dyn Type` expression and appends diagnostics.
///
/// `traits` is the checker's trait table; `bounds` are the resolved trait bounds
/// (generic arguments and associated bindings do not affect object safety);
/// `span` is the `dyn` type's span.
pub(crate) fn check_object_safety(
    traits: &HashMap<String, ast::TraitDecl>,
    bounds: &[crate::types::TraitBound],
    span: Span,
    diags: &mut Diagnostics,
) {
    let mut principal_count = 0usize;
    for bound in bounds {
        let name = bound.name.as_str();
        if is_auto_trait(name) {
            continue;
        }
        let Some(decl) = traits.get(name) else {
            diags.push(
                Diagnostic::error("E1022", format!("unknown trait `{name}` in `dyn` bound"))
                    .at(span)
                    .with_cause("no trait with this name is in scope")
                    .with_explanation(
                        "A `dyn` bound must name a trait that has been declared. Check the \
                         spelling and any import needed to bring the trait into scope.",
                    ),
            );
            continue;
        };
        principal_count += 1;
        check_trait(decl, span, diags);
    }
    if principal_count > 1 {
        diags.push(
            Diagnostic::error("E1024", "a `dyn` bound may name only one trait in v2")
                .at(span)
                .with_cause("v2 builds one vtable per trait object")
                .with_explanation(
                    "A trait object is a data pointer plus one vtable pointer, so it can carry \
                     the methods of a single trait. Combine the traits into one trait (using a \
                     supertrait) and use `dyn Combined`, or use `impl Trait` for a statically \
                     dispatched value. Auto traits such as `Send` and `Sync` do not count.",
                ),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::{FnDecl, GenericParam, PathSegment, TraitDecl};
    use crate::types::TraitBound;

    fn path(name: &str) -> ast::Path {
        ast::Path {
            segments: vec![PathSegment {
                name: name.into(),
                span: Span::DUMMY,
                args: vec![],
                bindings: vec![],
            }],
            span: Span::DUMMY,
        }
    }

    fn bound(name: &str) -> TraitBound {
        TraitBound { name: name.into(), args: vec![], assoc: vec![] }
    }

    fn method(name: &str, generics: Vec<GenericParam>, ret: Option<ast::TypeExpr>) -> FnDecl {
        FnDecl {
            name: name.into(),
            name_span: Span::DUMMY,
            fn_span: Span::DUMMY,
            extern_span: None,
            link_name: None,
            link_name_span: None,
            generics,
            params: vec![],
            self_param: None,
            ret,
            body: None,
            attrs: vec![],
            is_extern: false,
            is_pub: false,
            is_async: false,
            span: Span::DUMMY,
        }
    }

    fn decl(name: &str, methods: Vec<FnDecl>) -> TraitDecl {
        TraitDecl {
            name: name.into(),
            name_span: Span::DUMMY,
            generics: vec![],
            methods,
            span: Span::DUMMY,
        }
    }

    fn check(bounds: &[&str], traits: &HashMap<String, TraitDecl>) -> Vec<&'static str> {
        let resolved: Vec<TraitBound> = bounds.iter().map(|n| bound(n)).collect();
        let mut diags = Diagnostics::new();
        check_object_safety(traits, &resolved, Span::DUMMY, &mut diags);
        diags.items().iter().map(|d| d.code).collect()
    }

    fn safe() -> HashMap<String, TraitDecl> {
        HashMap::from([("Shape".into(), decl("Shape", vec![method("area", vec![], None)]))])
    }

    #[test]
    fn object_safe_trait_is_clean() {
        assert!(check(&["Shape"], &safe()).is_empty());
    }

    #[test]
    fn unknown_trait_is_e1022() {
        assert_eq!(check(&["Missing"], &safe()), vec!["E1022"]);
    }

    #[test]
    fn generic_method_is_e1023() {
        let generics = vec![GenericParam { name: "T".into(), span: Span::DUMMY, bounds: vec![] }];
        let traits = HashMap::from([("G".into(), decl("G", vec![method("get", generics, None)]))]);
        assert_eq!(check(&["G"], &traits), vec!["E1023"]);
    }

    #[test]
    fn self_return_is_e1023() {
        let ret = ast::TypeExpr::Path(path("Self"));
        let traits = HashMap::from([("C".into(), decl("C", vec![method("dup", vec![], Some(ret))]))]);
        assert_eq!(check(&["C"], &traits), vec!["E1023"]);
    }

    #[test]
    fn two_principal_traits_are_e1024() {
        let mut traits = safe();
        traits.insert("Other".into(), decl("Other", vec![]));
        assert_eq!(check(&["Shape", "Other"], &traits), vec!["E1024"]);
    }

    #[test]
    fn auto_trait_is_exempt() {
        assert!(check(&["Shape", "Send"], &safe()).is_empty());
    }
}
