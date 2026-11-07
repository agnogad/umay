//! Instruction selection: MIR to machine instructions.
//!
//! The selector is a straightforward tree walk over the MIR bodies. Each MIR
//! local already has a stack slot from [`crate::frame`], so a `Place` becomes a
//! memory operand and an `Operand` becomes either a register read, a memory
//! read, or an immediate.
//!
//! Selection intentionally produces *unoptimised* but correct code: results are
//! computed into fresh virtual registers and stored to their slots. Register
//! allocation removes most of those moves afterwards, and doing it that way
//! keeps this file readable and auditable, which matters more than generating
//! tight code on the first pass.

use crate::frame::Frame;
use crate::hir::LocalId;
use crate::layout::Layouts;
use crate::machine::{
    AluOp, Cond, Inst, Label, MachineBlock, MachineFunction, Mem, MoveKind, Reg, RegWidth,
};

/// The machine-level operand, kept distinct from the MIR operand so that a
/// mis-typed `Operand` cannot silently mean the wrong layer.
pub(crate) use crate::machine::Operand as MOperand;
use crate::mir::*;
use crate::types::{TyCtxt, TyId, TyKind};

pub(crate) mod term;
pub(crate) mod stmt;

/// State for selecting one function.
pub struct Isel<'a, 't> {
    pub(crate) body: &'a Body,
    pub(crate) frame: Frame,
    pub(crate) layouts: &'a mut Layouts<'t>,
    /// Blocks emitted so far.
    pub(crate) blocks: Vec<MachineBlock>,
    /// Next virtual register number.
    pub(crate) next_vreg: u32,
    /// Label for each MIR block.
    pub(crate) block_labels: Vec<Label>,
    /// Symbols referenced, for relocations.
    pub(crate) symbols: Vec<String>,
    /// The symbol each function id is called by, so a `Constant::Fn` callee
    /// becomes a direct call instead of an unsupported indirect one.
    pub(crate) fn_symbols: &'a std::collections::HashMap<crate::hir::FnId, String>,
}

/// Selects instructions for `body`.
pub fn select<'t>(
    body: &Body,
    frame: Frame,
    layouts: &mut Layouts<'t>,
    fn_symbols: &std::collections::HashMap<crate::hir::FnId, String>,
) -> Result<MachineFunction, String> {
    let block_labels: Vec<Label> = (0..body.blocks.len()).map(|i| Label(i as u32)).collect();
    let mut isel = Isel {
        body,
        frame,
        layouts,
        blocks: Vec::new(),
        next_vreg: 0,
        block_labels,
        symbols: Vec::new(),
        fn_symbols,
    };
    for (index, block) in body.blocks.iter().enumerate() {
        // Unreachable blocks were emptied by optimisation. Emitting them would
        // make the assembler complain about duplicate or dead labels, so they
        // are skipped; nothing jumps to them.
        let id = BlockId(index as u32);
        if !body.is_reachable(id) {
            continue;
        }
        isel.blocks.push(MachineBlock {
            label: isel.block_labels[index],
            insts: vec![Inst::Label(isel.block_labels[index])],
        });
        for statement in &block.statements {
            isel.statement(statement)?;
        }
        let mut term_insts = Vec::new();
        isel.terminator(&block.terminator, &mut term_insts)?;
        isel.emit_all(term_insts);
    }

    let arg_widths: Vec<RegWidth> = body
        .locals
        .iter()
        .take(body.arg_count as usize)
        .map(|l| match isel.width(l.ty) {
            Ok(w) => w,
            Err(_) => RegWidth::W64,
        })
        .collect();

    Ok(MachineFunction {
        name: body.name.clone(),
        blocks: isel.blocks,
        virtual_regs: Vec::new(),
        stack_size: isel.frame.local_bytes,
        ret_ty: body.ret_ty,
        local_slots: isel
            .frame
            .slots
            .iter()
            .enumerate()
            .map(|(i, s)| (LocalId(i as u32), s.offset))
            .collect(),
        symbol_refs: isel.symbols,
        arg_count: body.arg_count,
        block_count: body.blocks.len() as u32,
        arg_widths,
    })
}

impl<'a, 't> Isel<'a, 't> {
    /// The type context, read through the layout table that owns it.
    pub(crate) fn types(&self) -> &TyCtxt {
        self.layouts.types_ref()
    }

    /// A fresh virtual register.
    pub(crate) fn vreg(&mut self) -> Reg {
        let r = Reg::Virtual(self.next_vreg);
        self.next_vreg += 1;
        r
    }

    pub(crate) fn emit(&mut self, inst: Inst) {
        if let Inst::Call { target: MOperand::Label(l) } = &inst {
            self.note_call_target(*l);
        }
        if let Some(block) = self.blocks.last_mut() {
            block.insts.push(inst);
        }
    }

    pub(crate) fn emit_all(&mut self, insts: Vec<Inst>) {
        for inst in insts {
            self.emit(inst);
        }
    }

    /// Records that a call target symbol is referenced.
    pub(crate) fn note_call_target(&mut self, _label: Label) {
        // Direct calls are emitted as symbol references, recorded in
        // `call_symbol` before the call is emitted.
    }

    /// Records a symbol this function refers to.
    pub(crate) fn note_symbol(&mut self, name: &str) {
        if !self.symbols.iter().any(|s| s == name) {
            self.symbols.push(name.to_string());
        }
    }

    /// The width to load or store a type at.
    pub(crate) fn width(&mut self, ty: TyId) -> Result<RegWidth, String> {
        let kind = self.types().kind(ty).clone();
        Ok(match kind {
            TyKind::Bool | TyKind::I8 | TyKind::U8 => RegWidth::W8,
            TyKind::I16 | TyKind::U16 => RegWidth::W16,
            TyKind::I32 | TyKind::U32 | TyKind::Char | TyKind::F32 => RegWidth::W32,
            TyKind::Never | TyKind::Unit => RegWidth::W8,
            // Everything else, including aggregates and references, is moved a
            // word at a time by the selector; aggregates are copied bytewise by
            // the caller where that matters.
            _ => RegWidth::W64,
        })
    }

    /// True when the type is an aggregate handled bytewise rather than as a
    /// register-sized scalar.
    pub(crate) fn is_aggregate(&mut self, ty: TyId) -> bool {
        let size = self.layouts.layout(ty).map(|l| l.size).unwrap_or(0);
        size > 8 || matches!(self.types().kind(ty), TyKind::Tuple(_) | TyKind::Array { .. } | TyKind::Adt { .. })
    }

    /// The byte offset of a field within its parent type.
    pub(crate) fn field_offset(&mut self, base_ty: TyId, index: u32) -> Result<i64, String> {
        match self.types().kind_owned(base_ty) {
            TyKind::Tuple(items) => {
                let mut offset = 0u64;
                for (i, item) in items.iter().enumerate() {
                    let l = self.layouts.layout(*item).map_err(|e| format!("{e:?}"))?;
                    offset = l.align_up(offset);
                    if i == index as usize {
                        return Ok(offset as i64);
                    }
                    offset += l.size;
                }
                Err(format!("tuple field {index} out of range"))
            }
            TyKind::Adt { def, args } => {
                let offsets = self.layouts.field_offsets(def, &args).map_err(|e| format!("{e:?}"))?;
                offsets
                    .get(index as usize)
                    .map(|o| *o as i64)
                    .ok_or_else(|| format!("field {index} out of range"))
            }
            other => Err(format!("cannot take a field of {other:?}")),
        }
    }

    /// The type of a field within its parent type.
    pub(crate) fn field_ty(&self, base_ty: TyId, index: u32) -> Result<TyId, String> {
        match self.types().kind_owned(base_ty) {
            TyKind::Tuple(items) => items
                .get(index as usize)
                .copied()
                .ok_or_else(|| format!("tuple field {index} out of range")),
            TyKind::Adt { def, .. } => {
                let info = self
                    .layouts
                    .hir()
                    .structs
                    .iter()
                    .find(|s| s.def_id == def)
                    .ok_or_else(|| format!("unknown struct {def:?}"))?;
                info.fields
                    .get(index as usize)
                    .map(|f| f.ty)
                    .ok_or_else(|| format!("field {index} out of range"))
            }
            other => Err(format!("cannot take a field of {other:?}")),
        }
    }

    /// The memory address of a place, as an operand usable in a load or store.
    ///
    /// Handles field and constant-index projections directly. A `Deref` or a
    /// runtime index needs a register, so those are computed into one first.
    pub(crate) fn place_addr(&mut self, place: &Place) -> Result<Mem, String> {
        let slot = self.frame.slot(place.local);
        let mut mem = Mem::stack(slot.offset);
        let mut current_ty = slot.ty;

        for projection in &place.projection {
            match projection {
                Projection::Field(index) => {
                    let offset = self.field_offset(current_ty, *index)?;
                    mem.offset += offset;
                    current_ty = self.field_ty(current_ty, *index)?;
                }
                Projection::Deref => {
                    // Load the pointer, then use it as the base.
                    let ptr = self.load_place_mem(&mem, current_ty)?;
                    mem = Mem::base(ptr);
                    current_ty = match self.types().kind_owned(current_ty) {
                        TyKind::Ref(inner) | TyKind::RefMut(inner) => inner,
                        TyKind::RawPtr(inner) | TyKind::RawPtrMut(inner) => inner,
                        other => return Err(format!("deref of non-pointer {other:?}")),
                    };
                }
                Projection::Index(by) => {
                    let elem_ty = match self.types().kind_owned(current_ty) {
                        TyKind::Array { elem, .. } => elem,
                        TyKind::Slice(elem) => elem,
                        other => return Err(format!("index of non-array {other:?}")),
                    };
                    let size = self.layouts.layout(elem_ty).map_err(|e| format!("{e:?}"))?.size;
                    let index_reg = self.operand_to_reg(by)?;
                    let base_reg = self.mem_to_reg(&mem, RegWidth::W64)?;
                    let scaled = Mem::base(base_reg).indexed(index_reg, scale_for(size));
                    mem = Mem::base(scaled.base.unwrap()).with_offset(0);
                    mem.index = scaled.index;
                    current_ty = elem_ty;
                }
            }
        }
        Ok(mem)
    }

    /// Loads a value from `mem` into a fresh register.
    pub(crate) fn load_place_mem(&mut self, mem: &Mem, ty: TyId) -> Result<Reg, String> {
        let width = self.width(ty)?;
        let dst = self.vreg();
        // The load happens at the value's own width. A narrower register write
        // already clears the upper half, so a sub-word load is clean without an
        // explicit extension. Sign or zero *extension* is a separate concern and
        // belongs to `cast`, which knows both types: widening here would make
        // every `i32` read out as a non-negative 64-bit value, so `-1i32` would
        // come back as `4294967295` and any later sign-aware arithmetic would be
        // wrong.
        self.emit(Inst::Load {
            kind: MoveKind::Move(width),
            src: mem.clone(),
            dst: MOperand::Reg(dst),
        });
        Ok(dst)
    }

    /// Materialises `mem`'s address into a register.
    pub(crate) fn mem_to_reg(&mut self, mem: &Mem, _width: RegWidth) -> Result<Reg, String> {
        if let (Some(base), None) = (mem.base, mem.index) {
            if mem.offset == 0 && mem.symbol.is_none() {
                return Ok(base);
            }
        }
        let dst = self.vreg();
        self.emit(Inst::Lea { src: mem.clone(), dst: MOperand::Reg(dst) });
        Ok(dst)
    }

    /// Reads an operand into a register, loading from memory when needed.
    pub(crate) fn operand_to_reg(&mut self, op: &Operand) -> Result<Reg, String> {
        match op {
            Operand::Copy(place) | Operand::Move(place) => {
                let ty = self.place_ty(place);
                let mem = self.place_addr(place)?;
                self.load_place_mem(&mem, ty)
            }
            Operand::Constant(Constant::Str(id)) => {
                // A `&str` is the address of a NUL-terminated buffer in
                // `.rodata`. `lea` computes that address without reading memory.
                let name = self.str_symbol(*id);
                let dst = self.vreg();
                self.emit(Inst::Lea {
                    src: Mem::symbol(&name),
                    dst: MOperand::Reg(dst),
                });
                Ok(dst)
            }
            Operand::Constant(c) => {
                let (value, ty) = constant_value(c)?;
                let dst = self.vreg();
                let width = self.width(ty)?;
                self.emit(Inst::Move {
                    kind: MoveKind::Move(width),
                    src: MOperand::Imm(value),
                    dst: MOperand::Reg(dst),
                });
                Ok(dst)
            }
        }
    }

    /// An operand suitable for a read-only instruction input: a register, an
    /// immediate, or directly a memory reference.
    pub(crate) fn operand(&mut self, op: &Operand) -> Result<MOperand, String> {
        match op {
            Operand::Constant(Constant::Str(_)) => {
                // A string is an address, so it needs a register even when an
                // immediate would be accepted.
                Ok(MOperand::Reg(self.operand_to_reg(op)?))
            }
            Operand::Constant(c) => {
                let (value, _) = constant_value(c)?;
                Ok(MOperand::Imm(value))
            }
            Operand::Copy(place) | Operand::Move(place) => {
                let mem = self.place_addr(place)?;
                Ok(MOperand::Mem(mem))
            }
        }
    }

    /// The type of a place.
    pub(crate) fn place_ty(&mut self, place: &Place) -> TyId {
        let mut ty = self.frame.slot(place.local).ty;
        for projection in &place.projection {
            ty = match projection {
                Projection::Field(index) => self.field_ty(ty, *index).unwrap_or(ty),
                Projection::Deref => match self.types().kind_owned(ty) {
                    TyKind::Ref(inner) | TyKind::RefMut(inner) => inner,
                    TyKind::RawPtr(inner) | TyKind::RawPtrMut(inner) => inner,
                    _ => ty,
                },
                Projection::Index(_) => match self.types().kind_owned(ty) {
                    TyKind::Array { elem, .. } => elem,
                    TyKind::Slice(elem) => elem,
                    _ => ty,
                },
            };
        }
        ty
    }

    /// The assembly label of a string literal in the program's string table.
    pub(crate) fn str_symbol(&mut self, id: StringId) -> String {
        let name = format!(".Lstr_{}", id.0);
        if !self.symbols.contains(&name) {
            self.symbols.push(name.clone());
        }
        name
    }

    /// Stores `src` to a place, appending to `out` instead of the current block.
    ///
    /// The terminator selector buffers its instructions so a call and the store
    /// of its result stay together; writing directly to the block would place the
    /// store before the call.
    pub(crate) fn store_place_into(
        &mut self,
        place: &Place,
        src: MOperand,
        out: &mut Vec<Inst>,
    ) -> Result<(), String> {
        let ty = self.place_ty(place);
        let mem = self.place_addr(place)?;
        let width = self.width(ty)?;
        out.push(Inst::Store { kind: MoveKind::Move(width), src, dst: mem });
        Ok(())
    }

    /// Stores `src` to a place.
    pub(crate) fn store_place(&mut self, place: &Place, src: MOperand) -> Result<(), String> {
        let ty = self.place_ty(place);
        let mem = self.place_addr(place)?;
        let width = self.width(ty)?;
        self.emit(Inst::Store { kind: MoveKind::Move(width), src, dst: mem });
        Ok(())
    }
}

/// The scale exponent x86 addressing supports: 1, 2, 4 or 8 bytes.
pub(crate) fn scale_for(size: u64) -> u8 {
    match size {
        1 => 0,
        2 => 1,
        4 => 2,
        _ => 3,
    }
}

/// The value of a constant, and the type it should be materialised at.
pub(crate) fn constant_value(c: &Constant) -> Result<(i64, TyId), String> {
    Ok(match c {
        Constant::Int { value, ty } => (*value as i64, *ty),
        Constant::Uint { value, ty } => (*value as i64, *ty),
        Constant::Bool(b) => (*b as i64, TyId(0)),
        Constant::Char(ch) => (*ch as i64, TyId(0)),
        Constant::Float { .. } => return Err("float constants are not supported yet".into()),
        Constant::Unit => (0, TyId(0)),
        other => return Err(format!("constant {other:?} needs a symbol, handled elsewhere")),
    })
}
