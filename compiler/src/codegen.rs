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
            // A definition the program itself contains carries a prefix, so it can
            // never take over a C function of the same name: a plain `exit` would
            // otherwise resolve to itself inside the runtime and recurse until the
            // stack ran out. An `extern fn` has no body and keeps its exact name,
            // because that name is the one the linker has to find.
            // A name already reserved by the `umay_` prefix is the runtime's own
            // surface, which the prelude declares as `extern fn` and therefore
            // has to keep exactly.
            // An `extern fn "symbol" name(..)` says which symbol to call, so its
            // UMAY name can differ from the C one. The prelude relies on this: it
            // reaches `read` and `write` without occupying those names.
            _ if f.link_name.is_some() => f.link_name.clone().unwrap_or_else(|| f.name.clone()),
            None if f.body.is_some()
                && f.name != "main"
                && !f.name.starts_with("umay_") =>
            {
                // A name from a module is qualified (`lib::answer`). `::` is not
                // a symbol character in assembly, so the separator becomes `__`
                // -- the same one a method's symbol uses, which keeps `lib::x`
                // and a method `x` on `lib` from being confused for each other
                // only if they were ever the same item, which they cannot be.
                format!("umay_user_{}", f.name.replace("::", "__"))
            }
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
/// The result is a complete `.s` file: the caller assembles and links it. The
/// target decides how symbols are spelled and how the file is dressed, but not
/// what the instructions are.
pub fn generate(
    program: &MirProgram,
    types: &mut TyCtxt,
    hir: &crate::hir::Hir,
    target: crate::target::Target,
    is_program: bool,
) -> Result<String, Vec<CodegenError>> {
    let symbols = function_symbols(hir, program);
    let mut errors = Vec::new();
    let mut out = String::new();
    // Skip the prelude's own markers: they are bookkeeping for checking.
    let mut layouts = Layouts::new(types, hir);

    // Every symbol this file *defines*. A reference to one of these is a call
    // within the program and must be spelled exactly as its definition is; a
    // reference to anything else names a symbol from another object (the C
    // library) and must be left alone, because MinGW's CRT exports `malloc` and
    // not `_malloc`.
    // Symbols that name code outside this program: every `extern fn "symbol"`.
    // Those must be left exactly as written, because the linker has to find the
    // C library's spelling -- MinGW's CRT exports `malloc`, not `_malloc`.
    // Everything else is UMAY's own and takes the target's prefix, so a
    // definition and a reference can never disagree.
    let foreign: std::collections::HashSet<String> = hir
        .fns
        .iter()
        .filter_map(|f| f.link_name.clone())
        // `umay_*` is the runtime's own surface, declared `extern fn` so the
        // prelude can reach it, but *defined* by UMAY. It is spelled like the
        // rest of UMAY's code, not like the C library.
        .filter(|name| !name.starts_with("umay_"))
        .collect();

    // Only the functions a program can actually reach are emitted.
    //
    // The prelude is compiled with every program, and it is not small: a
    // four-line "hello" pulled in 160 function bodies, most of them the JSON
    // parser, the HTTP helpers and the hash map. Emitting them cost assembly
    // time, object size and link time for code no program would ever run, and
    // -- more importantly -- it forced every system's bindings into every
    // object, which is why a Windows program used to fail to link over a POSIX
    // `accept` it never called.
    //
    // The roots are `main` and anything whose address the linker needs: a
    // program's own entry point, and every body that optimisation kept. A
    // function reached only through a function pointer is still kept, because
    // the call graph records the address being taken as a use.
    let reachable = reachable_functions(program);

    out.push_str(".text\n");
    for body in &program.bodies {
        let keep = match body.fn_id {
            Some(id) => reachable.contains(&id),
            // A body with no function id (the runtime's own entry points) is
            // always emitted: nothing names it in the call graph, and the
            // linker does.
            None => true,
        };
        if !keep {
            continue;
        }
        match generate_one(body, &mut layouts, &symbols, target, &foreign) {
            Ok(text) => out.push_str(&text),
            Err(message) => errors.push(CodegenError { function: body.name.clone(), message }),
        }
    }

    if !errors.is_empty() {
        return Err(errors);
    }

    // Read-only data: string literals.
    //
    // The section directive is the one piece of the file that is not an
    // instruction, and it is spelled differently by the three object formats:
    // ELF and COFF both accept `.section .rodata`, Mach-O wants a segment name
    // as well. A string literal placed in the wrong section is either a link
    // failure or, worse, a writable string.
    if !program.strings.is_empty() {
        out.push_str(target.rodata_directive());
        for (index, text) in program.strings.iter().enumerate() {
            out.push_str(&format!(".Lstr_{index}:\n"));
            out.push_str(&format!("    .asciz {}\n", quote(text)));
        }
    }
    // Object-file metadata. ELF wants the stack marked non-executable and does
    // not care about anything else; COFF wants nothing; Mach-O needs the entry
    // point recorded. Emitting the wrong directive is a hard assembler error,
    // which is why this is a target decision rather than a constant.
    // The entry-point shim belongs to the program, not to the runtime: both
    // objects are linked into one executable, and a `WinMain` in each is a
    // multiple-definition error.
    if is_program || target.format == crate::target::ObjectFormat::Elf {
        out.push_str(target.object_trailer());
    }
    Ok(out)
}

/// Generates assembly for one body.
fn generate_one(
    body: &Body,
    layouts: &mut Layouts,
    symbols: &HashMap<FnId, String>,
    target: crate::target::Target,
    foreign: &std::collections::HashSet<String>,
) -> Result<String, String> {
    // Whether a large aggregate comes back through a hidden pointer depends on
    // its size, which is only known once layouts exist. Recording the decision
    // on the body keeps the frame builder and instruction selector from each
    // recomputing it, and lets the prologue agree with the callers.
    let mut body = body.clone();
    body.ret_is_sret = returns_through_pointer(&body, layouts, target);
    let body = &body;
    let frame = frame::build_frame(body, layouts).map_err(|e| format!("frame layout failed: {e:?}"))?;
    let mut machine: MachineFunction =
        isel::select(body, frame, layouts, symbols, target.abi)
            .map_err(|e| format!("instruction selection: {e}"))?;
    let allocation = regalloc::allocate(&mut machine, target.abi);
    Ok(crate::asm::print_function(
        &machine,
        allocation.spill_bytes,
        &allocation.used_callee_saved,
        target,
        foreign,
    ))
}

/// The functions a program can actually call.
///
/// Walks the call graph from `main`, following direct calls and the `fn` values a
/// program takes the address of. A function reached this way is emitted;
/// everything else is the prelude's unused surface and is dropped.
///
/// Missing a root would delete code the program runs, so the walk is generous
/// rather than clever: `main` is always a root, every `Constant::Fn(id)` anywhere
/// in the program is a root (an indirect call cannot be resolved statically, so
/// the value it might hold must survive), and every body with no function id is
/// kept because the linker names it rather than the program.
fn reachable_functions(program: &MirProgram) -> std::collections::HashSet<crate::hir::FnId> {
    use crate::mir::{Constant, Operand, Rvalue, Statement, Terminator};

    let mut seen: std::collections::HashSet<crate::hir::FnId> = std::collections::HashSet::new();
    let mut stack: Vec<crate::hir::FnId> = Vec::new();

    // The runtime object has no `main`: it is a library whose every entry point
    // is named by the linker, not by the program, so pruning it would delete the
    // functions the program calls. Only a program -- a unit with an entry point
    // -- is pruned.
    if !program.bodies.iter().any(|b| b.name == "main") {
        return program.bodies.iter().filter_map(|b| b.fn_id).collect();
    }

    let by_id: std::collections::HashMap<crate::hir::FnId, &crate::mir::Body> = program
        .bodies
        .iter()
        .filter_map(|b| b.fn_id.map(|id| (id, b)))
        .collect();

    // `main` is the entry point.
    for body in &program.bodies {
        if body.name == "main" {
            if let Some(id) = body.fn_id {
                stack.push(id);
            }
        }
    }

    // A function whose address is taken anywhere can be called through that
    // value, so it is reachable whatever the call graph says.
    // It can be named in three places: as the value of an assignment, as an
    // argument (`apply(twice, 3)`), or as the callee of a direct call.
    fn from_operand(op: &Operand, out: &mut Vec<crate::hir::FnId>) {
        if let Operand::Constant(Constant::Fn(id)) = op {
            out.push(*id);
        }
    }
    for body in &program.bodies {
        for block in &body.blocks {
            for statement in &block.statements {
                let Statement::Assign { value, .. } = statement else {
                    continue;
                };
                // A function's address can be taken bare (`const fn5`) or
                // through a cast (`const fn5 as *const u8`, which is how a
                // function becomes a callable value). Both name a function, so
                // both are roots.
                for id in function_constants(value) {
                    stack.push(id);
                }
            }
            if let Terminator::Call { args, .. } = &block.terminator {
                for arg in args {
                    from_operand(arg, &mut stack);
                }
            }
        }
    }

    fn function_constants(value: &Rvalue) -> Vec<crate::hir::FnId> {
        use crate::mir::{Constant, Operand, Rvalue};
        fn from_operand(op: &Operand, out: &mut Vec<crate::hir::FnId>) {
            if let Operand::Constant(Constant::Fn(id)) = op {
                out.push(*id);
            }
        }
        let mut out = Vec::new();
        match value {
            Rvalue::Use(op) => from_operand(op, &mut out),
            Rvalue::Cast { operand, .. } => from_operand(operand, &mut out),
            Rvalue::Binary { lhs, rhs, .. } => {
                from_operand(lhs, &mut out);
                from_operand(rhs, &mut out);
            }
            Rvalue::Unary { operand, .. } => from_operand(operand, &mut out),
            Rvalue::Aggregate { operands, .. } => {
                for op in operands {
                    from_operand(op, &mut out);
                }
            }
            _ => {}
        }
        out
    }

    while let Some(id) = stack.pop() {
        if !seen.insert(id) {
            continue;
        }
        let Some(body) = by_id.get(&id) else {
            continue;
        };
        for block in &body.blocks {
            let Terminator::Call { callee, .. } = &block.terminator else {
                continue;
            };
            // A direct call names its callee as a constant. A call through a
            // local is covered by the address-of roots above: the local got its
            // value from a `Constant::Fn`, and that function is already kept.
            if let Operand::Constant(Constant::Fn(callee_id)) = callee {
                stack.push(*callee_id);
            }
        }
    }
    seen
}

/// Whether `body` returns an aggregate through a hidden pointer.
///
/// The threshold is the ABI's. SysV returns an aggregate of up to sixteen bytes
/// -- two eightbytes -- in `rax` and `rdx`, and anything larger through a
/// caller-supplied pointer that arrives in `rdi`.
///
/// Win64 has no register return for aggregates at all: *every* aggregate,
/// however small, comes back through a pointer, and that pointer arrives in
/// `rcx` (the first argument register) rather than `rdi`. Returning a 12-byte
/// struct in `rax` on Windows would leave the caller reading a pointer as if it
/// were data.
fn returns_through_pointer(body: &Body, layouts: &mut Layouts, target: crate::target::Target) -> bool {
    if !layouts.is_aggregate(body.ret_ty) {
        return false;
    }
    let size = layouts.layout(body.ret_ty).map(|l| l.size).unwrap_or(0);
    aggregate_goes_through_pointer(target.abi, size)
}

/// Whether an aggregate of `size` bytes comes back through a hidden pointer.
///
/// System V hands back an aggregate of at most two eightbytes in `rax` and `rdx`;
/// anything larger goes through a pointer the caller supplies.
///
/// Win64 is not "everything goes through a pointer". A structure of 1, 2, 4 or 8
/// bytes comes back in `rax`, and one of 16, 32 or 64 bytes in `rax` and further
/// registers. Only sizes that are not one of those powers -- and anything past 64
/// bytes -- use the hidden pointer. Treating every aggregate as if it did is what
/// made a one-field structure crash: the caller read the field from `rax`, where
/// the callee had left the pointer it was told to write through.
///
/// The callee and the caller must reach the same answer, so both ask this.
pub(crate) fn aggregate_goes_through_pointer(abi: crate::target::Abi, size: u64) -> bool {
    match abi {
        crate::target::Abi::SysV => size > 16,
        crate::target::Abi::Win64 => !matches!(size, 1 | 2 | 4 | 8 | 16 | 32 | 64),
    }
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
