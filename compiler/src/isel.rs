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
        .map(|l| {
            // An aggregate travels in a register but is stored into a slot only as
            // wide as the value: writing a full word into a one-byte slot would
            // spill into whatever the frame placed next to it.
            let size = isel.layouts.layout(l.ty).map(|x| x.size).unwrap_or(8);
            if size <= 1 {
                RegWidth::W8
            } else if size <= 2 {
                RegWidth::W16
            } else if size <= 4 {
                RegWidth::W32
            } else {
                RegWidth::W64
            }
        })
        .collect();

    // The classification must match the caller's exactly, or an argument would
    // be read from a place nothing was written. The two rules that matter are
    // the SysV ones: an aggregate of two eightbytes or less travels in
    // registers, and once an argument goes on the stack every later one does
    // too, because the callee reads stack arguments from fixed offsets.
    let mut arg_kinds = Vec::with_capacity(body.arg_count as usize);
    let mut registers_used = usize::from(body.ret_is_sret);
    let mut spilled = false;
    for local in body.locals.iter().take(body.arg_count as usize) {
        let is_aggregate = isel.layouts.is_aggregate(local.ty);
        let size = isel.layouts.layout(local.ty).map(|l| l.size).unwrap_or(8);
        let needed = if !is_aggregate || size <= 8 {
            1
        } else if size <= 16 {
            2
        } else {
            0
        };
        if !spilled && needed > 0 && registers_used + needed <= 6 {
            arg_kinds.push(if needed == 1 {
                crate::machine::ArgKind::Register
            } else {
                crate::machine::ArgKind::RegisterPair
            });
            registers_used += needed;
        } else {
            arg_kinds.push(crate::machine::ArgKind::Stack(size));
            spilled = true;
        }
    }

    Ok(MachineFunction {
        // The body's name is unqualified; the symbol is what reaches the linker,
        // and a method's carries its type's name so it cannot take over a C
        // function of the same name.
        name: body
            .fn_id
            .and_then(|id| fn_symbols.get(&id))
            .cloned()
            .unwrap_or_else(|| body.name.clone()),
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
        sret_offset: isel.frame.sret.as_ref().map(|s| s.offset),
        arg_kinds,
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
        if let Inst::Call { target: MOperand::Label(l), .. } = &inst {
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
                Projection::EnumField { variant, index } => {
                    let (offset, field_ty) = self.enum_field_offset(current_ty, *variant, *index)?;
                    mem.offset += offset;
                    current_ty = field_ty;
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
                    // The base comes first and is pinned. Taking the index first
                    // could hand back the register the base is already in, and
                    // `(%rax,%rax,1)` addresses `base + base` rather than
                    // `base + i` -- a local array read would then fault.
                    let base_reg = self.mem_to_reg(&mem, RegWidth::W64)?;
                    let index_reg = self.operand_to_reg(by)?;
                    // The element address is computed once, into a register of its
                    // own, so that base and index never have to survive in two
                    // registers at the same time. Leaving both in a `Mem` operand
                    // let the allocator put them in one register, and
                    // `(%rax,%rax,1)` addresses `base + base` -- a local array read
                    // then faults.
                    let addr = self.vreg();
                    self.emit(Inst::Lea {
                        src: Mem::base(base_reg).indexed(index_reg, scale_for(size)),
                        dst: MOperand::Reg(addr),
                    });
                    mem = Mem::base(addr).with_offset(0);
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
        // A narrow load must zero the upper bits: writing `%al` leaves the rest of
        // `%rax` untouched, so a byte read straight into `%al` and then compared as
        // a word would compare whatever the register held before. `W32` needs no
        // extension because a 32-bit register write clears the top half on its own.
        //
        // A signed widening is a different matter and belongs to `cast`, which
        // knows both types: sign-extending here would make every `i32` read out to
        // 64 bits, so `-1i32` would become `4294967295`.
        let kind = match width {
            RegWidth::W8 => MoveKind::ZeroExtend { from: RegWidth::W8, to: RegWidth::W64 },
            RegWidth::W16 => MoveKind::ZeroExtend { from: RegWidth::W16, to: RegWidth::W64 },
            other => MoveKind::Move(other),
        };
        self.emit(Inst::Load { kind, src: mem.clone(), dst: MOperand::Reg(dst) });
        Ok(dst)
    }

    /// The place to read a function pointer from, when the projection ends in
    /// one dereference of a function pointer.
    ///
    /// A `*const fn(..)` is not a pointer to a pointer to code; it is a pointer
    /// to code. Dereferencing it once names the function, so the value to load is
    /// the pointer itself.
    pub(crate) fn fn_ptr_place(&mut self, place: &Place) -> Option<Place> {
        let last = place.projection.last()?;
        if !matches!(last, Projection::Deref) {
            return None;
        }
        let mut probe = Place { local: place.local, projection: place.projection[..place.projection.len() - 1].to_vec() };
        let ty = self.place_ty(&probe);
        if matches!(self.types().kind_owned(ty), TyKind::RawPtr(inner) | TyKind::Ref(inner) | TyKind::RefMut(inner) if matches!(self.types().kind_owned(inner), TyKind::FnPtr { .. })) {
            return Some(probe);
        }
        None
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
                // One level of dereference on a function pointer is already
                // applied. A `fn` value is the address of its code, so the place
                // holds the address itself; loading what it points at would fetch
                // the first instruction byte and call that as an address.
                if let Some(inner) = self.fn_ptr_place(place) {
                    let mem = self.place_addr(&inner)?;
                    return self.load_place_mem(&mem, ty);
                }
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
            Operand::Constant(Constant::Fn(id)) => {
                // A function used as a value is its address. Taking it is what
                // lets a function be handed to another function -- a thread's
                // entry point, a callback -- which previously had no form.
                let name = crate::isel::term::term_symbol_for(*id, self.fn_symbols);
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

    /// The byte offset and type of a field inside an enum variant's payload.
    fn enum_field_offset(&mut self, base_ty: TyId, variant: u32, index: u32) -> Result<(i64, TyId), String> {
        let TyKind::Adt { def, args } = self.types().kind_owned(base_ty) else {
            return Err(format!("enum field access on non-adt"));
        };
        let payload = self
            .layouts
            .enum_payload_offset(def, &args)
            .map_err(|e| format!("{e:?}"))?;
        let offsets = self
            .layouts
            .variant_field_offsets(def, variant, &args)
            .map_err(|e| format!("{e:?}"))?;
        let tys = self
            .layouts
            .variant_field_tys_pub(def, variant, &args)
            .map_err(|e| format!("{e:?}"))?;
        let field = *offsets.get(index as usize).ok_or_else(|| format!("field {index} out of range"))?;
        let ty = *tys.get(index as usize).ok_or_else(|| format!("field {index} out of range"))?;
        Ok(((payload + field) as i64, ty))
    }

    /// The type of a place.
    pub(crate) fn place_ty(&mut self, place: &Place) -> TyId {
        let mut ty = self.frame.slot(place.local).ty;
        for projection in &place.projection {
            ty = match projection {
                Projection::Field(index) => self.field_ty(ty, *index).unwrap_or(ty),
                Projection::EnumField { variant, index } => self
                    .enum_field_offset(ty, *variant, *index)
                    .map(|(_, t)| t)
                    .unwrap_or(ty),
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
