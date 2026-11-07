//! Type representations and the type interner.
//!
//! Types are interned into a [`TyCtxt`] so that equality is an integer
//! comparison and recursive types are representable. Every type in the compiler
//! is a `TyId` — a small `Copy` handle.
//!
//! Design notes (see `docs/type-system.md`):
//!
//! * `str` is unsized; it appears behind `&str`.
//! * Integer literals are polymorphic until constrained; an unconstrained one
//!   defaults to `i32` (floats to `f64`).
//! * There are no implicit conversions between numeric types.
//! * Generic parameters are interned as `TyKind::Param { name }` during
//!   Phase 2 (no monomorphisation yet — that is a codegen concern).

use std::collections::HashMap;
use std::fmt;

use crate::source::Span;

/// Interned type handle.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TyId(pub u32);

impl fmt::Debug for TyId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Ty#{}", self.0)
    }
}

/// The structure of an interned type.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum TyKind {
    /// The type of a diverging expression.
    Never,
    Unit,
    Bool,

    I8,
    I16,
    I32,
    I64,
    I128,
    Isize,
    U8,
    U16,
    U32,
    U64,
    U128,
    Usize,
    F32,
    F64,

    Char,
    /// Unsized UTF-8 string slice.
    Str,
    /// Owned, growable UTF-8 string (a standard-library struct).
    String,

    /// Tuple. The empty tuple is [`TyKind::Unit`] and never appears here.
    Tuple(Vec<TyId>),
    /// Fixed-size array.
    Array { elem: TyId, len: u64 },
    /// Unsized slice `[T]`.
    Slice(TyId),
    /// Shared reference `&T`.
    Ref(TyId),
    /// Exclusive reference `&mut T`.
    RefMut(TyId),
    /// Raw pointer `*const T`.
    RawPtr(TyId),
    /// Raw mutable pointer `*mut T`.
    RawPtrMut(TyId),

    /// User-defined struct instantiation.
    Adt { def: DefId, args: Vec<TyId> },
    /// Function pointer `fn(A, B) -> C`.
    FnPtr { params: Vec<TyId>, ret: TyId },

    /// A generic parameter, named (`T`). Equal names in the same scope are the
    /// same parameter for Phase 2 purposes.
    Param { name: String },
    /// Integer literal type, unconstrained until unified.
    IntVar(u32),
    /// Float literal type, unconstrained until unified.
    FloatVar(u32),

    /// Placeholder produced after an error; suppresses cascading diagnostics.
    Error,
}

/// Identifier for a user-defined type declaration (struct or enum).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub struct DefId(pub u32);

/// Interner for types.
pub struct TyCtxt {
    kinds: Vec<TyKind>,
    /// Deduplication table: a type's structure maps to its id.
    interned: HashMap<TyKind, TyId>,
    next_var: u32,
}

impl Default for TyCtxt {
    fn default() -> Self {
        Self::new()
    }
}

impl TyCtxt {
    /// Creates an interner pre-populated with every primitive type.
    pub fn new() -> Self {
        let cx = TyCtxt { kinds: Vec::new(), interned: HashMap::new(), next_var: 0 };
        // The primitives are still interned normally; this just makes the
        // common handles cheap to obtain via the `prim` helpers.
        cx
    }

    fn intern(&mut self, kind: TyKind) -> TyId {
        if let Some(&id) = self.interned.get(&kind) {
            return id;
        }
        let id = TyId(self.kinds.len() as u32);
        self.kinds.push(kind.clone());
        self.interned.insert(kind, id);
        id
    }

    /// Cloned kind, for callers that cannot hold a borrow of `self` across a
    /// mutable operation.
    pub fn kind_owned(&self, ty: TyId) -> TyKind {
        self.kind(ty).clone()
    }

    /// True when `ty` is an id this context knows about.
    pub fn contains(&self, ty: TyId) -> bool {
        (ty.0 as usize) < self.kinds.len()
    }

    pub fn kind(&self, ty: TyId) -> &TyKind {
        &self.kinds[ty.0 as usize]
    }

    // ---- primitives -------------------------------------------------------

    pub fn never(&mut self) -> TyId {
        self.intern(TyKind::Never)
    }
    pub fn unit(&mut self) -> TyId {
        self.intern(TyKind::Unit)
    }
    pub fn bool(&mut self) -> TyId {
        self.intern(TyKind::Bool)
    }
    pub fn char(&mut self) -> TyId {
        self.intern(TyKind::Char)
    }
    pub fn str_(&mut self) -> TyId {
        self.intern(TyKind::Str)
    }
    /// `&str`, the type of a string literal.
    ///
    /// A bare `str` is unsized and cannot be a value or a parameter type; the
    /// specification says it is used behind a reference, so literals produce
    /// `&str` directly.
    pub fn str_ref(&mut self) -> TyId {
        let inner = self.intern(TyKind::Str);
        self.intern(TyKind::Ref(inner))
    }

    pub fn string(&mut self) -> TyId {
        self.intern(TyKind::String)
    }
    pub fn error(&mut self) -> TyId {
        self.intern(TyKind::Error)
    }

    /// Resolves a primitive type name to its type, if it is a primitive.
    pub fn primitive(&mut self, name: &str) -> Option<TyId> {
        let kind = match name {
            "bool" => TyKind::Bool,
            "char" => TyKind::Char,
            "i8" => TyKind::I8,
            "i16" => TyKind::I16,
            "i32" => TyKind::I32,
            "i64" => TyKind::I64,
            "i128" => TyKind::I128,
            "isize" => TyKind::Isize,
            "u8" => TyKind::U8,
            "u16" => TyKind::U16,
            "u32" => TyKind::U32,
            "u64" => TyKind::U64,
            "u128" => TyKind::U128,
            "usize" => TyKind::Usize,
            "f32" => TyKind::F32,
            "f64" => TyKind::F64,
            "str" => TyKind::Str,
            "String" => TyKind::String,
            _ => return None,
        };
        Some(self.intern(kind))
    }

    /// Looks up an already-interned type without interning a new one.
    ///
    /// The primitives are interned on first use, so a read-only context (such as
    /// the backend, which holds `&TyCtxt`) can still ask for one once it exists.
    /// A missing type is a compiler bug: every primitive that codegen can see has
    /// already been interned by the front end.
    pub fn find(&self, kind: &TyKind) -> Option<TyId> {
        self.interned.get(kind).copied()
    }

    /// The `u8` type, for a context that cannot intern.
    pub fn cached_u8(&self) -> TyId {
        self.find(&TyKind::U8).expect("u8 is interned by the front end")
    }

    /// The `u32` type, for a context that cannot intern.
    pub fn cached_u32(&self) -> TyId {
        self.find(&TyKind::U32).expect("u32 is interned by the front end")
    }

    /// A pointer-width integer type, for a context that cannot intern.
    ///
    /// Any integer of pointer width will do, since the only property the caller
    /// needs is the machine width. `usize` is preferred because it is the type
    /// the language uses for addresses, but a program that never mentions it
    /// will not have it interned.
    pub fn cached_usize(&self) -> TyId {
        self.find(&TyKind::Usize)
            .or_else(|| self.find(&TyKind::U64))
            .or_else(|| self.find(&TyKind::I64))
            .unwrap_or_else(|| self.cached_unit())
    }

    /// The unit type, for a context that cannot intern.
    pub fn cached_unit(&self) -> TyId {
        self.find(&TyKind::Unit).expect("unit is interned by the front end")
    }

    // ---- constructors -----------------------------------------------------

    pub fn tuple(&mut self, elems: Vec<TyId>) -> TyId {
        if elems.is_empty() {
            return self.unit();
        }
        self.intern(TyKind::Tuple(elems))
    }

    pub fn array(&mut self, elem: TyId, len: u64) -> TyId {
        self.intern(TyKind::Array { elem, len })
    }

    pub fn slice(&mut self, elem: TyId) -> TyId {
        self.intern(TyKind::Slice(elem))
    }

    pub fn ref_to(&mut self, inner: TyId) -> TyId {
        self.intern(TyKind::Ref(inner))
    }

    pub fn ref_mut_to(&mut self, inner: TyId) -> TyId {
        self.intern(TyKind::RefMut(inner))
    }

    pub fn raw_ptr(&mut self, inner: TyId) -> TyId {
        self.intern(TyKind::RawPtr(inner))
    }

    pub fn raw_ptr_mut(&mut self, inner: TyId) -> TyId {
        self.intern(TyKind::RawPtrMut(inner))
    }

    pub fn adt(&mut self, def: DefId, args: Vec<TyId>) -> TyId {
        self.intern(TyKind::Adt { def, args })
    }

    /// Replaces generic parameters with the types they stand for.
    ///
    /// `TyKind::Param` is what a declaration writes; an actual value has no
    /// parameters at all. Instantiating a struct or a call means rewriting every
    /// parameter it mentions with the concrete type chosen at the use site, which
    /// is why `Vec<T>`'s `ptr: *mut T` field is a `*mut i64` before layout runs.
    /// The first generic parameter mentioned anywhere inside `ty`, if any.
    ///
    /// A parameter is rarely bare: a body's locals are `&Box<T>` and `*mut T` as
    /// often as `T`, so the reachability question has to walk the structure.
    pub fn first_param(&self, ty: TyId) -> Option<String> {
        self.first_param_in(ty, 0)
    }

    fn first_param_in(&self, ty: TyId, depth: u32) -> Option<String> {
        // A self-referential definition would otherwise recurse forever.
        if depth > 32 {
            return None;
        }
        match self.kind(ty).clone() {
            TyKind::Param { name } => Some(name),
            TyKind::Ref(inner)
            | TyKind::RefMut(inner)
            | TyKind::RawPtr(inner)
            | TyKind::RawPtrMut(inner)
            | TyKind::Slice(inner) => self.first_param_in(inner, depth + 1),
            TyKind::Array { elem, .. } => self.first_param_in(elem, depth + 1),
            TyKind::Tuple(items) => items.iter().find_map(|t| self.first_param_in(*t, depth + 1)),
            TyKind::Adt { args, .. } => args.iter().find_map(|t| self.first_param_in(*t, depth + 1)),
            TyKind::FnPtr { params, ret } => params
                .iter()
                .chain(std::iter::once(&ret))
                .find_map(|t| self.first_param_in(*t, depth + 1)),
            _ => None,
        }
    }

    pub fn substitute(&mut self, ty: TyId, subst: &HashMap<String, TyId>) -> TyId {
        if subst.is_empty() {
            return ty;
        }
        let kind = self.kind(ty).clone();
        match kind {
            TyKind::Param { ref name } => subst.get(name).copied().unwrap_or(ty),
            TyKind::Ref(inner) => {
                let inner = self.substitute(inner, subst);
                self.ref_to(inner)
            }
            TyKind::RefMut(inner) => {
                let inner = self.substitute(inner, subst);
                self.ref_mut_to(inner)
            }
            TyKind::RawPtr(inner) => {
                let inner = self.substitute(inner, subst);
                self.raw_ptr(inner)
            }
            TyKind::RawPtrMut(inner) => {
                let inner = self.substitute(inner, subst);
                self.raw_ptr_mut(inner)
            }
            TyKind::Slice(inner) => {
                let inner = self.substitute(inner, subst);
                self.slice(inner)
            }
            TyKind::Array { elem, len } => {
                let elem = self.substitute(elem, subst);
                self.array(elem, len)
            }
            TyKind::Tuple(items) => {
                let items: Vec<TyId> = items.iter().map(|t| self.substitute(*t, subst)).collect();
                self.tuple(items)
            }
            TyKind::Adt { def, args } => {
                let args: Vec<TyId> = args.iter().map(|t| self.substitute(*t, subst)).collect();
                self.adt(def, args)
            }
            TyKind::FnPtr { params, ret } => {
                let params: Vec<TyId> =
                    params.iter().map(|t| self.substitute(*t, subst)).collect();
                let ret = self.substitute(ret, subst);
                self.fn_ptr(params, ret)
            }
            _ => ty,
        }
    }

    pub fn fn_ptr(&mut self, params: Vec<TyId>, ret: TyId) -> TyId {
        self.intern(TyKind::FnPtr { params, ret })
    }

    pub fn param(&mut self, name: impl Into<String>) -> TyId {
        self.intern(TyKind::Param { name: name.into() })
    }

    pub fn fresh_int_var(&mut self) -> TyId {
        let n = self.next_var;
        self.next_var += 1;
        self.intern(TyKind::IntVar(n))
    }

    pub fn fresh_float_var(&mut self) -> TyId {
        let n = self.next_var;
        self.next_var += 1;
        self.intern(TyKind::FloatVar(n))
    }

    // ---- queries ----------------------------------------------------------

    pub fn is_error(&self, ty: TyId) -> bool {
        matches!(self.kind(ty), TyKind::Error)
    }

    pub fn is_infer_var(&self, ty: TyId) -> bool {
        matches!(self.kind(ty), TyKind::IntVar(_) | TyKind::FloatVar(_))
    }

    pub fn is_integer_var(&self, ty: TyId) -> bool {
        matches!(self.kind(ty), TyKind::IntVar(_))
    }

    pub fn is_float_var(&self, ty: TyId) -> bool {
        matches!(self.kind(ty), TyKind::FloatVar(_))
    }

    /// True for every concrete integer type (not literal vars).
    pub fn is_integer(&self, ty: TyId) -> bool {
        matches!(
            self.kind(ty),
            TyKind::I8
                | TyKind::I16
                | TyKind::I32
                | TyKind::I64
                | TyKind::I128
                | TyKind::Isize
                | TyKind::U8
                | TyKind::U16
                | TyKind::U32
                | TyKind::U64
                | TyKind::U128
                | TyKind::Usize
        )
    }

    /// True for concrete signed integer types.
    pub fn is_signed(&self, ty: TyId) -> bool {
        matches!(
            self.kind(ty),
            // An unconstrained integer literal defaults to `i32`, so codegen must
            // treat it as signed; otherwise division picks the unsigned opcode.
            TyKind::I8
                | TyKind::I16
                | TyKind::I32
                | TyKind::I64
                | TyKind::I128
                | TyKind::Isize
                | TyKind::IntVar(_)
        )
    }

    pub fn is_float(&self, ty: TyId) -> bool {
        matches!(self.kind(ty), TyKind::F32 | TyKind::F64)
    }

    /// True for types that are implicitly copied instead of moved.
    pub fn is_copy(&self, ty: TyId) -> bool {
        match self.kind(ty) {
            TyKind::Bool
            | TyKind::Char
            | TyKind::I8
            | TyKind::I16
            | TyKind::I32
            | TyKind::I64
            | TyKind::I128
            | TyKind::Isize
            | TyKind::U8
            | TyKind::U16
            | TyKind::U32
            | TyKind::U64
            | TyKind::U128
            | TyKind::Usize
            | TyKind::F32
            | TyKind::F64
            | TyKind::Unit
            | TyKind::Never
            | TyKind::Ref(_)
            | TyKind::RawPtr(_)
            | TyKind::RawPtrMut(_)
            | TyKind::FnPtr { .. }
            | TyKind::Error
            | TyKind::IntVar(_)
            | TyKind::FloatVar(_) => true,
            TyKind::Tuple(elems) => elems.iter().all(|&e| self.is_copy(e)),
            // Arrays of Copy types are Copy; array lengths are small in practice.
            TyKind::Array { elem, .. } => self.is_copy(*elem),
            // Structs and enums are Copy only when explicitly derived; Phase 2
            // conservatively treats them as move-only until derives are checked.
            _ => false,
        }
    }

    /// True for types whose size is not known at compile time.
    pub fn is_unsized(&self, ty: TyId) -> bool {
        match self.kind(ty) {
            TyKind::Str | TyKind::Slice(_) => true,
            _ => false,
        }
    }

    /// True for numeric types including unresolved literal variables.
    pub fn is_numeric(&self, ty: TyId) -> bool {
        self.is_integer(ty)
            || self.is_float(ty)
            || self.is_integer_var(ty)
            || self.is_float_var(ty)
    }

    /// Integer bounds `(min, max)` for a concrete integer type.
    pub fn int_bounds(&self, ty: TyId) -> Option<(i128, i128)> {
        Some(match self.kind(ty) {
            TyKind::I8 => (i8::MIN as i128, i8::MAX as i128),
            TyKind::I16 => (i16::MIN as i128, i16::MAX as i128),
            TyKind::I32 => (i32::MIN as i128, i32::MAX as i128),
            TyKind::I64 => (i64::MIN as i128, i64::MAX as i128),
            TyKind::I128 => (i128::MIN, i128::MAX),
            TyKind::Isize => (isize::MIN as i128, isize::MAX as i128),
            TyKind::U8 => (0, u8::MAX as i128),
            TyKind::U16 => (0, u16::MAX as i128),
            TyKind::U32 => (0, u32::MAX as i128),
            TyKind::U64 => (0, u64::MAX as i128),
            TyKind::U128 => (0, u128::MAX as i128),
            TyKind::Usize => (0, usize::MAX as i128),
            _ => return None,
        })
    }

    /// Human-readable name for a type, used in diagnostics.
    pub fn display(&self, ty: TyId) -> String {
        match self.kind(ty) {
            TyKind::Never => "!".into(),
            TyKind::Unit => "()".into(),
            TyKind::Bool => "bool".into(),
            TyKind::I8 => "i8".into(),
            TyKind::I16 => "i16".into(),
            TyKind::I32 => "i32".into(),
            TyKind::I64 => "i64".into(),
            TyKind::I128 => "i128".into(),
            TyKind::Isize => "isize".into(),
            TyKind::U8 => "u8".into(),
            TyKind::U16 => "u16".into(),
            TyKind::U32 => "u32".into(),
            TyKind::U64 => "u64".into(),
            TyKind::U128 => "u128".into(),
            TyKind::Usize => "usize".into(),
            TyKind::F32 => "f32".into(),
            TyKind::F64 => "f64".into(),
            TyKind::Char => "char".into(),
            TyKind::Str => "str".into(),
            TyKind::String => "String".into(),
            TyKind::Tuple(elems) => {
                let inner: Vec<String> = elems.iter().map(|&e| self.display(e)).collect();
                if inner.len() == 1 {
                    format!("({},)", inner[0])
                } else {
                    format!("({})", inner.join(", "))
                }
            }
            TyKind::Array { elem, len } => format!("[{}; {}]", self.display(*elem), len),
            TyKind::Slice(elem) => format!("[{}]", self.display(*elem)),
            TyKind::Ref(inner) => format!("&{}", self.display(*inner)),
            TyKind::RefMut(inner) => format!("&mut {}", self.display(*inner)),
            TyKind::RawPtr(inner) => format!("*const {}", self.display(*inner)),
            TyKind::RawPtrMut(inner) => format!("*mut {}", self.display(*inner)),
            TyKind::Adt { .. } => "<adt>".into(), // filled in by the checker, which knows names
            TyKind::FnPtr { params, ret } => {
                let ps: Vec<String> = params.iter().map(|&p| self.display(p)).collect();
                format!("fn({}) -> {}", ps.join(", "), self.display(*ret))
            }
            TyKind::Param { name } => name.clone(),
            TyKind::IntVar(_) => "{integer}".into(),
            TyKind::FloatVar(_) => "{float}".into(),
            TyKind::Error => "{error}".into(),
        }
    }
}

/// A semantic (HIR) type expression: the resolved form of `ast::TypeExpr`.
///
/// Kept separate from `TyKind` because resolvable syntax (paths with generic
/// arguments) is not the same thing as an interned type.
#[derive(Clone, Debug, PartialEq)]
pub enum TyExpr {
    /// `TyId` is fully resolved (primitives, interned composites).
    Resolved(TyId),
    /// A path that could not be resolved; recorded for diagnostics.
    Unresolved { path: String, span: Span },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interning_is_idempotent() {
        let mut cx = TyCtxt::new();
        let u = cx.unit();
        let a = cx.ref_to(u);
        let b = cx.ref_to(u);
        assert_eq!(a, b);
    }

    #[test]
    fn primitives_resolve() {
        let mut cx = TyCtxt::new();
        assert!(cx.primitive("i32").is_some());
        assert!(cx.primitive("String").is_some());
        assert!(cx.primitive("NoSuchType").is_none());
        let i32t = cx.primitive("i32").unwrap();
        assert!(cx.is_integer(i32t));
        assert!(!cx.is_float(i32t));
        assert!(cx.is_copy(i32t));
    }

    #[test]
    fn unit_and_empty_tuple_are_the_same() {
        let mut cx = TyCtxt::new();
        assert_eq!(cx.tuple(Vec::new()), cx.unit());
    }

    #[test]
    fn display_formats_composites() {
        let mut cx = TyCtxt::new();
        let i32t = cx.primitive("i32").unwrap();
        let one = cx.tuple(vec![i32t]);
        assert_eq!(cx.display(one), "(i32,)");
        let r = cx.ref_mut_to(i32t);
        assert_eq!(cx.display(r), "&mut i32");
        let arr = cx.array(i32t, 4);
        assert_eq!(cx.display(arr), "[i32; 4]");
    }

    #[test]
    fn copy_analysis() {
        let mut cx = TyCtxt::new();
        let i32t = cx.primitive("i32").unwrap();
        let string = cx.string();
        assert!(cx.is_copy(i32t));
        assert!(!cx.is_copy(string), "String owns a heap allocation");
        let tup = cx.tuple(vec![i32t, i32t]);
        assert!(cx.is_copy(tup));
        let mixed = cx.tuple(vec![i32t, string]);
        assert!(!cx.is_copy(mixed));
        let rm = cx.ref_mut_to(i32t);
        assert!(!cx.is_copy(rm), "&mut T must move, not copy");
    }

    #[test]
    fn int_bounds_are_correct() {
        let mut cx = TyCtxt::new();
        let u8t = cx.primitive("u8").unwrap();
        assert_eq!(cx.int_bounds(u8t), Some((0, 255)));
        let i8t = cx.primitive("i8").unwrap();
        assert_eq!(cx.int_bounds(i8t), Some((-128, 127)));
    }

    #[test]
    fn str_is_unsized_string_is_not() {
        let mut cx = TyCtxt::new();
        let s = cx.str_();
        let owned = cx.string();
        assert!(cx.is_unsized(s));
        assert!(!cx.is_unsized(owned));
    }
}
