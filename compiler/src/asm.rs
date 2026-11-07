//! Assembly printer: machine instructions to AT&T-syntax text.
//!
//! The output is assembled with the system `as`, which keeps this backend free
//! of a machine-code encoder. That is the same trade the roadmap makes for the
//! linker: the interesting work is selection and allocation, and re-implementing
//! a proven assembler would add risk without adding capability.
//!
//! The printer is total: every instruction in [`Inst`] has an arm, so a new
//! instruction cannot be added without deciding how it prints.

use crate::machine::*;
use std::fmt::Write as _;

/// Prints a whole function, including its prologue and epilogue.
pub fn print_function(function: &MachineFunction, spill_bytes: u64, callee_saved: &[u8]) -> String {
    let mut out = String::new();
    let symbol = symbol_name(&function.name);
    let _ = writeln!(out, "\n.globl {symbol}");
    let _ = writeln!(out, ".type {symbol}, @function");
    let _ = writeln!(out, "{symbol}:");

    // Prologue: establish the frame, then preserve the callee-saved registers
    // this function actually uses.
    let frame = align16(function.stack_size + spill_bytes);
    let _ = writeln!(out, "    pushq %rbp");
    let _ = writeln!(out, "    movq %rsp, %rbp");
    // Callee-saved registers are pushed before the frame is reserved, so they
    // sit directly below the saved `rbp` and `leave` can unwind past them only
    // after the explicit `popq`s in the epilogue. Reserving the frame first and
    // pushing afterwards would put them below `rsp`'s restoration point.
    for reg in callee_saved {
        let _ = writeln!(out, "    pushq %{}", register_name(*reg, RegWidth::W64));
    }
    if frame > 0 {
        let _ = writeln!(out, "    subq ${frame}, %rsp");
    }

    // Move incoming arguments into their stack slots. They are stored after the
    // prologue because the prologue uses the same registers.
    //
    // When the return value travels through a hidden pointer, `rdi` holds that
    // pointer instead of the first argument, so every argument moves along one
    // register.
    if let Some(offset) = function.sret_offset {
        let _ = writeln!(out, "    movq %rdi, {offset}(%rbp)");
    }
    let all_arg_regs = [7u8, 6, 2, 1, 8, 9]; // rdi, rsi, rdx, rcx, r8, r9
    let mut next_reg = usize::from(function.sret_offset.is_some());
    // Stack arguments begin just above the return address; an eightbyte of
    // padding sits there only when it keeps the area aligned, which the caller
    // already arranged.
    let mut stack_offset = 16i64;
    for (index, (_, offset)) in function.local_slots.iter().take(function.arg_count as usize).enumerate() {
        let kind = function
            .arg_kinds
            .get(index)
            .copied()
            .unwrap_or(crate::machine::ArgKind::Register);
        let width = function
            .arg_widths
            .get(index)
            .copied()
            .unwrap_or(RegWidth::W64);
        match kind {
            crate::machine::ArgKind::Register => {
                if next_reg < all_arg_regs.len() {
                    let _ = writeln!(
                        out,
                        "    mov{} %{}, {}(%rbp)",
                        width.suffix(),
                        register_name(all_arg_regs[next_reg], width),
                        offset
                    );
                    next_reg += 1;
                } else {
                    // Past the register file, delivered on the stack.
                    let _ = writeln!(
                        out,
                        "    mov{} {}(%rbp), %{}",
                        width.suffix(),
                        stack_offset,
                        register_name(0, width)
                    );
                    let _ = writeln!(
                        out,
                        "    mov{} %{}, {}(%rbp)",
                        width.suffix(),
                        register_name(0, width),
                        offset
                    );
                    stack_offset += 8;
                }
            }
            crate::machine::ArgKind::RegisterPair => {
                // Two eightbytes, in two registers. A member that does not need
                // all eight bytes of its slot still occupies one whole register.
                let pair = if next_reg + 1 < all_arg_regs.len() {
                    Some((all_arg_regs[next_reg], all_arg_regs[next_reg + 1]))
                } else {
                    None
                };
                match pair {
                    Some((low, high)) => {
                        let _ = writeln!(
                            out,
                            "    movq %{}, {}(%rbp)",
                            register_name(low, RegWidth::W64),
                            offset
                        );
                        let _ = writeln!(
                            out,
                            "    movq %{}, {}(%rbp)",
                            register_name(high, RegWidth::W64),
                            offset + 8
                        );
                        next_reg += 2;
                    }
                    None => {
                        for i in 0..2u64 {
                            let _ = writeln!(
                                out,
                                "    movq {}(%rbp), %rax",
                                stack_offset + (i as i64) * 8
                            );
                            let _ = writeln!(
                                out,
                                "    movq %rax, {}(%rbp)",
                                offset + (i as i64) * 8
                            );
                        }
                        stack_offset += 16;
                    }
                }
            }
            crate::machine::ArgKind::Stack(size) => {
                // An aggregate larger than two eightbytes travels in the
                // caller's outgoing area, which starts at `16(%rbp)`.
                let mut copied = 0u64;
                while copied < size {
                    let width = if size - copied >= 8 { RegWidth::W64 } else { RegWidth::W8 };
                    let _ = writeln!(
                        out,
                        "    mov{} {}(%rbp), %{}",
                        width.suffix(),
                        stack_offset + copied as i64,
                        register_name(0, width)
                    );
                    let _ = writeln!(
                        out,
                        "    mov{} %{}, {}(%rbp)",
                        width.suffix(),
                        register_name(0, width),
                        offset + copied as i64
                    );
                    copied += width.bytes();
                }
                stack_offset += size.div_ceil(8) as i64 * 8;
            }
        }
    }

    for block in &function.blocks {
        for inst in &block.insts {
            // Labels are printed flush left; instructions indented.
            if let Inst::Label(label) = inst {
                let _ = writeln!(out, ".L{}_{}:", symbol, label.0);
            } else {
                let line = print_inst(inst, &symbol);
                if !line.is_empty() {
                    let _ = writeln!(out, "    {line}");
                }
            }
        }
    }

    // Every `ret` in the body jumps here instead of returning directly, so the
    // epilogue runs exactly once regardless of how many paths leave the
    // function. Without this a body's own `ret` would skip the callee-saved
    // restores.
    // The epilogue label is numbered past every block label in the function, so
    // it cannot collide with one. See `Isel::epilogue_label`.
    let _ = writeln!(out, ".L{}_{}:", symbol, function.block_count);
    for reg in callee_saved.iter().rev() {
        let _ = writeln!(out, "    popq %{}", register_name(*reg, RegWidth::W64));
    }
    let _ = writeln!(out, "    leave");
    let _ = writeln!(out, "    ret");
    let _ = writeln!(out, ".size {symbol}, .-{symbol}");
    out
}

fn align16(n: u64) -> u64 {
    if n == 0 {
        0
    } else {
        n.div_ceil(16) * 16
    }
}

/// Prints one instruction, or an empty string for pseudo-instructions.
///
/// `function` namespaces the block labels so that two functions in one assembly
/// file cannot collide.
pub fn print_inst(inst: &Inst, function: &str) -> String {
    match inst {
        Inst::Label(_) => String::new(),
        Inst::Comment(text) => format!("# {text}"),
        Inst::Move { kind, src, dst } => print_move(*kind, src, dst),
        Inst::Alu { op, width, src, dst } => print_alu(*op, *width, src, dst),
        Inst::Cmp { width, lhs, rhs } => format!("cmp{} {}, {}", width.suffix(), op_str(rhs, *width), op_str(lhs, *width)),
        Inst::Test { width, lhs, rhs } => {
            format!("test{} {}, {}", width.suffix(), op_str(rhs, *width), op_str(lhs, *width))
        }
        Inst::SetCc { cond, dst } => format!("set{} {}", cond.suffix(), op_str(dst, RegWidth::W8)),
        Inst::Jmp(label) => format!("jmp .L{}_{}", function, label.0),
        Inst::Jcc { cond, target } => format!("j{} .L{}_{}", cond.suffix(), function, target.0),
        Inst::Call { target, variadic } => {
            // SysV requires `%al` to hold the number of vector registers used when
            // calling a variadic function. UMAY passes none, so it is zero; a call
            // to `open` would otherwise hand the callee whatever `%al` held.
            let prefix = if *variadic { "    movb $0, %al\n" } else { "" };
            let call = match target {
                Operand::Mem(mem) => match &mem.symbol {
                    Some(sym) => format!("call {}", sym.name),
                    None => format!("callq *{}", op_str(target, RegWidth::W64)),
                },
                other => format!("callq *{}", op_str(other, RegWidth::W64)),
            };
            format!("{prefix}{call}")
        }
        Inst::Ret => "ret".to_string(),
        Inst::Push(op) => format!("pushq {}", op_str(op, RegWidth::W64)),
        Inst::Pop(op) => format!("popq {}", op_str(op, RegWidth::W64)),
        Inst::Lea { src, dst } => format!("leaq {}, {}", mem_str(src, RegWidth::W64), op_str(dst, RegWidth::W64)),
        Inst::Load { kind, src, dst } => {
            // An extending load names both widths, as in `movzbq`: source first.
            // The register the value ends up in is the destination width; the
            // width the memory is read at is the source width.
            let (mnemonic, read, write) = match *kind {
                // When no extension is needed the load is a plain move.
                MoveKind::Move(w) => (format!("mov{}", w.suffix()), w, w),
                MoveKind::SignExtend { from, to } if from == to => {
                    (format!("mov{}", from.suffix()), from, to)
                }
                MoveKind::ZeroExtend { from, to } if from == to => {
                    (format!("mov{}", from.suffix()), from, to)
                }
                // Writing a 32-bit register already clears the upper half, so
                // `movl` is the zero-extending form for that case.
                MoveKind::ZeroExtend { from: RegWidth::W32, to: RegWidth::W64 } => {
                    ("movl".to_string(), RegWidth::W32, RegWidth::W32)
                }
                MoveKind::SignExtend { from, to } => {
                    (format!("movs{}{}", from.suffix(), to.suffix()), from, to)
                }
                MoveKind::ZeroExtend { from, to } => {
                    (format!("movz{}{}", from.suffix(), to.suffix()), from, to)
                }
            };
            format!("{mnemonic} {}, {}", mem_str(src, read), op_str(dst, write))
        }
        Inst::Store { kind, src, dst } => {
            let width = match kind {
                MoveKind::Move(w) | MoveKind::SignExtend { to: w, .. } | MoveKind::ZeroExtend { to: w, .. } => *w,
            };
            let mnemonic = match kind {
                MoveKind::Move(_) => format!("mov{}", width.suffix()),
                // A store never extends; the selector does not emit these.
                MoveKind::SignExtend { from, .. } => format!("movs{}", from.suffix()),
                MoveKind::ZeroExtend { from, .. } => format!("movz{}", from.suffix()),
            };
            format!("{mnemonic} {}, {}", op_str(src, width), mem_str(dst, width))
        }
        Inst::SignExtendAcc { .. } => "cqto".to_string(),
    }
}

fn print_move(kind: MoveKind, src: &Operand, dst: &Operand) -> String {
    match kind {
        MoveKind::Move(width) => {
            format!("mov{} {}, {}", width.suffix(), op_str(src, width), op_str(dst, width))
        }
        MoveKind::SignExtend { from, to } => {
            // `movsX` naming: source width first, then destination.
            format!("movs{}{} {}, {}", from.suffix(), to.suffix(), op_str(src, from), op_str(dst, to))
        }
        // `movz{32}{64}` does not exist: a 32-bit register write clears the upper
        // half on its own, so a 32-bit move is the zero-extending form.
        MoveKind::ZeroExtend { from: RegWidth::W32, to: RegWidth::W64 } => {
            format!("movl {}, {}", op_str(src, RegWidth::W32), op_str(dst, RegWidth::W32))
        }
        MoveKind::ZeroExtend { from, to } => {
            format!("movz{}{} {}, {}", from.suffix(), to.suffix(), op_str(src, from), op_str(dst, to))
        }
    }
}

fn print_alu(op: AluOp, width: RegWidth, src: &Operand, dst: &Operand) -> String {
    match op {
        AluOp::Add => format!("add{} {}, {}", width.suffix(), op_str(src, width), op_str(dst, width)),
        AluOp::Sub => format!("sub{} {}, {}", width.suffix(), op_str(src, width), op_str(dst, width)),
        AluOp::Imul => format!("imul{} {}, {}", width.suffix(), op_str(src, width), op_str(dst, width)),
        // Divisions read the accumulator implicitly; the selector has already set
        // up the operand, so only the divisor is printed.
        AluOp::Idiv => format!("idiv{} {}", width.suffix(), op_str(src, width)),
        AluOp::Div => format!("div{} {}", width.suffix(), op_str(src, width)),
        AluOp::And => format!("and{} {}, {}", width.suffix(), op_str(src, width), op_str(dst, width)),
        AluOp::Or => format!("or{} {}, {}", width.suffix(), op_str(src, width), op_str(dst, width)),
        AluOp::Xor => format!("xor{} {}, {}", width.suffix(), op_str(src, width), op_str(dst, width)),
        AluOp::Shl => format!("shl{} {}, {}", width.suffix(), op_str(src, width), op_str(dst, width)),
        AluOp::Shr => format!("shr{} {}, {}", width.suffix(), op_str(src, width), op_str(dst, width)),
        AluOp::Sar => format!("sar{} {}, {}", width.suffix(), op_str(src, width), op_str(dst, width)),
        AluOp::Neg => format!("neg{} {}", width.suffix(), op_str(dst, width)),
        AluOp::Not => format!("not{} {}", width.suffix(), op_str(dst, width)),
    }
}

/// Prints an operand with its width.
pub fn op_str(op: &Operand, width: RegWidth) -> String {
    match op {
        // AT&T syntax prefixes registers with `%`.
        Operand::Reg(reg) => format!("%{}", reg.name(width)),
        Operand::Imm(v) => format!("${v}"),
        Operand::Mem(mem) => mem_str(mem, width),
        // A label operand only appears inside a `jmp`/`jcc`, which print their
        // target themselves; printing a bare label here keeps `op_str` total.
        Operand::Label(label) => format!(".L{}", label.0),
    }
}

/// Prints a memory operand: `offset(%base,%index,scale)`.
pub fn mem_str(mem: &Mem, _width: RegWidth) -> String {
    let mut out = String::new();
    if let Some(sym) = &mem.symbol {
        out.push_str(&sym.name);
        let total = mem.offset + sym.addend;
        if total != 0 {
            if total > 0 {
                out.push('+');
            }
            let _ = write!(out, "{total}");
        }
        out.push_str("(%rip)");
        return out;
    }
    if mem.offset != 0 || mem.base.is_none() {
        let _ = write!(out, "{}", mem.offset);
    }
    out.push('(');
    if let Some(base) = mem.base {
        out.push('%');
        out.push_str(&register_name_of(base));
    }
    if let Some((index, scale)) = mem.index {
        out.push_str(",%");
        out.push_str(&register_name_of(index));
        let _ = write!(out, ",{}", 1u32 << scale);
    }
    out.push(')');
    out
}

fn register_name_of(reg: Reg) -> String {
    match reg {
        Reg::Phys(code) => reg_name(code).to_string(),
        Reg::Virtual(v) => format!("!v{v}"),
    }
}

/// Turns a UMAY-level name into something the assembler accepts.
///
/// Ordinary function names are already valid, and keeping them unchanged means
/// diagnostics, stack traces and the linker all agree. A constant body is named
/// `const NAME` for the MIR printer's benefit, which is not a legal symbol, so
/// the space becomes an underscore and the two words are joined.
pub fn symbol_name(name: &str) -> String {
    let mut out = String::with_capacity(name.len() + 5);
    for (i, ch) in name.chars().enumerate() {
        if ch.is_ascii_alphanumeric() || ch == '_' || (ch == '.' && i > 0) {
            out.push(ch);
        } else if ch == ' ' {
            out.push('_');
        } else {
            out.push('_');
        }
    }
    if out.chars().next().map(|c| c.is_ascii_digit()).unwrap_or(true) {
        out.insert_str(0, "umay_");
    }
    out
}
