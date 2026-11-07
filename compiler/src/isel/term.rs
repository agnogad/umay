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
        if self.is_aggregate(ty) {
            // A large value is returned through a hidden pointer that the caller
            // passes; that mechanism is not implemented yet, so the value is
            // copied into the register pair it fits in (16 bytes or less) and
            // rejected otherwise.
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
        out.push(Inst::Call { target: MOperand::Reg(reg) });

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
        let mut arg_regs = Vec::with_capacity(args.len());
        for arg in args {
            arg_regs.push(self.operand_to_reg(arg)?);
        }

        let stack_args = args.len().saturating_sub(ARG_REGS.len());
        let stack_bytes = stack_args as u64 * 8;
        let padding = if stack_bytes % 16 == 8 { 8 } else { 0 };
        if padding > 0 {
            out.push(Inst::Alu {
                op: AluOp::Sub,
                width: RegWidth::W64,
                src: MOperand::Imm(8),
                dst: MOperand::Reg(Reg::Phys(4)),
            });
        }
        for reg in arg_regs.iter().skip(ARG_REGS.len()).rev() {
            out.push(Inst::Push(MOperand::Reg(*reg)));
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

        // A call target names a symbol, not a block label. Using a memory
        // operand keeps the two namespaces separate in the instruction
        // representation, so a symbol can never be confused with a label.
        self.note_symbol(symbol);
        out.push(Inst::Call { target: MOperand::Mem(Mem::symbol(symbol)) });

        if stack_bytes + padding > 0 {
            out.push(Inst::Alu {
                op: AluOp::Add,
                width: RegWidth::W64,
                src: MOperand::Imm((stack_bytes + padding) as i64),
                dst: MOperand::Reg(Reg::Phys(4)),
            });
        }

        if let Some(dst) = destination {
            self.store_place_into(dst, MOperand::Reg(Reg::Phys(0)), out)?;
        }
        out.push(Inst::Jmp(self.block_labels[target.0 as usize]));
        Ok(())
    }

}

impl<'a, 't> Isel<'a, 't> {
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
