//! The AArch64 backend, starting with scalar code.
//!
//! This is deliberately a second, smaller path rather than an abstraction over
//! the x86_64 selector: the two instruction sets disagree about registers,
//! addressing modes and nearly everything else. The first slice lowers the MIR
//! subset a scalar program uses -- constants, locals, integer arithmetic,
//! branches, direct calls and returns -- and refuses aggregates, strings in
//! code, indirect calls and floats by name. A construct that is refused is a
//! `CodegenError`, never a wrong instruction.
//!
//! Every local lives in an eight-byte stack slot and every value is computed in
//! `x0`/`x1`. That is slower than register allocation and much easier to get
//! right; a later pass can replace it without changing the MIR interface. The
//! plan is in `docs/adr/0015-aarch64-backend.md`.

use std::collections::{HashMap, HashSet};
use std::fmt::Write;

use crate::codegen::{foreign_symbols, function_symbols, reachable_functions, CodegenError};
use crate::hir::{FnId, Hir};
use crate::mir::{
    BinOp, Body, Constant, Operand, Place, Rvalue, Statement, Terminator, UnOp,
};
use crate::target::Target;
use crate::types::{TyCtxt, TyId, TyKind};

/// Emits an AArch64 assembly file for every reachable body in `program`.
pub fn generate(
    program: &crate::mir::MirProgram,
    types: &mut TyCtxt,
    hir: &Hir,
    target: Target,
    _is_program: bool,
) -> Result<String, Vec<CodegenError>> {
    let symbols = function_symbols(hir, program);
    let foreign = foreign_symbols(hir);
    let reachable = reachable_functions(program);
    let mut out = String::from(".text\n");
    let mut errors = Vec::new();

    for (index, body) in program.bodies.iter().enumerate() {
        // Constant initialisers are carried as bodies with no function id. They
        // are not called; a use of `Constant::Const` is refused below. Emitting
        // them would also invent symbols such as `const NAME`, which is not a
        // legal assembler name.
        if body.fn_id.is_none() && body.name.starts_with("const ") {
            continue;
        }
        let keep = match body.fn_id {
            Some(id) => reachable.contains(&id),
            None => true,
        };
        if !keep {
            continue;
        }
        match generate_one(body, index, types, &symbols, &foreign, target) {
            Ok(text) => out.push_str(&text),
            Err(message) => errors.push(CodegenError {
                function: body.name.clone(),
                message,
            }),
        }
    }
    if !errors.is_empty() {
        return Err(errors);
    }

    if !program.strings.is_empty() {
        out.push_str(target.rodata_directive());
        for (index, text) in program.strings.iter().enumerate() {
            let _ = writeln!(out, ".Lstr_{index}:");
            let _ = writeln!(out, "    .asciz \"{}\"", quote(text));
        }
    }
    out.push_str(target.object_trailer());
    Ok(out)
}

fn generate_one(
    body: &Body,
    body_index: usize,
    types: &mut TyCtxt,
    symbols: &HashMap<FnId, String>,
    foreign: &HashSet<String>,
    target: Target,
) -> Result<String, String> {
    for local in &body.locals {
        require_scalar(types, local.ty, &format!("local `{}`", local.name))?;
    }
    if body.arg_count > 8 {
        return Err("AAPCS64 passes the first eight arguments in registers; more are not implemented".into());
    }

    let tag = body
        .fn_id
        .map(|id| id.0 as usize)
        .unwrap_or(10_000 + body_index);
    let symbol = body
        .fn_id
        .and_then(|id| symbols.get(&id))
        .map(|name| mangle(name, foreign, target))
        .unwrap_or_else(|| body.name.clone());
    let frame = frame_size(body.locals.len())?;

    let mut emitter = Emitter {
        out: String::new(),
        body,
        types,
        symbols,
        foreign,
        target,
        tag,
        frame,
    };

    let _ = writeln!(emitter.out, ".p2align 2");
    let _ = writeln!(emitter.out, ".globl {symbol}");
    let _ = writeln!(emitter.out, ".type {symbol}, %function");
    let _ = writeln!(emitter.out, "{symbol}:");
    let _ = writeln!(emitter.out, "    stp x29, x30, [sp, #-16]!");
    let _ = writeln!(emitter.out, "    mov x29, sp");
    if frame > 0 {
        let _ = writeln!(emitter.out, "    sub sp, sp, #{frame}");
    }
    for arg in 0..body.arg_count {
        let offset = local_offset(arg as usize);
        let _ = writeln!(emitter.out, "    sub x9, x29, #{offset}");
        let _ = writeln!(emitter.out, "    str x{arg}, [x9]");
    }

    for (block_index, block) in body.blocks.iter().enumerate() {
        let _ = writeln!(emitter.out, ".L{tag}_bb{block_index}:");
        for statement in &block.statements {
            emitter.statement(statement)?;
        }
        emitter.terminator(&block.terminator)?;
    }

    let _ = writeln!(emitter.out, ".size {symbol}, .-{symbol}");
    Ok(emitter.out)
}

struct Emitter<'a> {
    out: String,
    body: &'a Body,
    types: &'a mut TyCtxt,
    symbols: &'a HashMap<FnId, String>,
    foreign: &'a HashSet<String>,
    target: Target,
    tag: usize,
    frame: u32,
}

impl Emitter<'_> {
    fn statement(&mut self, statement: &Statement) -> Result<(), String> {
        match statement {
            Statement::Assign { place, value, .. } => {
                self.require_place(place)?;
                match value {
                    Rvalue::Use(operand) => self.operand(operand, 0)?,
                    Rvalue::Binary { op, lhs, rhs } => {
                        self.operand(lhs, 0)?;
                        self.operand(rhs, 1)?;
                        self.binary(*op, self.operand_ty(lhs), self.place_ty(place))?;
                    }
                    Rvalue::Unary { op, operand } => {
                        self.operand(operand, 0)?;
                        match op {
                            UnOp::Neg => {
                                let _ = writeln!(self.out, "    neg x0, x0");
                            }
                            UnOp::Not => {
                                if matches!(self.operand_ty(operand).map(|t| self.types.kind(t)), Some(TyKind::Bool)) {
                                    let _ = writeln!(self.out, "    eor x0, x0, #1");
                                } else {
                                    let _ = writeln!(self.out, "    mvn x0, x0");
                                }
                            }
                        }
                    }
                    Rvalue::Cast { operand, to: _ } => {
                        self.operand(operand, 0)?;
                    }
                    other => {
                        return Err(format!(
                            "the AArch64 backend does not support `{}` assignments yet",
                            rvalue_name(other)
                        ));
                    }
                }
                self.normalize(self.place_ty(place))?;
                self.store(place, 0)?;
                Ok(())
            }
            Statement::StorageLive(_)
            | Statement::StorageDead(_)
            | Statement::SetDropFlag { .. }
            | Statement::Nop => Ok(()),
        }
    }

    fn terminator(&mut self, terminator: &Terminator) -> Result<(), String> {
        match terminator {
            Terminator::Goto(target) => {
                let _ = writeln!(self.out, "    b .L{}_bb{}", self.tag, target.0);
                Ok(())
            }
            Terminator::SwitchInt { discr, targets, otherwise } => {
                self.operand(discr, 0)?;
                for (value, target) in targets {
                    let _ = writeln!(self.out, "    mov x1, #{value}");
                    let _ = writeln!(self.out, "    cmp x0, x1");
                    let _ = writeln!(self.out, "    b.eq .L{}_bb{}", self.tag, target.0);
                }
                let _ = writeln!(self.out, "    b .L{}_bb{}", self.tag, otherwise.0);
                Ok(())
            }
            Terminator::Call { callee, args, destination, target, .. } => {
                if args.len() > 8 {
                    return Err("AAPCS64 passes the first eight arguments in registers; more are not implemented".into());
                }
                let symbol = self.callee(callee)?;
                for (index, arg) in args.iter().enumerate() {
                    self.operand(arg, index as u8)?;
                }
                let _ = writeln!(self.out, "    bl {symbol}");
                if let Some(destination) = destination {
                    self.require_place(destination)?;
                    self.store(destination, 0)?;
                }
                let _ = writeln!(self.out, "    b .L{}_bb{}", self.tag, target.0);
                Ok(())
            }
            Terminator::Drop { target, .. } => {
                // Scalar locals have no destructor. Aggregates are refused when
                // their slots are inspected, so reaching here means there is
                // nothing to release.
                let _ = writeln!(self.out, "    b .L{}_bb{}", self.tag, target.0);
                Ok(())
            }
            Terminator::Return => {
                if let Some(local) = self.ret_local() {
                    let offset = local_offset(local);
                    let _ = writeln!(self.out, "    sub x9, x29, #{offset}");
                    let _ = writeln!(self.out, "    ldr x0, [x9]");
                } else {
                    let _ = writeln!(self.out, "    mov x0, #0");
                }
                if self.frame > 0 {
                    let _ = writeln!(self.out, "    add sp, sp, #{}", self.frame);
                }
                let _ = writeln!(self.out, "    ldp x29, x30, [sp], #16");
                let _ = writeln!(self.out, "    ret");
                Ok(())
            }
            Terminator::Unreachable => {
                let _ = writeln!(self.out, "    brk #0");
                Ok(())
            }
        }
    }

    fn callee(&self, operand: &Operand) -> Result<String, String> {
        match operand {
            Operand::Constant(Constant::Fn(id)) => self
                .symbols
                .get(id)
                .map(|name| mangle(name, self.foreign, self.target))
                .ok_or_else(|| "a call names a function with no symbol".to_string()),
            Operand::Constant(Constant::NamedFn(name)) => Ok(name.clone()),
            _ => Err("the AArch64 backend does not support indirect calls yet".into()),
        }
    }

    fn operand(&mut self, operand: &Operand, reg: u8) -> Result<(), String> {
        match operand {
            Operand::Copy(place) | Operand::Move(place) => self.load(place, reg),
            Operand::Constant(constant) => self.constant(constant, reg),
        }
    }

    fn constant(&mut self, constant: &Constant, reg: u8) -> Result<(), String> {
        match constant {
            Constant::Int { value, .. } => {
                let value = i64::try_from(*value)
                    .map_err(|_| "an integer constant does not fit in 64 bits".to_string())?;
                let _ = writeln!(self.out, "    mov x{reg}, #{value}");
            }
            Constant::Uint { value, .. } => {
                let value = u64::try_from(*value)
                    .map_err(|_| "an integer constant does not fit in 64 bits".to_string())?;
                let _ = writeln!(self.out, "    mov x{reg}, #{value}");
            }
            Constant::Bool(value) => {
                let _ = writeln!(self.out, "    mov x{reg}, #{}", u8::from(*value));
            }
            Constant::Char(value) => {
                let _ = writeln!(self.out, "    mov x{reg}, #{}", *value as u32);
            }
            Constant::Unit => {
                let _ = writeln!(self.out, "    mov x{reg}, #0");
            }
            Constant::Str(_) => {
                return Err("the AArch64 backend does not support string values yet".into());
            }
            other => {
                return Err(format!(
                    "the AArch64 backend does not support `{}` constants yet",
                    constant_name(other)
                ));
            }
        }
        Ok(())
    }

    fn load(&mut self, place: &Place, reg: u8) -> Result<(), String> {
        self.require_place(place)?;
        let offset = local_offset(place.local.0 as usize);
        let _ = writeln!(self.out, "    sub x9, x29, #{offset}");
        let _ = writeln!(self.out, "    ldr x{reg}, [x9]");
        Ok(())
    }

    fn store(&mut self, place: &Place, reg: u8) -> Result<(), String> {
        self.require_place(place)?;
        let offset = local_offset(place.local.0 as usize);
        let _ = writeln!(self.out, "    sub x9, x29, #{offset}");
        let _ = writeln!(self.out, "    str x{reg}, [x9]");
        Ok(())
    }

    fn binary(&mut self, op: BinOp, lhs_ty: Option<TyId>, result_ty: TyId) -> Result<(), String> {
        let unsigned = lhs_ty
            .map(|ty| self.is_unsigned(ty))
            .unwrap_or_else(|| self.is_unsigned(result_ty));
        let (mnemonic, comparison) = match op {
            BinOp::Add => ("add", None),
            BinOp::Sub => ("sub", None),
            BinOp::Mul => ("mul", None),
            BinOp::BitAnd => ("and", None),
            BinOp::BitOr => ("orr", None),
            BinOp::BitXor => ("eor", None),
            BinOp::Shl => ("lsl", None),
            BinOp::Shr => (if unsigned { "lsr" } else { "asr" }, None),
            BinOp::Div | BinOp::Rem => (if unsigned { "udiv" } else { "sdiv" }, None),
            BinOp::Eq => ("cmp", Some("eq")),
            BinOp::Ne => ("cmp", Some("ne")),
            BinOp::Lt => ("cmp", Some(if unsigned { "lo" } else { "lt" })),
            BinOp::Le => ("cmp", Some(if unsigned { "ls" } else { "le" })),
            BinOp::Gt => ("cmp", Some(if unsigned { "hi" } else { "gt" })),
            BinOp::Ge => ("cmp", Some(if unsigned { "hs" } else { "ge" })),
            BinOp::LogicalAnd | BinOp::LogicalOr => {
                return Err("short-circuit logical operators are lowered to branches, not binary operations".into());
            }
        };
        match op {
            BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => {
                let _ = writeln!(self.out, "    {mnemonic} x0, x1");
                let _ = writeln!(self.out, "    cset x0, {}", comparison.unwrap());
            }
            BinOp::Rem => {
                let _ = writeln!(self.out, "    {mnemonic} x2, x0, x1");
                let _ = writeln!(self.out, "    msub x0, x2, x1, x0");
            }
            _ => {
                let _ = writeln!(self.out, "    {mnemonic} x0, x0, x1");
            }
        }
        Ok(())
    }

    fn normalize(&mut self, ty: TyId) -> Result<(), String> {
        match self.types.kind(ty) {
            TyKind::Unit => {
                let _ = writeln!(self.out, "    mov x0, #0");
            }
            TyKind::Bool => {
                let _ = writeln!(self.out, "    and x0, x0, #1");
            }
            TyKind::I8 => {
                let _ = writeln!(self.out, "    sxtb x0, w0");
            }
            TyKind::I16 => {
                let _ = writeln!(self.out, "    sxth x0, w0");
            }
            TyKind::I32 => {
                let _ = writeln!(self.out, "    sxtw x0, w0");
            }
            TyKind::U8 | TyKind::Char => {
                let _ = writeln!(self.out, "    and x0, x0, #255");
            }
            TyKind::U16 => {
                let _ = writeln!(self.out, "    uxth w0, w0");
            }
            TyKind::U32 => {
                let _ = writeln!(self.out, "    mov w0, w0");
            }
            TyKind::I64
            | TyKind::U64
            | TyKind::Isize
            | TyKind::Usize
            | TyKind::Never
            | TyKind::Ref(_)
            | TyKind::RefMut(_)
            | TyKind::RawPtr(_)
            | TyKind::RawPtrMut(_)
            | TyKind::FnPtr { .. }
            | TyKind::IntVar(_) => {}
            _ => {
                return Err(format!(
                    "the AArch64 backend does not support values of type `{}` yet",
                    self.types.display(ty)
                ));
            }
        }
        Ok(())
    }

    fn is_unsigned(&self, ty: TyId) -> bool {
        matches!(
            self.types.kind(ty),
            TyKind::U8 | TyKind::U16 | TyKind::U32 | TyKind::U64 | TyKind::Usize | TyKind::Char
        )
    }

    fn operand_ty(&self, operand: &Operand) -> Option<TyId> {
        match operand {
            Operand::Copy(place) | Operand::Move(place) if place.projection.is_empty() => {
                Some(self.body.local(place.local).ty)
            }
            Operand::Constant(Constant::Int { ty, .. })
            | Operand::Constant(Constant::Uint { ty, .. })
            | Operand::Constant(Constant::Float { ty, .. }) => Some(*ty),
            _ => None,
        }
    }

    fn place_ty(&self, place: &Place) -> TyId {
        self.body.local(place.local).ty
    }

    fn ret_local(&self) -> Option<usize> {
        self.body.locals.iter().position(|l| l.name == "_ret")
    }

    fn require_place(&self, place: &Place) -> Result<(), String> {
        if !place.projection.is_empty() {
            return Err("the AArch64 backend does not support projected places yet".into());
        }
        require_scalar(
            self.types,
            self.place_ty(place),
            "the destination of an assignment",
        )
    }
}

fn require_scalar(types: &TyCtxt, ty: TyId, what: &str) -> Result<(), String> {
    if is_scalar(types.kind(ty)) {
        Ok(())
    } else {
        Err(format!(
            "the AArch64 backend does not support {what} of type `{}` yet",
            types.display(ty)
        ))
    }
}

fn is_scalar(kind: &TyKind) -> bool {
    matches!(
        kind,
        TyKind::Never
            | TyKind::Unit
            | TyKind::Bool
            | TyKind::I8
            | TyKind::I16
            | TyKind::I32
            | TyKind::I64
            | TyKind::Isize
            | TyKind::U8
            | TyKind::U16
            | TyKind::U32
            | TyKind::U64
            | TyKind::Usize
            | TyKind::Char
            | TyKind::Ref(_)
            | TyKind::RefMut(_)
            | TyKind::RawPtr(_)
            | TyKind::RawPtrMut(_)
            | TyKind::FnPtr { .. }
            | TyKind::IntVar(_)
    )
}

fn frame_size(locals: usize) -> Result<u32, String> {
    let bytes = (locals as u64) * 8;
    let frame = (bytes + 15) & !15;
    if frame > 4095 {
        return Err("the AArch64 frame is larger than a single `sub` immediate".into());
    }
    Ok(frame as u32)
}

fn local_offset(index: usize) -> u32 {
    ((index + 1) * 8) as u32
}

fn mangle(name: &str, foreign: &HashSet<String>, target: Target) -> String {
    if foreign.contains(name) {
        name.to_string()
    } else {
        target.mangle(name)
    }
}

fn constant_name(constant: &Constant) -> &'static str {
    match constant {
        Constant::Int { .. } => "integer",
        Constant::Uint { .. } => "unsigned integer",
        Constant::Float { .. } => "float",
        Constant::Bool(_) => "bool",
        Constant::Char(_) => "char",
        Constant::Str(_) => "string",
        Constant::Unit => "unit",
        Constant::Fn(_) => "function pointer",
        Constant::NamedFn(_) => "named function",
        Constant::Const(_) => "constant",
        Constant::UnitVariant { .. } => "enum variant",
    }
}

fn rvalue_name(value: &Rvalue) -> &'static str {
    match value {
        Rvalue::Use(_) => "use",
        Rvalue::Binary { .. } => "binary operation",
        Rvalue::Unary { .. } => "unary operation",
        Rvalue::Ref { .. } => "reference",
        Rvalue::Reborrow { .. } => "reborrow",
        Rvalue::Cast { .. } => "cast",
        Rvalue::Aggregate { .. } => "aggregate",
        Rvalue::Repeat { .. } => "array repeat",
        Rvalue::Discriminant(_) => "discriminant",
        Rvalue::Len(_) => "length",
        Rvalue::PtrToInt(_) => "pointer-to-int",
        Rvalue::PtrOffset { .. } => "pointer offset",
    }
}

fn quote(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            _ => out.push(ch),
        }
    }
    out
}
