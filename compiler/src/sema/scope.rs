//! Symbol tables and scope handling.
//!
//! Two levels of scope exist:
//!
//! * **Item scope** — program-wide. Structs, enums, traits, functions and
//!   constants live here and are visible everywhere (order independent).
//! * **Value scope** — per function body. Locals are visible from their `let`
//!   to the end of the enclosing block. Shadowing is permitted.
//!
//! Name lookup prefers the innermost value scope, then item scope, then
//! predeclared prelude names (standard library types and constructors). This
//! ordering matters for diagnostics: a local named `String` shadows the type,
//! and the error message must say so rather than reporting "unknown type".

use std::collections::HashMap;

use crate::hir::{LocalId, Res};
use crate::source::Span;
use crate::types::TyId;

/// A value-scope entry.
#[derive(Clone, Copy, Debug)]
pub struct LocalEntry {
    pub id: LocalId,
    pub mutable: bool,
    pub ty: TyId,
}

/// What kind of item a name refers to, for duplicate/conflict diagnostics.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ItemKind {
    Fn,
    Struct,
    Enum,
    Trait,
    Const,
    Variant,
}

impl ItemKind {
    pub fn describe(self) -> &'static str {
        match self {
            ItemKind::Fn => "function",
            ItemKind::Struct => "struct",
            ItemKind::Enum => "enum",
            ItemKind::Trait => "trait",
            ItemKind::Const => "constant",
            ItemKind::Variant => "enum variant",
        }
    }
}

#[derive(Clone, Debug)]
pub struct ItemEntry {
    pub res: Res,
    pub kind: ItemKind,
    pub span: Span,
    /// Number of parameters (for arity checks on callable items).
    pub arity: usize,
}

/// Program-wide item names.
#[derive(Default)]
pub struct ItemScope {
    /// Every name declared by the user.
    items: HashMap<String, ItemEntry>,
    /// Prelude names available without an import (types and constructors).
    prelude: HashMap<String, ItemEntry>,
}

impl ItemScope {
    pub fn new() -> Self {
        ItemScope::default()
    }

    /// Declares a user item, returning the existing entry if the name is taken.
    pub fn declare(&mut self, name: String, entry: ItemEntry) -> Result<(), ItemEntry> {
        if let Some(existing) = self.items.get(&name) {
            return Err(existing.clone());
        }
        self.items.insert(name, entry);
        Ok(())
    }

    pub fn declare_prelude(&mut self, name: String, entry: ItemEntry) {
        self.prelude.insert(name, entry);
    }

    /// Looks a name up: user items first (so locals/items shadow the prelude).
    pub fn lookup(&self, name: &str) -> Option<&ItemEntry> {
        self.items.get(name).or_else(|| self.prelude.get(name))
    }

    pub fn lookup_item_only(&self, name: &str) -> Option<&ItemEntry> {
        self.items.get(name)
    }

    pub fn all_names(&self) -> Vec<&str> {
        let mut names: Vec<&str> = self.items.keys().map(String::as_str).collect();
        names.sort_unstable();
        names
    }

    pub fn user_items(&self) -> impl Iterator<Item = (&String, &ItemEntry)> {
        self.items.iter()
    }
}

/// A stack of value scopes for one function body.
#[derive(Default)]
pub struct ValueScopes {
    /// Each frame is the set of locals introduced by one block.
    frames: Vec<Vec<(String, LocalEntry)>>,
}

impl ValueScopes {
    pub fn new() -> Self {
        ValueScopes { frames: vec![Vec::new()] }
    }

    pub fn push_frame(&mut self) {
        self.frames.push(Vec::new());
    }

    pub fn pop_frame(&mut self) {
        debug_assert!(self.frames.len() > 1, "cannot pop the function-level frame");
        self.frames.pop();
    }

    /// Introduces a local in the current frame.
    pub fn declare(&mut self, name: String, entry: LocalEntry) {
        self.frames.last_mut().expect("at least one frame").push((name, entry));
    }

    /// Looks up a local from the innermost frame outwards.
    pub fn lookup(&self, name: &str) -> Option<LocalEntry> {
        for frame in self.frames.iter().rev() {
            // Later declarations in the same frame shadow earlier ones.
            for (n, entry) in frame.iter().rev() {
                if n == name {
                    return Some(*entry);
                }
            }
        }
        None
    }

    /// True if the name is declared in the *current* frame (duplicate check).
    pub fn declared_in_current_frame(&self, name: &str) -> bool {
        self.frames.last().map(|f| f.iter().any(|(n, _)| n == name)).unwrap_or(false)
    }

    /// Every value currently in scope, outermost frame first, so later entries
    /// shadow earlier ones when a caller declares them in that order.
    pub fn all_entries(&self) -> Vec<(String, LocalEntry)> {
        self.frames.iter().flatten().cloned().collect()
    }
}

/// Closest-match suggestion for an unknown name, using edit distance.
///
/// Returns `None` when nothing is close enough, so that suggestions are never
/// invented for genuinely unrelated names.
pub fn suggest_name<'a>(unknown: &str, candidates: impl Iterator<Item = &'a str>) -> Option<String> {
    let mut best: Option<(usize, &'a str)> = None;
    let threshold = match unknown.len() {
        0..=3 => 1,
        4..=6 => 2,
        _ => 3,
    };
    for cand in candidates {
        if cand == unknown {
            continue;
        }
        let d = levenshtein(unknown, cand);
        let d = if cand.eq_ignore_ascii_case(unknown) { 0 } else { d };
        if d <= threshold && best.map(|(bd, _)| d < bd).unwrap_or(true) {
            best = Some((d, cand));
        }
    }
    best.map(|(_, s)| s.to_string())
}

/// Edit distance counting an adjacent transposition as one edit
/// (Damerau–Levenshtein, restricted).
///
/// Plain Levenshtein scores `gte`→`get` as 2, which puts many real typos over
/// the suggestion threshold. Transpositions are the most common typing error,
/// so counting them as one edit materially improves suggestions.
fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    if a.is_empty() {
        return b.len();
    }
    if b.is_empty() {
        return a.len();
    }
    // Full matrix: transposition needs the row before the previous one.
    let mut d = vec![vec![0usize; b.len() + 1]; a.len() + 1];
    for (i, row) in d.iter_mut().enumerate() {
        row[0] = i;
    }
    for (j, cell) in d[0].iter_mut().enumerate() {
        *cell = j;
    }
    for i in 1..=a.len() {
        for j in 1..=b.len() {
            let cost = if a[i - 1] == b[j - 1] { 0 } else { 1 };
            let mut best = (d[i - 1][j] + 1).min(d[i][j - 1] + 1).min(d[i - 1][j - 1] + cost);
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                best = best.min(d[i - 2][j - 2] + 1);
            }
            d[i][j] = best;
        }
    }
    d[a.len()][b.len()]
}

/// Convenience: describes what an item id is, for diagnostics.
pub fn describe_res(res: Res) -> &'static str {
    match res {
        Res::Local(_) => "local variable",
        Res::Fn(_) => "function",
        Res::Type(_) => "type",
        Res::Variant { .. } => "enum variant",
        Res::Const(_) => "constant",
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::TyCtxt;

    #[test]
    fn shadowing_resolves_to_innermost() {
        let mut cx = TyCtxt::new();
        let ty = cx.primitive("i32").unwrap();
        let mut scopes = ValueScopes::new();
        scopes.declare("x".into(), LocalEntry { id: LocalId(0), mutable: false, ty });
        scopes.push_frame();
        scopes.declare("x".into(), LocalEntry { id: LocalId(1), mutable: true, ty });
        assert_eq!(scopes.lookup("x").unwrap().id, LocalId(1));
        scopes.pop_frame();
        assert_eq!(scopes.lookup("x").unwrap().id, LocalId(0));
    }

    #[test]
    fn duplicate_in_same_frame_is_detected() {
        let mut cx = TyCtxt::new();
        let ty = cx.primitive("i32").unwrap();
        let mut scopes = ValueScopes::new();
        assert!(!scopes.declared_in_current_frame("x"));
        scopes.declare("x".into(), LocalEntry { id: LocalId(0), mutable: false, ty });
        assert!(scopes.declared_in_current_frame("x"));
    }

    #[test]
    fn unknown_name_has_no_lookup() {
        let scopes = ValueScopes::new();
        assert!(scopes.lookup("missing").is_none());
    }

    #[test]
    fn suggestions_are_close_matches_only() {
        let candidates = ["print", "prnt", "foo", "bar"];
        assert_eq!(suggest_name("prnt", candidates.iter().copied()), Some("print".into()));
        // Nothing remotely similar: no suggestion is offered.
        assert_eq!(suggest_name("zzzzzzzz", candidates.iter().copied()), None);
    }

    #[test]
    fn levenshtein_basics() {
        assert_eq!(levenshtein("", "abc"), 3);
        assert_eq!(levenshtein("abc", ""), 3);
        assert_eq!(levenshtein("abc", "abc"), 0);
        assert_eq!(levenshtein("kitten", "sitting"), 3);
    }

    #[test]
    fn item_scope_reports_duplicates() {
        use crate::types::DefId;
        let mut scope = ItemScope::new();
        let span = Span::DUMMY;
        let entry = ItemEntry { res: Res::Type(DefId(0)), kind: ItemKind::Struct, span, arity: 0 };
        assert!(scope.declare("Foo".into(), entry.clone()).is_ok());
        let err = scope.declare("Foo".into(), entry);
        assert!(err.is_err());
    }
}
