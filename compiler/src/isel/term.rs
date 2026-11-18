//! Instruction selection for MIR terminators: branches, calls, drops, returns.
//!
//! Calls follow the System V AMD64 ABI: the first six integer arguments go in
//! `rdi`, `rsi`, `rdx`, `rcx`, `r8`, `r9`, and the rest are pushed on the stack
//! in reverse order. SSE arguments are not handled yet because the language has
//! no float arithmetic beyond literals.

use super::*;
use crate::hir::LocalId;
use crate::machine::{MoveKind, RegWidth};

/// Where a call transfers control.
///
/// Direct calls name a symbol; indirect calls (a `fn` pointer, a capture-less
/// closure, or a `dyn Trait` vtable slot) jump through a register holding the
/// code address. Both forms go through the same ABI planner so aggregate
/// arguments, sret returns and Win64 shadow space behave identically.
enum CallTarget {
    Symbol(String),
    Register(Reg),
}

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
        if let Some(&reg) = self.local_regs.get(&local) {
            out.push(Inst::Move {
                kind: MoveKind::Move(width),
                src: MOperand::Reg(reg),
                dst: MOperand::Reg(Reg::Phys(0)),
            });
            return Ok(());
        }
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
        // A construct that lowers to a library call names its symbol directly.
        if let Operand::Constant(Constant::NamedFn(name)) = callee {
            return self.call_symbol(name, args, destination, target, out);
        }

        // Anything else is an indirect call through a value -- a `fn` pointer
        // parameter, a capture-less closure, or a `dyn Trait` vtable slot. Load
        // the code address into a register, then run exactly the same ABI
        // planning a direct call uses. `operand_to_reg` folds away the extra
        // dereference on a `*const fn` callee via `fn_ptr_place`, so the value
        // loaded is the code address itself, not the first instruction byte.
        let reg = self.operand_to_reg(callee)?;
        self.emit_call(CallTarget::Register(reg), args, destination, target, out)
    }

    /// Selects a direct call to the named function `symbol`.
    ///
    /// This is the path used for every statically-known callee. All ABI planning
    /// is shared with the indirect path in [`Self::emit_call`]; only the call
    /// target differs.
    pub(crate) fn call_symbol(
        &mut self,
        symbol: &str,
        args: &[Operand],
        destination: Option<&Place>,
        target: &BlockId,
        out: &mut Vec<Inst>,
    ) -> Result<(), String> {
        self.emit_call(CallTarget::Symbol(symbol.to_string()), args, destination, target, out)
    }

    /// Emits a call with full ABI planning for either a direct symbol or an
    /// indirect register target.
    ///
    /// The two forms differ only in the final `call` operand: a direct call names
    /// a symbol, while an indirect call jumps through the register holding the
    /// code address. Everything before and after -- sret classification, the
    /// outgoing area, Win64 shadow space, the register shuffle, stack arguments
    /// and result storage -- is identical and lives here once, so an indirect
    /// `dyn Trait` dispatch is as correct as a direct call to a known function.
    fn emit_call(
        &mut self,
        call_target: CallTarget,
        args: &[Operand],
        destination: Option<&Place>,
        target: &BlockId,
        out: &mut Vec<Inst>,
    ) -> Result<(), String> {
        // A callee returning a large aggregate writes it into storage the caller
        // provides and receives that storage's address in `rdi`. The decision is
        // made from the destination's type, which is the callee's return type, so
        // caller and callee agree without either consulting the other.
        //
        // The threshold is the ABI's: SysV returns up to sixteen bytes in two
        // registers; Win64 returns 1, 2, 4, 8, 16, 32 and 64 bytes in registers
        // and everything else through the pointer.
        let abi = self.abi;
        let sret = destination.filter(|dst| {
            let ty = self.place_ty(dst);
            if !self.layouts.is_aggregate(ty) {
                return false;
            }
            let size = self.layouts.layout(ty).map(|l| l.size).unwrap_or(0);
            // The rule lives in one place: the callee records it on its body,
            // and the caller must agree with it exactly, or one side reserves
            // storage the other never fills.
            crate::codegen::aggregate_goes_through_pointer(abi, size)
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
        // Win64 reserves 32 bytes of shadow space directly above the return
        // address for the callee to spill into. It is part of the outgoing area
        // the caller owns, and the fifth argument and beyond sit *above* it, not
        // in it: overlapping the two made every call with more than four
        // arguments read its fifth argument out of the shadow space, where no one
        // had written anything.
        //
        // System V has no shadow space, so `shadow` is zero there and the two
        // rules agree.
        let shadow = self.shadow_space();
        let stack_base = shadow + stack_bytes;
        let stack_total = stack_base.div_ceil(16) * 16;
        let arg_regs_of_abi = self.arg_regs();
        let count = arg_regs_of_abi.len().min(arg_regs.len());
        // An indirect callee lives in a register, and that register may be one
        // of the ABI argument registers. Park it alongside the arguments and
        // restore it *after* the shuffle, otherwise argument three overwrites
        // the code address in `%rdx` and the call jumps to garbage.
        let callee_reg = match &call_target {
            CallTarget::Register(reg) => Some(*reg),
            CallTarget::Symbol(_) => None,
        };
        let park_slots = count + usize::from(callee_reg.is_some());
        let park_bytes = if park_slots > 0 { ((park_slots as u64) * 8).div_ceil(16) * 16 } else { 0 };
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
        if let Some(reg) = callee_reg {
            out.push(Inst::Store {
                kind: MoveKind::Move(RegWidth::W64),
                src: MOperand::Reg(reg),
                dst: Mem {
                    base: Some(Reg::Phys(4)),
                    offset: park_base as i64 + (count as i64) * 8,
                    index: None,
                    symbol: None,
                },
            });
        }
        // Stack arguments are written before loading the ABI argument registers
        // so that a register-resident stack argument cannot be clobbered by the
        // shuffle into %rdi..%r9.
        for piece in &plan.stack_pieces {
            let mut copied = 0u64;
            while copied < piece.size {
                // The last partial eightbyte is copied byte by byte so a short
                // aggregate does not read past its own storage.
                let width = if piece.size - copied >= 8 {
                    RegWidth::W64
                } else if matches!(piece.source, StackSource::Imm(_)) && copied == 0 {
                    match piece.size {
                        4 => RegWidth::W32,
                        2 => RegWidth::W16,
                        _ => RegWidth::W8,
                    }
                } else {
                    RegWidth::W8
                };
                let dst = Mem {
                    base: Some(Reg::Phys(4)),
                    // `piece.offset` counts from the bottom of the outgoing area.
                    // On Win64 the shadow space sits below the stack arguments, so
                    // the first argument goes at its top and not at offset zero.
                    offset: shadow as i64 + piece.offset as i64 + copied as i64,
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
        for (i, arg_reg) in arg_regs_of_abi.iter().take(count).enumerate() {
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

        // Restore the indirect callee into a register that is never an ABI
        // argument register. Restoring it into its original register would
        // clobber an argument on SysV (`%rdx` is both the callee slot and the
        // third integer argument) and on Win64 (`%rcx`/`%rdx`/`%r8`/`%r9`).
        let call_reg = callee_reg.map(|_| Reg::Phys(10));
        if callee_reg.is_some() {
            out.push(Inst::Load {
                kind: MoveKind::Move(RegWidth::W64),
                src: Mem {
                    base: Some(Reg::Phys(4)),
                    offset: park_base as i64 + (count as i64) * 8,
                    index: None,
                    symbol: None,
                },
                dst: MOperand::Reg(Reg::Phys(10)),
            });
        }

        // A call target names a symbol for a direct call, or is the register
        // holding the code address for an indirect one. A symbol uses a memory
        // operand so it can never be confused with a block label.
        match &call_target {
            CallTarget::Symbol(symbol) => {
                self.note_symbol(symbol);
                // Every call out of the program may reach a C variadic function,
                // and SysV requires `%al` to be zero for those. A non-variadic
                // callee ignores the register, so setting it unconditionally
                // costs one instruction and cannot be wrong. UMAY itself has no
                // variadic functions.
                out.push(Inst::Call {
                    target: MOperand::Mem(Mem::symbol(symbol)),
                    variadic: true,
                });
            }
            CallTarget::Register(_) => {
                let reg = call_reg.expect("an indirect call has a target register");
                out.push(Inst::Call { target: MOperand::Reg(reg), variadic: false });
            }
        }

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
        let arg_regs = self.arg_regs();
        let win64 = self.abi == crate::target::Abi::Win64;
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
                // Win64 never splits an aggregate across two registers: one
                // eightbyte fits in one register and anything wider is passed by
                // pointer, which is not modelled yet and is therefore spilled --
                // a stack copy of the whole value, which the callee reads.
                let fits_in_regs = if win64 {
                    size <= 8
                } else {
                    size <= 16 && registers_used + needed <= arg_regs.len()
                };
                if !spilled && fits_in_regs && registers_used + needed <= arg_regs.len() {
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
            if !spilled && registers_used < arg_regs.len() {
                register_values.push(self.operand_to_reg(arg)?);
                registers_used += 1;
            } else {
                // Past the register file every scalar is delivered on the stack.
                let mem = match arg {
                    Operand::Copy(p) | Operand::Move(p) => {
                        if p.projection.is_empty() {
                            if let Some(&reg) = self.local_regs.get(&p.local) {
                                StackSource::Imm(MOperand::Reg(reg))
                            } else {
                                StackSource::Mem(self.place_addr(p)?)
                            }
                        } else {
                            StackSource::Mem(self.place_addr(p)?)
                        }
                    }
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
pub(crate) fn term_symbol_for(
    id: crate::hir::FnId,
    symbols: &std::collections::HashMap<crate::hir::FnId, String>,
) -> String {
    symbol_for(id, symbols)
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
