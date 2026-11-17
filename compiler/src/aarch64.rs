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
    let (offsets, frame, sret_offset) = frame_layout(body, types, hir, needs_sret)?;

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
    };

    let _ = writeln!(emitter.out, ".p2align 2");
    let _ = writeln!(emitter.out, ".globl {symbol}");
    let _ = writeln!(emitter.out, ".type {symbol}, %function");
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

    let _ = writeln!(emitter.out, ".size {symbol}, .-{symbol}");
    Ok(emitter.out)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ArgKind {
    Scalar,
    /// A scalar float, passed in an FP/SIMD register.
    Float,
    /// A composite of at most 16 bytes, passed in one or two general registers.
    Small,
    /// A composite larger than 16 bytes, passed by reference.
    Large,
}

#[derive(Debug, Clone)]
struct ArgLoc {
    regs: Vec<u8>,
    freg: Option<u8>,
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
}

impl Emitter<'_> {
    fn statement(&mut self, statement: &Statement) -> Result<(), String> {
        match statement {
            Statement::Assign { place, value, .. } => {
                let place_ty = self.place_ty(place)?;
                let mut float_result = false;
                match value {
                    Rvalue::Use(operand) => {
                        if is_aggregate(self.types.kind(place_ty)) {
                            if let Operand::Constant(Constant::UnitVariant { def, index }) = operand {
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
                        self.operand(operand, 0)?;
                        self.normalize(place_ty)?;
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
                                BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge
                            );
                        } else {
                            self.operand(lhs, 0)?;
                            self.operand(rhs, 1)?;
                            self.binary(*op, lhs_ty, place_ty)?;
                            self.normalize(place_ty)?;
                        }
                    }
                    Rvalue::Unary { op, operand } => {
                        self.operand(operand, 0)?;
                        match op {
                            UnOp::Neg => {
                                if self.operand_ty(operand).map(|ty| self.is_float(ty)).unwrap_or(false) {
                                    let float64 = self
                                        .operand_ty(operand)
                                        .map(|ty| matches!(self.types.kind(ty), TyKind::F64))
                                        .unwrap_or(false);
                                    self.operand_float(operand, 0, float64)?;
                                    let prefix = if float64 { "d" } else { "s" };
                                    let _ = writeln!(self.out, "    fneg {prefix}0, {prefix}0");
                                    float_result = true;
                                } else {
                                    let _ = writeln!(self.out, "    neg x0, x0");
                                }
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
                            self.emit_mov_u64(2, size)?;
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
                    Rvalue::Aggregate { kind: AggregateKind::Variant { def, index }, operands } => {
                        self.variant_literal(place, place_ty, *def, *index, operands)?;
                    }
                    Rvalue::Aggregate { kind: AggregateKind::Tuple, operands } => {
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

    fn repeat(&mut self, place: &Place, ty: TyId, operand: &Operand, count: u64) -> Result<(), String> {
        let element = self.element(ty)?;
        let size = self.size_of(element)?;
        let aggregate = is_aggregate(self.types.kind(element));
        let label = self.label();
        if aggregate {
            self.operand_address(operand)?;
            let _ = writeln!(self.out, "    mov x13, x11");
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
            self.store_at(element, 0)?;
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
        } else {
            self.operand(operand, 0)?;
            let _ = writeln!(self.out, "    mov x11, x{base}");
            self.add_to_address(offset)?;
            self.store_at(ty, 0)?;
        }
        Ok(())
    }

    fn tuple_literal(&mut self, place: &Place, ty: TyId, operands: &[Operand]) -> Result<(), String> {
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
            return Err("an enum variant literal and its definition disagree on field count".into());
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

    fn copy_aggregate(&mut self, destination: &Place, source: &Place, size: u64) -> Result<(), String> {
        self.address_of(source)?;
        let _ = writeln!(self.out, "    mov x13, x11");
        self.address_of(destination)?;
        let label = self.label();
        let _ = writeln!(self.out, "    mov x15, #0");
        let _ = writeln!(self.out, ".L{label}:");
        self.cmp_immediate(15, size)?;
        let _ = writeln!(self.out, "    b.hs .L{label}_done");
        let _ = writeln!(self.out, "    ldrb w0, [x13, x15]");
        let _ = writeln!(self.out, "    strb w0, [x11, x15]");
        let _ = writeln!(self.out, "    add x15, x15, #1");
        let _ = writeln!(self.out, "    b .L{label}");
        let _ = writeln!(self.out, ".L{label}_done:");
        Ok(())
    }

    /// Copies `size` bytes from `[x13]` to `[x11]`.
    fn copy_bytes(&mut self, source_register: u8, size: u64) -> Result<(), String> {
        let label = self.label();
        if source_register != 13 {
            let _ = writeln!(self.out, "    mov x13, x{source_register}");
        }
        let _ = writeln!(self.out, "    mov x15, #0");
        let _ = writeln!(self.out, ".L{label}:");
        self.cmp_immediate(15, size)?;
        let _ = writeln!(self.out, "    b.hs .L{label}_done");
        let _ = writeln!(self.out, "    ldrb w12, [x13, x15]");
        let _ = writeln!(self.out, "    strb w12, [x11, x15]");
        let _ = writeln!(self.out, "    add x15, x15, #1");
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
                    } else if self.is_float(ty) {
                        let float64 = matches!(self.types.kind(ty), TyKind::F64);
                        self.load_float(&place, 0, float64)?;
                    } else {
                        self.load(&place, 0)?;
                    }
                } else {
                    let _ = writeln!(self.out, "    mov x0, #0");
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
            if location.regs.is_empty() && location.freg.is_none() {
                continue;
            }
            match location.kind {
                ArgKind::Scalar => {
                    self.local_address(index, 11)?;
                    let _ = writeln!(self.out, "    str x{}, [x11]", location.regs[0]);
                }
                ArgKind::Small => {
                    self.local_address(index, 11)?;
                    let _ = writeln!(self.out, "    str x{}, [x11]", location.regs[0]);
                    if location.size > 8 {
                        let _ = writeln!(
                            self.out,
                            "    str x{}, [x11, #8]",
                            location.regs[1]
                        );
                    }
                }
                ArgKind::Large => {
                    self.copy_from_pointer(location.regs[0], index, location.size)?;
                }
                ArgKind::Float => {
                    self.local_address(index, 11)?;
                    let reg = location.freg.unwrap();
                    let kind = if location.float64 { "d" } else { "s" };
                    let _ = writeln!(self.out, "    str {kind}{reg}, [x11]");
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
                ArgKind::Scalar | ArgKind::Small => {
                    self.local_address(index, 11)?;
                    let _ = writeln!(self.out, "    ldr x12, [x29, #{from}]");
                    let _ = writeln!(self.out, "    str x12, [x11]");
                    if location.kind == ArgKind::Small && location.size > 8 {
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
                    self.local_address(index, 11)?;
                    let _ = writeln!(self.out, "    ldr {kind}12, [x29, #{from}]");
                    let _ = writeln!(self.out, "    str {kind}12, [x11]");
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
        let label = self.label();
        self.local_address(local, 11)?;
        let _ = writeln!(self.out, "    mov x13, x{pointer_register}");
        let _ = writeln!(self.out, "    mov x15, #0");
        let _ = writeln!(self.out, ".L{label}:");
        self.cmp_immediate(15, size)?;
        let _ = writeln!(self.out, "    b.hs .L{label}_done");
        let _ = writeln!(self.out, "    ldrb w12, [x13, x15]");
        let _ = writeln!(self.out, "    strb w12, [x11, x15]");
        let _ = writeln!(self.out, "    add x15, x15, #1");
        let _ = writeln!(self.out, "    b .L{label}");
        let _ = writeln!(self.out, ".L{label}_done:");
        Ok(())
    }

    fn emit_call(
        &mut self,
        callee: &Operand,
        args: &[Operand],
        destination: Option<&Place>,
    ) -> Result<(), String> {
        let types: Vec<Option<TyId>> = args.iter().map(|arg| self.call_arg_ty(arg)).collect::<Result<_, _>>()?;
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
                    self.operand_float(arg, 12, location.float64)?;
                    let kind = if location.float64 { "d" } else { "s" };
                    let _ = writeln!(self.out, "    str {kind}12, [sp, #{stack}]");
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
                    Operand::Copy(Place { local: place.local, projection })
                }
                other => other.clone(),
            };
            self.operand(&indirect, 9)?;
        }
        // Register arguments last, so nothing clobbers them.
        for (arg, location) in args.iter().zip(&locations) {
            if location.regs.is_empty() && location.freg.is_none() {
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
                        let _ = writeln!(
                            self.out,
                            "    ldr x{}, [x11, #8]",
                            location.regs[1]
                        );
                    }
                }
                ArgKind::Large => {
                    self.operand_address(arg)?;
                    let _ = writeln!(self.out, "    mov x{}, x11", location.regs[0]);
                }
                ArgKind::Float => {
                    self.operand_float(arg, location.freg.unwrap(), location.float64)?;
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
            if is_aggregate(self.types.kind(ty)) {
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
            let (kind, size, float64) = self.arg_kind(*ty)?;
            if kind == ArgKind::Float {
                if !fpr_stack && fregs < 8 {
                    locations.push(ArgLoc {
                        regs: Vec::new(),
                        freg: Some(fregs),
                        stack: None,
                        kind,
                        size,
                        float64,
                    });
                    fregs += 1;
                } else {
                    fpr_stack = true;
                    locations.push(ArgLoc {
                        regs: Vec::new(),
                        freg: None,
                        stack: Some(stack),
                        kind,
                        size,
                        float64,
                    });
                    stack += 8;
                }
                continue;
            }
            let needed = match kind {
                ArgKind::Scalar => 1,
                ArgKind::Small => size.div_ceil(8) as u8,
                ArgKind::Large => 1,
                ArgKind::Float => unreachable!(),
            };
            // AAPCS64 keeps a two-register composite on an even register.
            if kind == ArgKind::Small && needed == 2 && regs % 2 == 1 {
                regs += 1;
            }
            if !gpr_stack && regs + needed <= 8 {
                let assigned = (regs..regs + needed).collect();
                regs += needed;
                locations.push(ArgLoc {
                    regs: assigned,
                    freg: None,
                    stack: None,
                    kind,
                    size,
                    float64,
                });
            } else {
                gpr_stack = true;
                let slot = match kind {
                    ArgKind::Scalar | ArgKind::Large | ArgKind::Float => 8,
                    ArgKind::Small => (size + 7) & !7,
                };
                locations.push(ArgLoc {
                    regs: Vec::new(),
                    freg: None,
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

    fn arg_kind(&mut self, ty: Option<TyId>) -> Result<(ArgKind, u64, bool), String> {
        let Some(ty) = ty else {
            return Ok((ArgKind::Scalar, 8, false));
        };
        match self.types.kind(ty) {
            TyKind::F32 => Ok((ArgKind::Float, 4, false)),
            TyKind::F64 => Ok((ArgKind::Float, 8, true)),
            _ if is_aggregate(self.types.kind(ty)) => {
                let size = self.size_of(ty)?;
                if size > 16 {
                    Ok((ArgKind::Large, size, false))
                } else {
                    Ok((ArgKind::Small, size, false))
                }
            }
            _ => Ok((ArgKind::Scalar, 8, false)),
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
            _ => Err("a floating-point argument must be a float value".into()),
        }
    }

    fn load_float(&mut self, place: &Place, freg: u8, float64: bool) -> Result<(), String> {
        self.address_of(place)?;
        let kind = if float64 { "d" } else { "s" };
        let _ = writeln!(self.out, "    ldr {kind}{freg}, [x11]");
        Ok(())
    }

    fn store_float_result(&mut self, destination: &Place, float64: bool) -> Result<(), String> {
        self.address_of(destination)?;
        let kind = if float64 { "d" } else { "s" };
        let _ = writeln!(self.out, "    str {kind}0, [x11]");
        Ok(())
    }

    fn emit_mov_u64(&mut self, register: u8, value: u64) -> Result<(), String> {
        let _ = writeln!(self.out, "    movz x{register}, #{}", value & 0xffff);
        for (shift, chunk) in [(16, (value >> 16) & 0xffff), (32, (value >> 32) & 0xffff), (48, (value >> 48) & 0xffff)] {
            if chunk != 0 {
                let _ = writeln!(self.out, "    movk x{register}, #{chunk}, lsl #{shift}");
            }
        }
        Ok(())
    }

    fn float_binary(&mut self, op: BinOp, lhs: &Operand, rhs: &Operand, lhs_ty: TyId) -> Result<(), String> {
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
                return Err("the AArch64 backend supports only arithmetic and comparison on floats".into());
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
                let _ = writeln!(self.out, "    adrp x{reg}, .Lstr_{}", id.0);
                let _ = writeln!(self.out, "    add x{reg}, x{reg}, :lo12:.Lstr_{}", id.0);
            }
            Constant::Fn(id) => {
                let name = self
                    .symbols
                    .get(id)
                    .map(|name| mangle(name, self.foreign, self.target))
                    .ok_or_else(|| "a function pointer names no symbol".to_string())?;
                let _ = writeln!(self.out, "    adrp x{reg}, {name}");
                let _ = writeln!(self.out, "    add x{reg}, x{reg}, :lo12:{name}");
            }
            Constant::NamedFn(name) => {
                let _ = writeln!(self.out, "    adrp x{reg}, {name}");
                let _ = writeln!(self.out, "    add x{reg}, x{reg}, :lo12:{name}");
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
                        let _ = writeln!(self.out, "    add x11, x11, x10, lsl #{}", size.trailing_zeros());
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
            TyKind::I32 | TyKind::U32 | TyKind::Char | TyKind::F32 => {
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

    fn enum_field_access(&mut self, ty: TyId, variant: u32, index: u32) -> Result<(u64, TyId), String> {
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
        Ok(if self.enum_variant_count(def)? <= 256 { 1 } else { 4 })
    }

    fn enum_payload_offset(&mut self, def: DefId, args: &[TyId]) -> Result<u64, String> {
        let mut layouts = Layouts::new(self.types, self.hir);
        layouts
            .enum_payload_offset(def, args)
            .map_err(|e| format!("the AArch64 backend cannot lay out an enum: {e:?}"))
    }

    fn variant_offsets(&mut self, def: DefId, index: u32, args: &[TyId]) -> Result<Vec<u64>, String> {
        let mut layouts = Layouts::new(self.types, self.hir);
        layouts
            .variant_field_offsets(def, index, args)
            .map_err(|e| format!("the AArch64 backend cannot lay out an enum variant: {e:?}"))
    }

    fn variant_types(&mut self, def: DefId, index: u32, args: &[TyId]) -> Result<Vec<TyId>, String> {
        let mut layouts = Layouts::new(self.types, self.hir);
        layouts
            .variant_field_tys_pub(def, index, args)
            .map_err(|e| format!("the AArch64 backend cannot type an enum variant: {e:?}"))
    }

    fn local_address(&mut self, local: usize, dest: u8) -> Result<(), String> {
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
        || matches!(kind, TyKind::Array { .. } | TyKind::Adt { .. } | TyKind::Tuple(_))
}

fn is_aggregate(kind: &TyKind) -> bool {
    matches!(kind, TyKind::Array { .. } | TyKind::Adt { .. } | TyKind::Tuple(_))
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
) -> Result<(Vec<u32>, u32, Option<u32>), String> {
    let mut layouts = Layouts::new(types, hir);
    let mut cursor = 0u64;
    let mut offsets = Vec::with_capacity(body.locals.len());
    for local in &body.locals {
        let layout = layouts
            .layout(local.ty)
            .map_err(|e| format!("layout failed for `{}`: {e:?}", local.name))?;
        let size = if layout.size == 0 { 8 } else { (layout.size + 7) & !7 };
        let align = if layout.size == 0 { 8 } else { layout.align().max(8) };
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
