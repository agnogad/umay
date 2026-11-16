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
///
/// `target` decides how the symbol is spelled and which object-file directives
/// surround it. The instruction text itself is the same for every target that
/// shares an ABI: macOS is SysV like Linux, so the only difference there is the
/// leading underscore.
pub fn print_function(
    function: &MachineFunction,
    spill_bytes: u64,
    callee_saved: &[u8],
    target: crate::target::Target,
    foreign: &std::collections::HashSet<String>,
) -> String {
    let mut out = String::new();
    // The label and the local-label namespace use the target's spelling for the
    // symbol, so that internal branches and the global definition agree. The
    // prefix is applied once here rather than at every reference.
    let symbol = target.mangle(&symbol_name(&function.name));
    let global = target.globl_directive();
    if !global.is_empty() {
        let _ = writeln!(out, "{global} {symbol}");
    }
    // `.type` is ELF-only metadata. Mach-O and COFF reject it.
    if target.format == crate::target::ObjectFormat::Elf {
        let _ = writeln!(out, ".type {symbol}, @function");
    }
    let _ = writeln!(out, "{symbol}:");

    // Prologue: establish the frame, then preserve the callee-saved registers
    // this function actually uses.
    //
    // Win64 additionally reserves 32 bytes of shadow space inside the frame, so
    // that this function's own outgoing calls always have the area the callee
    // expects. SysV reserves nothing.
    let shadow = match target.abi {
        crate::target::Abi::SysV => 0,
        crate::target::Abi::Win64 => 32,
    };
    let callee_bytes = (callee_saved.len() as u64) * 8;
    let frame = align16(function.stack_size + spill_bytes + callee_bytes + shadow);
    let _ = writeln!(out, "    pushq %rbp");
    let _ = writeln!(out, "    movq %rsp, %rbp");
    if frame > 0 {
        let _ = writeln!(out, "    subq ${frame}, %rsp");
    }
    // Save callee-saved registers into their dedicated frame slots past locals
    // and spill slots so they neither collide with local_slots nor shift rsp
    // relative to rbp.
    for (i, reg) in callee_saved.iter().enumerate() {
        let off = -((function.stack_size + spill_bytes + (i as u64 + 1) * 8) as i64);
        let _ = writeln!(out, "    movq %{}, {off}(%rbp)", register_name(*reg, RegWidth::W64));
    }
    // The frame is aligned here rather than relied upon to be aligned already.
    //
    // Both ABIs promise a 16-byte stack pointer at a call instruction, and every
    // frame assumes it on entry. That holds for a function entered by `call` --
    // but a Windows thread starts its routine without one, so the pointer is
    // eight bytes off from what the frame expects, and the first call the thread
    // makes faults. Rounding down fixes that entry and changes nothing for the
    // ordinary one.
    //
    // `%rbp` was saved before this point, so the epilogue's `leave` restores the
    // caller's frame exactly whatever is trimmed here.
    let _ = writeln!(out, "    andq $-16, %rsp");
    // The frame is aligned here rather than relied upon to be aligned already.
    //
    // Both ABIs promise a 16-byte stack pointer at a call instruction, and every
    // frame assumes it on entry. That holds for a function entered by `call` --
    // but a Windows thread starts its routine without one, so the pointer is 16
    // bytes' worth of "off by eight" from what the frame expects, and the first
    // call the thread makes lands on a misaligned stack. Rounding down fixes it
    // for that entry and changes nothing for the ordinary one.
    //
    // `%rbp` was saved before this point, so the epilogue's `leave` restores the
    // caller's frame exactly whatever is trimmed here.


    // Move incoming arguments into their stack slots. They are stored after the
    // prologue because the prologue uses the same registers.
    //
    // When the return value travels through a hidden pointer, `rdi` holds that
    // pointer instead of the first argument, so every argument moves along one
    // register.
    if let Some(offset) = function.sret_offset {
        let _ = writeln!(out, "    movq %rdi, {offset}(%rbp)");
    }
    // The argument registers this ABI uses, in order. Win64 passes the first
    // argument in `%rcx` and has only four registers, so a function with five
    // parameters reads its fifth from the stack where SysV would have used
    // `%r8`.
    let all_arg_regs: &[u8] = match target.abi {
        crate::target::Abi::SysV => &crate::machine::ARG_REGS_SYSV,
        crate::target::Abi::Win64 => &crate::machine::ARG_REGS_WIN64,
    };
    let mut next_reg = usize::from(function.sret_offset.is_some());
    // Stack arguments begin just above the return address; an eightbyte of
    // padding sits there only when it keeps the area aligned, which the caller
    // already arranged.
    //
    // On Win64 the caller's frame puts 32 bytes of shadow space between the
    // return address and the first stack argument, so the offset starts past it.
    // Reading from `16(%rbp)` there would read the callee's own spill area.
    let mut stack_offset = match target.abi {
        crate::target::Abi::SysV => 16i64,
        crate::target::Abi::Win64 => 16 + 32,
    };
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
                let line = print_inst(inst, &symbol, target, foreign);
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
    for (i, reg) in callee_saved.iter().enumerate().rev() {
        let off = -((function.stack_size + spill_bytes + (i as u64 + 1) * 8) as i64);
        let _ = writeln!(out, "    movq {off}(%rbp), %{}", register_name(*reg, RegWidth::W64));
    }
    let _ = writeln!(out, "    leave");
    let _ = writeln!(out, "    ret");
    // `.size` records the function's extent for ELF debug info and is not part
    // of the other two formats.
    if target.format == crate::target::ObjectFormat::Elf {
        let _ = writeln!(out, ".size {symbol}, .-{symbol}");
    }
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
pub fn print_inst(
    inst: &Inst,
    function: &str,
    target: crate::target::Target,
    foreign: &std::collections::HashSet<String>,
) -> String {
    match inst {
        Inst::Label(_) => String::new(),
        Inst::Comment(text) => format!("# {text}"),
        Inst::Move { kind, src, dst } => print_move(*kind, src, dst, target, foreign),
        Inst::Alu { op, width, src, dst } => print_alu(*op, *width, src, dst, target, foreign),
        Inst::Cmp { width, lhs, rhs } => format!("cmp{} {}, {}", width.suffix(), op_str(rhs, *width, target, foreign), op_str(lhs, *width, target, foreign)),
        Inst::Test { width, lhs, rhs } => {
            format!("test{} {}, {}", width.suffix(), op_str(rhs, *width, target, foreign), op_str(lhs, *width, target, foreign))
        }
        Inst::SetCc { cond, dst } => format!("set{} {}", cond.suffix(), op_str(dst, RegWidth::W8, target, foreign)),
        Inst::Jmp(label) => format!("jmp .L{}_{}", function, label.0),
        Inst::Jcc { cond, target } => format!("j{} .L{}_{}", cond.suffix(), function, target.0),
        Inst::Call { target: callee, variadic } => {
            // SysV requires `%al` to hold the number of vector registers used when
            // calling a variadic function. UMAY passes none, so it is zero; a call
            // to `open` would otherwise hand the callee whatever `%al` held.
            let prefix = if *variadic { "    movb $0, %al\n" } else { "" };
            let call = match callee {
                Operand::Mem(mem) => match &mem.symbol {
                    // A call to a named function is a direct reference to a
                    // symbol that lives in another object, so it needs the same
                    // spelling as its definition. On COFF and Mach-O that means
                    // the target's leading underscore; without it every call
                    // into the runtime was an undefined reference and the link
                    // failed.
                    Some(sym) => format!("call {}", mangle_symbol(&sym.name, target, foreign)),
                    None => format!("callq *{}", op_str(callee, RegWidth::W64, target, foreign)),
                },
                other => format!("callq *{}", op_str(other, RegWidth::W64, target, foreign)),
            };
            format!("{prefix}{call}")
        }
        Inst::Ret => "ret".to_string(),
        Inst::Push(op) => format!("pushq {}", op_str(op, RegWidth::W64, target, foreign)),
        Inst::Pop(op) => format!("popq {}", op_str(op, RegWidth::W64, target, foreign)),
        Inst::Lea { src, dst } => format!("leaq {}, {}", mem_str(src, RegWidth::W64, target, foreign), op_str(dst, RegWidth::W64, target, foreign)),
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
            format!("{mnemonic} {}, {}", mem_str(src, read, target, foreign), op_str(dst, write, target, foreign))
        }
        Inst::Store { kind, src, dst } => {
            let width = match kind {
                MoveKind::Move(w) | MoveKind::SignExtend { to: w, .. } | MoveKind::ZeroExtend { to: w, .. } => *w,
            };
            // A 64-bit immediate that does not fit the sign-extended 32-bit
            // field `movq` offers has to go through a register. `movabsq`
            // reaches the whole 64-bit range, and the selector never keeps a
            // value in `%rax` across a store.
            if let Operand::Imm(v) = src {
                if width == RegWidth::W64 && i32::try_from(*v).is_err() {
                    return format!(
                        "movabsq ${v}, %rax\n    movq %rax, {}",
                        mem_str(dst, width, target, foreign)
                    );
                }
            }
            let mnemonic = match kind {
                MoveKind::Move(_) => format!("mov{}", width.suffix()),
                // A store never extends; the selector does not emit these.
                MoveKind::SignExtend { from, .. } => format!("movs{}", from.suffix()),
                MoveKind::ZeroExtend { from, .. } => format!("movz{}", from.suffix()),
            };
            format!("{mnemonic} {}, {}", op_str(src, width, target, foreign), mem_str(dst, width, target, foreign))
        }
        // `idiv` at width w reads `dx:ax` at that width, so the sign has to be
        // spread at the same width: `cqto` after a 32-bit move leaves `edx` zero
        // and `-7 / 2` came out as 2147483644.
        Inst::SignExtendAcc { width } => match width {
            RegWidth::W64 => "cqto",
            RegWidth::W32 => "cltd",
            _ => "cwtd",
        }
        .to_string(),
    }
}

fn print_move(
    kind: MoveKind,
    src: &Operand,
    dst: &Operand,
    target: crate::target::Target,
    foreign: &std::collections::HashSet<String>,
) -> String {
    match kind {
        MoveKind::Move(width) => {
            // `movq` only carries a sign-extended 32-bit immediate, so a wider
            // constant is loaded with `movabsq` first.
            if let (RegWidth::W64, Operand::Imm(v)) = (width, src) {
                if i32::try_from(*v).is_err() {
                    return format!("movabsq ${v}, {}", op_str(dst, width, target, foreign));
                }
            }
            format!("mov{} {}, {}", width.suffix(), op_str(src, width, target, foreign), op_str(dst, width, target, foreign))
        }
        MoveKind::SignExtend { from, to } => {
            // `movsX` naming: source width first, then destination.
            format!("movs{}{} {}, {}", from.suffix(), to.suffix(), op_str(src, from, target, foreign), op_str(dst, to, target, foreign))
        }
        // `movz{32}{64}` does not exist: a 32-bit register write clears the upper
        // half on its own, so a 32-bit move is the zero-extending form.
        MoveKind::ZeroExtend { from: RegWidth::W32, to: RegWidth::W64 } => {
            format!("movl {}, {}", op_str(src, RegWidth::W32, target, foreign), op_str(dst, RegWidth::W32, target, foreign))
        }
        MoveKind::ZeroExtend { from, to } => {
            format!("movz{}{} {}, {}", from.suffix(), to.suffix(), op_str(src, from, target, foreign), op_str(dst, to, target, foreign))
        }
    }
}

fn print_alu(
    op: AluOp,
    width: RegWidth,
    src: &Operand,
    dst: &Operand,
    target: crate::target::Target,
    foreign: &std::collections::HashSet<String>,
) -> String {
    match op {
        AluOp::Add => format!("add{} {}, {}", width.suffix(), op_str(src, width, target, foreign), op_str(dst, width, target, foreign)),
        AluOp::Sub => format!("sub{} {}, {}", width.suffix(), op_str(src, width, target, foreign), op_str(dst, width, target, foreign)),
        AluOp::Imul => format!("imul{} {}, {}", width.suffix(), op_str(src, width, target, foreign), op_str(dst, width, target, foreign)),
        // Divisions read the accumulator implicitly; the selector has already set
        // up the operand, so only the divisor is printed.
        AluOp::Idiv => format!("idiv{} {}", width.suffix(), op_str(src, width, target, foreign)),
        AluOp::Div => format!("div{} {}", width.suffix(), op_str(src, width, target, foreign)),
        AluOp::And => format!("and{} {}, {}", width.suffix(), op_str(src, width, target, foreign), op_str(dst, width, target, foreign)),
        AluOp::Or => format!("or{} {}, {}", width.suffix(), op_str(src, width, target, foreign), op_str(dst, width, target, foreign)),
        AluOp::Xor => format!("xor{} {}, {}", width.suffix(), op_str(src, width, target, foreign), op_str(dst, width, target, foreign)),
        // A shift's count is the only operand x86_64 reads from `cl`: even at a
        // wider operation (`sarl`), the low byte `%cl` is the register the CPU
        // consults, and `sarl %ecx, %ecx` is rejected by the assembler. Printing
        // the count as `%cl` regardless of the operation width is what makes the
        // instruction legal.
        AluOp::Shl => format!("shl{} {}, {}", width.suffix(), shift_count_str(src, target, foreign), op_str(dst, width, target, foreign)),
        AluOp::Shr => format!("shr{} {}, {}", width.suffix(), shift_count_str(src, target, foreign), op_str(dst, width, target, foreign)),
        AluOp::Sar => format!("sar{} {}, {}", width.suffix(), shift_count_str(src, target, foreign), op_str(dst, width, target, foreign)),
        AluOp::Neg => format!("neg{} {}", width.suffix(), op_str(dst, width, target, foreign)),
        AluOp::Not => format!("not{} {}", width.suffix(), op_str(dst, width, target, foreign)),
    }
}

/// Prints a shift's count operand.
///
/// A shift count is an immediate or the low byte of `%rcx`: the hardware reads
/// `%cl` whatever the operation's width, so a register count must never be
/// printed at the operation's width (`%ecx` is not a legal shift count).
fn shift_count_str(
    op: &Operand,
    target: crate::target::Target,
    foreign: &std::collections::HashSet<String>,
) -> String {
    match op {
        Operand::Reg(reg) => format!("%{}", reg.name(RegWidth::W8)),
        other => op_str(other, RegWidth::W8, target, foreign),
    }
}

/// Prints an operand with its width.
pub fn op_str(
    op: &Operand,
    width: RegWidth,
    target: crate::target::Target,
    foreign: &std::collections::HashSet<String>,
) -> String {
    match op {
        // AT&T syntax prefixes registers with `%`.
        Operand::Reg(reg) => format!("%{}", reg.name(width)),
        Operand::Imm(v) => format!("${v}"),
        Operand::Mem(mem) => mem_str(mem, width, target, foreign),
        // A label operand only appears inside a `jmp`/`jcc`, which print their
        // target themselves; printing a bare label here keeps `op_str` total.
        Operand::Label(label) => format!(".L{}", label.0),
    }
}

/// Prints a memory operand: `offset(%base,%index,scale)`.
/// Spells a symbol reference the way its definition is spelled.
///
/// A reference to `write_u16` and the definition `_write_u16` are two different
/// symbols to the linker. Only the definition went through `Target::mangle`, so
/// on Mach-O and COFF every call into the runtime and every `%rip`-relative
/// reference to read-only data was an undefined symbol -- the object assembled,
/// and the link then failed with a page of `undefined reference` errors.
///
/// Local labels (`.L...`) belong to the assembler and are never mangled.
fn mangle_symbol(
    name: &str,
    target: crate::target::Target,
    foreign: &std::collections::HashSet<String>,
) -> String {
    // `.L` labels belong to the assembler, not the linker.
    if name.starts_with(".L") {
        return name.to_string();
    }
    // A symbol that names code outside this program is used verbatim: MinGW's
    // CRT exports `malloc`, and adding the prefix MSVC uses turned every libc
    // call into an undefined reference. Everything else is UMAY's own and takes
    // the target's prefix, so its definition and its references agree.
    if foreign.contains(name) {
        name.to_string()
    } else {
        target.mangle(name)
    }
}

pub fn mem_str(
    mem: &Mem,
    _width: RegWidth,
    target: crate::target::Target,
    foreign: &std::collections::HashSet<String>,
) -> String {
    let mut out = String::new();
    if let Some(sym) = &mem.symbol {
        out.push_str(&mangle_symbol(&sym.name, target, foreign));
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
