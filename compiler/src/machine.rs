//! Machine-level intermediate representation for x86_64.
//!
//! Instruction selection turns a MIR body into a [`MachineFunction`]: a list of
//! blocks of instructions, each operating on virtual registers and stack slots.
//! Register allocation then rewrites the virtual registers to physical ones.
//!
//! Splitting the two means instruction selection never has to think about
//! register pressure, and register allocation never has to think about MIR.
//!
//! The instruction set modelled here is the subset of x86_64 that UMAY's current
//! language features need. It is not a complete x86 model and is not trying to
//! be: an instruction is added when a construct needs it, with a test.

use crate::hir::LocalId;
use crate::types::TyId;

/// A virtual or physical register.
///
/// Virtual registers come from instruction selection and are numbered from 0
/// per function. Register allocation replaces every `Virtual` with a `Phys`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Reg {
    /// A pre-colouring allocation pass assigns virtual index.
    Virtual(u32),
    /// A physical register, by its encoding (0 = rax, 1 = rcx, ...).
    Phys(u8),
}

impl Reg {
    pub fn is_virtual(self) -> bool {
        matches!(self, Reg::Virtual(_))
    }
}

/// The general-purpose registers the allocator may hand out.
///
/// Callee-saved registers are included: they are cheaper to use than spilling,
/// at the price of a save and restore in the prologue. `rsp` and `rbp` are
/// deliberately absent — they are reserved for the frame.
/// Caller-saved first, in the order the allocator prefers them, then
/// callee-saved, which cost a save and restore but avoid a spill.
pub const ALLOCATABLE: [u8; 12] = [
    0, // rax
    1, // rcx
    2, // rdx
    6, // rsi
    7, // rdi
    8, // r8
    9, // r9
    // Callee-saved.
    3, // rbx
    12, // r12
    13, // r13
    14, // r14
    15, // r15
];

/// Registers a call may clobber, in SysV order.
pub const CALLER_SAVED: [u8; 9] = [0, 1, 2, 6, 7, 8, 9, 10, 11];

/// A register the allocator never hands out, for scratch use inside a single
/// rewritten instruction.
///
/// It must not be allocatable: the rewrite that needs it runs *after*
/// allocation, so if a value were already living there the scratch use would
/// silently destroy it. `r11` is caller-saved and is not otherwise used, so
/// clobbering it cannot surprise anything.
pub const SCRATCH: u8 = 11;

pub fn reg_name(code: u8) -> &'static str {
    match code {
        0 => "rax",
        1 => "rcx",
        2 => "rdx",
        3 => "rbx",
        4 => "rsp",
        5 => "rbp",
        6 => "rsi",
        7 => "rdi",
        8 => "r8",
        9 => "r9",
        10 => "r10",
        11 => "r11",
        12 => "r12",
        13 => "r13",
        14 => "r14",
        15 => "r15",
        _ => "<invalid>",
    }
}

/// A register operand at a specific width.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegWidth {
    /// 64-bit general purpose.
    W64,
    /// 32-bit. Writing a 32-bit register zero-extends into the 64-bit one.
    W32,
    /// 16-bit.
    W16,
    /// 8-bit low byte.
    W8,
}

impl RegWidth {
    /// The suffix used in AT&T-style mnemonics (`q`, `l`, `w`, `b`).
    pub fn suffix(self) -> &'static str {
        match self {
            RegWidth::W64 => "q",
            RegWidth::W32 => "l",
            RegWidth::W16 => "w",
            RegWidth::W8 => "b",
        }
    }

    pub fn bytes(self) -> u64 {
        match self {
            RegWidth::W64 => 8,
            RegWidth::W32 => 4,
            RegWidth::W16 => 2,
            RegWidth::W8 => 1,
        }
    }
}

/// Reads a virtual/physical register at a width.
///
/// The width is not stored on [`Reg`] because register allocation cares only
/// about identity; the instruction that uses the register carries the width.
impl Reg {
    pub fn name(self, width: RegWidth) -> String {
        let code = match self {
            Reg::Phys(c) => c,
            Reg::Virtual(v) => panic!("virtual register {v} reached the printer"),
        };
        register_name(code, width)
    }
}

/// The name of physical register `code` at `width`, respecting the x86_64
/// naming quirks: `spl`/`bpl`/`sil`/`dil` for the low byte of rsp/rbp/rsi/rdi,
/// which share an encoding with the legacy high-byte registers.
pub fn register_name(code: u8, width: RegWidth) -> String {
    let bit64 = reg_name(code);
    match width {
        RegWidth::W64 => bit64.to_string(),
        RegWidth::W32 => match code {
            0 => "eax".into(),
            1 => "ecx".into(),
            2 => "edx".into(),
            3 => "ebx".into(),
            4 => "esp".into(),
            5 => "ebp".into(),
            6 => "esi".into(),
            7 => "edi".into(),
            8 => "r8d".into(),
            9 => "r9d".into(),
            10 => "r10d".into(),
            11 => "r11d".into(),
            12 => "r12d".into(),
            13 => "r13d".into(),
            14 => "r14d".into(),
            15 => "r15d".into(),
            _ => "<invalid>".into(),
        },
        RegWidth::W16 => match code {
            0 => "ax".into(),
            1 => "cx".into(),
            2 => "dx".into(),
            3 => "bx".into(),
            4 => "sp".into(),
            5 => "bp".into(),
            6 => "si".into(),
            7 => "di".into(),
            8 => "r8w".into(),
            9 => "r9w".into(),
            10 => "r10w".into(),
            11 => "r11w".into(),
            12 => "r12w".into(),
            13 => "r13w".into(),
            14 => "r14w".into(),
            15 => "r15w".into(),
            _ => "<invalid>".into(),
        },
        RegWidth::W8 => match code {
            0 => "al".into(),
            1 => "cl".into(),
            2 => "dl".into(),
            3 => "bl".into(),
            // The low-byte registers of rsp..rdi need their own names, because
            // `ah`..`bh` occupy the same encodings.
            4 => "spl".into(),
            5 => "bpl".into(),
            6 => "sil".into(),
            7 => "dil".into(),
            8 => "r8b".into(),
            9 => "r9b".into(),
            10 => "r10b".into(),
            11 => "r11b".into(),
            12 => "r12b".into(),
            13 => "r13b".into(),
            14 => "r14b".into(),
            15 => "r15b".into(),
            _ => "<invalid>".into(),
        },
    }
}

/// A memory operand: `offset(base)`, optionally indexed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mem {
    /// The base register, or `None` for an absolute address.
    pub base: Option<Reg>,
    /// Byte offset from the base.
    pub offset: i64,
    /// `base + index * scale` addressing, used for arrays.
    pub index: Option<(Reg, u8)>,
    /// Set when the operand refers to a named symbol instead of a register.
    pub symbol: Option<SymbolRef>,
}

impl Mem {
    pub fn stack(offset: i64) -> Self {
        Mem { base: Some(Reg::Phys(5)), offset, index: None, symbol: None }
    }

    pub fn base(reg: Reg) -> Self {
        Mem { base: Some(reg), offset: 0, index: None, symbol: None }
    }

    pub fn with_offset(mut self, offset: i64) -> Self {
        self.offset = offset;
        self
    }

    pub fn indexed(mut self, index: Reg, scale: u8) -> Self {
        self.index = Some((index, scale));
        self
    }

    pub fn symbol(name: &str) -> Self {
        Mem {
            base: None,
            offset: 0,
            index: None,
            symbol: Some(SymbolRef { name: name.to_string(), addend: 0 }),
        }
    }
}

/// A reference to a symbol, resolved by the assembler.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SymbolRef {
    pub name: String,
    pub addend: i64,
}

/// An operand of a machine instruction.
#[derive(Debug, Clone, PartialEq)]
pub enum Operand {
    Reg(Reg),
    /// An immediate, already truncated to the operand's width.
    Imm(i64),
    Mem(Mem),
    /// A label, for jumps and calls.
    Label(Label),
}

/// A block label within a machine function.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Label(pub u32);

/// Move width and sign handling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MoveKind {
    /// Same-width copy.
    Move(RegWidth),
    /// `movsx`: sign-extend from a narrower source.
    SignExtend { from: RegWidth, to: RegWidth },
    /// `movzx`: zero-extend. On x86_64 a 32-bit move already zero-extends, so
    /// this only appears for 8- and 16-bit sources.
    ZeroExtend { from: RegWidth, to: RegWidth },
}

/// The arithmetic and logical instructions the selector emits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AluOp {
    Add,
    Sub,
    Imul,
    /// Signed divide: `cqo` then `idiv`.
    Idiv,
    /// Unsigned divide: `xor rdx, rdx` then `div`.
    Div,
    And,
    Or,
    Xor,
    Shl,
    Shr,
    /// Arithmetic shift right.
    Sar,
    Neg,
    Not,
}

/// The condition codes, named by the comparison they express.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cond {
    Eq,
    Ne,
    /// Signed.
    Lt,
    Le,
    Gt,
    Ge,
    /// Unsigned.
    B,
    Be,
    A,
    Ae,
}

impl Cond {
    /// The AT&T mnemonic suffix for `set`/`j`.
    pub fn suffix(self) -> &'static str {
        match self {
            Cond::Eq => "e",
            Cond::Ne => "ne",
            Cond::Lt => "l",
            Cond::Le => "le",
            Cond::Gt => "g",
            Cond::Ge => "ge",
            Cond::B => "b",
            Cond::Be => "be",
            Cond::A => "a",
            Cond::Ae => "ae",
        }
    }

    /// The same condition with signedness flipped, used when a selector has only
    /// the unsigned form available.
    pub fn unsigned(self) -> Cond {
        match self {
            Cond::Lt => Cond::B,
            Cond::Le => Cond::Be,
            Cond::Gt => Cond::A,
            Cond::Ge => Cond::Ae,
            other => other,
        }
    }
}

/// One machine instruction.
#[derive(Debug, Clone, PartialEq)]
pub enum Inst {
    /// `mov src, dst`
    Move { kind: MoveKind, src: Operand, dst: Operand },
    /// `op src, dst` — dst is both source and destination.
    Alu { op: AluOp, width: RegWidth, src: Operand, dst: Operand },
    /// `cmp rhs, lhs` sets flags for `lhs - rhs`.
    Cmp { width: RegWidth, lhs: Operand, rhs: Operand },
    /// `setcc` writing a byte.
    SetCc { cond: Cond, dst: Operand },
    /// `test` for boolean negation and zero checks.
    Test { width: RegWidth, lhs: Operand, rhs: Operand },
    /// Unconditional jump.
    Jmp(Label),
    /// Conditional jump.
    Jcc { cond: Cond, target: Label },
    /// `call`. The target is a symbol or a register.
    Call { target: Operand },
    /// `ret`
    Ret,
    /// Push onto the stack.
    Push(Operand),
    /// Pop from the stack.
    Pop(Operand),
    /// `lea` computes an address without loading.
    Lea { src: Mem, dst: Operand },
    /// Load from memory to register.
    Load { kind: MoveKind, src: Mem, dst: Operand },
    /// Store from register to memory.
    Store { kind: MoveKind, src: Operand, dst: Mem },
    /// `cqo` / `cdq`: sign-extend the accumulator into the high half before a
    /// signed divide.
    SignExtendAcc { width: RegWidth },
    /// A pseudo-instruction: a label definition with no code of its own.
    Label(Label),
    /// A comment in the output, ignored by the assembler.
    Comment(String),
}

impl Inst {
    /// Every register this instruction reads.
    pub fn reads(&self) -> Vec<Reg> {
        let mut out = Vec::new();
        let from_operand = |op: &Operand, out: &mut Vec<Reg>| {
            if let Operand::Reg(r) = op {
                out.push(*r);
            }
        };
        let from_mem = |m: &Mem, out: &mut Vec<Reg>| {
            if let Some(b) = m.base {
                if b.is_virtual() {
                    out.push(b);
                }
            }
            if let Some((i, _)) = m.index {
                if i.is_virtual() {
                    out.push(i);
                }
            }
        };
        match self {
            Inst::Move { src, dst, .. } => {
                from_operand(src, &mut out);
                if let Operand::Mem(m) = dst {
                    from_mem(m, &mut out);
                }
            }
            Inst::Alu { src, dst, .. } => {
                from_operand(src, &mut out);
                from_operand(dst, &mut out);
            }
            Inst::Cmp { lhs, rhs, .. } | Inst::Test { lhs, rhs, .. } => {
                from_operand(lhs, &mut out);
                from_operand(rhs, &mut out);
            }
            Inst::SetCc { dst, .. } => from_operand(dst, &mut out),
            Inst::Call { target } => from_operand(target, &mut out),
            Inst::Push(op) | Inst::Pop(op) => from_operand(op, &mut out),
            Inst::Lea { src, dst } => {
                from_mem(src, &mut out);
                from_operand(dst, &mut out);
            }
            Inst::Load { src, dst, .. } => {
                from_mem(src, &mut out);
                if let Operand::Mem(m) = dst {
                    from_mem(m, &mut out);
                }
            }
            Inst::Store { src, dst, .. } => {
                from_operand(src, &mut out);
                from_mem(dst, &mut out);
            }
            Inst::Jmp(_) | Inst::Jcc { .. } | Inst::Ret | Inst::SignExtendAcc { .. } | Inst::Label(_)
            | Inst::Comment(_) => {}
        }
        out.retain(|r| r.is_virtual());
        out
    }

    /// Every register this instruction reads as part of an address.
    ///
    /// These cannot be spilled: turning one into a stack slot would need an
    /// extra load before the access, and the selector does not emit one.
    pub fn address_registers(&self) -> Vec<Reg> {
        let mut out = Vec::new();
        let from_mem = |m: &Mem, out: &mut Vec<Reg>| {
            if let Some(b) = m.base {
                out.push(b);
            }
            if let Some((i, _)) = m.index {
                out.push(i);
            }
        };
        match self {
            Inst::Move { dst: Operand::Mem(m), .. }
            | Inst::Load { src: m, .. }
            | Inst::Store { dst: m, .. }
            | Inst::Lea { src: m, .. } => from_mem(m, &mut out),
            _ => {}
        }
        out.retain(|r| r.is_virtual());
        out
    }

    /// Every register this instruction writes.
    pub fn writes(&self) -> Vec<Reg> {
        let mut out = Vec::new();
        match self {
            Inst::Move { dst, .. }
            | Inst::SetCc { dst, .. }
            | Inst::Lea { dst, .. }
            | Inst::Load { dst, .. } => {
                if let Operand::Reg(r) = dst {
                    if r.is_virtual() {
                        out.push(*r);
                    }
                }
            }
            Inst::Alu { dst, .. } => {
                if let Operand::Reg(r) = dst {
                    if r.is_virtual() {
                        out.push(*r);
                    }
                }
            }
            // A call clobbers caller-saved registers, modelled by the allocator
            // directly rather than here.
            Inst::Call { .. }
            | Inst::Cmp { .. }
            | Inst::Test { .. }
            | Inst::Jmp(_)
            | Inst::Jcc { .. }
            | Inst::Ret
            | Inst::Push(_)
            | Inst::Pop(_)
            | Inst::Store { .. }
            | Inst::SignExtendAcc { .. }
            | Inst::Label(_)
            | Inst::Comment(_) => {}
        }
        out
    }
}

/// A block of machine instructions.
#[derive(Debug, Clone)]
pub struct MachineBlock {
    pub label: Label,
    pub insts: Vec<Inst>,
}

/// A function after instruction selection, before register allocation.
#[derive(Debug, Clone)]
pub struct MachineFunction {
    pub name: String,
    pub blocks: Vec<MachineBlock>,
    /// Every virtual register used, with the width it is read at.
    pub virtual_regs: Vec<(Reg, RegWidth)>,
    /// Bytes of stack space this function needs for its own locals, excluding
    /// any outgoing call arguments and spills.
    pub stack_size: u64,
    /// True when the function has a `ret` in the ordinary flow, so a trailing
    /// `ret` is not needed.
    pub ret_ty: TyId,
    /// The MIR local each stack slot belongs to, for debugging.
    pub local_slots: Vec<(LocalId, i64)>,
    /// Symbols this function references, for the object file's relocations.
    pub symbol_refs: Vec<String>,
    /// Number of incoming parameters, all of which arrive in registers for now.
    pub arg_count: u32,
    /// How many labels the function's blocks occupy, so the epilogue label can
    /// be numbered after them.
    pub block_count: u32,
    /// The width each incoming parameter is stored at.
    pub arg_widths: Vec<RegWidth>,
}
