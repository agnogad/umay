# UMAY Language Specification — v0.1

Status: **draft**. This document is normative for the reference compiler. The
compiler is the final arbiter; where the compiler and this document disagree,
fix the one that is wrong (by our own rule: diagnostics-first, and the
compiler must never silently diverge from documented behaviour).

Versioning: `0.1` — this is the first frozen slice of syntax. Syntax is frozen
for the scope of this version; semantics that are not yet implemented are
explicitly marked **unimplemented**.

---

## 1. Goals and non-goals

UMAY is a native, statically typed systems language with:

- Rust-inspired memory safety (ownership + borrow checking, no GC),
- simpler and more approachable syntax than Rust,
- extremely useful, AI-agent-friendly compiler diagnostics,
- fast compilation with incremental caching,
- small native executables,
- a custom native code generation backend (no LLVM, no Cranelift).

Non-goals for v0.1: async/await, proc-macros, full trait solving, dynamic
dispatch, concurrency model formalisation. All may come later; none are
required for the v0.1 language surface.

## 2. Notation

- `x?` means optional `x`.
- `x*` means zero or more `x`.
- `x+` means one or more `x`.
- `(a | b)` means alternatives.
- Comments: `// line`, `/* block (nested) */`.
- Semicolons are optional (see §5.5).

## 3. Lexical structure

### 3.1 Source files

- UTF-8 encoded text.
- File extension `.um`.
- Identifiers, strings and comments may contain any Unicode; source outside
  these positions uses the ASCII punctuation set below.

### 3.2 Identifiers

```text
identifier ::= ID_START ID_CONTINUE*
ID_START   ::= alphabetic | '_'
ID_CONTINUE::= alphanumeric | '_'
```

Conventions (enforced by the linter, not the lexer):
- types and enum variants: `UpperCamelCase`
- functions, variables, fields: `snake_case`
- constants: `SCREAMING_SNAKE_CASE`

### 3.3 Keywords

Reserved (may not be used as identifiers):

```text
fn let mut struct enum trait impl for type const static
use mod pub self Self return if else match while loop in
break continue unsafe as where true false defer move ref dyn
```

`true` and `false` are boolean literals, not names.

### 3.4 Literals

```text
int_literal   ::= dec | hex | bin | oct
dec           ::= digit+ (with '_' separators allowed)
hex           ::= '0x' hexdigit+
bin           ::= '0b' ('0'|'1')+
oct           ::= '0o' octdigit+
int_suffix    ::= 'i8'|'i16'|'i32'|'i64'|'i128'|'u8'|'u16'|'u32'|'u64'|'u128'|'usize'|'isize'
float_literal ::= digits '.' digits
float_suffix  ::= 'f32' | 'f64'

char_literal ::= '\'' (char | escape) '\''
string_literal ::= '"' (char | escape | interpolation | '{{' | '}}')* '"'
```

Escapes: `\n \t \r \0 \\ \" \' \{ \} \xNN \u{...}`.

Interpolation: inside a string, `{expr}` splices the value of `expr`.
`{{` and `}}` are literal braces. Interpolation is sugar for the standard
library formatting facility and is available without imports.

### 3.5 Punctuation and operators

```text
( ) { } [ ] , ; : :: . .. ..= -> => | || & && * + - / % ^ !
? = == != < <= > >= += -= *= /= %= &= |= ^= << >> <<= >>= @ # _
```

---

## 4. Program structure

```text
program  ::= item*
item     ::= attribute* 'pub'? ( fn_decl | struct_decl | enum_decl
            | trait_decl | impl_block | const_decl | use_decl | mod_decl
            | type_alias )
```

### 4.1 Functions

```text
fn_decl ::= 'extern'? 'fn' identifier generic_params? '(' param_list ')' ('->' type)? (block | ';')
param    ::= (identifier | '_') ':' type
self_param ::= 'self' | '&' 'self' | '&' 'mut' 'self'
```

- Parameter types are mandatory (§ type system: inference does not cross
  function boundaries).
- A body is required unless the function is `extern`. An `extern fn` binds the C
  symbol of that exact name and has no body; a function without a body and
  without `extern` is `E0113`, and an `extern fn` with a body is `E0112`.
  `extern` carries no `unsafe`: the operations that make a call dangerous are the
  ones written in `unsafe` blocks, and a declaration does not perform any.

```umay
extern fn malloc(size: usize) -> *mut u8
extern fn write(fd: i32, buf: *const u8, count: usize) -> isize
```

`extern` is how the runtime reaches the operating system; the runtime itself is
UMAY source (see [ADR 0010](adr/0010-runtime-in-umay.md)).

### 4.2 Structs

```text
struct_decl ::= 'struct' identifier generic_params? '{' field* '}'
field        ::= 'mut'? identifier ':' type
```

### 4.3 Enums

```text
enum_decl ::= 'enum' identifier generic_params? '{' variant* '}'
variant   ::= identifier ( '(' type* ')' | '{' field* '}' | '=' expr )?
```

Discriminant values are assigned automatically (`0, 1, 2, ...`) and may be
overridden with `= expr`.

### 4.4 Traits and impls

```text
trait_decl ::= 'trait' identifier generic_params? '{' fn_decl* '}'
impl_block ::= 'impl' generic_params? (type ('for' type)?) '{' fn_decl* '}'
```

Trait methods without bodies are required by implementors. Default methods
(provided bodies) are a later-phase feature.

### 4.5 Constants, aliases, imports, modules

```text
const_decl ::= 'const' identifier (':' type)? '=' expr
type_alias ::= 'type' identifier generic_params? '=' type
use_decl   ::= 'use' path ('::' '{' identifier (',' identifier)* ','? '}')? ('as' identifier)?
mod_decl   ::= 'mod' identifier (block | ';')
```

---

## 5. Statements

```text
stmt ::= let_stmt | expr_stmt | item | ';'
let_stmt ::= 'let' 'mut'? pattern (':' type)? '=' expr ('else' block)?
```

### 5.1 `let`

- Immutable by default; `let mut` for mutation.
- **Every `let` initialises its binding.** Deferred initialisation is not
  permitted. This is what makes definite-initialisation checking sound and
  cheap (diagnostic `E0303`).
- `let ... else { ... }` (let-else) is recognised syntactically; semantics land
  with pattern exhaustiveness in Phase 2.

### 5.2 Expression statements

Any expression may be a statement. A trailing expression without a semicolon is
the block's value.

### 5.3 `for`

```text
'for' pattern 'in' expr block
```

`0..10` is the standard half-open range; `0..=10` is inclusive.

### 5.4 `defer` and `unsafe`

```text
defer   ::= 'defer' block       // runs at scope exit, LIFO
unsafe  ::= 'unsafe' block      // permits explicitly unsafe operations
```

`unsafe` is parsed now and enforced semantically in Phase 4.

### 5.5 Semicolons are optional

A statement ends at a line break unless:

- the previous token requires continuation (operator, `.`, `(`, `[`, `,`, `=`),
- the next token continues the expression (postfix, operator),
- the next token unambiguously starts a new statement (`let`, `return`, `if`,
  `fn`, ...).

Multiple statements on one line are separated by starting a new statement
keyword. `;` is always accepted and never required. A `;` on a statement
**suppresses** its value from becoming a block tail.

---

## 6. Expressions

Precedence, highest to lowest:

```text
postfix         expr.field  expr.method(args)  expr(args)  expr[i]  expr?  expr as T
unary           -expr  !expr  *expr  &expr  &mut expr  move expr
multiplicative  *  /  %
additive        +  -
shift           <<  >>
bitand          &
bitxor          ^
bitor           |
comparison      ==  !=  <  <=  >  >=
logical and     &&
logical or      ||
range           ..  ..=
assignment      =  +=  -=  *=  /=  %=  &=  |=  ^=  <<=  >>=   (right-associative)
```

### 6.1 Blocks

```text
block ::= '{' stmt* expr? '}'
```

The value of a block is its trailing expression. An `if`/`else`, `match`, and
`loop` are expressions; `while` and `for` evaluate to `()`.

### 6.2 `match`

```text
'match' expr '{' arm* '}'
arm ::= pattern ('if' expr)? '=>' expr
```

Match arms are separated by `,` or a newline. Matching must be exhaustive
(checked in Phase 2).

### 6.3 String interpolation

```text
str_interp ::= '"' (literal | '{' expr '}' | '{{' | '}}')* '"'
```

---

## 7. Patterns

```text
pattern ::= or_pattern
or_pattern ::= atom ('|' atom)*
atom ::= binding | literal | tuple | struct | variant | range | slice
binding ::= ('mut')? identifier | '_'
tuple   ::= '(' pattern (',' pattern)* ','? ')'
struct  ::= path '{' (field_pattern (',' field_pattern)* ','?)? ('..' | ..rest)? '}'
field_pattern ::= identifier (':' pattern)?
variant ::= path ( '(' pattern* ')' | '{' field_pattern* '}' )?
range   ::= literal '..' literal | literal '..=' literal
slice   ::= '[' pattern (',' pattern)* ','? '..'? ']'
```

Wildcards: `_` binds nothing. `..` in struct/slice patterns means "and the rest".

---

## 8. Type system (summary)

The full normative treatment is in [type-system.md](type-system.md).

Primitive types:

```text
bool   i8 i16 i32 i64 i128   u8 u16 u32 u64 u128   f32 f64
usize isize   char   str (unsized)   String (owned, heap)
```

Composite types: `struct`, `enum`, tuples `(T1, T2)`, arrays `[T; N]`, slices
`[T]`, references `&T`, `&mut T`, raw pointers `*const T`, `*mut T`,
function types `fn(A) -> B`, `impl Trait`, `dyn Trait`, `Option<T>`,
`Result<T, E>`.

A string literal has type `&str`: a pointer to NUL-terminated bytes. Strings are
not pointer-and-length pairs, which is what lets one be handed to a C function
without a conversion (see [ADR 0010](adr/0010-runtime-in-umay.md)).

Rules enforced in Phase 2:
- no implicit numeric conversion; `as` is explicit and checked,
- no silent truncation,
- pointer casts that are legal: `&str` and `&[u8; N]` to `*const u8`, pointer to
  integer and integer to pointer (in `unsafe`), and `*mut T` to `*const T`.
  A `*mut T` also satisfies a `*const T` parameter without a cast, because both
  are the same address with the same layout,
- `match` and `if let` are exhaustiveness-checked,
- generic parameters must be declared before use,
- `where` clauses exist syntactically; bounds checking lands with traits.

### 8.4 The prelude and the standard library

Names in the prelude are available in every program without an import. The
prelude is UMAY source (`stdlib/prelude.um`) compiled with the program, not a set
of compiler built-ins, so its functions are checked, borrowed and generated the
same way as code you write.

| Name | Signature | Meaning |
|------|-----------|---------|
| `print` | `fn print(s: *const u8)` | writes bytes, no newline |
| `println` | `fn println(s: *const u8)` | writes bytes and a newline |
| `eprintln` | `fn eprintln(s: *const u8)` | the same, to standard error |
| `println_i64` | `fn println_i64(v: i64)` | writes a decimal integer and a newline |
| `byte_len` | `fn byte_len(s: *const u8) -> usize` | bytes before the terminating zero |
| `alloc` | `fn alloc(bytes: usize) -> *mut u8` | allocates, aborting if it cannot |
| `dealloc` | `fn dealloc(ptr: *mut u8)` | releases an `alloc` block |
| `read_byte` | `fn read_byte(ptr: *const u8) -> u8` | reads one byte |
| `write_byte` | `fn write_byte(ptr: *mut u8, value: u8)` | writes one byte |
| `panic` | `fn panic(message: *const u8)` | prints and exits with status 101 |

A `&str` is accepted wherever `*const u8` is expected, so `println("text")` needs
no cast.

`String` (`stdlib/string.um`) is the first library type: an owned, growable
sequence of bytes held as a pointer, a length and a capacity.

| Method | Signature | Meaning |
|--------|-----------|---------|
| `new` | `fn new() -> String` | empty, owns nothing |
| `from_str` | `fn from_str(text: *const u8) -> String` | copies the bytes of `text` |
| `len` | `fn len(&self) -> usize` | bytes held |
| `is_empty` | `fn is_empty(&self) -> bool` | `len() == 0` |
| `as_ptr` | `fn as_ptr(&self) -> *const u8` | NUL-terminated bytes |
| `push` | `fn push(self, byte: u8) -> String` | appends one byte |
| `push_str` | `fn push_str(self, text: *const u8) -> String` | appends every byte of `text` |

Methods that change a string take `self` by value and return the new value, so
`let s = s.push(65)` is the way to grow one; the old value is moved and gone.
Growth doubles, and a terminator is kept one past the last byte so the buffer can
be handed to the printers directly. Lengths count bytes, never characters.

### Generic declarations

A generic parameter is written after the struct or function name and is in scope
in that declaration:

```umay
struct Box<T> { value: T }

impl<T> Box<T> {
    fn get(&self) -> T { return self.value }
}
```

**Instantiation at the type level works.** A struct literal infers its type
arguments from the values it is given — `Box { value: 7 }` is `Box<i32>`,
`Box { value: "hi" }` is `Box<&str>` — and the resulting value carries the
instance. Field reads check against the substituted field type, and a method's
signature is substituted from its receiver, so `Box<i64>::get` returns `i64`.
Two instantiations of one declaration are different types.

**Bodies are monomorphised.** Every generic function, method and impl method
that a concrete call reaches is copied with its type arguments substituted, and
the call is rewritten to that copy. A declaration is never compiled on its own:
only the instances a program actually needs exist in the output.

```umay
struct Box<T> { value: T }

impl<T> Box<T> {
    fn get(&self) -> T { return self.value }
}

fn main() -> i32 {
    let b: Box<i64> = Box { value: 7 }
    println_i64(b.get())   // 7
    return 0
}
```

A type annotation is also an inference hint, which matters where no value can
name the instance:

```umay
let empty: Option<i64> = Option::None   // `T` comes from the annotation
```

Without such an annotation an instance that nothing names is reported
("cannot infer the type parameter `T`") rather than guessed.

`Vec<T>` is available in the standard library — see `stdlib/vec.um`. Its stride
is currently fixed at eight bytes, so it serves eight-byte element types until
`size_of::<T>()` exists. `Option`, `Result` and `Iterator` are not in the standard
library yet, though the declaration form works.

---

## 9. Memory model (summary)

The full normative treatment is in [memory-model.md](memory-model.md).

- Every value has one owner at a time; assignment moves ownership.
- `let` transfers ownership; a moved value is unusable (diagnostic `E0201`).
- `&T` shares immutably; many readers allowed, no writers.
- `&mut T` is exclusive; one writer, no readers.
- References never outlive their referent.
- All of this is enforced at compile time in Phase 4.
- Destruction is deterministic, at scope exit, LIFO; `defer` runs after
  destructors of the same scope.
- `unsafe` blocks are the only place raw pointers and unsafe operations are
  legal, and they are tracked.

## 10. Error handling

- No exceptions. `Result<T, E>` for fallible operations, `Option<T>` for
  absence.
- `?` propagates errors: `let x = f()?` returns `Err` early from the function.
- `match` is the primary way to handle results.
- Compile-time errors and runtime errors are distinct: compile-time errors
  have codes `E*`, warnings `W*`.

## 11. Diagnostics (summary)

The full contract is in [diagnostics.md](diagnostics.md).

- Every diagnostic has a stable code: `E0001`–`E0999` lexical/structure,
  `E1000`+ semantic, `E2xxx` ownership/borrow, `W*` warnings.
- Codes, messages, cause, and JSON output follow a versioned schema.
- `umay explain CODE` documents every code.

## 12. Toolchain surface (summary)

The full contract is in [build-system.md](build-system.md).

- `umay new | check | ast | explain | doctor | clean | build | run | fmt | lint | lsp`
- Exit codes: `0` ok, `1` compile error, `2` usage, `3` I/O, `4` not implemented.
- No `target/` in projects; cache lives in the OS user cache directory.

## 13. Known gaps in v0.1

| Feature | Status |
|---------|--------|
| Type checking, inference, generics | Phase 2, unimplemented |
| Ownership / borrow checking | Phase 4, unimplemented |
| Code generation | Phase 5, unimplemented |
| Runtime + stdlib | Phase 6, unimplemented |
| Attributes with real semantics | Phase 7 (currently `W0010`) |
| `pub` visibility semantics | Phase 2 |
| `where` clause semantics | Phase 2 |
| let-else semantics | Phase 2 |
