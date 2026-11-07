//! Instruction selection for MIR terminators: branches, calls, drops, returns.
//!
//! Calls follow the System V AMD64 ABI: the first six integer arguments go in
//! `rdi`, `rsi`, `rdx`, `rcx`, `r8`, `r9`, and the rest are pushed on the stack
//! in reverse order. SSE arguments are not handled yet because the language has
//! no float arithmetic beyond literals.

use super::*;
use crate::hir::LocalId;
use crate::machine::{MoveKind, RegWidth};

/// Integer argument registers, in order.
const ARG_REGS: [u8; 6] = [7, 6, 2, 1, 8, 9]; // rdi, rsi, rdx, rcx, r8, r9

impl<'a, 't> Isel<'a, 't> {
    /// Selects a terminator, appending instructions to `out`.
    ///
    /// Terminators are selected into a buffer rather than emitted directly
    /// because a call needs to emit a sequence whose last instruction is the
    /// call itself, and keeping it contiguous makes the output readable.
    pub(crate) fn terminator(&mut self, term: &Terminator, out: &mut Vec<Inst>) -> Result<(), String> {
        match term {
            Terminator::Goto(target) => out.push(Inst::Jmp(self.block_labels[target.0 as usize])),
            Terminator::SwitchInt { discr, targets, otherwise } => {
                let value = self.operand_to_reg(discr)?;
                for (case, target) in targets {
                    let label = self.block_labels[target.0 as usize];
                    let imm = self.vreg();
                    out.push(Inst::Move {
                        kind: MoveKind::Move(RegWidth::W64),
                        src: MOperand::Imm(*case as i64),
                        dst: MOperand::Reg(imm),
                    });
                    out.push(Inst::Cmp { width: RegWidth::W64, lhs: MOperand::Reg(value), rhs: MOperand::Reg(imm) });
                    out.push(Inst::Jcc { cond: Cond::Eq, target: label });
                }
                out.push(Inst::Jmp(self.block_labels[otherwise.0 as usize]));
            }
            Terminator::Call { callee, args, destination, target, .. } => {
                self.call(callee, args, destination.as_ref(), target, out)?;
            }
            Terminator::Drop { place, target } => {
                // Drop is a no-op at codegen: values of types with destructors
                // do not exist yet. Once `Drop` impls land, this is where the
                // destructor call is selected.
                let _ = place;
                out.push(Inst::Jmp(self.block_labels[target.0 as usize]));
            }
            Terminator::Return => {
                // The return value is already in the return slot; move it to the
                // ABI's return register. Control then jumps to the shared
                // epilogue rather than returning directly, so the frame is torn
                // down and the callee-saved registers restored on every path.
                self.move_return_value(out)?;
                out.push(Inst::Jmp(self.epilogue_label()));
            }
            Terminator::Unreachable => {
                // Reaching this means the program did something the checker
                // proved impossible. Jumping to the epilogue makes that a
                // harmless return of whatever is in the return slot, which is
                // preferable to emitting a trap that a legal program might hit
                // if an analysis is ever wrong.
                out.push(Inst::Comment("unreachable".into()));
                out.push(Inst::Jmp(self.epilogue_label()));
            }
        }
        Ok(())
    }

    /// The MIR local a body returns through, matching the slot in the frame.
    fn return_slot_local(&self) -> Option<LocalId> {
        self.body.locals.iter().position(|l| l.name == "_ret").map(|i| LocalId(i as u32))
    }

    /// Copies the return slot into `rax` before `ret`.
    fn move_return_value(&mut self, out: &mut Vec<Inst>) -> Result<(), String> {
        let Some(local) = self.return_slot_local() else { return Ok(()) };
        let ty = self.frame.slot(local).ty;
        if self.layouts.layout(ty).map(|l| l.size).unwrap_or(0) == 0 {
            return Ok(());
        }
        let mem = Mem::stack(self.frame.slot(local).offset);
        if let Some(pointer) = self.frame.sret {
            // The caller reserved storage and passed its address in `rdi`; the
            // prologue saved it. Copying the value there and returning the
            // pointer in `rax` is the SysV convention for an aggregate that does
            // not fit in two registers.
            let size = self.layouts.layout(ty).map_err(|e| format!("{e:?}"))?.size;
            let target = self.vreg();
            out.push(Inst::Load {
                kind: MoveKind::Move(RegWidth::W64),
                src: Mem::stack(pointer.offset),
                dst: MOperand::Reg(target),
            });
            self.copy_bytes_to_pointer(&mem, target, size, out)?;
            out.push(Inst::Move {
                kind: MoveKind::Move(RegWidth::W64),
                src: MOperand::Reg(target),
                dst: MOperand::Reg(Reg::Phys(0)),
            });
            return Ok(());
        }
        if self.is_aggregate(ty) {
            let size = self.layouts.layout(ty).map_err(|e| format!("{e:?}"))?.size;
            if size > 16 {
                return Err(format!(
                    "returning a {size}-byte aggregate by value is not supported yet"
                ));
            }
            out.push(Inst::Load {
                kind: MoveKind::Move(RegWidth::W64),
                src: mem.clone(),
                dst: MOperand::Reg(Reg::Phys(0)),
            });
            if size > 8 {
                let mut high = mem;
                high.offset += 8;
                out.push(Inst::Load {
                    kind: MoveKind::Move(RegWidth::W64),
                    src: high,
                    dst: MOperand::Reg(Reg::Phys(2)),
                });
            }
            return Ok(());
        }
        let width = self.width(ty)?;
        out.push(Inst::Load { kind: MoveKind::Move(width), src: mem, dst: MOperand::Reg(Reg::Phys(0)) });
        Ok(())
    }

    /// Selects a call terminator.
    fn call(
        &mut self,
        callee: &Operand,
        args: &[Operand],
        destination: Option<&Place>,
        target: &BlockId,
        out: &mut Vec<Inst>,
    ) -> Result<(), String> {
        // A named callee is a direct call, which is the only form lowering
        // produces today. This is decided before any argument is evaluated, so
        // that the arguments are not loaded twice.
        if let Operand::Constant(Constant::Fn(id)) = callee {
            return self.call_symbol(&symbol_for(*id, self.fn_symbols), args, destination, target, out);
        }

        // Evaluate every argument into a register first. Doing this before
        // writing the argument registers is what makes the sequence correct when
        // an argument is itself a local that lives in one of them.
        let mut arg_regs = Vec::with_capacity(args.len());
        for arg in args {
            arg_regs.push(self.operand_to_reg(arg)?);
        }

        // Arguments beyond the register file go on the stack, pushed in reverse.
        let stack_args = args.len().saturating_sub(ARG_REGS.len());
        let stack_bytes = stack_args as u64 * 8;
        if stack_args > 0 {
            // Keep the stack 16-byte aligned at the call. The frame size is a
            // multiple of 16, so only the pushed arguments need rounding.
            let padding = if stack_bytes % 16 == 8 { 8 } else { 0 };
            if padding > 0 {
                out.push(Inst::Alu {
                    op: AluOp::Sub,
                    width: RegWidth::W64,
                    src: MOperand::Imm(8),
                    dst: MOperand::Reg(Reg::Phys(4)),
                });
            }
            for (i, reg) in arg_regs.iter().enumerate().skip(ARG_REGS.len()).rev() {
                let _ = i;
                out.push(Inst::Push(MOperand::Reg(*reg)));
            }
        }

        // Move arguments into their ABI registers.
        //
        // The values are read from registers that are themselves ABI registers,
        // so a naive sequence overwrites an argument that has not been moved yet
        // (`%rcx` holding argument 2 is clobbered by argument 4's move). Parking
        // every value on the stack first makes the shuffle order irrelevant, and
        // costs one store and load per argument, which the two-address x86 move
        // makes unavoidable anyway.
        let count = ARG_REGS.len().min(arg_regs.len());
        if count > 0 {
            let bytes = ((count as u64) * 8).div_ceil(16) * 16;
            out.push(Inst::Alu {
                op: AluOp::Sub,
                width: RegWidth::W64,
                src: MOperand::Imm(bytes as i64),
                dst: MOperand::Reg(Reg::Phys(4)),
            });
            // Park at `rsp`-relative slots, which cannot overlap the frame.
            for (i, reg) in arg_regs.iter().take(count).enumerate() {
                out.push(Inst::Store {
                    kind: MoveKind::Move(RegWidth::W64),
                    src: MOperand::Reg(*reg),
                    dst: Mem { base: Some(Reg::Phys(4)), offset: (i as i64) * 8, index: None, symbol: None },
                });
            }
            for (i, arg_reg) in ARG_REGS.iter().take(count).enumerate() {
                out.push(Inst::Load {
                    kind: MoveKind::Move(RegWidth::W64),
                    src: Mem { base: Some(Reg::Phys(4)), offset: (i as i64) * 8, index: None, symbol: None },
                    dst: MOperand::Reg(Reg::Phys(*arg_reg)),
                });
            }
            out.push(Inst::Alu {
                op: AluOp::Add,
                width: RegWidth::W64,
                src: MOperand::Imm(bytes as i64),
                dst: MOperand::Reg(Reg::Phys(4)),
            });
        }

        // Anything else is an indirect call through a value.
        let reg = self.operand_to_reg(callee)?;
        out.push(Inst::Call { target: MOperand::Reg(reg), variadic: false });

        // Restore the stack after a call with stack arguments.
        if stack_bytes > 0 {
            let total = stack_bytes + if stack_bytes % 16 == 8 { 8 } else { 0 };
            out.push(Inst::Alu {
                op: AluOp::Add,
                width: RegWidth::W64,
                src: MOperand::Imm(total as i64),
                dst: MOperand::Reg(Reg::Phys(4)),
            });
        }

        // The result arrives in `rax`.
        if let Some(dst) = destination {
            let ty = self.place_ty(dst);
            if self.is_aggregate(ty) {
                let size = self.layouts.layout(ty).map_err(|e| format!("{e:?}"))?.size;
                if size > 16 {
                    return Err(format!("returning a {size}-byte aggregate by value is not supported yet"));
                }
            }
            self.store_place_into(dst, MOperand::Reg(Reg::Phys(0)), out)?;
        }

        out.push(Inst::Jmp(self.block_labels[target.0 as usize]));
        Ok(())
    }

    /// Selects a direct call to the named function `symbol`.
    ///
    /// This is the path used for every statically-known callee; separating it
    /// from [`Self::call`] keeps the indirect case honest about what it does not
    /// support.
    pub(crate) fn call_symbol(
        &mut self,
        symbol: &str,
        args: &[Operand],
        destination: Option<&Place>,
        target: &BlockId,
        out: &mut Vec<Inst>,
    ) -> Result<(), String> {
        // A callee returning a large aggregate writes it into storage the caller
        // provides and receives that storage's address in `rdi`. The decision is
        // made from the destination's type, which is the callee's return type, so
        // caller and callee agree without either consulting the other.
        let sret = destination.filter(|dst| {
            let ty = self.place_ty(dst);
            self.layouts.is_aggregate(ty)
                && self.layouts.layout(ty).map(|l| l.size > 16).unwrap_or(false)
        });
        let sret_ptr = match sret {
            Some(dst) => {
                let place = dst.clone();
                let mem = self.place_addr(&place)?;
                Some(self.mem_to_reg(&mem, RegWidth::W64)?)
            }
            None => None,
        };

        // Each argument is classified before anything is emitted, so the
        // register file and the stack area can be planned in one go. An
        // aggregate of sixteen bytes or less travels in registers, two
        // eightbytes at a time; anything larger is copied to the stack, which is
        // what the SysV ABI requires and what makes `push(self) -> Buf` correct.
        let plan = self.plan_arguments(args, sret_ptr.is_some())?;

        let mut arg_regs = Vec::with_capacity(plan.register_values.len() + 1);
        if let Some(ptr) = sret_ptr {
            arg_regs.push(ptr);
        }
        arg_regs.extend(plan.register_values.iter().copied());

        // Build the outgoing area in a single allocation.
        //
        // The area holds two things: the argument registers are parked at its
        // bottom and then read back into their ABI registers (which makes the
        // shuffle order irrelevant, because a value about to be moved into
        // `%rsi` may currently live in `%rsi`), and above them sit the stack
        // arguments the callee will read at `16(%rbp)` onwards. One `subq` keeps
        // the whole area 16-byte aligned at the call.
        let stack_bytes = plan.stack_bytes;
        let stack_total = stack_bytes.div_ceil(16) * 16;
        let count = ARG_REGS.len().min(arg_regs.len());
        let park_bytes = if count > 0 { ((count as u64) * 8).div_ceil(16) * 16 } else { 0 };
        let area = park_bytes + stack_total;
        // Stack arguments occupy the bottom of the area, because the callee reads
        // them upward from `16(%rbp)`; the register parking area sits directly
        // above them.
        let park_base = stack_total;
        if area > 0 {
            out.push(Inst::Alu {
                op: AluOp::Sub,
                width: RegWidth::W64,
                src: MOperand::Imm(area as i64),
                dst: MOperand::Reg(Reg::Phys(4)),
            });
        }
        for (i, reg) in arg_regs.iter().take(count).enumerate() {
            out.push(Inst::Store {
                kind: MoveKind::Move(RegWidth::W64),
                src: MOperand::Reg(*reg),
                dst: Mem {
                    base: Some(Reg::Phys(4)),
                    offset: park_base as i64 + (i as i64) * 8,
                    index: None,
                    symbol: None,
                },
            });
        }
        for (i, arg_reg) in ARG_REGS.iter().take(count).enumerate() {
            out.push(Inst::Load {
                kind: MoveKind::Move(RegWidth::W64),
                src: Mem {
                    base: Some(Reg::Phys(4)),
                    offset: park_base as i64 + (i as i64) * 8,
                    index: None,
                    symbol: None,
                },
                dst: MOperand::Reg(Reg::Phys(*arg_reg)),
            });
        }
        // Stack arguments are written above the parking area. The callee reads
        // them upward from `16(%rbp)`, which is the bottom of this area, so the
        // first byte of the first stack argument belongs at its lowest address.
        for piece in &plan.stack_pieces {
            let mut copied = 0u64;
            while copied < piece.size {
                // The last partial eightbyte is copied byte by byte so a short
                // aggregate does not read past its own storage.
                let width = if piece.size - copied >= 8 { RegWidth::W64 } else { RegWidth::W8 };
                let dst = Mem {
                    base: Some(Reg::Phys(4)),
                    offset: piece.offset as i64 + copied as i64,
                    index: None,
                    symbol: None,
                };
                match &piece.source {
                    StackSource::Mem(mem) => {
                        let mut read = mem.clone();
                        read.offset += copied as i64;
                        let scratch = self.vreg();
                        out.push(Inst::Load {
                            kind: MoveKind::Move(width),
                            src: read,
                            dst: MOperand::Reg(scratch),
                        });
                        out.push(Inst::Store {
                            kind: MoveKind::Move(width),
                            src: MOperand::Reg(scratch),
                            dst,
                        });
                    }
                    StackSource::Imm(value) => {
                        out.push(Inst::Store {
                            kind: MoveKind::Move(width),
                            src: value.clone(),
                            dst,
                        });
                    }
                }
                copied += width.bytes();
            }
        }

        // A call target names a symbol, not a block label. Using a memory
        // operand keeps the two namespaces separate in the instruction
        // representation, so a symbol can never be confused with a label.
        self.note_symbol(symbol);
        // Every call out of the program may reach a C variadic function, and SysV
        // requires `%al` to be zero for those. A non-variadic callee ignores the
        // register, so setting it unconditionally costs one instruction and cannot
        // be wrong. UMAY itself has no variadic functions.
        out.push(Inst::Call { target: MOperand::Mem(Mem::symbol(symbol)), variadic: true });

        if area > 0 {
            out.push(Inst::Alu {
                op: AluOp::Add,
                width: RegWidth::W64,
                src: MOperand::Imm(area as i64),
                dst: MOperand::Reg(Reg::Phys(4)),
            });
        }

        // An sret call has already placed the value in the destination; copying
        // `rax` into it again would overwrite the aggregate with the pointer that
        // holds its address.
        if let Some(dst) = destination {
            if sret.is_none() {
                self.store_place_into(dst, MOperand::Reg(Reg::Phys(0)), out)?;
                // An aggregate of more than eight bytes comes back in a pair of
                // registers; the callee writes both and the caller must read both,
                // or the upper half of the value is whatever the call left behind.
                let dst_ty = self.place_ty(dst);
                let size = self.layouts.layout(dst_ty).map(|l| l.size).unwrap_or(0);
                if self.layouts.is_aggregate(dst_ty) && size > 8 {
                    let place = dst.clone();
                    let mem = self.place_addr(&place)?;
                    let mut high = mem;
                    high.offset += 8;
                    out.push(Inst::Store {
                        kind: MoveKind::Move(RegWidth::W64),
                        src: MOperand::Reg(Reg::Phys(2)),
                        dst: high,
                    });
                }
            }
        }
        out.push(Inst::Jmp(self.block_labels[target.0 as usize]));
        Ok(())
    }

}

/// Where one argument of a call goes.
pub(crate) struct ArgumentPlan {
    /// Registers holding the register-passed values, in argument order.
    pub register_values: Vec<Reg>,
    /// Aggregates passed on the stack, with their byte size and the offset from
    /// the bottom of the outgoing area where their first byte belongs.
    pub stack_pieces: Vec<StackPiece>,
    /// Total bytes the stack area must hold.
    pub stack_bytes: u64,
}

/// One argument copied into the outgoing stack area.
pub(crate) struct StackPiece {
    /// Where the bytes come from. A register source is used for a literal, which
    /// has no storage of its own.
    pub source: StackSource,
    pub size: u64,
    pub offset: u64,
}

/// The origin of a stack argument's bytes.
#[derive(Clone)]
pub(crate) enum StackSource {
    Mem(Mem),
    /// A value with no storage of its own. A literal is written straight from its
    /// bytes rather than parked in a register first: parking would claim a
    /// register before the argument shuffle runs, and the shuffle is free to
    /// overwrite it with another argument.
    Imm(crate::machine::Operand),
}

impl<'a, 't> Isel<'a, 't> {
    /// Classifies a call's arguments into registers and stack slots.
    ///
    /// SysV AMD64 passes an aggregate of two eightbytes or less in registers and
    /// anything larger on the stack. Getting this wrong is quiet: the callee
    /// would read a slot the caller never filled and produce plausible garbage,
    /// so the classification lives here, once, and both sides use its result.
    pub(crate) fn plan_arguments(
        &mut self,
        args: &[Operand],
        sret: bool,
    ) -> Result<ArgumentPlan, String> {
        let mut register_values: Vec<Reg> = Vec::new();
        if sret {
            // The hidden return pointer occupies `rdi`; the caller adds it
            // before this runs, so it is signalled by starting the register list
            // one short.
        }
        let mut stack_pieces = Vec::new();
        let mut stack_bytes = 0u64;
        let mut registers_used = usize::from(sret);
        // Once an argument has been placed on the stack, every later one stays
        // there: the callee reads stack arguments from a fixed position, so a
        // register argument after a stack argument would shift every offset.
        let mut spilled = false;
        for arg in args {
            let ty = self.operand_ty(arg);
            if self.layouts.is_aggregate(ty) {
                let size = self.layouts.layout(ty).map_err(|e| format!("{e:?}"))?.size;
                let mem = match arg {
                    Operand::Copy(p) | Operand::Move(p) => StackSource::Mem(self.place_addr(p)?),
                    Operand::Constant(_) => {
                        return Err("aggregate constant argument is not supported".into())
                    }
                };
                let needed = size.div_ceil(8) as usize;
                if !spilled && size <= 16 && registers_used + needed <= ARG_REGS.len() {
                    let mem = match &mem {
                        StackSource::Mem(m) => m.clone(),
                        StackSource::Imm(_) => {
                            return Err("aggregate literal argument is not supported".into())
                        }
                    };
                    // Two eightbytes at most: one register each. An aggregate of
                    // eight bytes or less needs a single register, exactly like a
                    // scalar, which is what the callee expects too.
                    let pieces = size.div_ceil(8);
                    for i in 0..pieces {
                        let mut read = mem.clone();
                        read.offset += (i * 8) as i64;
                        let reg = self.vreg();
                        // Each eightbyte is read at full width; the last one may
                        // reach past the aggregate's end, which is harmless in a
                        // register and never written back.
                        self.emit(Inst::Load {
                            kind: MoveKind::Move(RegWidth::W64),
                            src: read,
                            dst: MOperand::Reg(reg),
                        });
                        register_values.push(reg);
                        registers_used += 1;
                    }
                } else {
                    // Larger than two eightbytes: copied to the stack, aligned
                    // to eight.
                    stack_bytes = stack_bytes.div_ceil(8) * 8;
                    stack_pieces.push(StackPiece { source: mem, size, offset: stack_bytes });
                    stack_bytes += size;
                    spilled = true;
                }
                continue;
            }
            if !spilled && registers_used < ARG_REGS.len() {
                register_values.push(self.operand_to_reg(arg)?);
                registers_used += 1;
            } else {
                // Past the register file every scalar is delivered on the stack.
                let mem = match arg {
                    Operand::Copy(p) | Operand::Move(p) => StackSource::Mem(self.place_addr(p)?),
                    // A literal is written from its own bytes.
                    Operand::Constant(_) => StackSource::Imm(self.operand(arg)?),
                };
                // The slot is eight bytes wide, but only the value's own width is
                // written: the callee copies the declared type's size, and writing
                // eight bytes from a four-byte value would place bytes the callee
                // never reads at an offset it does not expect.
                let size = self.layouts.layout(ty).map_err(|e| format!("{e:?}"))?.size;
                stack_bytes = stack_bytes.div_ceil(8) * 8;
                stack_pieces.push(StackPiece { source: mem, size, offset: stack_bytes });
                stack_bytes += size.max(1).div_ceil(8) * 8;
                spilled = true;
            }
        }
        let _ = registers_used;
        Ok(ArgumentPlan { register_values, stack_pieces, stack_bytes })
    }

    /// The label of the shared epilogue.
    ///
    /// Numbered past the block labels so it cannot collide with one.
    pub(crate) fn epilogue_label(&self) -> Label {
        Label(self.block_labels.len() as u32)
    }
}

/// The symbol a function id is called by.
fn symbol_for(
    id: crate::hir::FnId,
    symbols: &std::collections::HashMap<crate::hir::FnId, String>,
) -> String {
    symbols
        .get(&id)
        .cloned()
        .unwrap_or_else(|| format!("fn{}", id.0))
}
