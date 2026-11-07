//! A parser for the printed MIR format.
//!
//! The printer is the only producer and this parser the only consumer, so they
//! can agree on a grammar that is convenient for both rather than general. Its
//! purpose is round-trip testing: `parse(print(mir)) == mir` proves that the
//! printer is faithful and that no information is lost in the text form.
//!
//! The parser is deliberately strict. Anything it does not recognise is an
//! error, so a printer change that silently drops information fails loudly.

use crate::hir::{ConstId, FnId, LocalId};
use crate::mir::*;
use crate::types::TyId;

/// A parse failure, with the line that caused it.
#[derive(Debug)]
pub struct ParseError {
    pub line: usize,
    pub message: String,
}

/// Parses a program printed by [`crate::mir_print::print_program`].
pub fn parse_program(text: &str) -> Result<MirProgram, ParseError> {
    let mut program = MirProgram::default();
    let mut lines = text.lines().enumerate().peekable();
    while let Some((_, line)) = lines.peek() {
        if line.trim().is_empty() {
            lines.next();
            continue;
        }
        if line.starts_with("fn ") {
            let (body, strings) = parse_body(&mut lines)?;
            for s in strings {
                // Keep the string table in the order the body used it.
                let exists = program.strings.iter().any(|x| x == &s);
                if !exists {
                    program.intern_string(&s);
                }
            }
            program.bodies.push(body);
        } else {
            let (n, l) = lines.next().unwrap();
            return Err(ParseError { line: n + 1, message: format!("unexpected line: {l}") });
        }
    }
    Ok(program)
}

type Lines<'a> = std::iter::Peekable<std::iter::Enumerate<std::str::Lines<'a>>>;

fn parse_body(lines: &mut Lines<'_>) -> Result<(Body, Vec<String>), ParseError> {
    let (n, header) = lines.next().unwrap();
    let header = header.trim();
    let Some(rest) = header.strip_prefix("fn ") else {
        return Err(ParseError { line: n + 1, message: "expected `fn`".into() });
    };
    let name = rest.trim_end_matches('(').to_string();

    let mut locals: Vec<LocalDecl> = Vec::new();
    let mut blocks: Vec<BasicBlock> = Vec::new();
    let mut ret_ty = TyId(0);
    let mut strings: Vec<String> = Vec::new();
    let mut current: Option<usize> = None;

    for (n, line) in lines.by_ref() {
        let trimmed = line.trim();
        if trimmed.starts_with(") -> ?") {
            // `) -> ?i32@3 {`
            let body = trimmed.trim_start_matches(") -> ?");
            ret_ty = TyId(parse_ty_annotation(body.trim_end_matches(" {")).ok_or_else(|| {
                ParseError { line: n + 1, message: format!("bad return type: {body}") }
            })?);
            continue;
        }
        if trimmed.starts_with("//") {
            continue;
        }
        if trimmed == "}" {
            break;
        }
        if let Some(rest) = trimmed.strip_prefix("let _") {
            let local = parse_local_decl(rest, n + 1)?;
            locals.push(local);
            continue;
        }
        if let Some(rest) = trimmed.strip_suffix("(x):") {
            let idx: usize = rest
                .trim_start_matches('b')
                .parse()
                .map_err(|_| ParseError { line: n + 1, message: "bad block header".into() })?;
            if idx != blocks.len() {
                return Err(ParseError { line: n + 1, message: "blocks out of order".into() });
            }
            blocks.push(BasicBlock::new(Terminator::Unreachable));
            current = Some(idx);
            continue;
        }
        let Some(idx) = current else {
            return Err(ParseError { line: n + 1, message: "statement outside a block".into() });
        };
        if is_terminator(trimmed) {
            blocks[idx].terminator = parse_terminator(trimmed, &mut strings, n + 1)?;
        } else {
            let (stmt, mut found) = parse_statement(trimmed, n + 1)?;
            strings.append(&mut found);
            blocks[idx].statements.push(stmt);
        }
    }

    Ok((
        Body {
            name,
            fn_id: None,
            ret_ty,
            arg_count: 0,
            locals,
            blocks,
            entry: BlockId(0),
            span: crate::source::Span::DUMMY,
            // A parsed body has no layouts yet, so the decision is made in
            // instruction selection once the return type's size is known.
            ret_is_sret: false,
        },
        strings,
    ))
}

fn parse_local_decl(rest: &str, line: usize) -> Result<LocalDecl, ParseError> {
    // `_0 mut /* name */ : ?ty@id;` after the `let _` prefix and index.
    let (idx_str, rest) = split_index(rest);
    let _idx: u32 = idx_str
        .parse()
        .map_err(|_| ParseError { line, message: "bad local index".into() })?;
    let rest = rest.trim_start();
    let (mutable, rest) = match rest.strip_prefix("mut ") {
        Some(r) => (true, r),
        None => (false, rest),
    };
    let rest = rest.trim_start();
    let name = if let Some(r) = rest.strip_prefix("/*") {
        let end = r
            .find("*/")
            .ok_or_else(|| ParseError { line, message: "unterminated comment".into() })?;
        r[..end].trim().to_string()
    } else {
        String::new()
    };
    let ty_start = rest
        .find("?")
        .ok_or_else(|| ParseError { line, message: "missing type".into() })?;
    let ty_text = rest[ty_start + 1..].trim_end_matches(';').trim();
    let ty = TyId(
        parse_ty_annotation(ty_text)
            .ok_or_else(|| ParseError { line, message: format!("bad type `{ty_text}`") })?,
    );
    Ok(LocalDecl { name, ty, mutable, span: crate::source::Span::DUMMY, is_temp: false })
}

fn split_index(s: &str) -> (&str, &str) {
    match s.find(|c: char| !c.is_ascii_digit()) {
        Some(i) => (&s[..i], &s[i..]),
        None => (s, ""),
    }
}

/// Parses `name@id` and returns the id.
fn parse_ty_annotation(s: &str) -> Option<u32> {
    let at = s.rfind('@')?;
    s[at + 1..].trim().parse().ok()
}

fn is_terminator(line: &str) -> bool {
    line.starts_with("goto ")
        || line.starts_with("switchInt(")
        || line.starts_with("return;")
        || line.starts_with("unreachable;")
        || line.starts_with("drop(")
        || find_call_eq(line).is_some()
}

/// `_0 = call f(...)` and its `_ = call f(...)` form: finds the ` = ` before
/// `call`, ignoring any ` = ` inside a place (there is none, but be explicit).
fn find_call_eq(line: &str) -> Option<usize> {
    let pos = line.find(" = call ")?;
    Some(pos)
}

fn parse_terminator(line: &str, strings: &mut Vec<String>, n: usize) -> Result<Terminator, ParseError> {
    if let Some(rest) = line.strip_prefix("goto -> b") {
        let target: u32 = rest
            .trim_end_matches(';')
            .parse()
            .map_err(|_| ParseError { line: n, message: "bad goto target".into() })?;
        return Ok(Terminator::Goto(BlockId(target)));
    }
    if line.starts_with("return;") {
        return Ok(Terminator::Return);
    }
    if line.starts_with("unreachable;") {
        return Ok(Terminator::Unreachable);
    }
    if let Some(rest) = line.strip_prefix("switchInt(") {
        // `switchInt(<operand>) -> [1: b5, otherwise: b6];`
        let close = rest
            .find(") -> [")
            .ok_or_else(|| ParseError { line: n, message: "bad switchInt".into() })?;
        let discr = parse_operand(&rest[..close], strings, n)?;
        let inner = rest[close + 6..].trim_end_matches(';');
        let inner = inner.trim_end_matches(']');
        let mut targets = Vec::new();
        let mut otherwise = BlockId(0);
        for part in inner.split(", ") {
            let part = part.trim();
            if part.is_empty() {
                continue;
            }
            if let Some(b) = part.strip_prefix("otherwise: b") {
                otherwise = BlockId(b.parse().map_err(|_| ParseError {
                    line: n,
                    message: "bad otherwise target".into(),
                })?);
            } else if let Some((v, b)) = part.split_once(": b") {
                let value: i128 = v
                    .parse()
                    .map_err(|_| ParseError { line: n, message: format!("bad switch value `{v}`") })?;
                let block: u32 = b
                    .parse()
                    .map_err(|_| ParseError { line: n, message: "bad switch target".into() })?;
                targets.push((value, BlockId(block)));
            }
        }
        return Ok(Terminator::SwitchInt { discr, targets, otherwise });
    }
    if let Some(eq) = find_call_eq(line) {
        // `<dest> = call <callee>(<args>) -> b<n>;`, also spelled `_ = call ...;`
        // when the call returns nothing worth keeping.
        let dest_text = line[..eq].trim();
        let destination = if dest_text == "_" {
            None
        } else {
            Some(parse_place(dest_text, strings, n)?)
        };
        let after = line[eq + " = call ".len()..].trim_start_matches("call ");
        let open = after
            .find('(')
            .ok_or_else(|| ParseError { line: n, message: "call without arguments".into() })?;
        let callee = parse_operand(&after[..open], strings, n)?;
        let arrow = after
            .rfind(") -> b")
            .ok_or_else(|| ParseError { line: n, message: "call without target".into() })?;
        let args_text = &after[open + 1..arrow];
        let args = if args_text.trim().is_empty() {
            Vec::new()
        } else {
            parse_operand_list(args_text, strings, n)?
        };
        let target: u32 = after[arrow + 6..]
            .trim_end_matches(';')
            .parse()
            .map_err(|_| ParseError { line: n, message: "bad call target".into() })?;
        return Ok(Terminator::Call {
            callee,
            args,
            destination,
            target: BlockId(target),
            unwind: Vec::new(),
            span: crate::source::Span::DUMMY,
        });
    }
    if let Some(rest) = line.strip_prefix("drop(") {
        // `drop(_0) -> b4;`
        let close = rest
            .find(") -> b")
            .ok_or_else(|| ParseError { line: n, message: "bad drop".into() })?;
        let place = parse_place(&rest[..close], strings, n)?;
        let target: u32 = rest[close + 6..]
            .trim_end_matches(';')
            .parse()
            .map_err(|_| ParseError { line: n, message: "bad drop target".into() })?;
        return Ok(Terminator::Drop { place, target: BlockId(target) });
    }
    Err(ParseError { line: n, message: format!("unknown terminator `{line}`") })
}

fn parse_statement(line: &str, n: usize) -> Result<(Statement, Vec<String>), ParseError> {
    if let Some(rest) = line.strip_prefix("StorageLive(_") {
        let idx: u32 = rest
            .trim_end_matches(");")
            .parse()
            .map_err(|_| ParseError { line: n, message: "bad StorageLive".into() })?;
        return Ok((Statement::StorageLive(LocalId(idx)), Vec::new()));
    }
    if let Some(rest) = line.strip_prefix("StorageDead(_") {
        let idx: u32 = rest
            .trim_end_matches(");")
            .parse()
            .map_err(|_| ParseError { line: n, message: "bad StorageDead".into() })?;
        return Ok((Statement::StorageDead(LocalId(idx)), Vec::new()));
    }
    if let Some(rest) = line.strip_prefix("SetDropFlag(_") {
        // Printed as `SetDropFlag(_0, true);`.
        let (idx_str, after) = split_index(rest);
        let idx: u32 = idx_str
            .parse()
            .map_err(|_| ParseError { line: n, message: "bad SetDropFlag local".into() })?;
        let value = after.trim_start_matches(',').trim().starts_with("true");
        return Ok((Statement::SetDropFlag { local: LocalId(idx), value }, Vec::new()));
    }
    if line == "nop;" {
        return Ok((Statement::Nop, Vec::new()));
    }
    // `_0 = <rvalue>;`
    let eq = line
        .find(" = ")
        .ok_or_else(|| ParseError { line: n, message: format!("not a statement: `{line}`") })?;
    let place = parse_place(&line[..eq], &mut Vec::new(), n)?;
    let rhs = line[eq + 3..].trim_end_matches(';');
    // A call terminates its block; it never appears as an assignment.
    if rhs.starts_with("call ") {
        return Err(ParseError { line: n, message: "a call is a terminator, not a statement".into() });
    }
    let mut strings = Vec::new();
    let value = parse_rvalue(rhs, &mut strings, n)?;
    Ok((Statement::Assign { place, value, span: crate::source::Span::DUMMY }, strings))
}

fn parse_rvalue(s: &str, strings: &mut Vec<String>, n: usize) -> Result<Rvalue, ParseError> {
    let s = s.trim();
    // A cast is checked before anything else: its right-hand side is a type, not
    // an operand, so a binary operator inside the type annotation must never be
    // mistaken for arithmetic.
    if let Some((lhs, rhs)) = split_cast(s) {
        let operand = parse_operand(lhs.trim(), strings, n)?;
        let to = parse_ty_annotation(rhs.trim()).ok_or_else(|| ParseError {
            line: n,
            message: format!("bad cast target `{}`", rhs.trim()),
        })?;
        return Ok(Rvalue::Cast { operand, to: crate::types::TyId(to) });
    }
    // Binary operators are checked next: `copy _0 > const 1` starts like a
    // plain operand, so the operator must win.
    if let Some((op, pos)) = find_binop(s) {
        let lhs = parse_operand(s[..pos].trim(), strings, n)?;
        let op_text = binop_text(op);
        let rhs = parse_operand(s[pos + op_text.len() + 2..].trim(), strings, n)?;
        return Ok(Rvalue::Binary { op, lhs, rhs });
    }
    if let Some(rest) = s.strip_prefix("copy ") {
        return Ok(Rvalue::Use(parse_operand(&format!("copy {rest}"), strings, n)?));
    }
    if let Some(rest) = s.strip_prefix("move ") {
        return Ok(Rvalue::Use(parse_operand(&format!("move {rest}"), strings, n)?));
    }
    if let Some(rest) = s.strip_prefix("const ") {
        return Ok(Rvalue::Use(parse_operand(&format!("const {rest}"), strings, n)?));
    }
    if let Some(rest) = s.strip_prefix("offset(") {
        // `offset(copy _1, copy _6)`
        let inner = rest.strip_suffix(')').unwrap_or(rest);
        let parts = parse_operand_list(inner, strings, n)?;
        if parts.len() != 2 {
            return Err(ParseError {
                line: n,
                message: format!("`offset` takes a pointer and a count, found {}", parts.len()),
            });
        }
        return Ok(Rvalue::PtrOffset { ptr: parts[0].clone(), count: parts[1].clone() });
    }
    if let Some(rest) = s.strip_prefix("discriminant(") {
        let place = parse_place(rest.trim_end_matches(')'), strings, n)?;
        return Ok(Rvalue::Discriminant(place));
    }
    if let Some(rest) = s.strip_prefix("len(") {
        let place = parse_place(rest.trim_end_matches(')'), strings, n)?;
        return Ok(Rvalue::Len(place));
    }
    if let Some(rest) = s.strip_prefix("&mut ") {
        if let Some(inner) = rest.strip_suffix(" (reborrow)") {
            return Ok(Rvalue::Reborrow { mutable: true, place: parse_place(inner, strings, n)? });
        }
        return Ok(Rvalue::Ref { mutable: true, place: parse_place(rest, strings, n)? });
    }
    if let Some(rest) = s.strip_prefix('&') {
        if let Some(inner) = rest.strip_suffix(" (reborrow)") {
            return Ok(Rvalue::Reborrow { mutable: false, place: parse_place(inner, strings, n)? });
        }
        return Ok(Rvalue::Ref { mutable: false, place: parse_place(rest, strings, n)? });
    }
    if let Some(rest) = s.strip_prefix('(') {
        if s == "()" {
            return Ok(Rvalue::Use(Operand::Constant(Constant::Unit)));
        }
        if let Some(inner) = rest.strip_suffix(')') {
            let ops = parse_operand_list(inner, strings, n)?;
            return Ok(Rvalue::Aggregate { kind: AggregateKind::Tuple, operands: ops });
        }
    }
    if let Some(rest) = s.strip_prefix('[') {
        if let Some(inner) = rest.strip_suffix(']') {
            let ops = parse_operand_list(inner, strings, n)?;
            return Ok(Rvalue::Aggregate { kind: AggregateKind::Array, operands: ops });
        }
    }
    if let Some(rest) = s.strip_prefix("struct") {
        let (def, rest) = split_number(rest, n)?;
        let inner = rest
            .trim_start_matches('{')
            .trim_end_matches('}');
        let ops = parse_operand_list(inner, strings, n)?;
        return Ok(Rvalue::Aggregate {
            kind: AggregateKind::Struct(crate::types::DefId(def)),
            operands: ops,
        });
    }
    if let Some(rest) = s.strip_prefix("variant(def") {
        let (def, rest) = split_number(rest, n)?;
        let rest = rest.trim_start_matches(", ");
        let (index, rest) = split_number(rest, n)?;
        let inner = rest.trim_start_matches(')').trim_start_matches('{').trim_end_matches('}');
        let ops = parse_operand_list(inner, strings, n)?;
        return Ok(Rvalue::Aggregate {
            kind: AggregateKind::Variant { def: crate::types::DefId(def), index },
            operands: ops,
        });
    }
    if let Some(rest) = s.strip_prefix('-') {
        return Ok(Rvalue::Unary { op: UnOp::Neg, operand: parse_operand(rest.trim(), strings, n)? });
    }
    if let Some(rest) = s.strip_prefix('!') {
        return Ok(Rvalue::Unary { op: UnOp::Not, operand: parse_operand(rest.trim(), strings, n)? });
    }
    Err(ParseError { line: n, message: format!("unknown rvalue `{s}`") })
}

const BINOPS: &[(&str, BinOp)] = &[
    ("==", BinOp::Eq),
    ("!=", BinOp::Ne),
    ("<=", BinOp::Le),
    (">=", BinOp::Ge),
    ("<", BinOp::Lt),
    (">", BinOp::Gt),
    ("&&", BinOp::LogicalAnd),
    ("||", BinOp::LogicalOr),
    ("<<", BinOp::Shl),
    (">>", BinOp::Shr),
    ("+", BinOp::Add),
    ("-", BinOp::Sub),
    ("*", BinOp::Mul),
    ("/", BinOp::Div),
    ("%", BinOp::Rem),
    ("&", BinOp::BitAnd),
    ("|", BinOp::BitOr),
    ("^", BinOp::BitXor),
];

/// Finds the binary operator in an rvalue, preferring the longest match so that
/// Finds `<operand> as <type>@<id>` and splits it at the `as`.
///
/// A boolean operator is checked first so that a comparison is never mistaken for
/// a cast, and the split takes the *last* ` as ` so an operand that cannot
/// contain one is unambiguous either way.
fn split_cast(s: &str) -> Option<(&str, &str)> {
    let pos = s.rfind(" as ")?;
    let lhs = &s[..pos];
    let rhs = &s[pos + 4..];
    if lhs.trim().is_empty() || !rhs.contains('@') {
        return None;
    }
    Some((lhs, rhs))
}

/// `<=` is not read as `<`.
fn find_binop(s: &str) -> Option<(BinOp, usize)> {
    let mut best: Option<(BinOp, usize)> = None;
    for (text, op) in BINOPS.iter() {
        let pattern = format!(" {text} ");
        if let Some(pos) = s.find(&pattern) {
            // An operator inside a place projection cannot appear with spaces,
            // so any match with surrounding spaces is a real operator.
            if best.map_or(true, |(_, p)| pos < p) {
                best = Some((*op, pos));
            }
        }
    }
    // Prefer the longest operator at the same position.
    match best {
        None => None,
        Some((op, pos)) => {
            let mut chosen = (op, pos);
            for (text, candidate) in BINOPS.iter() {
                let pattern = format!(" {text} ");
                if let Some(p) = s.find(&pattern) {
                    if p == pos && text.len() > binop_text(chosen.0).len() {
                        chosen = (*candidate, pos);
                    }
                }
            }
            Some(chosen)
        }
    }
}

fn binop_text(op: BinOp) -> &'static str {
    BINOPS.iter().find(|(_, o)| *o == op).map(|(t, _)| *t).unwrap_or("?")
}

fn split_number(s: &str, n: usize) -> Result<(u32, &str), ParseError> {
    let end = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
    let num: u32 = s[..end]
        .parse()
        .map_err(|_| ParseError { line: n, message: "expected a number".into() })?;
    Ok((num, &s[end..]))
}

fn parse_operand_list(s: &str, strings: &mut Vec<String>, n: usize) -> Result<Vec<Operand>, ParseError> {
    let s = s.trim();
    if s.is_empty() {
        return Ok(Vec::new());
    }
    // Split on top-level commas: `[(a, b)]` would nest, but operands never
    // contain commas at depth 0 except as separators.
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut start = 0usize;
    for (i, c) in s.char_indices() {
        match c {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth -= 1,
            ',' if depth == 0 => {
                out.push(parse_operand(s[start..i].trim(), strings, n)?);
                start = i + 1;
            }
            _ => {}
        }
    }
    out.push(parse_operand(s[start..].trim(), strings, n)?);
    Ok(out)
}

fn parse_operand(s: &str, strings: &mut Vec<String>, n: usize) -> Result<Operand, ParseError> {
    // The printer writes function constants as `const fn<id>`, matching the
    // spelling used for every other constant.
    let s = s.trim();
    if let Some(rest) = s.strip_prefix("copy ") {
        return Ok(Operand::Copy(parse_place(rest, strings, n)?));
    }
    if let Some(rest) = s.strip_prefix("move ") {
        return Ok(Operand::Move(parse_place(rest, strings, n)?));
    }
    if let Some(rest) = s.strip_prefix("const ") {
        return Ok(Operand::Constant(parse_constant(rest, strings, n)?));
    }
    Err(ParseError { line: n, message: format!("unknown operand `{s}`") })
}

fn parse_constant(s: &str, strings: &mut Vec<String>, n: usize) -> Result<Constant, ParseError> {
    let s = s.trim();
    if s == "()" {
        return Ok(Constant::Unit);
    }
    if s == "true" {
        return Ok(Constant::Bool(true));
    }
    if s == "false" {
        return Ok(Constant::Bool(false));
    }
    // A character constant is printed with `{:?}`, so it arrives quoted. The
    // escapes are the same ones a string literal uses.
    if s.starts_with('\'') && s.len() >= 3 && s.ends_with('\'') {
        let body = &s[1..s.len() - 1];
        let text = unescape(body);
        let mut chars = text.chars();
        if let (Some(ch), None) = (chars.next(), chars.next()) {
            return Ok(Constant::Char(ch));
        }
        return Err(ParseError { line: n, message: format!("bad character constant `{s}`") });
    }
    if s.starts_with('"') {
        // `"text"#3`
        let end = s
            .rfind('"')
            .ok_or_else(|| ParseError { line: n, message: "unterminated string".into() })?;
        let text: String = unescape(&s[1..end]);
        strings.push(text);
        let id: u32 = s[end + 1..]
            .trim_start_matches('#')
            .parse()
            .map_err(|_| ParseError { line: n, message: "bad string id".into() })?;
        return Ok(Constant::Str(StringId(id)));
    }
    if let Some(rest) = s.strip_prefix("fn") {
        let (id, _) = split_number(rest, n)?;
        return Ok(Constant::Fn(FnId(id)));
    }
    if let Some(rest) = s.strip_prefix('c') {
        if let Ok((id, _)) = split_number(rest, n) {
            if !rest.is_empty() && !rest.starts_with("variant") {
                return Ok(Constant::Const(ConstId(id)));
            }
        }
    }
    if let Some(rest) = s.strip_prefix("variant(def") {
        let (def, rest) = split_number(rest, n)?;
        let (index, _) = split_number(rest.trim_start_matches(", "), n)?;
        return Ok(Constant::UnitVariant { def: crate::types::DefId(def), index });
    }
    // Numbers: `42i@3`, `42u{integer}@5`, `1.5f@7`. The type annotation is an
    // arbitrary token sequence; only the trailing `@id` is meaningful here.
    // The numeric part is the leading run of digits (plus `-` and `.`); the
    // suffix character follows it directly. Matching that way avoids treating
    // the `i` in `{integer}` as a suffix.
    let num_end = s
        .find(|c: char| !(c.is_ascii_digit() || c == '-' || c == '.'))
        .unwrap_or(s.len());
    for (suffix, is_float) in [("i", false), ("u", false), ("f", true)] {
        if s[num_end..].starts_with(suffix) {
            let pos = num_end;
            let value = &s[..pos];
            let rest = &s[pos + 1..];
            if is_float {
                let v: f64 = value
                    .parse()
                    .map_err(|_| ParseError { line: n, message: format!("bad float `{value}`") })?;
                return Ok(Constant::Float { value: v, ty: TyId(parse_ty_annotation(rest).unwrap_or(0)) });
            }
            let v: i128 = value
                .parse()
                .map_err(|_| ParseError { line: n, message: format!("bad integer `{value}`") })?;
            let ty = TyId(parse_ty_annotation(rest).unwrap_or(0));
            if suffix == "i" {
                return Ok(Constant::Int { value: v, ty });
            }
            return Ok(Constant::Uint { value: v as u128, ty });
        }
    }
    Err(ParseError { line: n, message: format!("unknown constant `{s}`") })
}

fn unescape(s: &str) -> String {
    let mut out = String::new();
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('\\') => out.push('\\'),
            Some('"') => out.push('"'),
            Some('0') => out.push('\0'),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

fn parse_place(s: &str, strings: &mut Vec<String>, n: usize) -> Result<Place, ParseError> {
    let s = s.trim();
    let Some(rest) = s.strip_prefix('_') else {
        return Err(ParseError { line: n, message: format!("expected a place, got `{s}`") });
    };
    let (idx, mut rest) = split_number(rest, n)?;
    let mut place = Place::local(LocalId(idx));
    loop {
        if rest.is_empty() {
            break;
        }
        if let Some(r) = rest.strip_prefix('.') {
            if let Some(r2) = r.strip_prefix("(deref)") {
                place.projection.push(Projection::Deref);
                rest = r2;
                continue;
            }
            if let Some(r3) = r.strip_prefix('v') {
                let (variant, r4) = split_number(r3, n)?;
                let r5 = r4.strip_prefix('.').unwrap_or(r4);
                let (index, r6) = split_number(r5, n)?;
                place.projection.push(Projection::EnumField { variant, index });
                rest = r6;
                continue;
            }
            let (f, r2) = split_number(r, n)?;
            place.projection.push(Projection::Field(f));
            rest = r2;
            continue;
        }
        if let Some(r) = rest.strip_prefix('[') {
            let close = r
                .find(']')
                .ok_or_else(|| ParseError { line: n, message: "unclosed index".into() })?;
            let op = parse_operand(&r[..close], strings, n)?;
            place.projection.push(Projection::Index(op));
            rest = &r[close + 1..];
            continue;
        }
        return Err(ParseError { line: n, message: format!("bad projection `{rest}`") });
    }
    Ok(place)
}
