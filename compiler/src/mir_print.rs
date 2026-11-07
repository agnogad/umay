//! MIR printing.
//!
//! The text format exists for tests and for `umay mir`, not for codegen. It is
//! deliberately complete: every local, every block, every projection appears,
//! so a test can assert on the exact shape of the lowering. The parser in
//! [`crate::mir_parse`] reads it back, which is what makes round-trip tests
//! possible.

use std::fmt::Write as _;

use crate::mir::*;

/// Prints one body in the round-trippable format.
///
/// `types` supplies the human-readable name of each type; the numeric id is
/// kept alongside it so that the output stays unambiguous and machine-readable.
pub fn print_body(body: &Body, strings: &[String], types: &mut crate::types::TyCtxt) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "fn {}(", body.name);
    let _ = writeln!(out, "    // {} locals, {} blocks, entry b{}", body.locals.len(), body.blocks.len(), body.entry.0);
    let _ = writeln!(out, ") -> ?{} {{", ty_ann(body.ret_ty, types));
    for (i, l) in body.locals.iter().enumerate() {
        let _ = writeln!(
            out,
            "    let _{}{} /* {} */ : ?{};",
            i,
            if l.mutable { " mut" } else { "" },
            l.name,
            ty_ann(l.ty, types)
        );
    }
    for (i, b) in body.blocks.iter().enumerate() {
        let _ = writeln!(out, "  b{}(x):", i);
        for s in &b.statements {
            let _ = writeln!(out, "    {}", print_stmt(s, types, strings));
        }
        let _ = writeln!(out, "    {}", print_terminator(&b.terminator, types, strings));
    }
    let _ = writeln!(out, "}}");
    out
}

/// Prints a whole program.
pub fn print_program(p: &MirProgram, types: &mut crate::types::TyCtxt) -> String {
    let mut out = String::new();
    for body in &p.bodies {
        out.push_str(&print_body(body, &p.strings, types));
        out.push('\n');
    }
    out
}

/// `i32@3` — the name, then the interned id, so output is readable and exact.
fn ty_ann(ty: crate::types::TyId, types: &mut crate::types::TyCtxt) -> String {
    // A context that does not know the id (round-trip printing of parsed MIR)
    // still prints the id, so no information is lost.
    if types.contains(ty) {
        format!("{}@{}", types.display(ty), ty.0)
    } else {
        format!("?@{}", ty.0)
    }
}

/// A string constant prints its text, escaped, plus its interned id.
fn str_ann(id: StringId, strings: &[String]) -> String {
    match strings.get(id.0 as usize) {
        Some(s) => format!("{s:?}#{}", id.0),
        None => format!("?#{}", id.0),
    }
}

fn print_stmt(s: &Statement, types: &mut crate::types::TyCtxt, strings: &[String]) -> String {
    match s {
        Statement::Assign { place, value, .. } => {
            format!("{} = {};", print_place(place),
                print_rvalue(value, types, strings)
            )
        }
        Statement::StorageLive(l) => format!("StorageLive(_{});", l.0),
        Statement::StorageDead(l) => format!("StorageDead(_{});", l.0),
        Statement::SetDropFlag { local, value } => {
            format!("SetDropFlag(_{}, {});", local.0, value)
        }
        Statement::Nop => "nop;".to_string(),
    }
}

fn print_terminator(t: &Terminator, types: &mut crate::types::TyCtxt, strings: &[String]) -> String {
    match t {
        Terminator::Goto(b) => format!("goto -> b{};", b.0),
        Terminator::SwitchInt { discr, targets, otherwise } => {
            let mut s = format!("switchInt({}) -> [", print_operand(discr, types, strings));
            for (v, b) in targets {
                let _ = write!(s, "{}: b{}, ", v, b.0);
            }
            let _ = write!(s, "otherwise: b{}];", otherwise.0);
            s
        }
        Terminator::Call { callee, args, destination, target, .. } => {
            let dest = match destination {
                Some(p) => print_place(p),
                None => "_".to_string(),
            };
            let args: Vec<String> = args.iter().map(|a| print_operand(a, types, strings)).collect();
            format!(
                "{dest} = call {}({}) -> b{};",
                print_operand(callee, types, strings),
                args.join(", "),
                target.0
            )
        }
        Terminator::Drop { place, target } => {
            format!("drop({}) -> b{};", print_place(place), target.0)
        }
        Terminator::Return => "return;".to_string(),
        Terminator::Unreachable => "unreachable;".to_string(),
    }
}

fn print_place(p: &Place) -> String {
    let mut s = format!("_{}", p.local.0);
    for proj in &p.projection {
        match proj {
            Projection::Field(i) => {
                let _ = write!(s, ".{}", i);
            }
            Projection::Index(op) => {
                let _ = write!(s, "[{}]", print_operand(op, &mut crate::types::TyCtxt::new(), &[]));
            }
            Projection::Deref => s.push_str(".(deref)"),
        }
    }
    s
}

fn print_operand(op: &Operand, types: &mut crate::types::TyCtxt, strings: &[String]) -> String {
    match op {
        Operand::Copy(p) => format!("copy {}", print_place(p)),
        Operand::Move(p) => format!("move {}", print_place(p)),
        Operand::Constant(c) => print_constant(c, types, strings),
    }
}

fn print_constant(c: &Constant, types: &mut crate::types::TyCtxt, strings: &[String]) -> String {
    match c {
        Constant::Int { value, ty } => format!("const {}i{}", value, ty_ann(*ty, types)),
        Constant::Uint { value, ty } => format!("const {}u{}", value, ty_ann(*ty, types)),
        Constant::Float { value, ty } => format!("const {:?}f{}", value, ty_ann(*ty, types)),
        Constant::Bool(b) => format!("const {}", b),
        Constant::Char(ch) => format!("const {:?}", ch),
        Constant::Str(id) => format!("const {}", str_ann(*id, strings)),
        Constant::Unit => "const ()".to_string(),
        Constant::Fn(f) => format!("const fn{}", f.0),
        Constant::Const(c) => format!("const c{}", c.0),
        Constant::UnitVariant { def, index } => format!("const variant(def{}, {})", def.0, index),
    }
}

fn print_rvalue(r: &Rvalue, types: &mut crate::types::TyCtxt, strings: &[String]) -> String {
    match r {
        Rvalue::Use(op) => print_operand(op, types, strings),
        Rvalue::Binary { op, lhs, rhs } => {
            format!("{} {} {}", print_operand(lhs, types, strings), print_binop(*op), print_operand(rhs, types, strings))
        }
        Rvalue::Unary { op, operand } => format!("{}{}", print_unop(*op), print_operand(operand, types, strings)),
        Rvalue::Ref { mutable, place } => {
            format!("&{}{}", if *mutable { "mut " } else { "" }, print_place(place))
        }
        Rvalue::Reborrow { mutable, place } => {
            format!("&{}{} (reborrow)", if *mutable { "mut " } else { "" }, print_place(place))
        }
        Rvalue::Cast { operand, to } => format!("{} as {}", print_operand(operand, types, strings), ty_ann(*to, types)),
        Rvalue::Aggregate { kind, operands } => {
            let ops: Vec<String> = operands.iter().map(|o| print_operand(o, types, strings)).collect();
            match kind {
                AggregateKind::Tuple => format!("({})", ops.join(", ")),
                AggregateKind::Array => format!("[{}]", ops.join(", ")),
                AggregateKind::Struct(def) => format!("struct{}{{{}}}", def.0, ops.join(", ")),
                AggregateKind::Variant { def, index } => {
                    format!("variant(def{}, {}){{{}}}", def.0, index, ops.join(", "))
                }
            }
        }
        Rvalue::Repeat { operand, count } => format!("[{}; {}]", print_operand(operand, types, strings), count),
        Rvalue::Discriminant(p) => format!("discriminant({})", print_place(p)),
        Rvalue::Len(p) => format!("len({})", print_place(p)),
        Rvalue::PtrToInt(op) => format!("{} as ptr", print_operand(op, types, strings)),
        Rvalue::PtrOffset { ptr, count } => format!(
            "offset({}, {})",
            print_operand(ptr, types, strings),
            print_operand(count, types, strings)
        ),
    }
}

fn print_binop(op: BinOp) -> &'static str {
    match op {
        BinOp::Add => "+",
        BinOp::Sub => "-",
        BinOp::Mul => "*",
        BinOp::Div => "/",
        BinOp::Rem => "%",
        BinOp::BitAnd => "&",
        BinOp::BitOr => "|",
        BinOp::BitXor => "^",
        BinOp::Shl => "<<",
        BinOp::Shr => ">>",
        BinOp::Eq => "==",
        BinOp::Ne => "!=",
        BinOp::Lt => "<",
        BinOp::Le => "<=",
        BinOp::Gt => ">",
        BinOp::Ge => ">=",
        BinOp::LogicalAnd => "&&",
        BinOp::LogicalOr => "||",
    }
}

fn print_unop(op: UnOp) -> &'static str {
    match op {
        UnOp::Neg => "-",
        UnOp::Not => "!",
    }
}
