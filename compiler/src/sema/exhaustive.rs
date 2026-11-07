//! `match` exhaustiveness checking.
//!
//! The rule enforced here is deliberately simple and total: a `match` is
//! exhaustive when it contains an irrefutable pattern (a binding or `_`) or
//! covers every variant of the scrutinee's enum. Ranges and literals on
//! integers are not counted as covering the type, because enumerating an
//! integer type is not practical and silently accepting a partial `match` would
//! hide bugs. This keeps the check sound: it never accepts a non-exhaustive
//! `match`. It may reject a `match` that a smarter analyser would accept, and
//! the diagnostic says so.

use crate::ast;
use crate::diagnostics::{Diagnostic, Suggestion};

use crate::sema::check_expr::FnChecker;
use crate::types::TyKind;

/// Why a `match` was judged non-exhaustive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Gap {
    /// The enum has variants that no arm covers; indices are missing.
    MissingVariants { enum_name: String, missing: Vec<String> },
    /// The scrutinee is `bool` and one of the two values is missing.
    MissingBools { missing_true: bool, missing_false: bool },
    /// The scrutinee type has no finite set of patterns, so a catch-all is needed.
    NeedsCatchAll { type_name: String },
}

impl Gap {
    pub fn message(&self) -> String {
        match self {
            Gap::MissingVariants { missing, .. } => {
                let list: Vec<String> = missing.iter().map(|v| format!("`{v}`")).collect();
                format!("non-exhaustive `match`: {} is not covered", list.join(", "))
            }
            Gap::MissingBools { missing_true, missing_false } => match (missing_true, missing_false) {
                (true, true) => "non-exhaustive `match`: neither `true` nor `false` is covered".to_string(),
                (true, false) => "non-exhaustive `match`: `true` is not covered".to_string(),
                _ => "non-exhaustive `match`: `false` is not covered".to_string(),
            },
            Gap::NeedsCatchAll { type_name } => format!(
                "non-exhaustive `match`: a `{type_name}` value is not covered by a catch-all arm"
            ),
        }
    }

    /// The suggestion shown for this gap, as `(label, replacement, detail)`.
    pub fn suggestion(&self) -> Option<(String, String, Option<String>)> {
        match self {
            Gap::MissingVariants { enum_name, missing } => {
                let arms: Vec<String> = missing.iter().map(|v| format!("{enum_name}::{v} => todo!()")).collect();
                Some((
                    "add the missing variant arm(s)".to_string(),
                    arms.join("\n"),
                    Some("`todo!()` marks the arm as unimplemented; fill in the body".to_string()),
                ))
            }
            Gap::MissingBools { .. } => Some((
                "add the missing `bool` arm(s)".to_string(),
                "_ => todo!()".to_string(),
                None,
            )),
            Gap::NeedsCatchAll { .. } => Some((
                "add a catch-all arm".to_string(),
                "_ => todo!()".to_string(),
                None,
            )),
        }
    }
}

/// Result of an exhaustiveness check, exposed for tests and tooling.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExhaustivenessReport {
    pub exhaustive: bool,
    pub gap: Option<Gap>,
}

impl FnChecker<'_, '_> {
    pub fn check_exhaustive(&mut self, scrut_ty: crate::types::TyId, arms: &[ast::MatchArm], span: crate::source::Span) {
        let report = self.exhaustiveness_report(scrut_ty, arms);
        if report.exhaustive {
            return;
        }
        let Some(gap) = report.gap else { return };
        let mut d = Diagnostic::error("E1008", gap.message())
            .at(span)
            .with_cause("every possible value of the scrutinee must be handled")
            .with_explanation(
                "UMAY requires `match` to be exhaustive so that adding an enum variant later \
                 produces a compile error instead of a silent fallthrough.",
            );
        if let Some((label, replacement, detail)) = gap.suggestion() {
            let mut s = Suggestion::new(label, span, replacement);
            if let Some(detail) = detail {
                s = s.with_detail(detail);
            }
            d = d.with_suggestion(s);
        }
        self.push(d);
    }

    /// Computes the exhaustiveness report without emitting diagnostics.
    pub fn exhaustiveness_report(&mut self, scrut_ty: crate::types::TyId, arms: &[ast::MatchArm]) -> ExhaustivenessReport {
        // An irrefutable arm without a guard makes the whole `match` exhaustive.
        for arm in arms {
            if arm.guard.is_none() && is_irrefutable(&arm.pattern) {
                return ExhaustivenessReport { exhaustive: true, gap: None };
            }
        }

        match self.cx.cx.kind(scrut_ty).clone() {
            TyKind::Adt { def, .. } => {
                let Some(info) = self.cx.hir.enums.iter().find(|e| e.def_id == def) else {
                    // A struct scrutinee: only a catch-all can cover it.
                    return ExhaustivenessReport {
                        exhaustive: false,
                        gap: Some(Gap::NeedsCatchAll { type_name: self.cx.ty_name(scrut_ty) }),
                    };
                };
                let mut covered = vec![false; info.variants.len()];
                for arm in arms {
                    if arm.guard.is_some() {
                        continue;
                    }
                    for name in variant_names_in(&arm.pattern) {
                        if let Some(i) = info.variants.iter().position(|v| v.name == name) {
                            covered[i] = true;
                        }
                    }
                }
                let missing: Vec<String> = info
                    .variants
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| !covered[*i])
                    .map(|(_, v)| v.name.clone())
                    .collect();
                if missing.is_empty() {
                    ExhaustivenessReport { exhaustive: true, gap: None }
                } else {
                    ExhaustivenessReport {
                        exhaustive: false,
                        gap: Some(Gap::MissingVariants { enum_name: info.name.clone(), missing }),
                    }
                }
            }
            TyKind::Bool => {
                let mut has_true = false;
                let mut has_false = false;
                for arm in arms {
                    if arm.guard.is_some() {
                        continue;
                    }
                    match &arm.pattern.kind {
                        ast::PatternKind::Literal(e) => match &e.kind {
                            ast::ExprKind::BoolLit(true) => has_true = true,
                            ast::ExprKind::BoolLit(false) => has_false = true,
                            _ => {}
                        },
                        ast::PatternKind::Or(alts) => {
                            for a in alts {
                                if let ast::PatternKind::Literal(e) = &a.kind {
                                    if let ast::ExprKind::BoolLit(b) = e.kind {
                                        if b {
                                            has_true = true;
                                        } else {
                                            has_false = true;
                                        }
                                    }
                                }
                            }
                        }
                        _ => {}
                    }
                }
                if has_true && has_false {
                    ExhaustivenessReport { exhaustive: true, gap: None }
                } else {
                    ExhaustivenessReport {
                        exhaustive: false,
                        gap: Some(Gap::MissingBools {
                            missing_true: !has_true,
                            missing_false: !has_false,
                        }),
                    }
                }
            }
            TyKind::Error => ExhaustivenessReport { exhaustive: true, gap: None },
            _ => ExhaustivenessReport {
                exhaustive: false,
                gap: Some(Gap::NeedsCatchAll { type_name: self.cx.ty_name(scrut_ty) }),
            },
        }
    }
}

/// A pattern that matches every value, whatever the type.
pub fn is_irrefutable(pat: &ast::Pattern) -> bool {
    match &pat.kind {
        ast::PatternKind::Binding { name, .. } => name == "_" || !name.is_empty(),
        ast::PatternKind::Tuple(ps) => ps.iter().all(is_irrefutable),
        ast::PatternKind::Struct { fields, .. } => fields
            .iter()
            .all(|f| f.pattern.as_ref().map(is_irrefutable).unwrap_or(true)),
        ast::PatternKind::Or(alts) => alts.iter().any(is_irrefutable),
        _ => false,
    }
}

/// Every variant name a pattern can match, following or-patterns.
///
/// Names are used rather than `DefId`s because the pattern's enum has already
/// been type-checked against the scrutinee, so a matching name is unambiguous.
fn variant_names_in(pat: &ast::Pattern) -> Vec<String> {
    match &pat.kind {
        ast::PatternKind::Variant { path, .. } => {
            path.segments.last().map(|s| s.name.clone()).into_iter().collect()
        }
        ast::PatternKind::Or(alts) => alts.iter().flat_map(variant_names_in).collect(),
        _ => Vec::new(),
    }
}
