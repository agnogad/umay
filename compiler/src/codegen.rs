//! Code generation driver: MIR program to assembly text.
//!
//! This is the seam the rest of the compiler calls. It runs, per function:
//! optimisation (already done by the caller), frame layout, instruction
//! selection, register allocation, and printing.
//!
//! One decision worth stating: errors here are reported as `CodegenError` with a
//! body name and a message, not as a panic. A compiler that crashes on a program
//! it cannot yet handle is worse than one that says which construct it cannot
//! lower yet, because the user cannot tell whether their program or the compiler
//! is at fault.

use crate::frame;
use crate::isel;
use crate::layout::Layouts;
use crate::machine::MachineFunction;
use crate::mir::{Body, MirProgram};
use crate::regalloc;
use crate::hir::FnId;
use crate::types::TyCtxt;
use std::collections::HashMap;

/// A construct codegen does not support yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodegenError {
    /// The function being compiled when it failed.
    pub function: String,
    pub message: String,
}

impl std::fmt::Display for CodegenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "in `{}`: {}", self.function, self.message)
    }
}

/// Every function's assembly symbol, so a call target can be named.
///
/// This comes from the HIR rather than the MIR bodies because an `extern fn` has
/// no body: it exists only as a declaration, and a call to it must name the
/// symbol the C library provides (`puts`, `malloc`) rather than a placeholder.
fn function_symbols(hir: &crate::hir::Hir, program: &MirProgram) -> HashMap<FnId, String> {
    let mut map = HashMap::new();
    for f in &hir.fns {
        // A method's symbol carries its type's name: `read` on `File` is
        // `File__read`, so it cannot take over the C function of the same name at
        // link time, and two types may each have their own.
        let name = match &f.owner {
            Some(owner) => format!("{owner}__{}", f.name),
            None => f.name.clone(),
        };
        map.insert(f.id, name);
    }
    for body in &program.bodies {
        if let Some(id) = body.fn_id {
            map.entry(id).or_insert_with(|| body.name.clone());
        }
    }
    map
}

/// Emits assembly text for every body in `program`.
///
/// The result is a complete `.s` file: the caller assembles and links it.
pub fn generate(
    program: &MirProgram,
    types: &mut TyCtxt,
    hir: &crate::hir::Hir,
) -> Result<String, Vec<CodegenError>> {
    let symbols = function_symbols(hir, program);
    let mut errors = Vec::new();
    let mut out = String::new();
    // Skip the prelude's own markers: they are bookkeeping for checking.
    let mut layouts = Layouts::new(types, hir);

    out.push_str(".text\n");
    for body in &program.bodies {
        match generate_one(body, &mut layouts, &symbols) {
            Ok(text) => out.push_str(&text),
            Err(message) => errors.push(CodegenError { function: body.name.clone(), message }),
        }
    }

    if !errors.is_empty() {
        return Err(errors);
    }

    // Read-only data: string literals.
    if !program.strings.is_empty() {
        out.push_str("\n.section .rodata\n");
        for (index, text) in program.strings.iter().enumerate() {
            out.push_str(&format!(".Lstr_{index}:\n"));
            out.push_str(&format!("    .asciz {}\n", quote(text)));
        }
    }
    // Mark the stack as non-executable, which modern toolchains expect.
    out.push_str("\n.section .note.GNU-stack,\"\",@progbits\n");
    Ok(out)
}

/// Generates assembly for one body.
fn generate_one(
    body: &Body,
    layouts: &mut Layouts,
    symbols: &HashMap<FnId, String>,
) -> Result<String, String> {
    // Whether a large aggregate comes back through a hidden pointer depends on
    // its size, which is only known once layouts exist. Recording the decision
    // on the body keeps the frame builder and instruction selector from each
    // recomputing it, and lets the prologue agree with the callers.
    let mut body = body.clone();
    body.ret_is_sret = returns_through_pointer(&body, layouts);
    let body = &body;
    let frame = frame::build_frame(body, layouts).map_err(|e| format!("frame layout failed: {e:?}"))?;
    let mut machine: MachineFunction =
        isel::select(body, frame, layouts, symbols).map_err(|e| format!("instruction selection: {e}"))?;
    let allocation = regalloc::allocate(&mut machine);
    Ok(crate::asm::print_function(&machine, allocation.spill_bytes, &allocation.used_callee_saved))
}

/// Whether `body` returns an aggregate that does not fit in the two registers
/// the SysV ABI allows.
///
/// Sixteen bytes is the limit: two eightbytes. Anything larger is built in the
/// caller's storage, whose address arrives in `rdi`.
fn returns_through_pointer(body: &Body, layouts: &mut Layouts) -> bool {
    if !layouts.is_aggregate(body.ret_ty) {
        return false;
    }
    layouts.layout(body.ret_ty).map(|l| l.size > 16).unwrap_or(false)
}

/// Quotes a string for `.asciz`, escaping what GNU as treats specially.
fn quote(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for ch in text.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            '\0' => out.push_str("\\0"),
            // Everything else is written as UTF-8 bytes, which is what the
            // assembler stores; `as` accepts the characters directly.
            c if (c as u32) < 0x20 => out.push_str(&format!("\\{:03o}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strings_are_quoted_and_escaped() {
        assert_eq!(quote("hi"), "\"hi\"");
        assert_eq!(quote("a\"b"), "\"a\\\"b\"");
        assert_eq!(quote("a\nb"), "\"a\\nb\"");
        assert_eq!(quote("back\\slash"), "\"back\\\\slash\"");
    }
}
