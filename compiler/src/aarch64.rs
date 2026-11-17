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

    let tag = body
        .fn_id
        .map(|id| id.0 as usize)
        .unwrap_or(10_000 + body_index);
    let symbol = body
        .fn_id
        .and_then(|id| symbols.get(&id))
        .map(|name| mangle(name, foreign, target))
        .unwrap_or_else(|| body.name.clone());
    let ret_size = {
        let mut layouts = Layouts::new(types, hir);
        layouts
            .layout(body.ret_ty)
            .map(|layout| layout.size)
            .map_err(|e| format!("layout failed for the return type: {e:?}"))?
    };
    let needs_sret = is_aggregate(types.kind(body.ret_ty)) && ret_size > 16;
    let (local_reg, local_freg) = allocate_locals(body, types);
    let (offsets, frame, sret_offset) =
        frame_layout(body, types, hir, needs_sret, &local_reg, &local_freg)?;
    let mut saved_regs: Vec<u8> = local_reg.iter().flatten().copied().collect();
    saved_regs.sort_unstable();
    saved_regs.dedup();
    let mut saved_fregs: Vec<u8> = local_freg.iter().flatten().copied().collect();
    saved_fregs.sort_unstable();
    saved_fregs.dedup();

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
        sret_offset,
        saved_regs,
        saved_fregs,
        local_reg,
        local_freg,
    };

    let _ = writeln!(emitter.out, ".p2align 2");
    let _ = writeln!(emitter.out, ".globl {symbol}");
    if target.format == crate::target::ObjectFormat::Elf {
        let _ = writeln!(emitter.out, ".type {symbol}, %function");
    }
    let _ = writeln!(emitter.out, "{symbol}:");
    let _ = writeln!(emitter.out, "    stp x29, x30, [sp, #-16]!");
    let _ = writeln!(emitter.out, "    mov x29, sp");
    if frame > 0 {
        if frame <= 4095 {
            let _ = writeln!(emitter.out, "    sub sp, sp, #{frame}");
        } else {
            let _ = writeln!(emitter.out, "    mov x12, #{frame}");
            let _ = writeln!(emitter.out, "    sub sp, sp, x12");
        }
    }
    let saved = emitter.saved_regs.clone();
    for chunk in saved.chunks(2) {
        match chunk {
            [a, b] => {
                let _ = writeln!(emitter.out, "    stp x{a}, x{b}, [sp, #-16]!");
            }
            [a] => {
                let _ = writeln!(emitter.out, "    stp x{a}, xzr, [sp, #-16]!");
            }
            _ => {}
        }
    }
    for chunk in emitter.saved_fregs.clone().chunks(2) {
        match chunk {
            [a, b] => {
                let _ = writeln!(emitter.out, "    stp d{a}, d{b}, [sp, #-16]!");
            }
            [a] => {
                let _ = writeln!(emitter.out, "    str d{a}, [sp, #-16]!");
            }
            _ => {}
        }
    }
    if emitter.sret_offset.is_some() {
        emitter.sret_address(9)?;
        let _ = writeln!(emitter.out, "    str x8, [x9]");
    }
    emitter.store_incoming_args()?;

    for (block_index, block) in body.blocks.iter().enumerate() {
        let _ = writeln!(emitter.out, ".L{tag}_bb{block_index}:");
        for statement in &block.statements {
            emitter.statement(statement)?;
        }
        emitter.terminator(&block.terminator)?;
    }

    if target.format == crate::target::ObjectFormat::Elf {
        let _ = writeln!(emitter.out, ".size {symbol}, .-{symbol}");
    }
    Ok(emitter.out)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ArgKind {
    Scalar,
    /// A scalar float, passed in an FP/SIMD register.
    Float,
    /// A homogeneous floating-point aggregate: 1-4 floats of one width.
    Hfa,
    /// A composite of at most 16 bytes, passed in one or two general registers.
    Small,
    /// A composite larger than 16 bytes, passed by reference.
    Large,
}

#[derive(Debug, Clone)]
struct Hfa {
    offsets: Vec<u64>,
    float64: bool,
}

struct ArgLoc {
    regs: Vec<u8>,
    freg: Option<u8>,
    fregs: Vec<u8>,
    hfa: Option<Hfa>,
    stack: Option<u64>,
    kind: ArgKind,
    size: u64,
    float64: bool,
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
    sret_offset: Option<u32>,
    local_reg: Vec<Option<u8>>,
    local_freg: Vec<Option<u8>>,
    saved_regs: Vec<u8>,
    saved_fregs: Vec<u8>,
}

impl Emitter<'_> {
    fn statement(&mut self, statement: &Statement) -> Result<(), String> {
        match statement {
            Statement::Assign { place, value, .. } => {
                let place_ty = self.place_ty(place)?;
                if place.projection.is_empty()
                    && is_scalar(self.types.kind(place_ty))
                    && self.direct_assign(place, place_ty, value)?
                {
                    return Ok(());
                }
                let mut float_result = false;
                match value {
                    Rvalue::Use(operand) => {
                        if is_aggregate(self.types.kind(place_ty)) {
                            if let Operand::Constant(Constant::UnitVariant { def, index }) = operand
                            {
                                self.unit_variant(place, *def, *index)?;
                                return Ok(());
                            }
                            let source = match operand {
                                Operand::Copy(place) | Operand::Move(place) => place,
                                // An unreachable block still assigns `()` to the
                                // return slot. There is nothing to initialise.
                                Operand::Constant(Constant::Unit) => return Ok(()),
                                _ => {
                                    return Err("the AArch64 backend cannot initialise an aggregate from this value".into());
                                }
                            };
                            let size = self.size_of(place_ty)?;
                            self.copy_aggregate(place, source, size)?;
                            return Ok(());
                        }
                        if self.is_float(place_ty) {
                            if matches!(operand, Operand::Constant(Constant::Unit)) {
                                return Ok(());
                            }
                            self.operand_float(
                                operand,
                                0,
                                matches!(self.types.kind(place_ty), TyKind::F64),
                            )?;
                            float_result = true;
                        } else {
                            self.operand(operand, 0)?;
                            self.normalize(place_ty)?;
                        }
                    }
                    Rvalue::Binary { op, lhs, rhs } => {
                        let lhs_ty = self.operand_ty(lhs);
                        let float_operands = lhs_ty
                            .map(|ty| self.is_float(ty))
                            .unwrap_or_else(|| self.is_float(place_ty));
                        if float_operands {
                            self.float_binary(*op, lhs, rhs, lhs_ty.unwrap_or(place_ty))?;
                            float_result = !matches!(
                                op,
                                BinOp::Eq
                                    | BinOp::Ne
                                    | BinOp::Lt
                                    | BinOp::Le
                                    | BinOp::Gt
                                    | BinOp::Ge
                            );
                        } else {
                            self.operand(lhs, 0)?;
                            self.operand(rhs, 1)?;
                            self.binary(*op, lhs_ty, place_ty)?;
                            self.normalize(place_ty)?;
                        }
                    }
                    Rvalue::Unary { op, operand } => {
                        let float_operand = self
                            .operand_ty(operand)
                            .map(|ty| self.is_float(ty))
                            .unwrap_or(false);
                        match op {
                            UnOp::Neg if float_operand => {
                                let float64 = self
                                    .operand_ty(operand)
                                    .map(|ty| matches!(self.types.kind(ty), TyKind::F64))
                                    .unwrap_or(false);
                                self.operand_float(operand, 0, float64)?;
                                let prefix = if float64 { "d" } else { "s" };
                                let _ = writeln!(self.out, "    fneg {prefix}0, {prefix}0");
                                float_result = true;
                            }
                            UnOp::Neg => {
                                self.operand(operand, 0)?;
                                let _ = writeln!(self.out, "    neg x0, x0");
                            }
                            UnOp::Not => {
                                self.operand(operand, 0)?;
                                if matches!(
                                    self.operand_ty(operand).map(|t| self.types.kind(t)),
                                    Some(TyKind::Bool)
                                ) {
                                    let _ = writeln!(self.out, "    eor x0, x0, #1");
                                } else {
                                    let _ = writeln!(self.out, "    mvn x0, x0");
                                }
                            }
                        }
                        self.normalize(place_ty)?;
                    }
                    Rvalue::Cast { operand, to: _ } => {
                        let source_float = self
                            .operand_ty(operand)
                            .map(|ty| self.is_float(ty))
                            .unwrap_or(false);
                        let target_float = self.is_float(place_ty);
                        if source_float || target_float {
                            self.cast_float(operand, place_ty)?;
                            float_result = target_float;
                        } else {
                            self.operand(operand, 0)?;
                        }
                        self.normalize(place_ty)?;
                    }
                    Rvalue::Ref {
                        place: borrowed, ..
                    }
                    | Rvalue::Reborrow {
                        place: borrowed, ..
                    } => {
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
                            self.emit_mov_u64(2, size)?;
                            let _ = writeln!(self.out, "    mul x1, x1, x2");
                        }
                        let _ = writeln!(self.out, "    add x0, x0, x1");
                        self.normalize(place_ty)?;
                    }
                    Rvalue::Len(len_place) => {
                        let ty = self.place_ty(len_place)?;
                        let length =
                            match self.types.kind(ty) {
                                TyKind::Array { len, .. } => *len,
                                _ => return Err(
                                    "the AArch64 backend supports `len` only on fixed arrays yet"
                                        .into(),
                                ),
                            };
                        let _ = writeln!(self.out, "    mov x0, #{length}");
                        self.normalize(place_ty)?;
                    }
                    Rvalue::Repeat { operand, count } => {
                        self.repeat(place, place_ty, operand, *count)?;
                    }
                    Rvalue::Aggregate {
                        kind: AggregateKind::Array,
                        operands,
                    } => {
                        self.array_literal(place, place_ty, operands)?;
                    }
                    Rvalue::Aggregate {
                        kind: AggregateKind::Struct(def),
                        operands,
                    } => {
                        self.struct_literal(place, place_ty, *def, operands)?;
                    }
                    Rvalue::Aggregate {
                        kind: AggregateKind::Variant { def, index },
                        operands,
                    } => {
                        self.variant_literal(place, place_ty, *def, *index, operands)?;
                    }
                    Rvalue::Aggregate {
                        kind: AggregateKind::Tuple,
                        operands,
                    } => {
                        self.tuple_literal(place, place_ty, operands)?;
                    }
                    Rvalue::Discriminant(discriminant_place) => {
                        let enum_ty = self.place_ty(discriminant_place)?;
                        let (def, _) = self.enum_parts(enum_ty)?;
                        let size = self.tag_size(def)?;
                        self.address_of(discriminant_place)?;
                        if size == 1 {
                            let _ = writeln!(self.out, "    ldrb w0, [x11]");
                        } else {
                            let _ = writeln!(self.out, "    ldr w0, [x11]");
                        }
                        self.normalize(place_ty)?;
                    }
                }
                if is_scalar(self.types.kind(place_ty)) {
                    if float_result {
                        let float64 = matches!(self.types.kind(place_ty), TyKind::F64);
                        self.store_float_result(place, float64)?;
                    } else {
                        self.store(place, 0)?;
                    }
                }
                Ok(())
            }
            Statement::StorageLive(_)
            | Statement::StorageDead(_)
            | Statement::SetDropFlag { .. }
            | Statement::Nop => Ok(()),
        }
    }

    fn promoted_int(&self, operand: &Operand) -> Option<u8> {
        match operand {
            Operand::Copy(place) | Operand::Move(place) if place.projection.is_empty() => {
                self.local_reg[place.local.0 as usize]
            }
            _ => None,
        }
    }

    fn promoted_float(&self, operand: &Operand) -> Option<u8> {
        match operand {
            Operand::Copy(place) | Operand::Move(place) if place.projection.is_empty() => {
                self.local_freg[place.local.0 as usize]
            }
            _ => None,
        }
    }

    /// Lowers a scalar assignment directly into its allocated register.
    ///
    /// The stack-machine path computes every value in `x0`/`x1` or `d0`/`d1`
    /// and then moves it into the local's register. When the destination and
    /// both operands already have registers, that round trip is unnecessary:
    /// `add x21, x19, x20` replaces two moves, the add and a third move.
    /// Returning `false` means nothing was emitted and the caller must use the
    /// general path.
    /// Puts an integer operand in a register, using `scratch` when it is not
    /// already allocated. `x8`/`x9` are caller-saved and not used by the
    /// address calculation a stack operand may need.
    fn int_operand(&mut self, operand: &Operand, scratch: u8) -> Result<u8, String> {
        if let Some(register) = self.promoted_int(operand) {
            return Ok(register);
        }
        self.operand(operand, scratch)?;
        Ok(scratch)
    }

    /// Puts both float operands in registers, using `d0`/`d1` for the ones
    /// that do not already have an allocated register.
    fn float_operands(
        &mut self,
        lhs: &Operand,
        rhs: &Operand,
        float64: bool,
    ) -> Result<(u8, u8), String> {
        let lhs_reg = match self.promoted_float(lhs) {
            Some(register) => register,
            None => {
                self.operand_float(lhs, 0, float64)?;
                0
            }
        };
        let rhs_reg = match self.promoted_float(rhs) {
            Some(register) => register,
            None => {
                self.operand_float(rhs, 1, float64)?;
                1
            }
        };
        Ok((lhs_reg, rhs_reg))
    }

    fn direct_assign(
        &mut self,
        place: &Place,
        place_ty: TyId,
        value: &Rvalue,
    ) -> Result<bool, String> {
        let local = place.local.0 as usize;
        let dest_int = self.local_reg[local];
        let dest_float = self.local_freg[local];
        match value {
            Rvalue::Use(operand) => {
                if let Some(dest) = dest_int {
                    self.operand(operand, dest)?;
                    self.normalize_reg(place_ty, dest)?;
                    return Ok(true);
                }
                if let Some(dest) = dest_float {
                    if matches!(operand, Operand::Constant(Constant::Unit)) {
                        return Ok(true);
                    }
                    let float64 = matches!(self.types.kind(place_ty), TyKind::F64);
                    self.operand_float(operand, dest, float64)?;
                    return Ok(true);
                }
                Ok(false)
            }
            Rvalue::Binary { op, lhs, rhs } => {
                let lhs_ty = self.operand_ty(lhs);
                let float_comparison = lhs_ty
                    .map(|ty| self.is_float(ty))
                    .unwrap_or(false)
                    && matches!(
                        op,
                        BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge
                    );
                if float_comparison {
                    let Some(dest) = dest_int else { return Ok(false) };
                    let float64 = lhs_ty
                        .map(|ty| matches!(self.types.kind(ty), TyKind::F64))
                        .unwrap_or(false);
                    let (lhs_reg, rhs_reg) = self.float_operands(lhs, rhs, float64)?;
                    let prefix = if float64 { "d" } else { "s" };
                    let condition = match op {
                        BinOp::Eq => "eq",
                        BinOp::Ne => "ne",
                        BinOp::Lt => "mi",
                        BinOp::Le => "ls",
                        BinOp::Gt => "gt",
                        _ => "ge",
                    };
                    let _ = writeln!(self.out, "    fcmp {prefix}{lhs_reg}, {prefix}{rhs_reg}");
                    let _ = writeln!(self.out, "    cset x{dest}, {condition}");
                    self.normalize_reg(place_ty, dest)?;
                    return Ok(true);
                }
                if self.is_float(place_ty) {
                    let Some(dest) = dest_float else { return Ok(false) };
                    let float64 = self
                        .operand_ty(lhs)
                        .map(|ty| matches!(self.types.kind(ty), TyKind::F64))
                        .unwrap_or(false);
                    let (lhs_reg, rhs_reg) = self.float_operands(lhs, rhs, float64)?;
                    let prefix = if float64 { "d" } else { "s" };
                    match op {
                        BinOp::Add => {
                            let _ = writeln!(
                                self.out,
                                "    fadd {prefix}{dest}, {prefix}{lhs_reg}, {prefix}{rhs_reg}"
                            );
                        }
                        BinOp::Sub => {
                            let _ = writeln!(
                                self.out,
                                "    fsub {prefix}{dest}, {prefix}{lhs_reg}, {prefix}{rhs_reg}"
                            );
                        }
                        BinOp::Mul => {
                            let _ = writeln!(
                                self.out,
                                "    fmul {prefix}{dest}, {prefix}{lhs_reg}, {prefix}{rhs_reg}"
                            );
                        }
                        BinOp::Div => {
                            let _ = writeln!(
                                self.out,
                                "    fdiv {prefix}{dest}, {prefix}{lhs_reg}, {prefix}{rhs_reg}"
                            );
                        }
                        _ => return Ok(false),
                    }
                    Ok(true)
                } else {
                    let Some(dest) = dest_int else { return Ok(false) };
                    let lhs_reg = self.int_operand(lhs, 8)?;
                    let rhs_reg = self.int_operand(rhs, 9)?;
                    self.direct_int_binary(*op, lhs_reg, rhs_reg, lhs, place_ty, dest)?;
                    Ok(true)
                }
            }
            Rvalue::Unary { op, operand } => {
                if self.is_float(place_ty) {
                    let Some(dest) = dest_float else { return Ok(false) };
                    if !matches!(op, UnOp::Neg) {
                        return Ok(false);
                    }
                    let float64 = matches!(self.types.kind(place_ty), TyKind::F64);
                    let prefix = if float64 { "d" } else { "s" };
                    self.operand_float(operand, dest, float64)?;
                    let _ = writeln!(self.out, "    fneg {prefix}{dest}, {prefix}{dest}");
                    Ok(true)
                } else {
                    let Some(dest) = dest_int else { return Ok(false) };
                    self.operand(operand, dest)?;
                    match op {
                        UnOp::Neg => {
                            let _ = writeln!(self.out, "    neg x{dest}, x{dest}");
                        }
                        UnOp::Not => {
                            let bool_operand = self
                                .operand_ty(operand)
                                .map(|ty| matches!(self.types.kind(ty), TyKind::Bool))
                                .unwrap_or(false);
                            if bool_operand {
                                let _ = writeln!(self.out, "    eor x{dest}, x{dest}, #1");
                            } else {
                                let _ = writeln!(self.out, "    mvn x{dest}, x{dest}");
                            }
                        }
                    }
                    self.normalize_reg(place_ty, dest)?;
                    Ok(true)
                }
            }
            Rvalue::Cast { operand, to: _ } => {
                let Some(source_ty) = self.operand_ty(operand) else {
                    return Ok(false);
                };
                let source_float = self.is_float(source_ty);
                let target_float = self.is_float(place_ty);
                if source_float {
                    let source64 = matches!(self.types.kind(source_ty), TyKind::F64);
                    let source = match self.promoted_float(operand) {
                        Some(register) => register,
                        None => {
                            self.operand_float(operand, 0, source64)?;
                            0
                        }
                    };
                    if target_float {
                        let Some(dest) = dest_float else { return Ok(false) };
                        let target64 = matches!(self.types.kind(place_ty), TyKind::F64);
                        if source64 == target64 {
                            let prefix = if target64 { "d" } else { "s" };
                            if dest != source {
                                let _ = writeln!(
                                    self.out,
                                    "    fmov {prefix}{dest}, {prefix}{source}"
                                );
                            }
                        } else if target64 {
                            let _ = writeln!(self.out, "    fcvt d{dest}, s{source}");
                        } else {
                            let _ = writeln!(self.out, "    fcvt s{dest}, d{source}");
                        }
                    } else {
                        let Some(dest) = dest_int else { return Ok(false) };
                        let unsigned = self.is_unsigned(place_ty);
                        let op = if unsigned { "fcvtzu" } else { "fcvtzs" };
                        let prefix = if source64 { "d" } else { "s" };
                        let _ = writeln!(self.out, "    {op} x{dest}, {prefix}{source}");
                        self.normalize_reg(place_ty, dest)?;
                    }
                } else if target_float {
                    let Some(dest) = dest_float else { return Ok(false) };
                    let source = match self.promoted_int(operand) {
                        Some(register) => register,
                        None => {
                            self.operand(operand, 8)?;
                            8
                        }
                    };
                    let unsigned = self.is_unsigned(source_ty);
                    let op = if unsigned { "ucvtf" } else { "scvtf" };
                    let target64 = matches!(self.types.kind(place_ty), TyKind::F64);
                    let prefix = if target64 { "d" } else { "s" };
                    let _ = writeln!(self.out, "    {op} {prefix}{dest}, x{source}");
                } else {
                    let Some(dest) = dest_int else { return Ok(false) };
                    self.operand(operand, dest)?;
                    self.normalize_reg(place_ty, dest)?;
                }
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    fn direct_int_binary(
        &mut self,
        op: BinOp,
        lhs: u8,
        rhs: u8,
        lhs_operand: &Operand,
        result_ty: TyId,
        dest: u8,
    ) -> Result<(), String> {
        let lhs_ty = self.operand_ty(lhs_operand);
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
                return Err(
                    "short-circuit logical operators are lowered to branches, not binary operations"
                        .into(),
                );
            }
        };
        if matches!(op, BinOp::Shl | BinOp::Shr) {
            let bits = self.size_of(result_ty)? * 8;
            let mask = if bits < 64 { 31 } else { 63 };
            if mask < 63 {
                let _ = writeln!(self.out, "    and x12, x{rhs}, #{mask}");
                let _ = writeln!(self.out, "    {mnemonic} x{dest}, x{lhs}, x12");
            } else {
                let _ = writeln!(self.out, "    {mnemonic} x{dest}, x{lhs}, x{rhs}");
            }
        } else {
            match op {
                BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => {
                    let _ = writeln!(self.out, "    cmp x{lhs}, x{rhs}");
                    let _ = writeln!(self.out, "    cset x{dest}, {}", comparison.unwrap());
                }
                BinOp::Rem => {
                    let _ = writeln!(self.out, "    {mnemonic} x12, x{lhs}, x{rhs}");
                    let _ = writeln!(self.out, "    msub x{dest}, x12, x{rhs}, x{lhs}");
                }
                _ => {
                    let _ = writeln!(self.out, "    {mnemonic} x{dest}, x{lhs}, x{rhs}");
                }
            }
        }
        self.normalize_reg(result_ty, dest)
    }

    fn repeat(
        &mut self,
        place: &Place,
        ty: TyId,
        operand: &Operand,
        count: u64,
    ) -> Result<(), String> {
        let element = self.element(ty)?;
        let size = self.size_of(element)?;
        let aggregate = is_aggregate(self.types.kind(element));
        let float = self.is_float(element);
        let float64 = matches!(self.types.kind(element), TyKind::F64);
        let label = self.label();
        if aggregate {
            self.operand_address(operand)?;
            let _ = writeln!(self.out, "    mov x13, x11");
        } else if float {
            self.operand_float(operand, 0, float64)?;
        } else {
            self.operand(operand, 0)?;
        }
        self.address_of(place)?;
        let _ = writeln!(self.out, "    mov x14, x11");
        let _ = writeln!(self.out, "    mov x10, #0");
        let _ = writeln!(self.out, ".L{label}:");
        self.cmp_immediate(10, count)?;
        let _ = writeln!(self.out, "    b.hs .L{label}_done");
        if aggregate {
            self.emit_mov_u64(12, size)?;
            let _ = writeln!(self.out, "    mul x12, x10, x12");
            let _ = writeln!(self.out, "    add x11, x14, x12");
            self.copy_bytes(13, size)?;
        } else if size == 1 {
            let _ = writeln!(self.out, "    strb w0, [x14, x10]");
        } else {
            self.emit_mov_u64(12, size)?;
            let _ = writeln!(self.out, "    mul x12, x10, x12");
            let _ = writeln!(self.out, "    add x11, x14, x12");
            if float {
                let kind = if float64 { "d" } else { "s" };
                let _ = writeln!(self.out, "    str {kind}0, [x11]");
            } else {
                self.store_at(element, 0)?;
            }
        }
        let _ = writeln!(self.out, "    add x10, x10, #1");
        let _ = writeln!(self.out, "    b .L{label}");
        let _ = writeln!(self.out, ".L{label}_done:");
        Ok(())
    }

    fn array_literal(
        &mut self,
        place: &Place,
        ty: TyId,
        operands: &[Operand],
    ) -> Result<(), String> {
        let element = self.element(ty)?;
        let size = self.size_of(element)?;
        self.address_of(place)?;
        let _ = writeln!(self.out, "    mov x14, x11");
        for (index, operand) in operands.iter().enumerate() {
            self.store_operand_field(operand, element, 14, index as u64 * size)?;
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
        let fields = self.field_types(def, &args)?;
        if operands.len() != fields.len() {
            return Err("a struct literal and its definition disagree on field count".into());
        }
        self.address_of(place)?;
        let _ = writeln!(self.out, "    mov x14, x11");
        for ((operand, offset), field_ty) in operands.iter().zip(offsets).zip(fields) {
            self.store_operand_field(operand, field_ty, 14, offset)?;
        }
        Ok(())
    }

    fn store_operand_field(
        &mut self,
        operand: &Operand,
        ty: TyId,
        base: u8,
        offset: u64,
    ) -> Result<(), String> {
        if is_aggregate(self.types.kind(ty)) {
            let size = self.size_of(ty)?;
            let _ = writeln!(self.out, "    mov x11, x{base}");
            self.add_to_address(offset)?;
            let _ = writeln!(self.out, "    mov x9, x11");
            self.operand_address(operand)?;
            let _ = writeln!(self.out, "    mov x13, x11");
            let _ = writeln!(self.out, "    mov x11, x9");
            self.copy_bytes(13, size)?;
        } else if self.is_float(ty) {
            let kind = if matches!(self.types.kind(ty), TyKind::F64) { "d" } else { "s" };
            self.operand_float(operand, 0, kind == "d")?;
            let _ = writeln!(self.out, "    mov x11, x{base}");
            self.add_to_address(offset)?;
            let _ = writeln!(self.out, "    str {kind}0, [x11]");
        } else {
            self.operand(operand, 0)?;
            let _ = writeln!(self.out, "    mov x11, x{base}");
            self.add_to_address(offset)?;
            self.store_at(ty, 0)?;
        }
        Ok(())
    }

    fn tuple_literal(
        &mut self,
        place: &Place,
        ty: TyId,
        operands: &[Operand],
    ) -> Result<(), String> {
        let (offsets, fields) = self.tuple_fields(ty)?;
        if operands.len() != fields.len() {
            return Err("a tuple literal and its type disagree on element count".into());
        }
        self.address_of(place)?;
        let _ = writeln!(self.out, "    mov x14, x11");
        for ((operand, offset), field_ty) in operands.iter().zip(offsets).zip(fields) {
            self.store_operand_field(operand, field_ty, 14, offset)?;
        }
        Ok(())
    }

    fn store_tag(&mut self, base: u8, def: DefId, index: u32) -> Result<(), String> {
        let size = self.tag_size(def)?;
        let _ = writeln!(self.out, "    mov x12, #{index}");
        if size == 1 {
            let _ = writeln!(self.out, "    strb w12, [x{base}]");
        } else {
            let _ = writeln!(self.out, "    str w12, [x{base}]");
        }
        Ok(())
    }

    fn variant_literal(
        &mut self,
        place: &Place,
        ty: TyId,
        def: DefId,
        index: u32,
        operands: &[Operand],
    ) -> Result<(), String> {
        let (_, args) = self.enum_parts(ty)?;
        let payload = self.enum_payload_offset(def, &args)?;
        let offsets = self.variant_offsets(def, index, &args)?;
        let fields = self.variant_types(def, index, &args)?;
        if operands.len() != fields.len() {
            return Err(
                "an enum variant literal and its definition disagree on field count".into(),
            );
        }
        self.address_of(place)?;
        let _ = writeln!(self.out, "    mov x14, x11");
        self.store_tag(14, def, index)?;
        for ((operand, offset), field_ty) in operands.iter().zip(offsets).zip(fields) {
            self.store_operand_field(operand, field_ty, 14, payload + offset)?;
        }
        Ok(())
    }

    fn unit_variant(&mut self, place: &Place, def: DefId, index: u32) -> Result<(), String> {
        self.address_of(place)?;
        self.store_tag(11, def, index)
    }

    fn copy_aggregate(
        &mut self,
        destination: &Place,
        source: &Place,
        size: u64,
    ) -> Result<(), String> {
        self.address_of(source)?;
        let _ = writeln!(self.out, "    mov x13, x11");
        self.address_of(destination)?;
        self.copy_memory(size)
    }

    /// Copies `size` bytes from `[x13]` to `[x11]`.
    ///
    /// Eight bytes at a time with 4/2/1-byte tails, rather than the old
    /// byte-at-a-time loop. Structs, arrays and returns are copied often enough
    /// that the loop overhead was visible.
    fn copy_memory(&mut self, size: u64) -> Result<(), String> {
        if size == 0 {
            return Ok(());
        }
        if size <= 16 {
            let mut offset = 0u64;
            while offset + 8 <= size {
                let _ = writeln!(self.out, "    ldr x12, [x13, #{offset}]");
                let _ = writeln!(self.out, "    str x12, [x11, #{offset}]");
                offset += 8;
            }
            if offset + 4 <= size {
                let _ = writeln!(self.out, "    ldr w12, [x13, #{offset}]");
                let _ = writeln!(self.out, "    str w12, [x11, #{offset}]");
                offset += 4;
            }
            if offset + 2 <= size {
                let _ = writeln!(self.out, "    ldrh w12, [x13, #{offset}]");
                let _ = writeln!(self.out, "    strh w12, [x11, #{offset}]");
                offset += 2;
            }
            if offset < size {
                let _ = writeln!(self.out, "    ldrb w12, [x13, #{offset}]");
                let _ = writeln!(self.out, "    strb w12, [x11, #{offset}]");
            }
            return Ok(());
        }
        let label = self.label();
        self.emit_mov_u64(16, size)?;
        let _ = writeln!(self.out, "    mov x15, #0");
        let _ = writeln!(self.out, ".L{label}:");
        let _ = writeln!(self.out, "    add x17, x15, #8");
        let _ = writeln!(self.out, "    cmp x17, x16");
        let _ = writeln!(self.out, "    b.hi .L{label}_tail");
        let _ = writeln!(self.out, "    ldr x12, [x13, x15]");
        let _ = writeln!(self.out, "    str x12, [x11, x15]");
        let _ = writeln!(self.out, "    add x15, x15, #8");
        let _ = writeln!(self.out, "    b .L{label}");
        let _ = writeln!(self.out, ".L{label}_tail:");
        let _ = writeln!(self.out, "    sub x17, x16, x15");
        let _ = writeln!(self.out, "    cmp x17, #4");
        let _ = writeln!(self.out, "    b.lo .L{label}_2");
        let _ = writeln!(self.out, "    ldr w12, [x13, x15]");
        let _ = writeln!(self.out, "    str w12, [x11, x15]");
        let _ = writeln!(self.out, "    add x15, x15, #4");
        let _ = writeln!(self.out, "    sub x17, x17, #4");
        let _ = writeln!(self.out, ".L{label}_2:");
        let _ = writeln!(self.out, "    cmp x17, #2");
        let _ = writeln!(self.out, "    b.lo .L{label}_1");
        let _ = writeln!(self.out, "    ldrh w12, [x13, x15]");
        let _ = writeln!(self.out, "    strh w12, [x11, x15]");
        let _ = writeln!(self.out, "    add x15, x15, #2");
        let _ = writeln!(self.out, "    sub x17, x17, #2");
        let _ = writeln!(self.out, ".L{label}_1:");
        let _ = writeln!(self.out, "    cmp x17, #1");
        let _ = writeln!(self.out, "    b.lo .L{label}_done");
        let _ = writeln!(self.out, "    ldrb w12, [x13, x15]");
        let _ = writeln!(self.out, "    strb w12, [x11, x15]");
        let _ = writeln!(self.out, ".L{label}_done:");
        Ok(())
    }

    fn copy_bytes(&mut self, source_register: u8, size: u64) -> Result<(), String> {
        if source_register != 13 {
            let _ = writeln!(self.out, "    mov x13, x{source_register}");
        }
        self.copy_memory(size)
    }

    fn terminator(&mut self, terminator: &Terminator) -> Result<(), String> {
        match terminator {
            Terminator::Goto(target) => {
                let _ = writeln!(self.out, "    b .L{}_bb{}", self.tag, target.0);
                Ok(())
            }
            Terminator::SwitchInt {
                discr,
                targets,
                otherwise,
            } => {
                self.operand(discr, 0)?;
                for (value, target) in targets {
                    let _ = writeln!(self.out, "    mov x1, #{value}");
                    let _ = writeln!(self.out, "    cmp x0, x1");
                    let _ = writeln!(self.out, "    b.eq .L{}_bb{}", self.tag, target.0);
                }
                let _ = writeln!(self.out, "    b .L{}_bb{}", self.tag, otherwise.0);
                Ok(())
            }
            Terminator::Call {
                callee,
                args,
                destination,
                target,
                ..
            } => {
                self.emit_call(callee, args, destination.as_ref())?;
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
                    if is_aggregate(self.types.kind(ty)) {
                        if let Some(hfa) = self.hfa_of(ty)? {
                            self.address_of(&place)?;
                            let _ = writeln!(self.out, "    mov x13, x11");
                            for (index, offset) in hfa.offsets.iter().enumerate() {
                                let _ = writeln!(self.out, "    mov x11, x13");
                                self.add_to_address(*offset)?;
                                let kind = if hfa.float64 { "d" } else { "s" };
                                let _ = writeln!(self.out, "    ldr {kind}{index}, [x11]");
                            }
                        } else {
                            let size = self.size_of(ty)?;
                            if size > 16 {
                                self.address_of(&place)?;
                                let _ = writeln!(self.out, "    mov x13, x11");
                                self.sret_address(11)?;
                                let _ = writeln!(self.out, "    ldr x11, [x11]");
                                self.copy_bytes(13, size)?;
                            } else {
                                self.address_of(&place)?;
                                let _ = writeln!(self.out, "    ldr x0, [x11]");
                                if size > 8 {
                                    let _ = writeln!(self.out, "    ldr x1, [x11, #8]");
                                }
                            }
                        }
                    } else if self.is_float(ty) {
                        let float64 = matches!(self.types.kind(ty), TyKind::F64);
                        self.load_float(&place, 0, float64)?;
                    } else {
                        self.load(&place, 0)?;
                    }
                } else {
                    let _ = writeln!(self.out, "    mov x0, #0");
                }
                for chunk in self.saved_fregs.chunks(2).rev() {
                    match chunk {
                        [a, b] => {
                            let _ = writeln!(self.out, "    ldp d{a}, d{b}, [sp], #16");
                        }
                        [a] => {
                            let _ = writeln!(self.out, "    ldr d{a}, [sp], #16");
                        }
                        _ => {}
                    }
                }
                for chunk in self.saved_regs.chunks(2).rev() {
                    match chunk {
                        [a, b] => {
                            let _ = writeln!(self.out, "    ldp x{a}, x{b}, [sp], #16");
                        }
                        [a] => {
                            let _ = writeln!(self.out, "    ldp x{a}, xzr, [sp], #16");
                        }
                        _ => {}
                    }
                }
                if self.frame > 0 {
                    if self.frame <= 4095 {
                        let _ = writeln!(self.out, "    add sp, sp, #{}", self.frame);
                    } else {
                        let _ = writeln!(self.out, "    mov x12, #{}", self.frame);
                        let _ = writeln!(self.out, "    add sp, sp, x12");
                    }
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

    fn store_incoming_args(&mut self) -> Result<(), String> {
        let types: Vec<Option<TyId>> = self
            .body
            .locals
            .iter()
            .take(self.body.arg_count as usize)
            .map(|local| Some(local.ty))
            .collect();
        let (locations, _) = self.assign_args(&types)?;
        // Register arguments first. Every one of them is in x0..x7 at entry, so
        // copying a large aggregate must use scratch registers that are not
        // argument registers.
        for (index, location) in locations.iter().enumerate() {
            if location.regs.is_empty() && location.freg.is_none() && location.fregs.is_empty() {
                continue;
            }
            match location.kind {
                ArgKind::Scalar => {
                    if let Some(local) = self.local_reg[index] {
                        let _ = writeln!(self.out, "    mov x{local}, x{}", location.regs[0]);
                    } else {
                        self.local_address(index, 11)?;
                        let _ = writeln!(self.out, "    str x{}, [x11]", location.regs[0]);
                    }
                }
                ArgKind::Small => {
                    self.local_address(index, 11)?;
                    let _ = writeln!(self.out, "    str x{}, [x11]", location.regs[0]);
                    if location.size > 8 {
                        let _ = writeln!(self.out, "    str x{}, [x11, #8]", location.regs[1]);
                    }
                }
                ArgKind::Large => {
                    self.copy_from_pointer(location.regs[0], index, location.size)?;
                }
                ArgKind::Float => {
                    let reg = location.freg.unwrap();
                    let kind = if location.float64 { "d" } else { "s" };
                    if let Some(local) = self.local_freg[index] {
                        let _ = writeln!(self.out, "    fmov {kind}{local}, {kind}{reg}");
                    } else {
                        self.local_address(index, 11)?;
                        let _ = writeln!(self.out, "    str {kind}{reg}, [x11]");
                    }
                }
                ArgKind::Hfa => {
                    let hfa = location.hfa.as_ref().unwrap();
                    for (freg, offset) in location.fregs.iter().zip(&hfa.offsets) {
                        self.local_address(index, 11)?;
                        self.add_to_address(*offset)?;
                        let kind = if location.float64 { "d" } else { "s" };
                        let _ = writeln!(self.out, "    str {kind}{freg}, [x11]");
                    }
                }
            }
        }
        // Then the stack arguments, read from the caller's outgoing area at
        // `x29 + 16`.
        for (index, location) in locations.iter().enumerate() {
            let Some(stack) = location.stack else {
                continue;
            };
            let from = 16 + stack;
            match location.kind {
                ArgKind::Scalar => {
                    let _ = writeln!(self.out, "    ldr x12, [x29, #{from}]");
                    if let Some(local) = self.local_reg[index] {
                        let _ = writeln!(self.out, "    mov x{local}, x12");
                    } else {
                        self.local_address(index, 11)?;
                        let _ = writeln!(self.out, "    str x12, [x11]");
                    }
                }
                ArgKind::Small => {
                    self.local_address(index, 11)?;
                    let _ = writeln!(self.out, "    ldr x12, [x29, #{from}]");
                    let _ = writeln!(self.out, "    str x12, [x11]");
                    if location.size > 8 {
                        let _ = writeln!(self.out, "    ldr x12, [x29, #{}]", from + 8);
                        let _ = writeln!(self.out, "    str x12, [x11, #8]");
                    }
                }
                ArgKind::Large => {
                    let _ = writeln!(self.out, "    ldr x13, [x29, #{from}]");
                    self.copy_from_register_pointer(13, index, location.size)?;
                }
                ArgKind::Float => {
                    let kind = if location.float64 { "d" } else { "s" };
                    // `d0`-`d7` are free here: every register argument has
                    // already been stored, and `d8`-`d15` may hold promoted
                    // parameters that must survive this copy.
                    let _ = writeln!(self.out, "    ldr {kind}0, [x29, #{from}]");
                    if let Some(local) = self.local_freg[index] {
                        let _ = writeln!(self.out, "    fmov {kind}{local}, {kind}0");
                    } else {
                        self.local_address(index, 11)?;
                        let _ = writeln!(self.out, "    str {kind}0, [x11]");
                    }
                }
                ArgKind::Hfa => {
                    let hfa = location.hfa.as_ref().unwrap();
                    let _ = writeln!(self.out, "    add x13, x29, #{from}");
                    for offset in &hfa.offsets {
                        let _ = writeln!(self.out, "    mov x11, x13");
                        self.add_to_address(*offset)?;
                        let kind = if location.float64 { "d" } else { "s" };
                        let _ = writeln!(self.out, "    ldr {kind}0, [x11]");
                        self.local_address(index, 11)?;
                        self.add_to_address(*offset)?;
                        let _ = writeln!(self.out, "    str {kind}0, [x11]");
                    }
                }
            }
        }
        Ok(())
    }

    fn copy_from_pointer(&mut self, register: u8, local: usize, size: u64) -> Result<(), String> {
        let _ = writeln!(self.out, "    mov x13, x{register}");
        self.copy_from_register_pointer(13, local, size)
    }

    fn copy_from_register_pointer(
        &mut self,
        pointer_register: u8,
        local: usize,
        size: u64,
    ) -> Result<(), String> {
        self.local_address(local, 11)?;
        let _ = writeln!(self.out, "    mov x13, x{pointer_register}");
        self.copy_memory(size)
    }

    fn emit_call(
        &mut self,
        callee: &Operand,
        args: &[Operand],
        destination: Option<&Place>,
    ) -> Result<(), String> {
        let types: Vec<Option<TyId>> = args
            .iter()
            .map(|arg| self.call_arg_ty(arg))
            .collect::<Result<_, _>>()?;
        let (locations, stack_bytes) = self.assign_args(&types)?;
        let symbol = self.callee(callee)?;

        if stack_bytes > 0 {
            if stack_bytes <= 4095 {
                let _ = writeln!(self.out, "    sub sp, sp, #{stack_bytes}");
            } else {
                self.emit_mov_u64(12, stack_bytes)?;
                let _ = writeln!(self.out, "    sub sp, sp, x12");
            }
        }
        // Stack arguments first: the scratch registers used to copy them are
        // the argument registers themselves.
        for (arg, location) in args.iter().zip(&locations) {
            let Some(stack) = location.stack else {
                continue;
            };
            match location.kind {
                ArgKind::Scalar => {
                    self.operand(arg, 12)?;
                    let _ = writeln!(self.out, "    str x12, [sp, #{stack}]");
                }
                ArgKind::Small => {
                    self.operand_address(arg)?;
                    let _ = writeln!(self.out, "    ldr x12, [x11]");
                    let _ = writeln!(self.out, "    str x12, [sp, #{stack}]");
                    if location.size > 8 {
                        let _ = writeln!(self.out, "    ldr x12, [x11, #8]");
                        let _ = writeln!(self.out, "    str x12, [sp, #{}]", stack + 8);
                    }
                }
                ArgKind::Large => {
                    self.operand_address(arg)?;
                    let _ = writeln!(self.out, "    str x11, [sp, #{stack}]");
                }
                ArgKind::Float => {
                    // Register arguments are emitted after this, so `d0` is
                    // free and cannot clobber a promoted caller local.
                    self.operand_float(arg, 0, location.float64)?;
                    let kind = if location.float64 { "d" } else { "s" };
                    let _ = writeln!(self.out, "    str {kind}0, [sp, #{stack}]");
                }
                ArgKind::Hfa => {
                    let hfa = location.hfa.as_ref().unwrap();
                    self.operand_address(arg)?;
                    let _ = writeln!(self.out, "    mov x13, x11");
                    for offset in &hfa.offsets {
                        let _ = writeln!(self.out, "    mov x11, x13");
                        self.add_to_address(*offset)?;
                        let kind = if location.float64 { "d" } else { "s" };
                        let _ = writeln!(self.out, "    ldr {kind}0, [x11]");
                        if stack + offset <= 4095 {
                            let _ = writeln!(self.out, "    str {kind}0, [sp, #{}]", stack + offset);
                        } else {
                            self.emit_mov_u64(12, stack + offset)?;
                            let _ = writeln!(self.out, "    add x12, sp, x12");
                            let _ = writeln!(self.out, "    str {kind}0, [x12]");
                        }
                    }
                }
            }
        }
        // A large return goes through x8, which the callee reads before any
        // call it makes.
        if let Some(destination) = destination {
            let ty = self.place_ty(destination)?;
            if is_aggregate(self.types.kind(ty)) && self.size_of(ty)? > 16 {
                self.address_of(destination)?;
                let _ = writeln!(self.out, "    mov x8, x11");
            }
        }
        if symbol.is_none() {
            // `(*f)()` models a function value through a `*const fn`. The
            // reference backend treats the pointer itself as the entry point;
            // loading the pointee would read the first instruction as an
            // address. Drop the final `Deref` and load the pointer value.
            let indirect = match callee {
                Operand::Copy(place) | Operand::Move(place)
                    if matches!(place.projection.last(), Some(Projection::Deref)) =>
                {
                    let mut projection = place.projection.clone();
                    projection.pop();
                    Operand::Copy(Place {
                        local: place.local,
                        projection,
                    })
                }
                other => other.clone(),
            };
            self.operand(&indirect, 9)?;
        }
        // Register arguments last, so nothing clobbers them.
        for (arg, location) in args.iter().zip(&locations) {
            if location.regs.is_empty() && location.freg.is_none() && location.fregs.is_empty() {
                continue;
            }
            match location.kind {
                ArgKind::Scalar => {
                    self.operand(arg, location.regs[0])?;
                }
                ArgKind::Small => {
                    self.operand_address(arg)?;
                    let _ = writeln!(self.out, "    ldr x{}, [x11]", location.regs[0]);
                    if location.size > 8 {
                        let _ = writeln!(self.out, "    ldr x{}, [x11, #8]", location.regs[1]);
                    }
                }
                ArgKind::Large => {
                    self.operand_address(arg)?;
                    let _ = writeln!(self.out, "    mov x{}, x11", location.regs[0]);
                }
                ArgKind::Float => {
                    self.operand_float(arg, location.freg.unwrap(), location.float64)?;
                }
                ArgKind::Hfa => {
                    let hfa = location.hfa.as_ref().unwrap();
                    self.operand_address(arg)?;
                    let _ = writeln!(self.out, "    mov x13, x11");
                    for (freg, offset) in location.fregs.iter().zip(&hfa.offsets) {
                        let _ = writeln!(self.out, "    mov x11, x13");
                        self.add_to_address(*offset)?;
                        let kind = if location.float64 { "d" } else { "s" };
                        let _ = writeln!(self.out, "    ldr {kind}{freg}, [x11]");
                    }
                }
            }
        }
        match symbol {
            Some(symbol) => {
                let _ = writeln!(self.out, "    bl {symbol}");
            }
            None => {
                let _ = writeln!(self.out, "    blr x9");
            }
        }
        if stack_bytes > 0 {
            if stack_bytes <= 4095 {
                let _ = writeln!(self.out, "    add sp, sp, #{stack_bytes}");
            } else {
                self.emit_mov_u64(12, stack_bytes)?;
                let _ = writeln!(self.out, "    add sp, sp, x12");
            }
        }
        if let Some(destination) = destination {
            let ty = self.place_ty(destination)?;
            let hfa = if is_aggregate(self.types.kind(ty)) {
                self.hfa_of(ty)?
            } else {
                None
            };
            if let Some(hfa) = hfa {
                self.store_hfa_result(destination, &hfa)?;
            } else if is_aggregate(self.types.kind(ty)) {
                let size = self.size_of(ty)?;
                if size <= 16 {
                    self.address_of(destination)?;
                    let _ = writeln!(self.out, "    str x0, [x11]");
                    if size > 8 {
                        let _ = writeln!(self.out, "    str x1, [x11, #8]");
                    }
                }
            } else if matches!(self.types.kind(ty), TyKind::F32 | TyKind::F64) {
                self.store_float_result(destination, matches!(self.types.kind(ty), TyKind::F64))?;
            } else {
                self.normalize(ty)?;
                self.store(destination, 0)?;
            }
        }
        Ok(())
    }

    fn call_arg_ty(&mut self, operand: &Operand) -> Result<Option<TyId>, String> {
        match operand {
            Operand::Copy(place) | Operand::Move(place) => Ok(Some(self.place_ty(place)?)),
            Operand::Constant(Constant::Int { ty, .. })
            | Operand::Constant(Constant::Uint { ty, .. })
            | Operand::Constant(Constant::Float { ty, .. }) => Ok(Some(*ty)),
            _ => Ok(None),
        }
    }

    fn operand_address(&mut self, operand: &Operand) -> Result<(), String> {
        match operand {
            Operand::Copy(place) | Operand::Move(place) => self.address_of(place),
            _ => Err("an aggregate argument must be a place, not a constant".into()),
        }
    }

    fn assign_args(&mut self, types: &[Option<TyId>]) -> Result<(Vec<ArgLoc>, u64), String> {
        let mut regs = 0u8;
        let mut fregs = 0u8;
        let mut stack = 0u64;
        let mut gpr_stack = false;
        let mut fpr_stack = false;
        let mut locations = Vec::with_capacity(types.len());
        for ty in types {
            let (kind, size, float64, hfa) = self.arg_kind(*ty)?;
            if matches!(kind, ArgKind::Float | ArgKind::Hfa) {
                let needed = if kind == ArgKind::Hfa {
                    hfa.as_ref().map(|h| h.offsets.len()).unwrap_or(0) as u8
                } else {
                    1
                };
                if !fpr_stack && fregs + needed <= 8 {
                    let assigned: Vec<u8> = (fregs..fregs + needed).collect();
                    fregs += needed;
                    locations.push(ArgLoc {
                        regs: Vec::new(),
                        freg: if kind == ArgKind::Float {
                            Some(assigned[0])
                        } else {
                            None
                        },
                        fregs: if kind == ArgKind::Hfa {
                            assigned
                        } else {
                            Vec::new()
                        },
                        hfa,
                        stack: None,
                        kind,
                        size,
                        float64,
                    });
                } else {
                    fpr_stack = true;
                    let slot = (size + 7) & !7;
                    locations.push(ArgLoc {
                        regs: Vec::new(),
                        freg: None,
                        fregs: Vec::new(),
                        hfa,
                        stack: Some(stack),
                        kind,
                        size,
                        float64,
                    });
                    stack += slot.max(8);
                }
                continue;
            }
            let needed = match kind {
                ArgKind::Scalar => 1,
                ArgKind::Small => size.div_ceil(8) as u8,
                ArgKind::Large => 1,
                ArgKind::Float | ArgKind::Hfa => unreachable!(),
            };
            if kind == ArgKind::Small && needed == 2 && regs % 2 == 1 {
                regs += 1;
            }
            if !gpr_stack && regs + needed <= 8 {
                let assigned: Vec<u8> = (regs..regs + needed).collect();
                regs += needed;
                locations.push(ArgLoc {
                    regs: assigned,
                    freg: None,
                    fregs: Vec::new(),
                    hfa: None,
                    stack: None,
                    kind,
                    size,
                    float64,
                });
            } else {
                gpr_stack = true;
                let slot = match kind {
                    ArgKind::Scalar | ArgKind::Large => 8,
                    ArgKind::Small => (size + 7) & !7,
                    ArgKind::Float | ArgKind::Hfa => unreachable!(),
                };
                locations.push(ArgLoc {
                    regs: Vec::new(),
                    freg: None,
                    fregs: Vec::new(),
                    hfa: None,
                    stack: Some(stack),
                    kind,
                    size,
                    float64,
                });
                stack += slot;
            }
        }
        let stack_bytes = (stack + 15) & !15;
        Ok((locations, stack_bytes))
    }

    fn hfa_of(&mut self, ty: TyId) -> Result<Option<Hfa>, String> {
        let mut offsets = Vec::new();
        let mut float64 = None;
        let pure = self.collect_hfa(ty, 0, &mut offsets, &mut float64)?;
        if !pure || offsets.is_empty() || offsets.len() > 4 {
            return Ok(None);
        }
        let Some(float64) = float64 else {
            return Ok(None);
        };
        Ok(Some(Hfa { offsets, float64 }))
    }

    fn collect_hfa(
        &mut self,
        ty: TyId,
        base: u64,
        offsets: &mut Vec<u64>,
        float64: &mut Option<bool>,
    ) -> Result<bool, String> {
        match self.types.kind(ty).clone() {
            TyKind::F32 => {
                if *float64 == Some(true) {
                    return Ok(false);
                }
                *float64 = Some(false);
                offsets.push(base);
                Ok(true)
            }
            TyKind::F64 => {
                if *float64 == Some(false) {
                    return Ok(false);
                }
                *float64 = Some(true);
                offsets.push(base);
                Ok(true)
            }
            TyKind::Array { elem, len } => {
                if len == 0 {
                    return Ok(false);
                }
                let size = self.size_of(elem)?;
                for index in 0..len {
                    if !self.collect_hfa(elem, base + index * size, offsets, float64)? {
                        return Ok(false);
                    }
                }
                Ok(true)
            }
            TyKind::Tuple(_) => {
                let (field_offsets, elems) = self.tuple_fields(ty)?;
                for (offset, elem) in field_offsets.iter().zip(elems) {
                    if !self.collect_hfa(elem, base + offset, offsets, float64)? {
                        return Ok(false);
                    }
                }
                Ok(true)
            }
            TyKind::Adt { def, args } => {
                if self.hir.enums.iter().any(|e| e.def_id == def) {
                    return Ok(false);
                }
                let field_offsets = self.field_offsets(def, &args)?;
                let field_types = self.field_types(def, &args)?;
                if field_types.is_empty() {
                    return Ok(false);
                }
                for (offset, field_ty) in field_offsets.iter().zip(field_types) {
                    if !self.collect_hfa(field_ty, base + offset, offsets, float64)? {
                        return Ok(false);
                    }
                }
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    fn arg_kind(&mut self, ty: Option<TyId>) -> Result<(ArgKind, u64, bool, Option<Hfa>), String> {
        let Some(ty) = ty else {
            return Ok((ArgKind::Scalar, 8, false, None));
        };
        match self.types.kind(ty) {
            TyKind::F32 => Ok((ArgKind::Float, 4, false, None)),
            TyKind::F64 => Ok((ArgKind::Float, 8, true, None)),
            _ if is_aggregate(self.types.kind(ty)) => {
                if let Some(hfa) = self.hfa_of(ty)? {
                    let size = self.size_of(ty)?;
                    let float64 = hfa.float64;
                    Ok((ArgKind::Hfa, size, float64, Some(hfa)))
                } else {
                    let size = self.size_of(ty)?;
                    if size > 16 {
                        Ok((ArgKind::Large, size, false, None))
                    } else {
                        Ok((ArgKind::Small, size, false, None))
                    }
                }
            }
            _ => Ok((ArgKind::Scalar, 8, false, None)),
        }
    }

    fn is_float(&self, ty: TyId) -> bool {
        matches!(self.types.kind(ty), TyKind::F32 | TyKind::F64)
    }

    fn operand_float(&mut self, operand: &Operand, freg: u8, float64: bool) -> Result<(), String> {
        match operand {
            Operand::Copy(place) | Operand::Move(place) => self.load_float(place, freg, float64),
            Operand::Constant(Constant::Float { value, ty: _ }) => {
                if float64 {
                    self.emit_mov_u64(12, value.to_bits())?;
                    let _ = writeln!(self.out, "    fmov d{freg}, x12");
                } else {
                    self.emit_mov_u64(12, u64::from((*value as f32).to_bits()))?;
                    let _ = writeln!(self.out, "    fmov s{freg}, w12");
                }
                Ok(())
            }
            other => Err(format!(
                "a floating-point argument must be a float value, found {other:?}"
            )),
        }
    }

    fn emit_symbol_address(&mut self, register: u8, symbol: &str) {
        if self.target.format == crate::target::ObjectFormat::MachO {
            let _ = writeln!(self.out, "    adrp x{register}, {symbol}@PAGE");
            let _ = writeln!(
                self.out,
                "    add x{register}, x{register}, {symbol}@PAGEOFF"
            );
        } else {
            let _ = writeln!(self.out, "    adrp x{register}, {symbol}");
            let _ = writeln!(self.out, "    add x{register}, x{register}, :lo12:{symbol}");
        }
    }

    fn load_float(&mut self, place: &Place, freg: u8, float64: bool) -> Result<(), String> {
        let kind = if float64 { "d" } else { "s" };
        if place.projection.is_empty() {
            if let Some(local) = self.local_freg[place.local.0 as usize] {
                if local != freg {
                    let _ = writeln!(self.out, "    fmov {kind}{freg}, {kind}{local}");
                }
                return Ok(());
            }
        }
        self.address_of(place)?;
        let _ = writeln!(self.out, "    ldr {kind}{freg}, [x11]");
        Ok(())
    }

    fn store_float_result(&mut self, destination: &Place, float64: bool) -> Result<(), String> {
        let kind = if float64 { "d" } else { "s" };
        if destination.projection.is_empty() {
            if let Some(local) = self.local_freg[destination.local.0 as usize] {
                let _ = writeln!(self.out, "    fmov {kind}{local}, {kind}0");
                return Ok(());
            }
        }
        self.address_of(destination)?;
        let _ = writeln!(self.out, "    str {kind}0, [x11]");
        Ok(())
    }

    fn emit_mov_u64(&mut self, register: u8, value: u64) -> Result<(), String> {
        let _ = writeln!(self.out, "    movz x{register}, #{}", value & 0xffff);
        for (shift, chunk) in [
            (16, (value >> 16) & 0xffff),
            (32, (value >> 32) & 0xffff),
            (48, (value >> 48) & 0xffff),
        ] {
            if chunk != 0 {
                let _ = writeln!(self.out, "    movk x{register}, #{chunk}, lsl #{shift}");
            }
        }
        Ok(())
    }

    fn float_binary(
        &mut self,
        op: BinOp,
        lhs: &Operand,
        rhs: &Operand,
        lhs_ty: TyId,
    ) -> Result<(), String> {
        let float64 = matches!(self.types.kind(lhs_ty), TyKind::F64);
        let prefix = if float64 { "d" } else { "s" };
        self.operand_float(lhs, 0, float64)?;
        self.operand_float(rhs, 1, float64)?;
        match op {
            BinOp::Add => {
                let _ = writeln!(self.out, "    fadd {prefix}0, {prefix}0, {prefix}1");
            }
            BinOp::Sub => {
                let _ = writeln!(self.out, "    fsub {prefix}0, {prefix}0, {prefix}1");
            }
            BinOp::Mul => {
                let _ = writeln!(self.out, "    fmul {prefix}0, {prefix}0, {prefix}1");
            }
            BinOp::Div => {
                let _ = writeln!(self.out, "    fdiv {prefix}0, {prefix}0, {prefix}1");
            }
            BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => {
                let condition = match op {
                    BinOp::Eq => "eq",
                    BinOp::Ne => "ne",
                    BinOp::Lt => "mi",
                    BinOp::Le => "ls",
                    BinOp::Gt => "gt",
                    _ => "ge",
                };
                let _ = writeln!(self.out, "    fcmp {prefix}0, {prefix}1");
                let _ = writeln!(self.out, "    cset x0, {condition}");
            }
            _ => {
                return Err(
                    "the AArch64 backend supports only arithmetic and comparison on floats".into(),
                );
            }
        }
        Ok(())
    }

    fn cast_float(&mut self, operand: &Operand, to: TyId) -> Result<(), String> {
        let from = self
            .operand_ty(operand)
            .ok_or_else(|| "a float cast has no source type".to_string())?;
        let from_float = self.is_float(from);
        let to_float = self.is_float(to);
        match (from_float, to_float) {
            (true, true) => {
                if from == to {
                    return Ok(());
                }
                if matches!(self.types.kind(from), TyKind::F64) {
                    self.operand_float(operand, 0, true)?;
                    let _ = writeln!(self.out, "    fcvt s0, d0");
                } else {
                    self.operand_float(operand, 0, false)?;
                    let _ = writeln!(self.out, "    fcvt d0, s0");
                }
            }
            (true, false) => {
                let float64 = matches!(self.types.kind(from), TyKind::F64);
                self.operand_float(operand, 0, float64)?;
                let prefix = if float64 { "d" } else { "s" };
                let unsigned = self.is_unsigned(to);
                let op = if unsigned { "fcvtzu" } else { "fcvtzs" };
                let _ = writeln!(self.out, "    {op} x0, {prefix}0");
            }
            (false, true) => {
                self.operand(operand, 0)?;
                let unsigned = self.is_unsigned(from);
                let op = if unsigned { "ucvtf" } else { "scvtf" };
                if matches!(self.types.kind(to), TyKind::F64) {
                    let _ = writeln!(self.out, "    {op} d0, x0");
                } else {
                    let _ = writeln!(self.out, "    {op} s0, x0");
                }
            }
            (false, false) => {}
        }
        Ok(())
    }

    fn store_hfa_result(&mut self, destination: &Place, hfa: &Hfa) -> Result<(), String> {
        self.address_of(destination)?;
        let _ = writeln!(self.out, "    mov x13, x11");
        for (index, offset) in hfa.offsets.iter().enumerate() {
            let _ = writeln!(self.out, "    mov x11, x13");
            self.add_to_address(*offset)?;
            let kind = if hfa.float64 { "d" } else { "s" };
            let _ = writeln!(self.out, "    str {kind}{index}, [x11]");
        }
        Ok(())
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
                self.emit_mov_u64(reg, value as u64)?;
            }
            Constant::Uint { value, .. } => {
                let value = u64::try_from(*value)
                    .map_err(|_| "an integer constant does not fit in 64 bits".to_string())?;
                self.emit_mov_u64(reg, value)?;
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
            Constant::Float { value, ty } => {
                let bits = if matches!(self.types.kind(*ty), TyKind::F32) {
                    u64::from((*value as f32).to_bits())
                } else {
                    value.to_bits()
                };
                self.emit_mov_u64(reg, bits)?;
            }
            Constant::Str(id) => {
                let symbol = format!(".Lstr_{}", id.0);
                self.emit_symbol_address(reg, &symbol);
            }
            Constant::Fn(id) => {
                let name = self
                    .symbols
                    .get(id)
                    .map(|name| mangle(name, self.foreign, self.target))
                    .ok_or_else(|| "a function pointer names no symbol".to_string())?;
                self.emit_symbol_address(reg, &name);
            }
            Constant::NamedFn(name) => {
                self.emit_symbol_address(reg, name);
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
        if place.projection.is_empty()
            && (self.local_reg[place.local.0 as usize].is_some()
                || self.local_freg[place.local.0 as usize].is_some())
        {
            return Err(format!(
                "the AArch64 backend tried to take the address of register-promoted local {}",
                place.local.0
            ));
        }
        let mut ty = self.body.local(place.local).ty;
        self.local_address(place.local.0 as usize, 11)?;
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
                        let _ = writeln!(
                            self.out,
                            "    add x11, x11, x10, lsl #{}",
                            size.trailing_zeros()
                        );
                    } else {
                        let _ = writeln!(self.out, "    mov x12, #{size}");
                        let _ = writeln!(self.out, "    mul x10, x10, x12");
                        let _ = writeln!(self.out, "    add x11, x11, x10");
                    }
                    ty = element;
                }
                Projection::Field(index) => {
                    let (offset, field_ty) = self.field_access(ty, *index)?;
                    self.add_to_address(offset)?;
                    ty = field_ty;
                }
                Projection::EnumField { variant, index } => {
                    let (offset, field_ty) = self.enum_field_access(ty, *variant, *index)?;
                    self.add_to_address(offset)?;
                    ty = field_ty;
                }
            }
        }
        Ok(())
    }

    fn load(&mut self, place: &Place, reg: u8) -> Result<(), String> {
        if place.projection.is_empty() {
            if let Some(local) = self.local_reg[place.local.0 as usize] {
                if local != reg {
                    let _ = writeln!(self.out, "    mov x{reg}, x{local}");
                }
                return Ok(());
            }
        }
        let ty = self.place_ty(place)?;
        self.address_of(place)?;
        self.load_at(ty, reg)
    }

    fn load_at(&mut self, ty: TyId, reg: u8) -> Result<(), String> {
        match self.types.kind(ty) {
            // Signed loads extend to the full register; a zero-extended `w`
            // would make a negative value compare as a large positive one.
            TyKind::I8 => {
                let _ = writeln!(self.out, "    ldrsb x{reg}, [x11]");
            }
            TyKind::U8 | TyKind::Bool => {
                let _ = writeln!(self.out, "    ldrb w{reg}, [x11]");
            }
            TyKind::I16 => {
                let _ = writeln!(self.out, "    ldrsh x{reg}, [x11]");
            }
            TyKind::U16 => {
                let _ = writeln!(self.out, "    ldrh w{reg}, [x11]");
            }
            TyKind::I32 => {
                let _ = writeln!(self.out, "    ldrsw x{reg}, [x11]");
            }
            TyKind::U32 | TyKind::Char | TyKind::F32 => {
                let _ = writeln!(self.out, "    ldr w{reg}, [x11]");
            }
            _ => {
                let _ = writeln!(self.out, "    ldr x{reg}, [x11]");
            }
        }
        Ok(())
    }

    fn store(&mut self, place: &Place, reg: u8) -> Result<(), String> {
        if place.projection.is_empty() {
            if let Some(local) = self.local_reg[place.local.0 as usize] {
                if local != reg {
                    let _ = writeln!(self.out, "    mov x{local}, x{reg}");
                }
                return Ok(());
            }
        }
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
            TyKind::I32 | TyKind::U32 | TyKind::Char | TyKind::F32 => {
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
        if matches!(op, BinOp::Shl | BinOp::Shr) {
            let bits = self.size_of(result_ty)? * 8;
            // x86 masks the count to five bits below 64-bit width; match that
            // so the two backends agree on the same program.
            let mask = if bits < 64 { 31 } else { 63 };
            if mask < 63 {
                let _ = writeln!(self.out, "    and x1, x1, #{mask}");
            }
        }
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
        self.normalize_reg(ty, 0)
    }

    fn normalize_reg(&mut self, ty: TyId, reg: u8) -> Result<(), String> {
        match self.types.kind(ty) {
            TyKind::Unit => {
                let _ = writeln!(self.out, "    mov x{reg}, #0");
            }
            TyKind::Bool => {
                let _ = writeln!(self.out, "    and x{reg}, x{reg}, #1");
            }
            TyKind::I8 => {
                let _ = writeln!(self.out, "    sxtb x{reg}, w{reg}");
            }
            TyKind::I16 => {
                let _ = writeln!(self.out, "    sxth x{reg}, w{reg}");
            }
            TyKind::I32 => {
                let _ = writeln!(self.out, "    sxtw x{reg}, w{reg}");
            }
            TyKind::U8 | TyKind::Char => {
                let _ = writeln!(self.out, "    and x{reg}, x{reg}, #255");
            }
            TyKind::U16 => {
                let _ = writeln!(self.out, "    uxth w{reg}, w{reg}");
            }
            TyKind::U32 => {
                let _ = writeln!(self.out, "    mov w{reg}, w{reg}");
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
            | TyKind::F32
            | TyKind::F64
            | TyKind::Array { .. }
            | TyKind::Adt { .. }
            | TyKind::Tuple(_) => {}
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

    fn operand_ty(&mut self, operand: &Operand) -> Option<TyId> {
        match operand {
            Operand::Copy(place) | Operand::Move(place) => self.place_ty(place).ok(),
            Operand::Constant(Constant::Int { ty, .. })
            | Operand::Constant(Constant::Uint { ty, .. })
            | Operand::Constant(Constant::Float { ty, .. }) => Some(*ty),
            _ => None,
        }
    }

    fn place_ty(&mut self, place: &Place) -> Result<TyId, String> {
        let mut ty = self.body.local(place.local).ty;
        for projection in &place.projection {
            ty = match projection {
                Projection::Deref => self.pointee(ty)?,
                Projection::Index(_) => self.element(ty)?,
                Projection::Field(index) => self.field_access(ty, *index)?.1,
                Projection::EnumField { variant, index } => {
                    self.enum_field_access(ty, *variant, *index)?.1
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
        layout.map(|layout| layout.size).map_err(|e| {
            format!(
                "the AArch64 backend cannot lay out `{}`: {e:?}",
                self.types.display(ty)
            )
        })
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

    fn field_types(&mut self, def: DefId, args: &[TyId]) -> Result<Vec<TyId>, String> {
        let mut layouts = Layouts::new(self.types, self.hir);
        layouts
            .struct_field_tys(def, args)
            .map_err(|e| format!("the AArch64 backend cannot type struct fields: {e:?}"))
    }

    fn field_type(&mut self, def: DefId, index: u32, args: &[TyId]) -> Result<TyId, String> {
        self.field_types(def, args)?
            .get(index as usize)
            .copied()
            .ok_or_else(|| "a struct field index is out of range".to_string())
    }

    fn tuple_fields(&mut self, ty: TyId) -> Result<(Vec<u64>, Vec<TyId>), String> {
        let elems = match self.types.kind(ty) {
            TyKind::Tuple(elems) => elems.clone(),
            _ => return Err("a tuple field was requested from a non-tuple".into()),
        };
        let mut layouts = Layouts::new(self.types, self.hir);
        let mut offset = 0u64;
        let mut offsets = Vec::with_capacity(elems.len());
        for elem in &elems {
            let layout = layouts
                .layout(*elem)
                .map_err(|e| format!("the AArch64 backend cannot lay out a tuple: {e:?}"))?;
            let align = layout.align().max(1);
            offset = (offset + align - 1) & !(align - 1);
            offsets.push(offset);
            offset += layout.size;
        }
        Ok((offsets, elems))
    }

    fn field_access(&mut self, ty: TyId, index: u32) -> Result<(u64, TyId), String> {
        match self.types.kind(ty).clone() {
            TyKind::Adt { def, args } => {
                let offsets = self.field_offsets(def, &args)?;
                let offset = *offsets
                    .get(index as usize)
                    .ok_or_else(|| "a struct field index is out of range".to_string())?;
                let field_ty = self.field_type(def, index, &args)?;
                Ok((offset, field_ty))
            }
            TyKind::Tuple(_) => {
                let (offsets, elems) = self.tuple_fields(ty)?;
                let offset = *offsets
                    .get(index as usize)
                    .ok_or_else(|| "a tuple index is out of range".to_string())?;
                let field_ty = *elems
                    .get(index as usize)
                    .ok_or_else(|| "a tuple index is out of range".to_string())?;
                Ok((offset, field_ty))
            }
            _ => Err(format!(
                "the AArch64 backend cannot take field {index} of `{}`",
                self.types.display(ty)
            )),
        }
    }

    fn enum_field_access(
        &mut self,
        ty: TyId,
        variant: u32,
        index: u32,
    ) -> Result<(u64, TyId), String> {
        let (def, args) = self.enum_parts(ty)?;
        let payload = self.enum_payload_offset(def, &args)?;
        let offsets = self.variant_offsets(def, variant, &args)?;
        let offset = *offsets
            .get(index as usize)
            .ok_or_else(|| "an enum field index is out of range".to_string())?;
        let field_ty = self
            .variant_types(def, variant, &args)?
            .get(index as usize)
            .copied()
            .ok_or_else(|| "an enum field index is out of range".to_string())?;
        Ok((payload + offset, field_ty))
    }

    fn enum_parts(&self, ty: TyId) -> Result<(DefId, Vec<TyId>), String> {
        match self.types.kind(ty) {
            TyKind::Adt { def, args } => Ok((*def, args.clone())),
            _ => Err("an enum field was requested from a non-enum".into()),
        }
    }

    fn enum_variant_count(&self, def: DefId) -> Result<usize, String> {
        self.hir
            .enums
            .iter()
            .find(|e| e.def_id == def)
            .map(|e| e.variants.len())
            .ok_or_else(|| "an enum has no definition in the HIR".to_string())
    }

    fn tag_size(&self, def: DefId) -> Result<u64, String> {
        Ok(if self.enum_variant_count(def)? <= 256 {
            1
        } else {
            4
        })
    }

    fn enum_payload_offset(&mut self, def: DefId, args: &[TyId]) -> Result<u64, String> {
        let mut layouts = Layouts::new(self.types, self.hir);
        layouts
            .enum_payload_offset(def, args)
            .map_err(|e| format!("the AArch64 backend cannot lay out an enum: {e:?}"))
    }

    fn variant_offsets(
        &mut self,
        def: DefId,
        index: u32,
        args: &[TyId],
    ) -> Result<Vec<u64>, String> {
        let mut layouts = Layouts::new(self.types, self.hir);
        layouts
            .variant_field_offsets(def, index, args)
            .map_err(|e| format!("the AArch64 backend cannot lay out an enum variant: {e:?}"))
    }

    fn variant_types(
        &mut self,
        def: DefId,
        index: u32,
        args: &[TyId],
    ) -> Result<Vec<TyId>, String> {
        let mut layouts = Layouts::new(self.types, self.hir);
        layouts
            .variant_field_tys_pub(def, index, args)
            .map_err(|e| format!("the AArch64 backend cannot type an enum variant: {e:?}"))
    }

    fn local_address(&mut self, local: usize, dest: u8) -> Result<(), String> {
        if self.local_reg[local].is_some() || self.local_freg[local].is_some() {
            return Err(format!(
                "the AArch64 backend tried to address register-allocated local {local}"
            ));
        }
        let offset = self.offsets[local];
        if offset <= 4095 {
            let _ = writeln!(self.out, "    sub x{dest}, x29, #{offset}");
        } else {
            self.emit_mov_u64(12, u64::from(offset))?;
            let _ = writeln!(self.out, "    sub x{dest}, x29, x12");
        }
        Ok(())
    }

    fn sret_address(&mut self, dest: u8) -> Result<(), String> {
        let offset = self
            .sret_offset
            .ok_or_else(|| "a large aggregate return has no hidden pointer slot".to_string())?;
        if offset <= 4095 {
            let _ = writeln!(self.out, "    sub x{dest}, x29, #{offset}");
        } else {
            self.emit_mov_u64(12, u64::from(offset))?;
            let _ = writeln!(self.out, "    sub x{dest}, x29, x12");
        }
        Ok(())
    }

    fn cmp_immediate(&mut self, register: u8, value: u64) -> Result<(), String> {
        if value <= 4095 {
            let _ = writeln!(self.out, "    cmp x{register}, #{value}");
        } else {
            self.emit_mov_u64(14, value)?;
            let _ = writeln!(self.out, "    cmp x{register}, x14");
        }
        Ok(())
    }

    /// Adds a compile-time offset to the address in `x11`.
    fn add_to_address(&mut self, offset: u64) -> Result<(), String> {
        if offset <= 4095 {
            let _ = writeln!(self.out, "    add x11, x11, #{offset}");
        } else {
            self.emit_mov_u64(12, offset)?;
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

    fn label(&mut self) -> String {
        let label = format!("{}_{}", self.tag, self.next_label);
        self.next_label += 1;
        label
    }
}

/// Assigns up to ten scalar locals to callee-saved `x19`-`x28`.
///
/// This is not a full allocator: a local is promoted only when its address is
/// never taken and no projected place names it. That covers counters, indices
/// and scalar temporaries, which are the values that otherwise pay two stack
/// accesses each. Aggregates, floats and address-taken values stay in memory.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum RegClass {
    Int,
    Float,
}

fn class_of(kind: &TyKind) -> Option<RegClass> {
    if is_promotable_int(kind) {
        Some(RegClass::Int)
    } else if is_promotable_float(kind) {
        Some(RegClass::Float)
    } else {
        None
    }
}

/// Assigns scalar locals to registers using liveness and graph colouring.
///
/// Every MIR local is a candidate unless its address is taken or a projected
/// place names it. Liveness is computed over the CFG, an interference graph is
/// built from definitions and live sets, and each class is coloured with its
/// callee-saved registers: `x19`-`x28` for integers and pointers, `d8`-`d15`
/// for floats. Locals that do not fit keep their stack slot, so spilling needs
/// no extra machinery.
fn allocate_locals(
    body: &Body,
    types: &TyCtxt,
) -> (Vec<Option<u8>>, Vec<Option<u8>>) {
    let count = body.locals.len();
    let mut address_taken = vec![false; count];
    let mut projected = vec![false; count];
    for block in &body.blocks {
        for statement in &block.statements {
            if let Statement::Assign { place, value, .. } = statement {
                scan_place(place, &mut address_taken, &mut projected);
                scan_rvalue(value, &mut address_taken, &mut projected);
            }
        }
        scan_terminator(&block.terminator, &mut address_taken, &mut projected);
    }

    let mut class: Vec<Option<RegClass>> = vec![None; count];
    for (index, local) in body.locals.iter().enumerate() {
        if address_taken[index] || projected[index] {
            continue;
        }
        class[index] = class_of(types.kind(local.ty));
    }
    if class.iter().all(Option::is_none) {
        return (vec![None; count], vec![None; count]);
    }

    let blocks = body.blocks.len();
    let mut block_use = vec![HashSet::new(); blocks];
    let mut block_def = vec![HashSet::new(); blocks];
    for (index, block) in body.blocks.iter().enumerate() {
        let mut uses = HashSet::new();
        let mut defs = HashSet::new();
        for statement in &block.statements {
            let (statement_uses, statement_defs) = statement_access(statement);
            uses.extend(statement_uses.difference(&defs).copied());
            defs.extend(statement_defs);
        }
        let (terminator_uses, terminator_defs) = terminator_access(body, &block.terminator);
        uses.extend(terminator_uses.difference(&defs).copied());
        defs.extend(terminator_defs);
        block_use[index] = uses;
        block_def[index] = defs;
    }

    let mut live_in = block_use.clone();
    let mut live_out = vec![HashSet::new(); blocks];
    loop {
        let mut changed = false;
        for index in (0..blocks).rev() {
            let mut out = HashSet::new();
            for successor in body.blocks[index].terminator.successors() {
                out.extend(live_in[successor.0 as usize].iter().copied());
            }
            let mut input = block_use[index].clone();
            input.extend(out.difference(&block_def[index]).copied());
            if input != live_in[index] || out != live_out[index] {
                live_in[index] = input;
                live_out[index] = out;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }

    let mut graph = vec![HashSet::new(); count];
    for (index, out) in live_out.iter().enumerate() {
        let mut live = out.clone();
        let (_, terminator_defs) = terminator_access(body, &body.blocks[index].terminator);
        interfere(&mut graph, &terminator_defs, &live, &class);
        let (terminator_uses, _) = terminator_access(body, &body.blocks[index].terminator);
        live.retain(|local| !terminator_defs.contains(local));
        live.extend(terminator_uses);
        for statement in body.blocks[index].statements.iter().rev() {
            let (uses, defs) = statement_access(statement);
            interfere(&mut graph, &defs, &live, &class);
            live.retain(|local| !defs.contains(local));
            live.extend(uses);
        }
    }
    // Parameters are all live at entry and have no defining assignment, so
    // their interference cannot be discovered from a definition.
    let entry_live: Vec<usize> = live_in[body.entry.0 as usize].iter().copied().collect();
    for (position, &a) in entry_live.iter().enumerate() {
        for &b in &entry_live[position + 1..] {
            add_edge(&mut graph, a, b, &class);
        }
    }

    let mut int_regs = Vec::new();
    let mut float_regs = Vec::new();
    let int_candidates: Vec<usize> = (0..count)
        .filter(|index| class[*index] == Some(RegClass::Int))
        .collect();
    let float_candidates: Vec<usize> = (0..count)
        .filter(|index| class[*index] == Some(RegClass::Float))
        .collect();
    colour(&int_candidates, &graph, &(19u8..=28).collect::<Vec<_>>(), &mut int_regs);
    colour(&float_candidates, &graph, &(8u8..=15).collect::<Vec<_>>(), &mut float_regs);

    let mut integer = vec![None; count];
    let mut floating = vec![None; count];
    for (local, register) in int_regs {
        integer[local] = Some(register);
    }
    for (local, register) in float_regs {
        floating[local] = Some(register);
    }
    (integer, floating)
}

fn add_edge(graph: &mut [HashSet<usize>], a: usize, b: usize, class: &[Option<RegClass>]) {
    if a == b || class[a].is_none() || class[a] != class[b] {
        return;
    }
    graph[a].insert(b);
    graph[b].insert(a);
}

fn interfere(
    graph: &mut [HashSet<usize>],
    defs: &HashSet<usize>,
    live: &HashSet<usize>,
    class: &[Option<RegClass>],
) {
    for &defined in defs {
        if class[defined].is_none() {
            continue;
        }
        for &value in live {
            add_edge(graph, defined, value, class);
        }
    }
}

/// A greedy colouring with saturation-degree ordering. A local that finds no
/// free colour stays spilled and keeps its stack slot.
fn colour(
    candidates: &[usize],
    graph: &[HashSet<usize>],
    registers: &[u8],
    assigned: &mut Vec<(usize, u8)>,
) {
    let mut remaining: Vec<usize> = candidates.to_vec();
    let mut colours: Vec<Option<u8>> = vec![None; graph.len()];
    while !remaining.is_empty() {
        // Keep the tie-break on the local index so the emitted assembly is
        // reproducible; a hash-set iteration order would not be.
        let mut best = 0;
        let mut best_key = (0usize, 0usize);
        for (position, node) in remaining.iter().enumerate() {
            let mut seen = HashSet::new();
            for neighbour in &graph[*node] {
                if let Some(colour) = colours[*neighbour] {
                    seen.insert(colour);
                }
            }
            let key = (seen.len(), graph[*node].len());
            if key > best_key {
                best = position;
                best_key = key;
            }
        }
        let node = remaining.remove(best);
        let mut taken = HashSet::new();
        for neighbour in &graph[node] {
            if let Some(colour) = colours[*neighbour] {
                taken.insert(colour);
            }
        }
        if let Some(register) = registers.iter().find(|register| !taken.contains(register)) {
            colours[node] = Some(*register);
            assigned.push((node, *register));
        }
    }
}

fn is_promotable_int(kind: &TyKind) -> bool {
    matches!(
        kind,
        TyKind::Bool
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

fn is_promotable_float(kind: &TyKind) -> bool {
    matches!(kind, TyKind::F32 | TyKind::F64)
}

fn statement_access(statement: &Statement) -> (HashSet<usize>, HashSet<usize>) {
    let mut uses = HashSet::new();
    let mut defs = HashSet::new();
    match statement {
        Statement::Assign { place, value, .. } => {
            if place.projection.is_empty() {
                defs.insert(place.local.0 as usize);
            } else {
                place_address_use(place, &mut uses);
                defs.insert(place.local.0 as usize);
            }
            rvalue_use(value, &mut uses);
        }
        Statement::StorageLive(_)
        | Statement::StorageDead(_)
        | Statement::SetDropFlag { .. }
        | Statement::Nop => {}
    }
    (uses, defs)
}

fn terminator_access(body: &Body, terminator: &Terminator) -> (HashSet<usize>, HashSet<usize>) {
    let mut uses = HashSet::new();
    let mut defs = HashSet::new();
    match terminator {
        Terminator::Goto(_) | Terminator::Unreachable => {}
        Terminator::SwitchInt { discr, .. } => operand_use(discr, &mut uses),
        Terminator::Call { callee, args, destination, .. } => {
            operand_use(callee, &mut uses);
            for arg in args {
                operand_use(arg, &mut uses);
            }
            if let Some(destination) = destination {
                if destination.projection.is_empty() {
                    defs.insert(destination.local.0 as usize);
                } else {
                    place_address_use(destination, &mut uses);
                    defs.insert(destination.local.0 as usize);
                }
            }
        }
        Terminator::Drop { place, .. } => place_address_use(place, &mut uses),
        Terminator::Return => {
            if let Some(local) = body.locals.iter().position(|local| local.name == "_ret") {
                uses.insert(local);
            }
        }
    }
    (uses, defs)
}

fn operand_use(operand: &Operand, uses: &mut HashSet<usize>) {
    if let Operand::Copy(place) | Operand::Move(place) = operand {
        place_address_use(place, uses);
    }
}

fn place_address_use(place: &Place, uses: &mut HashSet<usize>) {
    uses.insert(place.local.0 as usize);
    for projection in &place.projection {
        if let Projection::Index(index) = projection {
            operand_use(index, uses);
        }
    }
}

fn rvalue_use(value: &Rvalue, uses: &mut HashSet<usize>) {
    match value {
        Rvalue::Use(operand) => operand_use(operand, uses),
        Rvalue::Binary { lhs, rhs, .. } => {
            operand_use(lhs, uses);
            operand_use(rhs, uses);
        }
        Rvalue::Unary { operand, .. }
        | Rvalue::Cast { operand, .. }
        | Rvalue::PtrToInt(operand) => operand_use(operand, uses),
        Rvalue::Ref { place, .. } | Rvalue::Reborrow { place, .. } => {
            place_address_use(place, uses);
        }
        Rvalue::Aggregate { operands, .. } => {
            for operand in operands {
                operand_use(operand, uses);
            }
        }
        Rvalue::Repeat { operand, .. } => operand_use(operand, uses),
        Rvalue::Discriminant(place) | Rvalue::Len(place) => place_address_use(place, uses),
        Rvalue::PtrOffset { ptr, count } => {
            operand_use(ptr, uses);
            operand_use(count, uses);
        }
    }
}

fn scan_place(place: &Place, _address_taken: &mut [bool], projected: &mut [bool]) {
    if !place.projection.is_empty() {
        projected[place.local.0 as usize] = true;
    }
}

fn scan_operand(operand: &Operand, address_taken: &mut [bool], projected: &mut [bool]) {
    if let Operand::Copy(place) | Operand::Move(place) = operand {
        scan_place(place, address_taken, projected);
    }
}

fn scan_rvalue(value: &Rvalue, address_taken: &mut [bool], projected: &mut [bool]) {
    match value {
        Rvalue::Use(operand) => scan_operand(operand, address_taken, projected),
        Rvalue::Binary { lhs, rhs, .. } => {
            scan_operand(lhs, address_taken, projected);
            scan_operand(rhs, address_taken, projected);
        }
        Rvalue::Unary { operand, .. }
        | Rvalue::Cast { operand, .. }
        | Rvalue::PtrToInt(operand) => {
            scan_operand(operand, address_taken, projected);
        }
        Rvalue::Ref { place, .. } | Rvalue::Reborrow { place, .. } => {
            address_taken[place.local.0 as usize] = true;
            scan_place(place, address_taken, projected);
        }
        Rvalue::Aggregate { operands, .. } => {
            for operand in operands {
                scan_operand(operand, address_taken, projected);
            }
        }
        Rvalue::Repeat { operand, .. } => scan_operand(operand, address_taken, projected),
        Rvalue::Discriminant(place) | Rvalue::Len(place) => {
            scan_place(place, address_taken, projected);
        }
        Rvalue::PtrOffset { ptr, count } => {
            scan_operand(ptr, address_taken, projected);
            scan_operand(count, address_taken, projected);
        }
    }
}

fn scan_terminator(terminator: &Terminator, address_taken: &mut [bool], projected: &mut [bool]) {
    match terminator {
        Terminator::SwitchInt { discr, .. } => scan_operand(discr, address_taken, projected),
        Terminator::Call {
            callee,
            args,
            destination,
            ..
        } => {
            scan_operand(callee, address_taken, projected);
            for arg in args {
                scan_operand(arg, address_taken, projected);
            }
            if let Some(destination) = destination {
                scan_place(destination, address_taken, projected);
            }
        }
        Terminator::Drop { place, .. } => scan_place(place, address_taken, projected),
        Terminator::Goto(_) | Terminator::Return | Terminator::Unreachable => {}
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
    is_scalar(kind)
        || matches!(
            kind,
            TyKind::Array { .. } | TyKind::Adt { .. } | TyKind::Tuple(_)
        )
}

fn is_aggregate(kind: &TyKind) -> bool {
    matches!(
        kind,
        TyKind::Array { .. } | TyKind::Adt { .. } | TyKind::Tuple(_)
    )
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
            | TyKind::F32
            | TyKind::F64
            | TyKind::Ref(_)
            | TyKind::RefMut(_)
            | TyKind::RawPtr(_)
            | TyKind::RawPtrMut(_)
            | TyKind::FnPtr { .. }
            | TyKind::IntVar(_)
    )
}

fn frame_layout(
    body: &Body,
    types: &mut TyCtxt,
    hir: &Hir,
    needs_sret: bool,
    local_reg: &[Option<u8>],
    local_freg: &[Option<u8>],
) -> Result<(Vec<u32>, u32, Option<u32>), String> {
    let mut layouts = Layouts::new(types, hir);
    let mut cursor = 0u64;
    let mut offsets = Vec::with_capacity(body.locals.len());
    for (index, local) in body.locals.iter().enumerate() {
        if local_reg[index].is_some() || local_freg[index].is_some() {
            offsets.push(0);
            continue;
        }
        let layout = layouts
            .layout(local.ty)
            .map_err(|e| format!("layout failed for `{}`: {e:?}", local.name))?;
        let size = if layout.size == 0 {
            8
        } else {
            (layout.size + 7) & !7
        };
        let align = if layout.size == 0 {
            8
        } else {
            layout.align().max(8)
        };
        cursor = (cursor + align - 1) & !(align - 1);
        cursor += size;
        offsets.push(cursor as u32);
    }
    let sret_offset = if needs_sret {
        cursor += 8;
        Some(cursor as u32)
    } else {
        None
    };
    let frame = (cursor + 15) & !15;
    if frame > (1 << 20) {
        return Err("the AArch64 frame is larger than one megabyte".into());
    }
    Ok((offsets, frame as u32, sret_offset))
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
