//! Type layouts and the System V AMD64 aggregate classification.
//!
//! The backend needs two things from a type: how many bytes it occupies and how
//! it is passed to a function. Both are target properties, not language
//! properties, so they live here rather than in [`crate::types`].
//!
//! Every rule in this file is a rule of the System V ABI for x86_64, and the
//! line references are to the AMD64 ABI specification, section 3.2.3.

use crate::hir::Hir;
use crate::types::{DefId, TyCtxt, TyId, TyKind};
use std::collections::HashMap;

/// A type's size and alignment in bytes, plus enough shape information for the
/// ABI classifier to walk it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Layout {
    pub size: u64,
    /// Alignment is always a power of two, so it is stored as its exponent to
    /// make "round up to a multiple of align" a shift rather than a division.
    pub align_log2: u32,
}

impl Layout {
    pub fn new(size: u64, align: u64) -> Self {
        debug_assert!(align.is_power_of_two(), "alignment must be a power of two");
        Layout { size, align_log2: align.trailing_zeros() }
    }

    pub fn align(&self) -> u64 {
        1 << self.align_log2
    }

    /// Rounds `n` up to a multiple of this layout's alignment.
    pub fn align_up(&self, n: u64) -> u64 {
        let a = self.align();
        n.div_ceil(a) * a
    }

    pub fn is_zero_sized(&self) -> bool {
        self.size == 0
    }
}

/// Why a type has no known layout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LayoutError {
    /// The type is still an inference variable, which means checking did not
    /// finish. Reaching codegen in this state is a compiler bug.
    Unresolved(TyId),
    /// A user-defined type whose definition is missing from the HIR.
    UnknownType(DefId),
    /// `str` and `[T]` have no size known at compile time.
    Unsized(String),
}

/// Computes and caches layouts for one program.
pub struct Layouts<'a> {
    types: &'a mut TyCtxt,
    hir: &'a Hir,
    /// Memoised layouts, keyed by interned type id.
    cache: HashMap<TyId, Layout>,
    /// Field offsets for struct instantiations, keyed by (def, args).
    offsets: HashMap<(DefId, Vec<TyId>), Vec<u64>>,
    recursion: Vec<TyId>,
}

impl<'a> Layouts<'a> {
    pub fn new(types: &'a mut TyCtxt, hir: &'a Hir) -> Self {
        Layouts {
            types,
            hir,
            cache: HashMap::new(),
            offsets: HashMap::new(),
            recursion: Vec::new(),
        }
    }

    pub fn types(&self) -> &TyCtxt {
        self.types
    }

    /// The type context, borrowed for as long as this borrow of `self` lives.
    ///
    /// Distinct from [`Self::types`], which returns the full `'a` lifetime and so
    /// keeps `self` immutably borrowed while `layouts` still needs to be used
    /// mutably.
    pub fn types_ref(&self) -> &TyCtxt {
        self.types
    }

    /// Interning needs `&mut TyCtxt`. Layouts are keyed by `TyId`, which is
    /// stable for a given kind, so interning a new type never invalidates an
    /// existing cache entry.
    pub fn types_mut(&mut self) -> &mut TyCtxt {
        self.types
    }

    pub fn hir(&self) -> &'a Hir {
        self.hir
    }

    /// The layout of a type, or the reason it has none.
    /// Whether `ty` is an aggregate: a struct, enum, tuple or array.
    pub fn is_aggregate(&self, ty: TyId) -> bool {
        matches!(
            self.types.kind(ty),
            crate::types::TyKind::Adt { .. }
                | crate::types::TyKind::Array { .. }
                | crate::types::TyKind::Tuple(_)
        )
    }

    pub fn layout(&mut self, ty: TyId) -> Result<Layout, LayoutError> {
        if let Some(&l) = self.cache.get(&ty) {
            return Ok(l);
        }
        // A generic parameter has no size on its own; only an instantiation does.
        // Saying so here, before the recursion guard, keeps the diagnostic about
        // the real problem instead of reporting a self-referential type.
        if let crate::types::TyKind::Param { name } = self.types.kind(ty) {
            return Err(LayoutError::Unsized(format!("unsubstituted parameter `{name}` at {}", ty.0)));
        }
        // A type cannot contain itself: the recursion guard turns a malformed
        // definition into a diagnostic instead of a hang.
        if self.recursion.contains(&ty) {
            return Err(LayoutError::Unsized(format!("recursive type `{}`", self.types.display(ty))));
        }
        self.recursion.push(ty);
        let l = self.compute(ty)?;
        self.recursion.pop();
        self.cache.insert(ty, l);
        Ok(l)
    }

    fn compute(&mut self, ty: TyId) -> Result<Layout, LayoutError> {
        let kind = self.types.kind(ty).clone();
        Ok(match kind {
            TyKind::Never | TyKind::Unit => Layout::new(0, 1),
            TyKind::Bool | TyKind::I8 | TyKind::U8 => Layout::new(1, 1),
            TyKind::I16 | TyKind::U16 => Layout::new(2, 2),
            TyKind::I32 | TyKind::U32 | TyKind::F32 | TyKind::Char => Layout::new(4, 4),
            TyKind::I64 | TyKind::U64 | TyKind::Isize | TyKind::Usize | TyKind::F64 => {
                Layout::new(8, 8)
            }
            TyKind::I128 | TyKind::U128 => Layout::new(16, 16),
            // A reference, raw pointer or function pointer is one word. `str` is
            // passed as a pointer in UMAY's own convention, not as a fat pointer.
            TyKind::Ref(_) | TyKind::RefMut(_) | TyKind::RawPtr(_) | TyKind::RawPtrMut(_) | TyKind::FnPtr { .. } => {
                Layout::new(8, 8)
            }
            // `str` and `String` are a pointer plus a length once Phase 6 lands a
            // real owning string; today `str` is a single pointer to a literal and
            // this placeholder keeps the layout honest for the code that exists.
            TyKind::Str | TyKind::String => Layout::new(8, 8),
            TyKind::Slice(_) => {
                return Err(LayoutError::Unsized("slice".to_string()));
            }
            TyKind::Tuple(items) => {
                let mut offset = 0u64;
                let mut align = 1u64;
                for item in items {
                    let l = self.layout(item)?;
                    align = align.max(l.align());
                    offset = l.align_up(offset) + l.size;
                }
                let total = round_up(offset, align);
                Layout::new(total, align)
            }
            TyKind::Array { elem, len } => {
                let l = self.layout(elem)?;
                Layout::new(l.size * len, l.align())
            }
            TyKind::Adt { def, args } => self.adt_layout(def, &args)?,
            // An unconstrained literal type keeps its default: `i32` for integers
            // and `f64` for floats, matching the language specification. Reporting
            // it as unresolved would stop the backend on programs the front end
            // accepted, which is a worse failure than picking the documented
            // default.
            //
            // The slot is a full word even though the default is `i32`: the
            // selector moves an unresolved literal a word at a time, because
            // `width` has no smaller answer for it, and a four-byte slot would
            // let that store overwrite the local placed in front of it.
            TyKind::IntVar(_) => Layout::new(8, 8),
            TyKind::FloatVar(_) => Layout::new(8, 8),
            TyKind::Param { name } => {
                return Err(LayoutError::Unsized(format!("unsubstituted parameter `{name}` at {}", ty.0)));
            }

            TyKind::Error => return Err(LayoutError::Unresolved(ty)),
            // `dyn Trait` is a two-word fat pointer (ADR 0018 §3). Vtable
            // emission and dispatch are later slices; this only gives the
            // first-class type an honest size.
            // ponytail: layout only; upgrade when dyn codegen lands.
            TyKind::DynTrait { .. } => Layout::new(16, 8),
        })
    }

    /// Field offsets of a struct, in declaration order.
    pub fn field_offsets(&mut self, def: DefId, args: &[TyId]) -> Result<Vec<u64>, LayoutError> {
        let key = (def, args.to_vec());
        if let Some(v) = self.offsets.get(&key) {
            return Ok(v.clone());
        }
        let field_tys = self.struct_field_tys(def, args)?;
        let mut offset = 0u64;
        let mut offsets = Vec::with_capacity(field_tys.len());
        for ty in field_tys {
            let l = self.layout(ty)?;
            offset = l.align_up(offset);
            offsets.push(offset);
            offset += l.size;
        }
        self.offsets.insert(key, offsets.clone());
        Ok(offsets)
    }

    /// Field offsets of a specific enum variant's payload.
    pub fn variant_field_offsets(
        &mut self,
        def: DefId,
        index: u32,
        args: &[TyId],
    ) -> Result<Vec<u64>, LayoutError> {
        let tys = self.variant_field_tys(def, index, args)?;
        let mut offset = 0u64;
        let mut offsets = Vec::with_capacity(tys.len());
        for ty in tys {
            let l = self.layout(ty)?;
            offset = l.align_up(offset);
            offsets.push(offset);
            offset += l.size;
        }
        Ok(offsets)
    }

    /// The types of a struct's fields, with generic arguments substituted.
    ///
    /// `Box<i64>` stores an `i64`, not a `T`: the declaration's field types are a
    /// template, and the instantiation fills it in. Without this step the layout
    /// would be asked for the size of a parameter, which has none.
    pub fn struct_field_tys(&mut self, def: DefId, args: &[TyId]) -> Result<Vec<TyId>, LayoutError> {
        let info = self
            .hir
            .structs
            .iter()
            .find(|s| s.def_id == def)
            .ok_or(LayoutError::UnknownType(def))?;
        let declared: Vec<TyId> = info.fields.iter().map(|f| f.ty).collect();
        if info.generics.is_empty() || args.is_empty() {
            return Ok(declared);
        }
        let mut subst = std::collections::HashMap::new();
        for (name, arg) in info.generics.iter().zip(args.iter()) {
            subst.insert(name.clone(), *arg);
        }
        Ok(declared.into_iter().map(|t| self.types.substitute(t, &subst)).collect())
    }

    /// The types of a variant's fields, with the enum's generic arguments filled in.
    pub fn variant_field_tys_pub(
        &mut self,
        def: DefId,
        index: u32,
        args: &[TyId],
    ) -> Result<Vec<TyId>, LayoutError> {
        self.variant_field_tys(def, index, args)
    }

    fn variant_field_tys(
        &mut self,
        def: DefId,
        index: u32,
        args: &[TyId],
    ) -> Result<Vec<TyId>, LayoutError> {
        let info = self
            .hir
            .enums
            .iter()
            .find(|e| e.def_id == def)
            .ok_or(LayoutError::UnknownType(def))?;
        let generics = info.generics.clone();
        let variant = info.variants.get(index as usize).ok_or(LayoutError::UnknownType(def))?;
        let declared: Vec<TyId> = match &variant.fields {
            crate::hir::VariantFields::Unit => Vec::new(),
            crate::hir::VariantFields::Tuple(tys) => tys.clone(),
            crate::hir::VariantFields::Struct(fields) => fields.iter().map(|f| f.ty).collect(),
        };
        if generics.is_empty() || args.is_empty() {
            return Ok(declared);
        }
        let mut subst = std::collections::HashMap::new();
        for (name, arg) in generics.iter().zip(args.iter()) {
            subst.insert(name.clone(), *arg);
        }
        Ok(declared.into_iter().map(|t| self.types.substitute(t, &subst)).collect())
    }

    fn adt_layout(&mut self, def: DefId, args: &[TyId]) -> Result<Layout, LayoutError> {
        if self.hir.structs.iter().any(|s| s.def_id == def) {
            let offsets = self.field_offsets(def, args)?;
            let tys = self.struct_field_tys(def, args)?;
            return self.struct_from_fields(&offsets, &tys);
        }
        if self.hir.enums.iter().any(|e| e.def_id == def) {
            return self.enum_layout(def, args);
        }
        Err(LayoutError::UnknownType(def))
    }

    fn struct_from_fields(&mut self, offsets: &[u64], tys: &[TyId]) -> Result<Layout, LayoutError> {
        let mut size = 0u64;
        let mut align = 1u64;
        for (offset, ty) in offsets.iter().zip(tys) {
            let l = self.layout(*ty)?;
            align = align.max(l.align());
            size = size.max(offset + l.size);
        }
        Ok(Layout::new(round_up(size, align), align))
    }

    /// An enum is a tag followed by the payload, sized for the largest variant.
    ///
    /// This is the simple representation, not a niche optimisation. It costs
    /// space and buys correctness and a small amount of backend code, which is
    /// the right trade until there is a reason to do otherwise.
    fn enum_layout(&mut self, def: DefId, args: &[TyId]) -> Result<Layout, LayoutError> {
        let info = self
            .hir
            .enums
            .iter()
            .find(|e| e.def_id == def)
            .ok_or(LayoutError::UnknownType(def))?;
        let variant_count = info.variants.len();
        // The tag is the smallest unsigned type that can hold every variant.
        let tag = if variant_count <= 256 { Layout::new(1, 1) } else { Layout::new(4, 4) };

        let mut payload_size = 0u64;
        let mut payload_align = 1u64;
        for (index, _) in info.variants.iter().enumerate() {
            let offsets = self.variant_field_offsets(def, index as u32, args)?;
            let tys = self.variant_field_tys(def, index as u32, args)?;
            let l = self.struct_from_fields(&offsets, &tys)?;
            payload_align = payload_align.max(l.align());
            payload_size = payload_size.max(l.size);
        }

        let align = tag.align().max(payload_align);
        let payload_offset = round_up(tag.size, payload_align);
        let size = round_up(payload_offset + payload_size, align);
        Ok(Layout::new(size, align))
    }

    /// Offset of the payload inside an enum, i.e. the size of the tag rounded up
    /// to the payload's alignment.
    pub fn enum_payload_offset(&mut self, def: DefId, args: &[TyId]) -> Result<u64, LayoutError> {
        let info = self
            .hir
            .enums
            .iter()
            .find(|e| e.def_id == def)
            .ok_or(LayoutError::UnknownType(def))?;
        let tag = if info.variants.len() <= 256 { 1 } else { 4 };
        let mut payload_align = 1u64;
        for index in 0..info.variants.len() {
            let offsets = self.variant_field_offsets(def, index as u32, args)?;
            let tys = self.variant_field_tys(def, index as u32, args)?;
            let l = self.struct_from_fields(&offsets, &tys)?;
            payload_align = payload_align.max(l.align());
        }
        Ok(round_up(tag, payload_align))
    }

    /// How a value of this type is passed in the SysV AMD64 ABI.
    ///
    /// The classifier walks the type in eightbyte chunks and gives each chunk a
    /// class: `Integer` for anything that moves through general-purpose
    /// registers, `Sse` for floats, `Memory` when the value must go on the stack.
    pub fn classify(&mut self, ty: TyId) -> Result<AbiClass, LayoutError> {
        let size = self.layout(ty)?.size;
        if size > 16 {
            return Ok(AbiClass::Memory);
        }
        let mut classes: Vec<Option<EightbyteClass>> = Vec::new();
        self.classify_into(ty, &mut classes, 0)?;
        if classes.is_empty() {
            // Zero-sized: nothing to pass.
            return Ok(AbiClass::Registers(Vec::new()));
        }
        // A slot that stayed unclassed holds nothing, so it travels as integer
        // padding rather than as an SSE value.
        let classes: Vec<EightbyteClass> = classes
            .into_iter()
            .map(|c| c.unwrap_or(EightbyteClass::Integer))
            .collect();
        if classes.contains(&EightbyteClass::Memory) {
            Ok(AbiClass::Memory)
        } else {
            Ok(AbiClass::Registers(classes))
        }
    }

    /// Classifies each eightbyte of `ty`, offset by `base` bytes.
    ///
    /// `out` grows as needed; `None` means "this eightbyte has no class yet",
    /// which is distinct from `Integer` because a float must not be merged with
    /// an empty slot as though it were an integer.
    fn classify_into(
        &mut self,
        ty: TyId,
        out: &mut Vec<Option<EightbyteClass>>,
        base: u64,
    ) -> Result<(), LayoutError> {
        let kind = self.types.kind(ty).clone();
        match kind {
            TyKind::F32 | TyKind::F64 => {
                // A float starting in the upper half of its eightbyte still
                // occupies an SSE register.
                let index = (base / 8) as usize;
                grow(out, index + 1);
                out[index] = Some(merge(out[index], EightbyteClass::Sse));
            }
            TyKind::I128 | TyKind::U128 => {
                let index = (base / 8) as usize;
                grow(out, index + 2);
                out[index] = Some(merge(out[index], EightbyteClass::Integer));
                out[index + 1] = Some(merge(out[index + 1], EightbyteClass::Integer));
            }
            TyKind::Tuple(items) => {
                let mut offset = 0u64;
                for item in items {
                    let l = self.layout(item)?;
                    offset = l.align_up(offset);
                    self.classify_into(item, out, base + offset)?;
                    offset += l.size;
                }
            }
            TyKind::Array { elem, len } => {
                let l = self.layout(elem)?;
                for i in 0..len {
                    self.classify_into(elem, out, base + l.size * i)?;
                }
            }
            TyKind::Adt { def, args } => {
                if self.hir.structs.iter().any(|s| s.def_id == def) {
                    let offsets = self.field_offsets(def, &args)?;
                    let tys = self.struct_field_tys(def, &args)?;
                    for (offset, field) in offsets.iter().zip(&tys) {
                        self.classify_into(*field, out, base + offset)?;
                    }
                } else {
                    // An enum is classified as an integer: the tag and payload
                    // travel as opaque bytes for the purpose of the ABI.
                    let size = self.layout(ty)?.size;
                    let eightbytes = size.div_ceil(8).max(1) as usize;
                    grow(out, eightbytes);
                    for c in out.iter_mut() {
                        *c = Some(merge(*c, EightbyteClass::Integer));
                    }
                }
            }
            TyKind::Never | TyKind::Unit => {}
            TyKind::IntVar(_) | TyKind::FloatVar(_) | TyKind::Error => {
                return Err(LayoutError::Unresolved(ty));
            }
            _ => {
                let size = self.layout(ty)?.size;
                let eightbytes = size.div_ceil(8).max(1) as usize;
                let index = (base / 8) as usize;
                grow(out, index + eightbytes);
                for i in 0..eightbytes {
                    out[index + i] = Some(merge(out[index + i], EightbyteClass::Integer));
                }
            }
        }
        Ok(())
    }
}

/// The class of one eightbyte of an argument or return value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EightbyteClass {
    /// Passed in a general-purpose register.
    Integer,
    /// Passed in an SSE register.
    Sse,
    /// The whole value goes in memory.
    Memory,
}

impl EightbyteClass {
    pub fn is_sse(self) -> bool {
        self == EightbyteClass::Sse
    }
}

/// How a value is passed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AbiClass {
    /// Whole value in memory, on the stack.
    Memory,
    /// One class per eightbyte, in order.
    Registers(Vec<EightbyteClass>),
}

impl AbiClass {
    /// Number of general-purpose registers this value needs.
    pub fn int_registers(&self) -> usize {
        match self {
            AbiClass::Memory => 0,
            AbiClass::Registers(classes) => classes.iter().filter(|c| !c.is_sse()).count(),
        }
    }

    /// Number of SSE registers this value needs.
    pub fn sse_registers(&self) -> usize {
        match self {
            AbiClass::Memory => 0,
            AbiClass::Registers(classes) => classes.iter().filter(|c| c.is_sse()).count(),
        }
    }
}

fn grow(out: &mut Vec<Option<EightbyteClass>>, len: usize) {
    while out.len() < len {
        out.push(None);
    }
}

/// Merges an incoming class into an eightbyte that may not have one yet.
///
/// Per the ABI, if either is `Integer` the result is `Integer`, because an
/// integer write could clobber the whole eightbyte. `Sse` only survives when
/// both halves are `Sse`.
fn merge(existing: Option<EightbyteClass>, incoming: EightbyteClass) -> EightbyteClass {
    match existing {
        None => incoming,
        Some(EightbyteClass::Memory) | Some(EightbyteClass::Integer) => EightbyteClass::Integer,
        Some(EightbyteClass::Sse) if incoming == EightbyteClass::Sse => EightbyteClass::Sse,
        Some(_) => EightbyteClass::Integer,
    }
}

fn round_up(n: u64, align: u64) -> u64 {
    if align == 0 {
        return n;
    }
    n.div_ceil(align) * align
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::SourceMap;

    /// Runs `f` with layouts for a program, plus a `TyCtxt` holding one extra
    /// type interned by the caller.
    ///
    /// The extra type is built inside the closure through `l.types_mut()`, so
    /// that interning order stays deterministic and the closure reads like the
    /// language rather than like the interner.
    fn with<R>(src: &str, f: impl FnOnce(&mut Layouts) -> R) -> R {
        let mut map = SourceMap::new();
        let id = map.add("test.um", src);
        let out = crate::parse_file(&map, id);
        let sema = crate::sema::analyze(&out.program, &map);
        let mut types = sema.types;
        let mut layouts = Layouts::new(&mut types, &sema.hir);
        f(&mut layouts)
    }

    #[test]
    fn primitives_have_their_natural_layouts() {
        with("fn main() { }", |l| {
            let u8_ = l.types_mut().primitive("u8").unwrap();
            assert_eq!(l.layout(u8_).unwrap(), Layout::new(1, 1));
            let u32_ = l.types_mut().primitive("u32").unwrap();
            assert_eq!(l.layout(u32_).unwrap(), Layout::new(4, 4));
            let u64_ = l.types_mut().primitive("u64").unwrap();
            assert_eq!(l.layout(u64_).unwrap(), Layout::new(8, 8));
            let i128_ = l.types_mut().primitive("i128").unwrap();
            assert_eq!(l.layout(i128_).unwrap(), Layout::new(16, 16));
        });
    }

    #[test]
    fn struct_fields_are_padded_to_alignment() {
        with("struct S { a: u8, b: u32, c: u8 }\nfn main() { }", |l| {
            let info = l.hir().structs.iter().find(|s| s.name == "S").unwrap().clone();
            let offsets = l.field_offsets(info.def_id, &[]).unwrap();
            // a at 0, b padded to 4, c at 8, then padded to 12 for align 4.
            assert_eq!(offsets, vec![0, 4, 8]);
            let ty = l.types_mut().adt(info.def_id, Vec::new());
            assert_eq!(l.layout(ty).unwrap(), Layout::new(12, 4));
        });
    }

    #[test]
    fn tuple_layout_matches_struct_rules() {
        with("fn main() { }", |l| {
            let u8_ = l.types_mut().primitive("u8").unwrap();
            let u64_ = l.types_mut().primitive("u64").unwrap();
            let t = l.types_mut().tuple(vec![u8_, u64_]);
            assert_eq!(l.layout(t).unwrap(), Layout::new(16, 8));
        });
    }

    #[test]
    fn array_layout_multiplies_the_element() {
        with("fn main() { }", |l| {
            let u16_ = l.types_mut().primitive("u16").unwrap();
            let a = l.types_mut().array(u16_, 5);
            assert_eq!(l.layout(a).unwrap(), Layout::new(10, 2));
        });
    }

    #[test]
    fn enum_is_a_tag_plus_the_largest_payload() {
        // `u8` tag, `u64` payload: payload at offset 8, total 16, align 8.
        with("enum E { A, B(u64) }\nfn main() { }", |l| {
            // Prelude enums hold the first definition ids, so find by name.
            let def = l.hir().enums.iter().find(|e| e.name == "E").unwrap().def_id;
            let ty = l.types_mut().adt(def, Vec::new());
            assert_eq!(l.layout(ty).unwrap(), Layout::new(16, 8));
            assert_eq!(l.enum_payload_offset(def, &[]).unwrap(), 8);
        });
    }

    #[test]
    fn small_scalars_travel_in_integer_registers() {
        with("fn main() { }", |l| {
            let u32_ = l.types_mut().primitive("u32").unwrap();
            assert_eq!(
                l.classify(u32_).unwrap(),
                AbiClass::Registers(vec![EightbyteClass::Integer])
            );
        });
    }

    #[test]
    fn floats_travel_in_sse_registers() {
        with("fn main() { }", |l| {
            let f64_ = l.types_mut().primitive("f64").unwrap();
            let c = l.classify(f64_).unwrap();
            assert_eq!(c.sse_registers(), 1);
            assert_eq!(c.int_registers(), 0);
        });
    }

    #[test]
    fn a_pair_of_f32_fits_in_one_eightbyte() {
        with("fn main() { }", |l| {
            let f32_ = l.types_mut().primitive("f32").unwrap();
            let t = l.types_mut().tuple(vec![f32_, f32_]);
            let c = l.classify(t).unwrap();
            assert_eq!(c.sse_registers(), 1, "two f32 fit in one eightbyte");
        });
    }

    #[test]
    fn mixed_aggregates_merge_to_integer_eightbytes() {
        // An integer next to a float in the same eightbyte must be
        // Integer-classed, because an integer write clobbers the whole eightbyte.
        with("struct S { a: f32, b: i32 }\nfn main() { }", |l| {
            let def = l.hir().structs.iter().find(|s| s.name == "S").unwrap().def_id;
            let ty = l.types_mut().adt(def, Vec::new());
            assert_eq!(
                l.classify(ty).unwrap(),
                AbiClass::Registers(vec![EightbyteClass::Integer])
            );
        });
    }

    #[test]
    fn large_aggregates_go_to_memory() {
        with("struct Big { a: u64, b: u64, c: u64 }\nfn main() { }", |l| {
            let def = l.hir().structs.iter().find(|s| s.name == "Big").unwrap().def_id;
            let ty = l.types_mut().adt(def, Vec::new());
            assert_eq!(l.classify(ty).unwrap(), AbiClass::Memory);
        });
    }

    #[test]
    fn unsized_types_are_rejected() {
        with("fn main() { }", |l| {
            let i32_ = l.types_mut().primitive("i32").unwrap();
            let s = l.types_mut().slice(i32_);
            assert!(matches!(l.layout(s), Err(LayoutError::Unsized(_))));
        });
    }
}
