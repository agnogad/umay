//! Instruction selection for MIR statements and rvalues.

use super::*;
use crate::hir::LocalId;
use crate::machine::{MoveKind, RegWidth};

impl<'a, 't> Isel<'a, 't> {
    /// Selects one statement.
    pub(crate) fn statement(&mut self, statement: &Statement) -> Result<(), String> {
        match statement {
            Statement::Assign { place, value, .. } => self.assign(place, value),
            // Storage markers and drop flags are bookkeeping for earlier passes
            // and produce no code.
            Statement::StorageLive(_)
            | Statement::StorageDead(_)
            | Statement::SetDropFlag { .. }
            | Statement::Nop => Ok(()),
        }
    }

    /// Selects `place = value`.
    fn assign(&mut self, place: &Place, value: &Rvalue) -> Result<(), String> {
        let ty = self.place_ty(place);
        match value {
            Rvalue::Use(op) => {
                if self.is_aggregate(ty) {
                    return self.copy_bytes(place, op, ty);
                }
                let src = self.operand(op)?;
                self.store_place(place, src)
            }
            // `&place` is the address of the place.
            Rvalue::Ref { place: inner, .. } | Rvalue::Reborrow { place: inner, .. } => {
                let addr = self.place_addr(inner)?;
                let reg = self.mem_to_reg(&addr, RegWidth::W64)?;
                self.store_place(place, MOperand::Reg(reg))
            }
            Rvalue::Binary { op, lhs, rhs } => self.binary(place, *op, lhs, rhs, ty),
            Rvalue::Unary { op, operand } => self.unary(place, *op, operand, ty),
            Rvalue::Cast { operand, to } => self.cast(place, operand, *to),
            Rvalue::Aggregate { kind, operands } => self.aggregate(place, kind, operands, ty),
            Rvalue::Len(inner) => {
                let inner_ty = self.place_ty(inner);
                let len = match self.types().kind_owned(inner_ty) {
                    TyKind::Array { len, .. } => len,
                    other => return Err(format!("len of non-array {other:?}")),
                };
                self.store_place(place, MOperand::Imm(len as i64))
            }
            Rvalue::Discriminant(inner) => {
                let mem = self.place_addr(inner)?;
                // The tag is the first byte of the enum.
                let inner_ty = self.place_ty(inner);
                let tag = self.load_place_mem(&mem, inner_ty)?;
                let dst = self.vreg();
                self.emit(Inst::Move {
                    kind: MoveKind::ZeroExtend { from: RegWidth::W8, to: RegWidth::W64 },
                    src: MOperand::Reg(tag),
                    dst: MOperand::Reg(dst),
                });
                self.store_place(place, MOperand::Reg(dst))
            }
            Rvalue::PtrToInt(op) => {
                let reg = self.operand_to_reg(op)?;
                self.store_place(place, MOperand::Reg(reg))
            }
            Rvalue::PtrOffset { ptr, count } => {
                // `lea base + count * elem_size` computes the address in one
                // instruction. The element size must be 1, 2, 4 or 8 to be an
                // addressing-mode scale; anything else is multiplied first.
                let base = self.operand_to_reg(ptr)?;
                let elem = match self.types().kind_owned(ty) {
                    TyKind::RawPtr(inner) | TyKind::RawPtrMut(inner) => inner,
                    other => return Err(format!("offset of non-pointer {other:?}")),
                };
                let elem_size = self.layouts.layout(elem).map_err(|e| format!("{e:?}"))?.size;
                let index = match count {
                    Operand::Constant(c) => {
                        let (value, _) = crate::isel::constant_value(c)?;
                        MOperand::Imm(value * elem_size as i64)
                    }
                    _ => MOperand::Reg(self.operand_to_reg(count)?),
                };
                let dst = self.vreg();
                match index {
                    MOperand::Imm(byte_offset) => {
                        self.emit(Inst::Lea {
                            src: Mem { base: Some(base), offset: byte_offset, index: None, symbol: None },
                            dst: MOperand::Reg(dst),
                        });
                    }
                    MOperand::Reg(index_reg) => {
                        // Addressing modes scale by 1, 2, 4 or 8 only. A larger
                        // element needs an explicit multiply first.
                        if matches!(elem_size, 1 | 2 | 4 | 8) {
                            self.emit(Inst::Lea {
                                src: Mem {
                                    base: Some(base),
                                    offset: 0,
                                    index: Some((index_reg, crate::isel::scale_for(elem_size))),
                                    symbol: None,
                                },
                                dst: MOperand::Reg(dst),
                            });
                        } else {
                            self.emit(Inst::Alu {
                                op: AluOp::Imul,
                                width: RegWidth::W64,
                                src: MOperand::Imm(elem_size as i64),
                                dst: MOperand::Reg(index_reg),
                            });
                            self.emit(Inst::Lea {
                                src: Mem {
                                    base: Some(base),
                                    offset: 0,
                                    index: Some((index_reg, 0)),
                                    symbol: None,
                                },
                                dst: MOperand::Reg(dst),
                            });
                        }
                    }
                    MOperand::Mem(_) | MOperand::Label(_) => {
                        // `index` is built above as an immediate or a register,
                        // so neither of these can occur.
                        return Err("pointer offset with a non-scalar count".into());
                    }
                }
                self.store_place(place, MOperand::Reg(dst))
            }
            Rvalue::Repeat { operand, count } => {
                // `[v; n]` writes `v` into each element in turn.
                let elem_ty = match self.types().kind_owned(ty) {
                    TyKind::Array { elem, .. } => elem,
                    other => return Err(format!("repeat of non-array {other:?}")),
                };
                let elem_size = self.layouts.layout(elem_ty).map_err(|e| format!("{e:?}"))?.size;
                let src = self.operand_to_reg(operand)?;
                let base = self.place_addr(place)?;
                for i in 0..*count {
                    let mut mem = base.clone();
                    mem.offset += (i * elem_size) as i64;
                    let width = self.width(elem_ty)?;
                    self.emit(Inst::Store {
                        kind: MoveKind::Move(width),
                        src: MOperand::Reg(src),
                        dst: mem,
                    });
                }
                Ok(())
            }
        }
    }

    /// Copies an aggregate value byte by byte.
    ///
    /// Aggregates are not kept in registers, so a move between two of them is a
    /// memory-to-memory copy through one scratch register. Eight-byte chunks are
    /// used where possible, then the remainder a byte at a time.
    fn copy_bytes(&mut self, dst: &Place, src: &Operand, ty: TyId) -> Result<(), String> {
        let size = self.layouts.layout(ty).map_err(|e| format!("{e:?}"))?.size;
        let src_mem = match src {
            Operand::Copy(p) | Operand::Move(p) => self.place_addr(p)?,
            Operand::Constant(_) => return Err("aggregate constant is not supported".into()),
        };
        let dst_mem = self.place_addr(dst)?;
        let scratch = self.vreg();
        let mut offset = 0u64;
        while offset + 8 <= size {
            let mut s = src_mem.clone();
            s.offset += offset as i64;
            let mut d = dst_mem.clone();
            d.offset += offset as i64;
            self.emit(Inst::Load { kind: MoveKind::Move(RegWidth::W64), src: s, dst: MOperand::Reg(scratch) });
            self.emit(Inst::Store { kind: MoveKind::Move(RegWidth::W64), src: MOperand::Reg(scratch), dst: d });
            offset += 8;
        }
        while offset < size {
            let mut s = src_mem.clone();
            s.offset += offset as i64;
            let mut d = dst_mem.clone();
            d.offset += offset as i64;
            self.emit(Inst::Load { kind: MoveKind::Move(RegWidth::W8), src: s, dst: MOperand::Reg(scratch) });
            self.emit(Inst::Store { kind: MoveKind::Move(RegWidth::W8), src: MOperand::Reg(scratch), dst: d });
            offset += 1;
        }
        Ok(())
    }

    /// Copies `size` bytes from `src` to the address in `dst_ptr`.
    ///
    /// Used when a value leaves the function through a caller-supplied pointer,
    /// where the destination is an address in a register rather than a stack
    /// slot this frame owns.
    pub(crate) fn copy_bytes_to_pointer(
        &mut self,
        src: &Mem,
        dst_ptr: Reg,
        size: u64,
        out: &mut Vec<Inst>,
    ) -> Result<(), String> {
        let scratch = self.vreg();
        let mut offset = 0u64;
        while offset + 8 <= size {
            let mut s = src.clone();
            s.offset += offset as i64;
            let mut d = Mem::base(dst_ptr);
            d.offset = offset as i64;
            out.push(Inst::Load {
                kind: MoveKind::Move(RegWidth::W64),
                src: s,
                dst: MOperand::Reg(scratch),
            });
            out.push(Inst::Store {
                kind: MoveKind::Move(RegWidth::W64),
                src: MOperand::Reg(scratch),
                dst: d,
            });
            offset += 8;
        }
        while offset < size {
            let mut s = src.clone();
            s.offset += offset as i64;
            let mut d = Mem::base(dst_ptr);
            d.offset = offset as i64;
            out.push(Inst::Load {
                kind: MoveKind::Move(RegWidth::W8),
                src: s,
                dst: MOperand::Reg(scratch),
            });
            out.push(Inst::Store {
                kind: MoveKind::Move(RegWidth::W8),
                src: MOperand::Reg(scratch),
                dst: d,
            });
            offset += 1;
        }
        Ok(())
    }

    /// Selects a binary operation.
    fn binary(
        &mut self,
        dst: &Place,
        op: BinOp,
        lhs: &Operand,
        rhs: &Operand,
        ty: TyId,
    ) -> Result<(), String> {
        let width = self.binop_width(op, ty, lhs)?;
        // Signedness comes from the operands, not the result: a comparison of two
        // `i64`s produces a `bool`, and reading signedness from `bool` selects the
        // unsigned condition codes, making every negative number compare as huge.
        let operand_ty = self.operand_ty(lhs);
        let signed = self.types().is_signed(operand_ty);

        // Comparisons produce a bool and are handled separately.
        if matches!(op, BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge) {
            let left = self.operand_to_reg(lhs)?;
            let right = self.operand_to_reg(rhs)?;
            self.emit(Inst::Cmp {
                width,
                lhs: MOperand::Reg(left),
                rhs: MOperand::Reg(right),
            });
            let cond = match op {
                BinOp::Eq => Cond::Eq,
                BinOp::Ne => Cond::Ne,
                BinOp::Lt if signed => Cond::Lt,
                BinOp::Lt => Cond::B,
                BinOp::Le if signed => Cond::Le,
                BinOp::Le => Cond::Be,
                BinOp::Gt if signed => Cond::Gt,
                BinOp::Gt => Cond::A,
                BinOp::Ge if signed => Cond::Ge,
                BinOp::Ge => Cond::Ae,
                _ => unreachable!("comparisons handled above"),
            };
            let result = self.vreg();
            // `setcc` writes a byte; clearing the register first makes the upper
            // bits zero so the value is a well-formed 0 or 1.
            let wide = self.vreg();
            self.emit(Inst::Move {
                kind: MoveKind::Move(RegWidth::W32),
                src: MOperand::Imm(0),
                dst: MOperand::Reg(wide),
            });
            let byte = self.byte_of(wide);
            self.emit(Inst::SetCc { cond, dst: MOperand::Reg(byte) });
            self.emit(Inst::Move {
                kind: MoveKind::ZeroExtend { from: RegWidth::W8, to: RegWidth::W32 },
                src: MOperand::Reg(wide),
                dst: MOperand::Reg(result),
            });
            return self.store_place(dst, MOperand::Reg(result));
        }

        let left = self.operand_to_reg(lhs)?;
        // `imul` needs its destination in a register; everything else can take a
        // memory or immediate right operand.
        let right = self.operand(rhs)?;
        let alu = match op {
            BinOp::Add => AluOp::Add,
            BinOp::Sub => AluOp::Sub,
            BinOp::Mul => AluOp::Imul,
            BinOp::Div | BinOp::Rem => {
                return self.divide(dst, op, left, right, width, signed, ty);
            }
            BinOp::BitAnd => AluOp::And,
            BinOp::BitOr => AluOp::Or,
            BinOp::BitXor => AluOp::Xor,
            BinOp::Shl => AluOp::Shl,
            BinOp::Shr if signed => AluOp::Sar,
            BinOp::Shr => AluOp::Shr,
            BinOp::LogicalAnd | BinOp::LogicalOr => {
                return Err("short-circuit operators must be lowered to branches".into())
            }
            _ => unreachable!("comparisons handled above"),
        };

        // Variable shift counts must live in `cl`.
        if matches!(op, BinOp::Shl | BinOp::Shr) {
            return self.shift(dst, alu, left, rhs, width);
        }

        let result = self.vreg();
        self.emit(Inst::Move {
            kind: MoveKind::Move(width),
            src: MOperand::Reg(left),
            dst: MOperand::Reg(result),
        });
        self.emit(Inst::Alu { op: alu, width, src: right, dst: MOperand::Reg(result) });
        self.store_place(dst, MOperand::Reg(result))
    }

    /// The width a binary operation is performed at.
    ///
    /// Arithmetic is done at the type's own width so that wrapping matches the
    /// language definition; comparing at 64 bits would hide overflow.
    /// The width a binary operation is performed at.
    ///
    /// For a comparison this is the width of the *operands*, not of the result:
    /// comparing two `i64`s must compare eight bytes even though the answer is a
    /// single `bool`. Getting this wrong silently compares only the low byte.
    fn binop_width(&mut self, op: BinOp, ty: TyId, lhs: &Operand) -> Result<RegWidth, String> {
        if matches!(op, BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge) {
            let operand_ty = self.operand_ty(lhs);
            return self.width(operand_ty);
        }
        self.width(ty)
    }

    /// A byte-width alias of a register, for `setcc`.
    fn byte_of(&mut self, reg: Reg) -> Reg {
        reg
    }

    /// Selects `x / y` and `x % y`.
    ///
    /// x86 divides `rdx:rax` by its operand, so the left operand must be moved
    /// into the accumulator and the high half sign- or zero-extended first.
    /// Division by zero raises `SIGFPE`, which is the behaviour the language
    /// specifies, so no explicit check is emitted.
    #[allow(clippy::too_many_arguments)] // each name is one semantic choice
    fn divide(
        &mut self,
        dst: &Place,
        op: BinOp,
        _left: Reg,
        right: MOperand,
        width: RegWidth,
        signed: bool,
        ty: TyId,
    ) -> Result<(), String> {
        let divisor = match right {
            MOperand::Imm(v) => {
                let reg = self.vreg();
                self.emit(Inst::Move { kind: MoveKind::Move(width), src: MOperand::Imm(v), dst: MOperand::Reg(reg) });
                reg
            }
            MOperand::Reg(r) => r,
            MOperand::Mem(_) | MOperand::Label(_) => {
                // `idiv` accepts a memory operand, but keeping one path is
                // simpler and the allocator removes the extra move.
                let reg = self.vreg();
                self.emit(Inst::Load {
                    kind: MoveKind::Move(width),
                    src: match right {
                        MOperand::Mem(m) => m,
                        _ => return Err("unsupported divisor".into()),
                    },
                    dst: MOperand::Reg(reg),
                });
                reg
            }
        };

        let acc = Reg::Phys(0); // rax
        let high = Reg::Phys(2); // rdx
        self.emit(Inst::Move {
            kind: MoveKind::Move(width),
            src: MOperand::Reg(_left),
            dst: MOperand::Reg(acc),
        });
        if signed {
            self.emit(Inst::SignExtendAcc { width });
        } else {
            // The high half must be cleared at full width; a 32-bit write would
            // leave the upper 32 bits of `rdx` holding whatever was there.
            self.emit(Inst::Move {
                kind: MoveKind::Move(RegWidth::W64),
                src: MOperand::Imm(0),
                dst: MOperand::Reg(high),
            });
        }
        self.emit(Inst::Alu {
            op: if signed { AluOp::Idiv } else { AluOp::Div },
            width,
            src: MOperand::Reg(divisor),
            dst: MOperand::Reg(acc),
        });
        let result = if matches!(op, BinOp::Rem) { high } else { acc };
        let out = self.vreg();
        self.emit(Inst::Move {
            kind: MoveKind::Move(width),
            src: MOperand::Reg(result),
            dst: MOperand::Reg(out),
        });
        let _ = ty;
        self.store_place(dst, MOperand::Reg(out))
    }

    /// Selects a shift, moving the count into `cl` first.
    fn shift(&mut self, dst: &Place, alu: AluOp, left: Reg, rhs: &Operand, width: RegWidth) -> Result<(), String> {
        let count = self.operand_to_reg(rhs)?;
        let cl = Reg::Phys(1);
        self.emit(Inst::Move {
            kind: MoveKind::Move(RegWidth::W8),
            src: MOperand::Reg(count),
            dst: MOperand::Reg(cl),
        });
        let result = self.vreg();
        self.emit(Inst::Move { kind: MoveKind::Move(width), src: MOperand::Reg(left), dst: MOperand::Reg(result) });
        self.emit(Inst::Alu { op: alu, width, src: MOperand::Reg(cl), dst: MOperand::Reg(result) });
        self.store_place(dst, MOperand::Reg(result))
    }

    /// Selects a unary operation.
    fn unary(&mut self, dst: &Place, op: UnOp, operand: &Operand, ty: TyId) -> Result<(), String> {
        let width = self.width(ty)?;
        let src = self.operand_to_reg(operand)?;
        let result = self.vreg();
        self.emit(Inst::Move { kind: MoveKind::Move(width), src: MOperand::Reg(src), dst: MOperand::Reg(result) });
        match op {
            UnOp::Neg => self.emit(Inst::Alu {
                op: AluOp::Neg,
                width,
                src: MOperand::Reg(result),
                dst: MOperand::Reg(result),
            }),
            UnOp::Not => {
                if matches!(self.types().kind_owned(ty), TyKind::Bool) {
                    // Logical not: `x ^ 1`.
                    self.emit(Inst::Alu {
                        op: AluOp::Xor,
                        width,
                        src: MOperand::Imm(1),
                        dst: MOperand::Reg(result),
                    });
                } else {
                    self.emit(Inst::Alu {
                        op: AluOp::Not,
                        width,
                        src: MOperand::Reg(result),
                        dst: MOperand::Reg(result),
                    });
                }
            }
        }
        self.store_place(dst, MOperand::Reg(result))
    }

    /// Selects a numeric cast.
    fn cast(&mut self, dst: &Place, operand: &Operand, to: TyId) -> Result<(), String> {
        let from = self.operand_ty(operand);
        // A string literal becomes an address, which is a full word regardless of
        // what `operand_ty` reports for a constant.
        let from_width = if matches!(operand, Operand::Constant(Constant::Str(_))) {
            RegWidth::W64
        } else {
            self.width(from)?
        };
        let to_width = self.width(to)?;
        let src = self.operand_to_reg(operand)?;
        let result = self.vreg();
        let signed = self.types().is_signed(from);
        let kind = if from_width.bytes() > to_width.bytes() {
            // Narrowing: a plain move at the narrower width truncates.
            MoveKind::Move(to_width)
        } else if from_width.bytes() < to_width.bytes() {
            if signed {
                MoveKind::SignExtend { from: from_width, to: to_width }
            } else {
                MoveKind::ZeroExtend { from: from_width, to: to_width }
            }
        } else {
            MoveKind::Move(to_width)
        };
        self.emit(Inst::Move { kind, src: MOperand::Reg(src), dst: MOperand::Reg(result) });
        self.store_place(dst, MOperand::Reg(result))
    }

    /// The type an operand is read at.
    ///
    /// Bool and char constants carry no type id; their machine width is fixed,
    /// so `u8` and `u32` stand in for them rather than allocating a new type.
    pub(crate) fn operand_ty(&mut self, op: &Operand) -> TyId {
        match op {
            Operand::Copy(p) | Operand::Move(p) => self.place_ty(p),
            Operand::Constant(c) => match c {
                Constant::Int { ty, .. } | Constant::Uint { ty, .. } | Constant::Float { ty, .. } => *ty,
                Constant::Bool(_) | Constant::Char(_) | Constant::Unit => self.width_probe_ty(c),
                // A string literal becomes the address of its bytes, so it is a
                // pointer for width purposes; treating it as `()` would truncate
                // the address to a byte when it is cast.
                Constant::Str(_) => self.types().cached_usize(),
                _ => self.types().cached_unit(),
            },
        }
    }

    /// A stable type id whose machine width matches a typeless constant.
    fn width_probe_ty(&self, c: &Constant) -> TyId {
        match c {
            Constant::Bool(_) => self.types().cached_u8(),
            Constant::Char(_) => self.types().cached_u32(),
            _ => self.types().cached_unit(),
        }
    }

    /// Selects an aggregate construction, writing each field into place.
    fn aggregate(
        &mut self,
        dst: &Place,
        kind: &AggregateKind,
        operands: &[Operand],
        ty: TyId,
    ) -> Result<(), String> {
        let base = self.place_addr(dst)?;
        // An enum needs its tag written before the payload.
        if let AggregateKind::Variant { def, index } = kind {
            let payload = self
                .layouts
                .enum_payload_offset(*def)
                .map_err(|e| format!("{e:?}"))?;
            let mut tag_mem = base.clone();
            tag_mem.offset += 0;
            self.emit(Inst::Store {
                kind: MoveKind::Move(RegWidth::W8),
                src: MOperand::Imm(*index as i64),
                dst: tag_mem,
            });
            self.write_fields(&base, payload as i64, operands, VariantFieldTypes::Enum(*def, *index))
        } else {
            self.write_fields(&base, 0, operands, VariantFieldTypes::Aggregate(ty))
        }
    }

    /// Writes each operand into the corresponding field of an aggregate.
    fn write_fields(
        &mut self,
        base: &Mem,
        base_offset: i64,
        operands: &[Operand],
        field_types: VariantFieldTypes,
    ) -> Result<(), String> {
        for (index, operand) in operands.iter().enumerate() {
            let (offset, field_ty) = match field_types {
                VariantFieldTypes::Aggregate(ty) => {
                    let offsets = self.aggregate_offsets(ty)?;
                    let tys = self.aggregate_field_types(ty)?;
                    (
                        *offsets.get(index).ok_or_else(|| format!("field {index} out of range"))?,
                        *tys.get(index).ok_or_else(|| format!("field {index} out of range"))?,
                    )
                }
                VariantFieldTypes::Enum(def, variant) => {
                    let offsets = self
                        .layouts
                        .variant_field_offsets(def, variant)
                        .map_err(|e| format!("{e:?}"))?;
                    let tys = self.variant_field_types(def, variant)?;
                    (
                        *offsets.get(index).ok_or_else(|| format!("field {index} out of range"))?,
                        *tys.get(index).ok_or_else(|| format!("field {index} out of range"))?,
                    )
                }
            };
            let mut mem = base.clone();
            mem.offset += base_offset + offset as i64;
            if self.is_aggregate(field_ty) {
                let src = match operand {
                    Operand::Copy(p) | Operand::Move(p) => p.clone(),
                    _ => return Err("aggregate field from a constant".into()),
                };
                let dest_place = Place {
                    local: LocalId(0),
                    projection: Vec::new(),
                };
                let _ = dest_place;
                // Copy the inner aggregate bytewise into the field.
                let src_mem = self.place_addr(&src)?;
                let size = self.layouts.layout(field_ty).map_err(|e| format!("{e:?}"))?.size;
                let scratch = self.vreg();
                let mut offset_in = 0u64;
                while offset_in < size {
                    let mut s = src_mem.clone();
                    s.offset += offset_in as i64;
                    let mut d = mem.clone();
                    d.offset += offset_in as i64;
                    self.emit(Inst::Load { kind: MoveKind::Move(RegWidth::W8), src: s, dst: MOperand::Reg(scratch) });
                    self.emit(Inst::Store { kind: MoveKind::Move(RegWidth::W8), src: MOperand::Reg(scratch), dst: d });
                    offset_in += 1;
                }
            } else {
                let src = self.operand(operand)?;
                let width = self.width(field_ty)?;
                self.emit(Inst::Store { kind: MoveKind::Move(width), src, dst: mem });
            }
        }
        Ok(())
    }

    /// Field offsets of a composite type.
    fn aggregate_offsets(&mut self, ty: TyId) -> Result<Vec<u64>, String> {
        match self.types().kind_owned(ty) {
            TyKind::Tuple(items) => {
                let mut offset = 0u64;
                let mut offsets = Vec::with_capacity(items.len());
                for item in &items {
                    let l = self.layouts.layout(*item).map_err(|e| format!("{e:?}"))?;
                    offset = l.align_up(offset);
                    offsets.push(offset);
                    offset += l.size;
                }
                Ok(offsets)
            }
            TyKind::Array { elem, len } => {
                let l = self.layouts.layout(elem).map_err(|e| format!("{e:?}"))?;
                Ok((0..len).map(|i| l.size * i).collect())
            }
            TyKind::Adt { def, args } => self.layouts.field_offsets(def, &args).map_err(|e| format!("{e:?}")),
            other => Err(format!("no fields for {other:?}")),
        }
    }

    /// Field types of a composite type.
    fn aggregate_field_types(&mut self, ty: TyId) -> Result<Vec<TyId>, String> {
        match self.types().kind_owned(ty) {
            TyKind::Tuple(items) => Ok(items),
            TyKind::Array { elem, len } => Ok(vec![elem; len as usize]),
            TyKind::Adt { def, .. } => {
                let info = self
                    .layouts
                    .hir()
                    .structs
                    .iter()
                    .find(|s| s.def_id == def)
                    .ok_or_else(|| format!("unknown struct {def:?}"))?;
                Ok(info.fields.iter().map(|f| f.ty).collect())
            }
            other => Err(format!("no fields for {other:?}")),
        }
    }

    /// Field types of an enum variant's payload.
    fn variant_field_types(&mut self, def: crate::types::DefId, variant: u32) -> Result<Vec<TyId>, String> {
        let info = self
            .layouts
            .hir()
            .enums
            .iter()
            .find(|e| e.def_id == def)
            .ok_or_else(|| format!("unknown enum {def:?}"))?;
        let v = info
            .variants
            .get(variant as usize)
            .ok_or_else(|| format!("variant {variant} out of range"))?;
        Ok(match &v.fields {
            crate::hir::VariantFields::Unit => Vec::new(),
            crate::hir::VariantFields::Tuple(tys) => tys.clone(),
            crate::hir::VariantFields::Struct(fields) => fields.iter().map(|f| f.ty).collect(),
        })
    }
}

/// Which field-type source to use when writing an aggregate.
#[derive(Clone, Copy)]
enum VariantFieldTypes {
    Aggregate(TyId),
    Enum(crate::types::DefId, u32),
}
