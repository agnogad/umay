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
use crate::layout::Layouts;
use crate::mir::{
    AggregateKind, BinOp, Body, Constant, Operand, Place, Projection, Rvalue, Statement,
    Terminator, UnOp,
};
use crate::target::Target;
use crate::types::{DefId, TyCtxt, TyId, TyKind};

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
        match generate_one(body, index, types, hir, &symbols, &foreign, target) {
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
    hir: &Hir,
    symbols: &HashMap<FnId, String>,
    foreign: &HashSet<String>,
    target: Target,
) -> Result<String, String> {
    for local in &body.locals {
        require_value(types, local.ty, &format!("local `{}`", local.name))?;
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
    let (offsets, frame) = frame_layout(body, types, hir)?;

    let mut emitter = Emitter {
        out: String::new(),
        body,
        types,
        symbols,
        foreign,
        target,
        tag,
        frame,
        offsets,
        next_label: 0,
        hir,
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
        let offset = emitter.offsets[arg as usize];
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
    offsets: Vec<u32>,
    next_label: usize,
    hir: &'a Hir,
}

impl Emitter<'_> {
    fn statement(&mut self, statement: &Statement) -> Result<(), String> {
        match statement {
            Statement::Assign { place, value, .. } => {
                let place_ty = self.place_ty(place)?;
                match value {
                    Rvalue::Use(operand) => {
                        if is_aggregate(self.types.kind(place_ty)) {
                            let source = match operand {
                                Operand::Copy(place) | Operand::Move(place) => place,
                                _ => {
                                    return Err("the AArch64 backend cannot initialise an aggregate from this value".into());
                                }
                            };
                            let size = self.size_of(place_ty)?;
                            self.copy_aggregate(place, source, size)?;
                            return Ok(());
                        }
                        self.operand(operand, 0)?;
                        self.normalize(place_ty)?;
                    }
                    Rvalue::Binary { op, lhs, rhs } => {
                        self.operand(lhs, 0)?;
                        self.operand(rhs, 1)?;
                        let lhs_ty = self.operand_ty(lhs);
                        self.binary(*op, lhs_ty, place_ty)?;
                        self.normalize(place_ty)?;
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
                        self.normalize(place_ty)?;
                    }
                    Rvalue::Cast { operand, to: _ } => {
                        self.operand(operand, 0)?;
                        self.normalize(place_ty)?;
                    }
                    Rvalue::Ref { place: borrowed, .. } | Rvalue::Reborrow { place: borrowed, .. } => {
                        self.address_of(borrowed)?;
                        let _ = writeln!(self.out, "    mov x0, x11");
                        self.normalize(place_ty)?;
                    }
                    Rvalue::PtrToInt(_) => {
                        if let Rvalue::PtrToInt(operand) = value {
                            self.operand(operand, 0)?;
                        }
                        self.normalize(place_ty)?;
                    }
                    Rvalue::PtrOffset { ptr, count } => {
                        self.operand(ptr, 0)?;
                        self.operand(count, 1)?;
                        let pointee = self.pointee(place_ty)?;
                        let size = self.size_of(pointee)?;
                        if size != 1 {
                            let _ = writeln!(self.out, "    mov x2, #{size}");
                            let _ = writeln!(self.out, "    mul x1, x1, x2");
                        }
                        let _ = writeln!(self.out, "    add x0, x0, x1");
                        self.normalize(place_ty)?;
                    }
                    Rvalue::Len(len_place) => {
                        let ty = self.place_ty(len_place)?;
                        let length = match self.types.kind(ty) {
                            TyKind::Array { len, .. } => *len,
                            _ => return Err("the AArch64 backend supports `len` only on fixed arrays yet".into()),
                        };
                        let _ = writeln!(self.out, "    mov x0, #{length}");
                        self.normalize(place_ty)?;
                    }
                    Rvalue::Repeat { operand, count } => {
                        self.repeat(place, place_ty, operand, *count)?;
                    }
                    Rvalue::Aggregate { kind: AggregateKind::Array, operands } => {
                        self.array_literal(place, place_ty, operands)?;
                    }
                    Rvalue::Aggregate { kind: AggregateKind::Struct(def), operands } => {
                        self.struct_literal(place, place_ty, *def, operands)?;
                    }
                    other => {
                        return Err(format!(
                            "the AArch64 backend does not support `{}` assignments yet",
                            rvalue_name(other)
                        ));
                    }
                }
                if is_scalar(self.types.kind(place_ty)) {
                    self.store(place, 0)?;
                }
                Ok(())
            }
            Statement::StorageLive(_)
            | Statement::StorageDead(_)
            | Statement::SetDropFlag { .. }
            | Statement::Nop => Ok(()),
        }
    }

    fn repeat(&mut self, place: &Place, ty: TyId, operand: &Operand, count: u64) -> Result<(), String> {
        let element = self.element(ty)?;
        let size = self.size_of(element)?;
        let label = self.label();
        self.operand(operand, 0)?;
        self.address_of(place)?;
        let _ = writeln!(self.out, "    mov x10, #0");
        let _ = writeln!(self.out, ".L{label}:");
        let _ = writeln!(self.out, "    cmp x10, #{count}");
        let _ = writeln!(self.out, "    b.hs .L{label}_done");
        if size == 1 {
            let _ = writeln!(self.out, "    strb w0, [x11, x10]");
        } else {
            let _ = writeln!(self.out, "    mov x12, #{size}");
            let _ = writeln!(self.out, "    mul x12, x10, x12");
            let _ = writeln!(self.out, "    add x12, x11, x12");
            let _ = writeln!(self.out, "    str x0, [x12]");
        }
        let _ = writeln!(self.out, "    add x10, x10, #1");
        let _ = writeln!(self.out, "    b .L{label}");
        let _ = writeln!(self.out, ".L{label}_done:");
        Ok(())
    }

    fn array_literal(&mut self, place: &Place, ty: TyId, operands: &[Operand]) -> Result<(), String> {
        let element = self.element(ty)?;
        let size = self.size_of(element)?;
        self.address_of(place)?;
        // Each operand load may address a local and overwrite `x11`.
        let _ = writeln!(self.out, "    mov x13, x11");
        for (index, operand) in operands.iter().enumerate() {
            self.operand(operand, 0)?;
            let at = index as u64 * size;
            if size == 1 {
                let _ = writeln!(self.out, "    strb w0, [x13, #{at}]");
            } else {
                let _ = writeln!(self.out, "    mov x12, #{at}");
                let _ = writeln!(self.out, "    add x12, x13, x12");
                let _ = writeln!(self.out, "    str x0, [x12]");
            }
        }
        Ok(())
    }

    fn struct_literal(
        &mut self,
        place: &Place,
        ty: TyId,
        def: DefId,
        operands: &[Operand],
    ) -> Result<(), String> {
        let (_, args) = self.adt_parts(ty)?;
        let offsets = self.field_offsets(def, &args)?;
        let fields = self.field_types(def)?;
        if operands.len() != fields.len() {
            return Err("a struct literal and its definition disagree on field count".into());
        }
        self.address_of(place)?;
        let _ = writeln!(self.out, "    mov x13, x11");
        for ((operand, offset), field_ty) in operands.iter().zip(offsets).zip(fields) {
            self.operand(operand, 0)?;
            // Loading the operand may have used `x11` for a local's address.
            let _ = writeln!(self.out, "    mov x11, x13");
            self.add_to_address(offset)?;
            self.store_at(field_ty, 0)?;
        }
        Ok(())
    }

    fn copy_aggregate(&mut self, destination: &Place, source: &Place, size: u64) -> Result<(), String> {
        self.address_of(source)?;
        let _ = writeln!(self.out, "    mov x13, x11");
        self.address_of(destination)?;
        let label = self.label();
        let _ = writeln!(self.out, "    mov x10, #0");
        let _ = writeln!(self.out, ".L{label}:");
        let _ = writeln!(self.out, "    cmp x10, #{size}");
        let _ = writeln!(self.out, "    b.hs .L{label}_done");
        let _ = writeln!(self.out, "    ldrb w0, [x13, x10]");
        let _ = writeln!(self.out, "    strb w0, [x11, x10]");
        let _ = writeln!(self.out, "    add x10, x10, #1");
        let _ = writeln!(self.out, "    b .L{label}");
        let _ = writeln!(self.out, ".L{label}_done:");
        Ok(())
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
                if symbol.is_none() {
                    self.operand(callee, 9)?;
                }
                for (index, arg) in args.iter().enumerate() {
                    self.operand(arg, index as u8)?;
                }
                match symbol {
                    Some(symbol) => {
                        let _ = writeln!(self.out, "    bl {symbol}");
                    }
                    None => {
                        let _ = writeln!(self.out, "    blr x9");
                    }
                }
                if let Some(destination) = destination {
                    let ty = self.place_ty(destination)?;
                    if is_array(self.types.kind(ty)) {
                        return Err("the AArch64 backend does not support returning an array yet".into());
                    }
                    self.normalize(ty)?;
                    self.store(destination, 0)?;
                }
                let _ = writeln!(self.out, "    b .L{}_bb{}", self.tag, target.0);
                Ok(())
            }
            Terminator::Drop { target, .. } => {
                // Scalar and array-of-scalar locals have no destructor.
                let _ = writeln!(self.out, "    b .L{}_bb{}", self.tag, target.0);
                Ok(())
            }
            Terminator::Return => {
                if let Some(local) = self.ret_local() {
                    let place = Place::local(local);
                    let ty = self.place_ty(&place)?;
                    if is_array(self.types.kind(ty)) {
                        return Err("the AArch64 backend does not support returning an array yet".into());
                    }
                    self.load(&place, 0)?;
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

    fn callee(&self, operand: &Operand) -> Result<Option<String>, String> {
        match operand {
            Operand::Constant(Constant::Fn(id)) => self
                .symbols
                .get(id)
                .map(|name| Some(mangle(name, self.foreign, self.target)))
                .ok_or_else(|| "a call names a function with no symbol".to_string()),
            Operand::Constant(Constant::NamedFn(name)) => Ok(Some(name.clone())),
            _ => Ok(None),
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
            Constant::Str(id) => {
                let _ = writeln!(self.out, "    adrp x{reg}, .Lstr_{}", id.0);
                let _ = writeln!(self.out, "    add x{reg}, x{reg}, :lo12:.Lstr_{}", id.0);
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

    /// Computes the address of `place` into `x11`.
    fn address_of(&mut self, place: &Place) -> Result<(), String> {
        let offset = self.offsets[place.local.0 as usize];
        let mut ty = self.body.local(place.local).ty;
        let _ = writeln!(self.out, "    sub x11, x29, #{offset}");
        for projection in &place.projection {
            match projection {
                Projection::Deref => {
                    ty = self.pointee(ty)?;
                    let _ = writeln!(self.out, "    ldr x11, [x11]");
                }
                Projection::Index(index) => {
                    let element = self.element(ty)?;
                    let size = self.size_of(element)?;
                    // Loading the index may address a local and therefore write
                    // `x11`. Keep the array's base in `x13` across that load.
                    let _ = writeln!(self.out, "    mov x13, x11");
                    self.operand(index, 10)?;
                    let _ = writeln!(self.out, "    mov x11, x13");
                    if size == 1 {
                        let _ = writeln!(self.out, "    add x11, x11, x10");
                    } else if size.is_power_of_two() {
                        let _ = writeln!(self.out, "    add x11, x11, x10, lsl #{}", size.trailing_zeros());
                    } else {
                        let _ = writeln!(self.out, "    mov x12, #{size}");
                        let _ = writeln!(self.out, "    mul x10, x10, x12");
                        let _ = writeln!(self.out, "    add x11, x11, x10");
                    }
                    ty = element;
                }
                Projection::Field(index) => {
                    let (def, args) = self.adt_parts(ty)?;
                    let offsets = self.field_offsets(def, &args)?;
                    let offset = *offsets
                        .get(*index as usize)
                        .ok_or_else(|| "a struct field index is out of range".to_string())?;
                    self.add_to_address(offset)?;
                    ty = self.field_type(def, *index)?;
                }
                Projection::EnumField { .. } => {
                    return Err("the AArch64 backend does not support enum fields yet".into());
                }
            }
        }
        Ok(())
    }

    fn load(&mut self, place: &Place, reg: u8) -> Result<(), String> {
        let ty = self.place_ty(place)?;
        self.address_of(place)?;
        self.load_at(ty, reg)
    }

    fn load_at(&mut self, ty: TyId, reg: u8) -> Result<(), String> {
        match self.types.kind(ty) {
            TyKind::I8 | TyKind::U8 | TyKind::Bool => {
                let _ = writeln!(self.out, "    ldrb w{reg}, [x11]");
            }
            TyKind::I16 | TyKind::U16 => {
                let _ = writeln!(self.out, "    ldrh w{reg}, [x11]");
            }
            TyKind::I32 | TyKind::U32 | TyKind::Char => {
                let _ = writeln!(self.out, "    ldr w{reg}, [x11]");
            }
            _ => {
                let _ = writeln!(self.out, "    ldr x{reg}, [x11]");
            }
        }
        Ok(())
    }

    fn store(&mut self, place: &Place, reg: u8) -> Result<(), String> {
        let ty = self.place_ty(place)?;
        self.address_of(place)?;
        self.store_at(ty, reg)
    }

    fn store_at(&mut self, ty: TyId, reg: u8) -> Result<(), String> {
        match self.types.kind(ty) {
            TyKind::I8 | TyKind::U8 | TyKind::Bool => {
                let _ = writeln!(self.out, "    strb w{reg}, [x11]");
            }
            TyKind::I16 | TyKind::U16 => {
                let _ = writeln!(self.out, "    strh w{reg}, [x11]");
            }
            TyKind::I32 | TyKind::U32 | TyKind::Char => {
                let _ = writeln!(self.out, "    str w{reg}, [x11]");
            }
            _ => {
                let _ = writeln!(self.out, "    str x{reg}, [x11]");
            }
        }
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
            | TyKind::IntVar(_)
            | TyKind::Array { .. }
            | TyKind::Adt { .. } => {}
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
            Operand::Copy(place) | Operand::Move(place) => self.place_ty(place).ok(),
            Operand::Constant(Constant::Int { ty, .. })
            | Operand::Constant(Constant::Uint { ty, .. })
            | Operand::Constant(Constant::Float { ty, .. }) => Some(*ty),
            _ => None,
        }
    }

    fn place_ty(&self, place: &Place) -> Result<TyId, String> {
        let mut ty = self.body.local(place.local).ty;
        for projection in &place.projection {
            ty = match projection {
                Projection::Deref => self.pointee(ty)?,
                Projection::Index(_) => self.element(ty)?,
                Projection::Field(index) => {
                    let (def, _) = self.adt_parts(ty)?;
                    self.field_type(def, *index)?
                }
                Projection::EnumField { .. } => {
                    return Err("the AArch64 backend does not support enum fields yet".into());
                }
            };
        }
        Ok(ty)
    }

    fn pointee(&self, ty: TyId) -> Result<TyId, String> {
        match self.types.kind(ty) {
            TyKind::Ref(inner)
            | TyKind::RefMut(inner)
            | TyKind::RawPtr(inner)
            | TyKind::RawPtrMut(inner) => Ok(*inner),
            _ => Err(format!(
                "the AArch64 backend cannot dereference `{}`",
                self.types.display(ty)
            )),
        }
    }

    fn element(&self, ty: TyId) -> Result<TyId, String> {
        match self.types.kind(ty) {
            TyKind::Array { elem, .. } | TyKind::Slice(elem) => Ok(*elem),
            _ => Err(format!(
                "the AArch64 backend cannot index `{}`",
                self.types.display(ty)
            )),
        }
    }

    fn size_of(&mut self, ty: TyId) -> Result<u64, String> {
        let layout = {
            let mut layouts = Layouts::new(self.types, self.hir);
            layouts.layout(ty)
        };
        layout
            .map(|layout| layout.size)
            .map_err(|e| format!("the AArch64 backend cannot lay out `{}`: {e:?}", self.types.display(ty)))
    }

    fn adt_parts(&self, ty: TyId) -> Result<(DefId, Vec<TyId>), String> {
        match self.types.kind(ty) {
            TyKind::Adt { def, args } => Ok((*def, args.clone())),
            _ => Err(format!(
                "the AArch64 backend cannot take a field of `{}`",
                self.types.display(ty)
            )),
        }
    }

    fn field_offsets(&mut self, def: DefId, args: &[TyId]) -> Result<Vec<u64>, String> {
        let mut layouts = Layouts::new(self.types, self.hir);
        layouts
            .field_offsets(def, args)
            .map_err(|e| format!("the AArch64 backend cannot lay out fields: {e:?}"))
    }

    fn field_types(&self, def: DefId) -> Result<Vec<TyId>, String> {
        let info = self
            .hir
            .structs
            .iter()
            .find(|s| s.def_id == def)
            .ok_or_else(|| "a struct has no definition in the HIR".to_string())?;
        let types: Vec<TyId> = info.fields.iter().map(|f| f.ty).collect();
        for ty in &types {
            if matches!(self.types.kind(*ty), TyKind::Param { .. }) {
                return Err("the AArch64 backend does not substitute generic struct fields yet".into());
            }
        }
        Ok(types)
    }

    fn field_type(&self, def: DefId, index: u32) -> Result<TyId, String> {
        self.field_types(def)?
            .get(index as usize)
            .copied()
            .ok_or_else(|| "a struct field index is out of range".to_string())
    }

    /// Adds a compile-time offset to the address in `x11`.
    fn add_to_address(&mut self, offset: u64) -> Result<(), String> {
        if offset <= 4095 {
            let _ = writeln!(self.out, "    add x11, x11, #{offset}");
        } else {
            let _ = writeln!(self.out, "    mov x12, #{offset}");
            let _ = writeln!(self.out, "    add x11, x11, x12");
        }
        Ok(())
    }

    fn ret_local(&self) -> Option<crate::hir::LocalId> {
        self.body
            .locals
            .iter()
            .position(|l| l.name == "_ret")
            .map(|i| crate::hir::LocalId(i as u32))
    }

    fn label(&mut self) -> usize {
        let label = self.next_label;
        self.next_label += 1;
        label
    }

}

fn require_value(types: &TyCtxt, ty: TyId, what: &str) -> Result<(), String> {
    if is_value(types.kind(ty)) {
        Ok(())
    } else {
        Err(format!(
            "the AArch64 backend does not support {what} of type `{}` yet",
            types.display(ty)
        ))
    }
}

fn is_value(kind: &TyKind) -> bool {
    is_scalar(kind) || matches!(kind, TyKind::Array { .. } | TyKind::Adt { .. })
}

fn is_aggregate(kind: &TyKind) -> bool {
    matches!(kind, TyKind::Array { .. } | TyKind::Adt { .. })
}

fn is_array(kind: &TyKind) -> bool {
    matches!(kind, TyKind::Array { .. })
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

fn frame_layout(body: &Body, types: &mut TyCtxt, hir: &Hir) -> Result<(Vec<u32>, u32), String> {
    let mut layouts = Layouts::new(types, hir);
    let mut cursor = 0u64;
    let mut offsets = Vec::with_capacity(body.locals.len());
    for local in &body.locals {
        let layout = layouts
            .layout(local.ty)
            .map_err(|e| format!("layout failed for `{}`: {e:?}", local.name))?;
        let size = layout.size.max(8);
        let align = if layout.size == 0 { 8 } else { layout.align().max(1) };
        cursor = (cursor + align - 1) & !(align - 1);
        cursor += size;
        offsets.push(cursor as u32);
    }
    let frame = (cursor + 15) & !15;
    if frame > 4095 {
        return Err("the AArch64 frame is larger than a single `sub` immediate".into());
    }
    Ok((offsets, frame as u32))
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
