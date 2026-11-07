//! The `umay explain EXXXX` database.
//!
//! Every diagnostic code the compiler can emit is documented here: a one-line
//! summary (used as the `cause:` line) and a longer explanation (used by
//! `umay explain` and by the `why:` section of verbose renders).
//!
//! Keeping this in one table guarantees that no code can be emitted without an
//! explanation, which is a hard requirement of the AI-first diagnostics design.

pub struct ExplainEntry {
    pub code: &'static str,
    /// One-line root cause.
    pub summary: &'static str,
    /// Multi-paragraph explanation.
    pub explanation: &'static str,
    /// Concrete example, optionally showing the fix.
    pub example: Option<&'static str>,
}

const fn e(
    code: &'static str,
    summary: &'static str,
    explanation: &'static str,
    example: Option<&'static str>,
) -> ExplainEntry {
    ExplainEntry { code, summary, explanation, example }
}

pub static EXPLAIN: &[ExplainEntry] = &[
    // ---- lexical errors ---------------------------------------------------
    e(
        "E0001",
        "the character is not valid UMAY syntax",
        "UMAY source is UTF-8 and allows non-ASCII characters inside identifiers, string literals \
         and comments. Outside those positions only the documented punctuation set is permitted. \
         If you meant to write a string, wrap the text in double quotes.",
        Some("let price = \"10$\"   // not: let price = 10$"),
    ),
    e(
        "E0002",
        "a string literal was never closed",
        "String literals start and end with a double quote and may span several lines. A missing \
         closing quote swallows the rest of the file, so the lexer reports the position where the \
         literal started.",
        Some("print(\"hello\")"),
    ),
    e(
        "E0003",
        "an interpolation expression was never closed",
        "Inside a string, `{` starts an interpolation and must be closed by `}`. To print a literal \
         brace, write `{{` or `}}`.",
        Some("print(\"value: {x}\")   // literal brace: \"{{x}}\""),
    ),
    e(
        "E0004",
        "a block comment was never closed",
        "Block comments `/* ... */` nest, so every `/*` needs a matching `*/`.",
        Some("/* outer /* inner */ still a comment */"),
    ),
    e(
        "E0005",
        "a character literal is malformed",
        "Character literals hold exactly one character: `'a'`, `'\\n'`, `'\\u{1F600}'`. To build a \
         string, use double quotes.",
        Some("let c = 'a'"),
    ),
    e(
        "E0006",
        "a numeric literal is malformed or out of range",
        "Integer literals may be decimal, `0x` hexadecimal, `0b` binary or `0o` octal, and may \
         contain `_` separators. Literals larger than 128 bits cannot be represented.",
        Some("let mask = 0xFF_FF"),
    ),
    e(
        "E0007",
        "an escape sequence is not valid",
        "Supported escapes are `\\n`, `\\t`, `\\r`, `\\0`, `\\\\`, `\\\"`, `\\'`, `\\{`, `\\}`, \
         `\\xNN` and `\\u{...}`.",
        Some("let s = \"tab\\there\""),
    ),
    // ---- structural errors ------------------------------------------------
    e(
        "E0052",
        "nesting is too deep",
        "UMAY bounds expression and type nesting depth so that compilation time and stack usage \
         stay predictable for every input, including machine-generated code.",
        None,
    ),
    // ---- item-level errors ------------------------------------------------
    e(
        "E0100",
        "expected an item declaration",
        "Top-level source is a sequence of declarations: `fn`, `struct`, `enum`, `trait`, `impl`, \
         `const`, `use`, `mod` and `type`. Statements are only allowed inside a block.",
        Some("fn main() {\n    print(\"hi\")\n}"),
    ),
    e(
        "E0101",
        "unknown name",
        "Name resolution failed: no function, variable, type or module with this name is in scope at \
         this position. Check spelling, imports (`use ...`) and the scope the name was declared in. \
         UMAY has no implicit globals.",
        Some("use std::io\n\nfn main() {\n    io::print(\"hi\")\n}"),
    ),
    e(
        "E0110",
        "expected a function name",
        "A function declaration is `fn name(params) -> Type { ... }`.",
        Some("fn add(a: i32, b: i32) -> i32 {\n    return a + b\n}"),
    ),
    e("E0111", "malformed parameter list", "Parameters are comma-separated `name: Type` pairs inside parentheses.", Some("fn greet(name: str, times: u32) { }")),
    e("E0112", "expected a parameter name", "Each parameter needs a name followed by `:` and its type.", Some("fn f(count: u32) { }")),
    e(
        "E0113",
        "missing parameter type annotation",
        "Function parameter types are mandatory. Inferring them across function boundaries would \
         require whole-program analysis and make separate compilation impossible.",
        Some("fn f(count: u32) { }"),
    ),
    e("E0114", "expected a generic parameter name", "Generic parameters are identifiers inside `<...>`.", None),
    e("E0115", "malformed generic parameter list", "The list opened with `<` must be closed with `>`.", None),
    e("E0120", "expected `[` after `#`", "Attributes are written `#[name]` or `#[name(arg, ...)]`.", Some("#[inline]\nfn f() { }")),
    e("E0121", "expected an attribute name", "The attribute name follows `#[` directly and is an identifier.", Some("#[inline]")),
    e("E0122", "unexpected token in attribute arguments", "Attribute arguments are plain identifiers, integers or string literals.", Some("#[link(\"m\")]")),
    e("E0123", "malformed attribute arguments", "Attribute argument lists must be closed with `)`.", None),
    e("E0124", "malformed attribute", "Attributes end with `]`.", None),
    e(
        "E0130",
        "expected a struct name",
        "A struct is declared as `struct Name { field: Type }`.",
        Some("struct User {\n    id: u64,\n    name: String\n}"),
    ),
    e("E0131", "unexpected end of file in a struct body", "The struct body was opened with `{` but never closed.", None),
    e("E0132", "malformed struct body", "Struct fields are comma separated `name: Type` pairs inside braces.", Some("struct Point {\n    x: i32,\n    y: i32\n}")),
    e("E0133", "expected a field name", "Each field needs a name before its `:` type annotation.", None),
    e("E0134", "missing field type annotation", "Struct fields must declare their type explicitly; field types are never inferred.", Some("struct P { x: i32 }")),
    e(
        "E0140",
        "expected an enum name",
        "An enum is declared as `enum Name { A, B(T), C { x: T } }`.",
        Some("enum Status {\n    Pending,\n    Active,\n    Disabled\n}"),
    ),
    e("E0141", "unexpected end of file in an enum body", "The enum body was opened with `{` but never closed.", None),
    e("E0142", "malformed enum body", "Enum variants are comma separated.", None),
    e("E0143", "expected an enum variant name", "Variant names are identifiers and start with an uppercase letter by convention.", None),
    e("E0144", "malformed variant tuple fields", "Tuple variants list their field types inside parentheses: `Some(T)`.", Some("enum Option<T> {\n    None,\n    Some(T)\n}")),
    e("E0145", "malformed variant struct fields", "Struct variants declare named fields inside braces.", Some("enum Shape {\n    Rect { w: i32, h: i32 }\n}")),
    e("E0150", "expected a trait name", "A trait is declared as `trait Name { fn method(&self) -> T }`.", None),
    e("E0151", "unexpected end of file in a trait body", "The trait body was opened with `{` but never closed.", None),
    e("E0152", "expected `fn` in a trait body", "Traits contain only function signatures (and, later, associated types and constants).", None),
    e("E0153", "malformed trait body", "The trait body must be a brace-delimited list of function signatures closed with `}`.", None),
    e("E0160", "expected a type after `impl`", "An impl block is `impl Type { ... }` or `impl Trait for Type { ... }`.", None),
    e("E0161", "unexpected end of file in an impl body", "The impl body was opened with `{` but never closed.", None),
    e("E0162", "expected `fn` in an impl body", "Impl blocks contain function definitions.", None),
    e("E0163", "malformed impl body", "The impl body must be a brace-delimited list of functions closed with `}`.", None),
    e("E0170", "expected a constant name", "A constant is declared as `const NAME: Type = value`.", Some("const MAX: u32 = 1024")),
    e("E0171", "constant is missing its initializer", "Constants are evaluated at compile time and therefore must have a value.", Some("const MAX: u32 = 1024")),
    e("E0180", "expected a path after `use`", "Imports name a module path, e.g. `use std::io`.", None),
    e("E0181", "expected a name in a `use` list", "Grouped imports list names separated by commas: `use std::{io, fs}`.", None),
    e("E0182", "malformed `use` list", "A grouped import must be closed with `}`.", None),
    e("E0183", "expected an alias after `as`", "Aliases rename an import: `use std::io as input`.", None),
    e("E0190", "expected a module name", "Modules are declared `mod name;` (file module) or `mod name { ... }` (inline).", None),
    e("E0191", "malformed module body", "An inline module body must be closed with `}`.", None),
    e("E0195", "expected a type alias name", "Type aliases are `type Name = Type`.", None),
    e("E0196", "type alias is missing `=`", "Type aliases are `type Name = Type`.", None),
    // ---- type syntax errors ----------------------------------------------
    e("E0220", "malformed array type", "Array types are `[T; N]` where `N` is a constant length.", Some("let buf: [u8; 64]")),
    e("E0221", "malformed slice type", "Slice types are `[T]` and almost always appear behind a reference: `&[T]`.", None),
    e("E0222", "malformed tuple type", "Tuple types are comma separated type lists: `(i32, bool)`.", None),
    e("E0223", "malformed function type parameters", "Function types are `fn(A, B) -> C`.", None),
    e("E0224", "expected a type", "A type expression was required here but a different token was found.", None),
    e("E0225", "expected a type name", "Type paths begin with an identifier, `Self` or `::`.", None),
    e("E0226", "malformed generic arguments", "Generic arguments are comma separated types inside angle brackets: `Vec<i32>`.", None),
    e("E0230", "malformed generic arguments in a path", "Generic arguments must be closed with `>`.", None),
    // ---- statement/expression syntax errors ------------------------------
    e("E0300", "expected `{`", "Blocks, function bodies and `match` bodies are opened with `{`.", None),
    e("E0301", "unexpected end of file inside a block", "A block opened with `{` must be closed with `}`.", None),
    e("E0302", "malformed block", "A block must be closed with `}`.", None),
    e(
        "E0303",
        "`let` binding has no initializer",
        "Every `let` initializes its binding immediately. Definite initialization checking relies on \
         this invariant: a binding is either fully initialized at its declaration or it was never \
         declared.",
        Some("let mut count = 0\ncount = 1"),
    ),
    e("E0310", "malformed call arguments", "Arguments are comma separated expressions inside parentheses.", None),
    e("E0311", "malformed index expression", "Indexing is `base[index]` and must be closed with `]`.", None),
    e("E0312", "malformed method arguments", "Method arguments are comma separated expressions inside parentheses.", None),
    e("E0313", "expected a field or method name", "After `.` UMAY expects an identifier: `value.field` or `value.method()`.", None),
    e("E0320", "`for` loop is missing `in`", "Iteration is written `for pattern in iterator { ... }`.", Some("for item in list {\n    print(\"{item}\")\n}")),
    e("E0321", "expected a path after `::`", "Path separators must be followed by a name.", None),
    e("E0322", "expected an expression", "An expression was required here: a literal, a name, a call, a block, or an operator applied to those.", None),
    e("E0330", "malformed generic arguments", "Generic arguments must be closed with `>`.", None),
    e("E0340", "expected a struct field name", "Struct literal fields are `name: value` pairs.", None),
    e("E0341", "malformed struct literal", "Struct literals are `Point { x: 1, y: 2 }`.", Some("let p = Point { x: 1, y: 2 }")),
    e("E0342", "malformed tuple expression", "Tuple expressions list values separated by commas.", None),
    e("E0343", "malformed parenthesized expression", "A parenthesized expression must be closed with `)`.", None),
    e("E0344", "malformed repeat array expression", "The repeat form is `[value; count]`.", Some("let zeros = [0; 16]")),
    e("E0345", "malformed array literal", "Array literals are comma separated values inside brackets.", None),
    e("E0350", "`match` is missing its body", "A match expression is `match value { pattern => expr, ... }`.", None),
    e("E0351", "malformed match arm", "Each arm is `pattern => expression`, optionally guarded: `pattern if cond => expression`.", None),
    e("E0352", "malformed match body", "The match body must be closed with `}`.", None),
    e("E0360", "expected a name for the binding pattern", "Binding patterns are identifiers, `mut name`, or `_`.", None),
    e("E0361", "malformed tuple pattern", "Tuple patterns list sub-patterns inside parentheses.", None),
    e("E0362", "malformed slice pattern", "Slice patterns use `[a, b, ..rest]`.", None),
    e("E0363", "expected a pattern", "Patterns appear in `let`, `match` arms and `for` loops.", None),
    e("E0364", "malformed variant pattern", "Variant patterns are `Enum::Variant(...)` or `Enum::Variant { .. }`.", None),
    e("E0365", "expected a pattern field name", "Struct pattern fields are `name` or `name: pattern`.", None),
    e("E0366", "malformed struct pattern", "A struct pattern must be closed with `}`.", None),
    // ---- name resolution and type checking (Phase 2) ----------------------
    e(
        "E1001",
        "type mismatch",
        "The checker found two types that must be identical but are not. UMAY performs no implicit \
         conversion: not from integer literals to a wider type, not from `&T` to `&U`, and not \
         from `T` to `Box<T>`. Fix the expression, add `as` for a deliberate numeric conversion, \
         or change the annotation.",
        Some("let x: i64 = 1      // ok: the literal adopts the annotation\nlet y: i64 = 1i32   // error: i32 is not i64"),
    ),
    e(
        "E1002",
        "cannot infer this type",
        "Inference is local to a function body and flows only from annotations and concrete \
         expressions. A `_` placeholder with nothing to infer from, or a value whose only use is \
         itself, cannot be resolved. Add an explicit type annotation.",
        Some("let v: Vec<i32> = Vec::new()"),
    ),
    e(
        "E1003",
        "unknown type",
        "No struct, enum, trait or type alias with this name is in scope. Types are resolved before \
         function bodies, so a typo in a signature is reported here rather than as a cascade of \
         errors inside the body.",
        Some("fn f(count: u32) { }   // not: fn f(count: unt32)"),
    ),
    e(
        "E1004",
        "wrong number of arguments",
        "The call supplies a different number of arguments than the function or method declares. \
         UMAY has no optional parameters and no variadic functions, so the count must match exactly.",
        Some("fn add(a: i32, b: i32) -> i32 { a + b }\nlet n = add(1, 2)"),
    ),
    e(
        "E1005",
        "wrong enum variant arity",
        "The variant was declared with a different shape than the pattern or constructor uses: a \
         unit variant takes nothing, a tuple variant takes a fixed number of values, and a struct \
         variant takes named fields.",
        Some("enum E { A, B(i32) }\nlet b = E::B(1)   // E::A takes nothing"),
    ),
    e(
        "E1006",
        "no such field",
        "The field name does not exist on this struct, variant or tuple. Field names are resolved \
         against the declaration, so a typo is reported at the access site with the nearest field \
         name as a suggestion.",
        Some("struct P { x: i32, y: i32 }\nlet p = P { x: 1, y: 2 }\nlet _ = p.x"),
    ),
    e(
        "E1007",
        "no such enum variant",
        "The enum does not declare this variant. The available variants are listed in the \
         explanation, and the closest name is suggested.",
        Some("enum Color { Red, Green }\nlet c = Color::Red"),
    ),
    e(
        "E1008",
        "non-exhaustive `match`",
        "Every possible value of the scrutinee must be handled. UMAY proves exhaustiveness for \
         enums, `bool`, and any type reached through a catch-all arm. This turns a newly added \
         enum variant into a compile error at every `match` site instead of a silent runtime \
         fallthrough.",
        Some("match color {\n    Color::Red => 1,\n    Color::Green => 2,\n    Color::Blue => 3,\n}"),
    ),
    e(
        "E1009",
        "struct field not initialised",
        "A struct literal must provide a value for every field. Partial initialisation is rejected \
         because the omitted field would be left in an undefined state. Copy the remaining fields \
         from an existing value with `..base`.",
        Some("let p = Point { x: 1, y: 2 }\nlet q = Point { x: 5, ..p }"),
    ),
    e(
        "E1010",
        "duplicate definition",
        "Two items in the same scope use the same name. Item names are collected before any body \
         is checked so that declaration order does not matter, which means duplicates are reported \
         regardless of where they appear.",
        Some("struct User { id: u64 }\n// a second `struct User` in the same module is an error"),
    ),
    e(
        "E1011",
        "method has no `self` parameter",
        "A function called as `receiver.method()` must declare a receiver. An associated function \
         declared without `self` is called with a path instead: `Type::name(...)`.",
        Some("impl Counter {\n    fn new() -> Counter { Counter { n: 0 } }\n}\nlet c = Counter::new()"),
    ),
    e(
        "E1012",
        "value cannot be interpolated into a string",
        "String interpolation formats its values through the `Display` trait. Numbers, `bool`, \
         `char`, `str` and `String` implement it out of the box; other types need an \
         `impl Display for YourType`.",
        Some("let s = \"count={n}\"      // n: i32\nlet t = \"user={u}\"       // needs `impl Display for User`"),
    ),
    e(
        "E1013",
        "invalid `as` cast",
        "`as` performs a total, deterministic conversion: between numeric types, between `char` \
         and integers, and between pointers and `usize`. Casting *to* `char` is never allowed \
         because most integers are not valid Unicode scalar values; use `char::from_u32`, which \
         returns `Option<char>`.",
        Some("let big = small as i64\nlet c = char::from_u32(code)   // not: code as char"),
    ),
    e(
        "E1014",
        "operation not defined for this type",
        "The operator or operation does not exist for the operand type. Arithmetic and bitwise \
         operators require numeric types, comparisons require an ordering, and `for` requires an \
         iterable. There are no implicit trait implementations.",
        Some("let n = 1 + 2        // integers\nlet s = a.len()      // methods, not operators"),
    ),
    e(
        "E1015",
        "`?` in a function that cannot propagate",
        "`expr?` returns early with `None` or `Err`. The enclosing function must therefore return \
         a type that can carry that failure, otherwise the early return would have nowhere to go.",
        Some("fn parse(s: str) -> Option<i32> {\n    let n = read_number(s)?\n    Some(n)\n}"),
    ),
    // ---- ownership and borrowing ------------------------------------------
    e(
        "E0201",
        "use of a moved value",
        "A value of a type that is not `Copy` was moved out of a variable, and the variable was \
         used again. Moving transfers ownership: the original binding no longer holds a value, so \
         there is nothing to read. Borrow the value instead of moving it when both uses need it.",
        Some("let s = \"hello\"\ntake(s)\nprintln(s)   // error: `s` was moved"),
    ),
    e(
        "E0202",
        "use of a possibly-uninitialised value",
        "The checker proved that a value is assigned on some paths to this point but not on all of \
         them. Initialising it unconditionally, or in both branches, removes the doubt. `let` \
         always takes an initialiser in UMAY, so this concerns assignments, which are the only way \
         to leave a variable without a value.",
        Some("let x: i32\nif c { x = 1 }\nprintln(x)   // error: only assigned when `c` holds"),
    ),
    e(
        "E0203",
        "reference may outlive its referent",
        "The lifetime of a reference is inferred from its uses, and must end before the value it \
         points at is destroyed. When inference cannot prove that, the reference would dangle. \
         UMAY's inference is deliberately simpler than Rust's, so some programs a full checker \
         would accept are rejected here: that limitation is intentional and reported under this \
         code so it is never mistaken for a bug.",
        Some("fn f(x: &i32, y: &i32) -> &i32 { x }   // error: which input? needed"),
    ),
    e(
        "E0204",
        "conflicting borrows",
        "Two mutable borrows of the same place overlapped in time. A mutable borrow is exclusive: \
         while it is alive nothing else may read or write the place, otherwise one of the two \
         would observe a value changed through the other. A borrow lives until its last use, so \
         ending the earlier use before the second borrow fixes this.",
        Some("let a = &mut x\nlet b = &mut x   // error: `a` is used later"),
    ),
    e(
        "E0205",
        "mutable and shared borrows overlap",
        "A mutable borrow cannot coexist with any other borrow of the same place. Sharing it while \
         something may write to it is precisely the aliasing that UMAY forbids, because a reader \
         through the shared reference could see the value change underneath it.",
        Some("let s = &x\nlet m = &mut x   // error: `s` is still used later"),
    ),
    e(
        "E0206",
        "assignment to a borrowed place",
        "A place was assigned to, or moved out of, while a borrow of it was still in use. The \
         borrower holds a reference to that storage, so changing the value would be visible through \
         it. Finish the borrow's last use first, then assign.",
        Some("let r = &x\nx = 5   // error: `r` is used after this"),
    ),
    e(
        "E0207",
        "return of a reference to a local value",
        "The function returns a reference to storage it owns. That storage is destroyed when the \
         function returns, so the caller would receive a dangling reference. Returning a borrow of \
         a parameter is fine, because the caller owns the parameter's referent and outlives the \
         call; returning a borrow of a `let` binding or a temporary is not.",
        Some("fn bad() -> &i32 {\n    let x = 1\n    &x   // error: `x` dies here\n}"),
    ),
    e(
        "E0208",
        "double drop, or move out of a type with a destructor",
        "A value with a destructor was moved out of a place that will still be dropped, or was \
         dropped twice. Drop flags make the guarantee static: each value is destroyed exactly \
         once, on every path. Moving out of a partially borrowed or partially moved value is \
         rejected for the same reason.",
        Some("struct Guard { n: i32 }\nfn f(g: Guard) {\n    take(g)\n    drop(g)   // error: already destroyed\n}"),
    ),
    e(
        "E0209",
        "cannot move out of an index or a dereference",
        "Indexing and dereferencing produce a place inside value the compiler cannot hand over \
         wholesale: the index may be out of range, and a reference does not own its pointee. Copy \
         the value out when the element type is `Copy`, or borrow it instead of moving.",
        Some("let p = &v[0]\nlet mine = *p   // error for a non-Copy element"),
    ),
    e(
        "E0210",
        "reference to a temporary value",
        "The `&` operator needs a place: storage that will still exist afterwards. A literal or the \
         result of an expression is a temporary that is destroyed at the end of the enclosing \
         statement, so a reference to it would dangle. Bind the value with `let` first, then \
         borrow the binding.",
        Some("let r = &count()   // error: the result is a temporary\nlet n = count()\nlet r = &n   // ok"),
    ),
    e(
        "E0211",
        "raw pointer dereference outside `unsafe`",
        "Dereferencing a raw pointer is not checked: the pointer may be null, dangling, or point at \
         memory of the wrong type. That is why it is confined to `unsafe` blocks, which mark the \
         code as taking responsibility for the guarantee the checker cannot provide.",
        Some("let p: *i32 = 0\nlet v = *p            // error\nlet v = unsafe { *p } // ok, but must be valid"),
    ),
    e(
        "E0212",
        "non-`Send` value crosses a thread boundary",
        "A value may only be moved to another thread when its type is `Send`, and shared across \
         threads only when it is `Sync`. Without that bound, safe code could construct a data race. \
         Borrowing rules already prevent two threads from holding `&mut` to the same place.",
        Some("let r = Rc.new(x)\nthread.spawn(|| r.use_it())   // error: `Rc` is not `Send`"),
    ),
    e(
        "E0213",
        "aliasing violation through a raw pointer",
        "`unsafe` relaxes the checks, not the rules. Forming two mutable raw pointers to the same \
         storage and writing through both breaks the assumption that a mutable reference is unique, \
         which is undefined behaviour even though the compiler cannot see it. This diagnostic \
         reports the cases that are visible statically.",
        Some("let a: *mut i32 = &mut x\nlet b: *mut i32 = &mut x   // two mutable pointers to one place"),
    ),
    // ---- warnings ---------------------------------------------------------
    e(
        "W0101",
        "binding shadows an earlier binding in the same scope",
        "Two `let` statements in one block introduced the same name. Unlike Rust, UMAY treats \
         same-scope shadowing as a probable mistake; use an inner block if the shadow is \
         intentional.",
        Some("let x = 1\n{\n    let x = 2   // ok: inner scope\n}"),
    ),
    e(
        "W0102",
        "narrowing cast can truncate",
        "The destination type cannot represent every value of the source type, so the cast may \
         silently discard information. This is a warning rather than an error because narrowing is \
         sometimes exactly what is wanted.",
        Some("let small = big as i32   // warns when `big` is i64"),
    ),
    e(
        "W0103",
        "`if` without `else` discards a value",
        "The `then` branch produces a value, but there is no `else` branch to produce a matching \
         one, so the result is dropped. End the block with `;` if discarding is intended.",
        Some("if ready { do_work() }   // result discarded"),
    ),
    e(
        "W0010",
        "attribute has no effect yet",
        "This attribute is recognised syntactically but not implemented in the current phase. It is \
         reported as a warning rather than silently ignored so that builds behave predictably across \
         compiler versions.",
        None,
    ),
];

pub fn lookup(code: &str) -> Option<&'static ExplainEntry> {
    EXPLAIN.iter().find(|e| e.code.eq_ignore_ascii_case(code))
}

/// All documented codes, sorted and deduplicated.
pub fn all_codes() -> Vec<&'static str> {
    let mut codes: Vec<&'static str> = EXPLAIN.iter().map(|e| e.code).collect();
    codes.sort_unstable();
    codes.dedup();
    codes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookup_is_case_insensitive() {
        assert!(lookup("e0001").is_some());
        assert!(lookup("E0201").is_some(), "ownership codes are documented");
    }

    #[test]
    fn codes_are_unique() {
        let codes = all_codes();
        assert_eq!(codes.len(), EXPLAIN.len(), "duplicate explain codes");
        // Ensure the base slice really has no duplicates either.
        let mut sorted: Vec<&str> = EXPLAIN.iter().map(|e| e.code).collect();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), EXPLAIN.len());
    }

    #[test]
    fn every_entry_has_text() {
        for entry in EXPLAIN {
            assert!(!entry.summary.is_empty(), "{} has empty summary", entry.code);
            assert!(!entry.explanation.is_empty(), "{} has empty explanation", entry.code);
        }
    }
}
