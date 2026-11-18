//! Machine-readable description of a program's public surface.
//!
//! `umay metadata` answers a question that a compiler is usually the worst tool
//! to ask: what does this program *contain*? An editor wants that answer to offer
//! completion, a doc generator wants it to build a page, and a test harness wants
//! it to know what to call. None of them can read MIR, and none of them should
//! have to: MIR is the compiler's internal form and changes whenever the backend
//! does.
//!
//! So this module reads the syntax tree, after it has been parsed and checked, and
//! prints what a reader of the source would write down. Types are rendered from
//! the source text of their span rather than from the type table, because the
//! type table has already resolved every alias and this output is meant to look
//! like the program the user wrote.
//!
//! The output is versioned. `VERSION` changes only when the *shape* changes, so a
//! driver can depend on the current shape and reject a document it does not
//! recognise instead of misreading it.

use crate::ast::{
    Item, Program, StructDecl, EnumDecl, TraitDecl, ImplBlock, FnDecl, ConstDecl, TypeAlias,
    TypeExpr,
};
use crate::source::SourceMap;

/// The document's shape. Bump this only for an incompatible change.
pub const VERSION: u32 = 1;

/// One named thing a program declares.
#[derive(Debug, Clone, PartialEq)]
pub enum Symbol {
    /// `fn name(..) -> T`, or a method when `self` is present.
    Fn {
        name: String,
        /// The type the method is implemented on, when this is an `impl` method.
        owner: Option<String>,
        /// The name of the trait, when this is a trait declaration's method.
        trait_name: Option<String>,
        /// `T` for each `fn f<T: Bound>`.
        generics: Vec<String>,
        /// `self`, `&self`, `&mut self`, or empty for a free function.
        receiver: Option<String>,
        params: Vec<(String, String)>,
        ret: Option<String>,
        is_extern: bool,
        /// The symbol the linker resolves, when it differs from `name`.
        link_name: Option<String>,
    },
    Struct {
        name: String,
        generics: Vec<String>,
        fields: Vec<(String, String, bool)>,
    },
    Enum {
        name: String,
        generics: Vec<String>,
        variants: Vec<Variant>,
    },
    Trait {
        name: String,
        generics: Vec<String>,
        methods: Vec<String>,
    },
    /// `impl Owner` or `impl Trait for Owner`.
    Impl {
        trait_name: Option<String>,
        owner: String,
        methods: Vec<String>,
    },
    Const {
        name: String,
        ty: String,
    },
    Alias {
        name: String,
        generics: Vec<String>,
        ty: String,
    },
    /// A `mod name;` whose file was not part of this pass.
    Module {
        name: String,
    },
}

/// An enum variant, with the shape of its payload.
#[derive(Debug, Clone, PartialEq)]
pub struct Variant {
    pub name: String,
    /// Field types, for a tuple variant.
    pub tuple: Vec<String>,
    /// Named field types, for a struct variant.
    pub struct_fields: Vec<(String, String)>,
}

/// The symbols of `text`, or none if it does not parse.
///
/// An editor asks while the program is half-written, so a parse failure is the
/// normal case rather than an error: the answer is "nothing to offer yet".
pub fn symbols_or_empty(text: &str) -> Vec<Symbol> {
    symbols_or_empty_at("<lsp>", text)
}

/// The same, for a document that has a path, so its `mod name;` declarations
/// resolve against the directory it is really in.
pub fn symbols_or_empty_at(name: &str, text: &str) -> Vec<Symbol> {
    crate::metadata_source(name, text).unwrap_or_default()
}

impl Symbol {
    /// The name this symbol declares, without its module or owner prefix.
    pub fn short_name(&self) -> &str {
        match self {
            Symbol::Fn { name, .. }
            | Symbol::Struct { name, .. }
            | Symbol::Enum { name, .. }
            | Symbol::Trait { name, .. }
            | Symbol::Const { name, .. }
            | Symbol::Alias { name, .. }
            | Symbol::Module { name } => name,
            Symbol::Impl { owner, .. } => owner,
        }
    }

    /// The name as written in the document, including any prefix.
    pub fn name(&self) -> &str {
        self.short_name()
    }
}

/// Collects the symbols a program declares, in source order.
///
/// Items are gathered depth-first through `mod` blocks so that a program split
/// across modules lists everything, not just what sits at the top level. The
/// module path is kept on the name, so two `open` functions in two modules stay
/// distinguishable.
pub fn symbols(program: &Program, map: &SourceMap) -> Vec<Symbol> {
    let mut out = Vec::new();
    walk(&program.items, "", map, &mut out);
    out
}

fn walk(items: &[Item], prefix: &str, map: &SourceMap, out: &mut Vec<Symbol>) {
    for item in items {
        match item {
            Item::Fn(decl) => out.push(fn_symbol(decl, prefix, None, None, map)),
            Item::Struct(decl) => out.push(struct_symbol(decl, prefix, map)),
            Item::Enum(decl) => out.push(enum_symbol(decl, prefix, map)),
            Item::Trait(decl) => out.push(trait_symbol(decl, prefix)),
            Item::Impl(block) => {
                let owner = render(&block.self_ty, map);
                let trait_name = block.trait_path.as_ref().map(path_text);
                for method in &block.methods {
                    // A method is a function a caller can call, so it is listed
                    // in full: naming it inside the impl would leave a tool that
                    // asks "what is `S::get`" with only the name.
                    out.push(fn_symbol(
                        method,
                        &format!("{prefix}{owner}::"),
                        Some(owner.clone()),
                        trait_name.clone(),
                        map,
                    ));
                }
                out.push(impl_symbol(block, prefix, map));
            }
            Item::Const(decl) => out.push(const_symbol(decl, prefix, map)),
            Item::TypeAlias(alias) => out.push(alias_symbol(alias, prefix, map)),
            Item::Mod(decl) => match &decl.items {
                Some(items) => {
                    let inner = format!("{prefix}{}::", decl.name);
                    walk(items, &inner, map, out);
                }
                // `mod name;` names a file this pass was not given, so the names
                // inside it are not visible here. Listing a module with no
                // contents is more honest than pretending it is empty.
                None => out.push(Symbol::Module { name: format!("{prefix}{}", decl.name) }),
            },
            // A `use` renames something declared elsewhere; it declares nothing
            // this module owns, so listing it would make every import look like a
            // symbol of its own.
            Item::Use(_) => {}
        }
    }
}

fn fn_symbol(
    decl: &FnDecl,
    prefix: &str,
    owner: Option<String>,
    trait_name: Option<String>,
    map: &SourceMap,
) -> Symbol {
    Symbol::Fn {
        name: format!("{prefix}{}", decl.name),
        owner,
        trait_name,
        generics: decl.generics.iter().map(generic_text).collect(),
        receiver: decl.self_param.as_ref().map(|s| match s.by_ref {
            None => "self".to_string(),
            Some(false) => "&self".to_string(),
            Some(true) => "&mut self".to_string(),
        }),
        params: decl
            .params
            .iter()
            .map(|p| {
                let name = p.name.clone().unwrap_or_else(|| "_".to_string());
                (name, render(&p.ty, map))
            })
            .collect(),
        ret: decl.ret.as_ref().map(|t| render(t, map)),
        is_extern: decl.is_extern,
        link_name: decl.link_name.clone(),
    }
}

fn struct_symbol(decl: &StructDecl, prefix: &str, map: &SourceMap) -> Symbol {
    Symbol::Struct {
        name: format!("{prefix}{}", decl.name),
        generics: decl.generics.iter().map(|g| g.name.clone()).collect(),
        fields: decl
            .fields
            .iter()
            .map(|f| (f.name.clone(), render(&f.ty, map), f.mutable))
            .collect(),
    }
}

fn enum_symbol(decl: &EnumDecl, prefix: &str, map: &SourceMap) -> Symbol {
    Symbol::Enum {
        name: format!("{prefix}{}", decl.name),
        generics: decl.generics.iter().map(|g| g.name.clone()).collect(),
        variants: decl
            .variants
            .iter()
            .map(|v| Variant {
                name: v.name.clone(),
                tuple: v
                    .tuple_fields
                    .as_ref()
                    .map(|fields| fields.iter().map(|t| render(t, map)).collect())
                    .unwrap_or_default(),
                struct_fields: v
                    .struct_fields
                    .as_ref()
                    .map(|fields| {
                        fields.iter().map(|f| (f.name.clone(), render(&f.ty, map))).collect()
                    })
                    .unwrap_or_default(),
            })
            .collect(),
    }
}

fn trait_symbol(decl: &TraitDecl, prefix: &str) -> Symbol {
    Symbol::Trait {
        name: format!("{prefix}{}", decl.name),
        generics: decl.generics.iter().map(|g| g.name.clone()).collect(),
        methods: decl.methods.iter().map(|m| m.name.clone()).collect(),
    }
}

fn impl_symbol(block: &ImplBlock, prefix: &str, map: &SourceMap) -> Symbol {
    Symbol::Impl {
        trait_name: block.trait_path.as_ref().map(path_text),
        owner: format!("{prefix}{}", render(&block.self_ty, map)),
        methods: block.methods.iter().map(|m| m.name.clone()).collect(),
    }
}

fn const_symbol(decl: &ConstDecl, prefix: &str, map: &SourceMap) -> Symbol {
    Symbol::Const {
        name: format!("{prefix}{}", decl.name),
        ty: decl.ty.as_ref().map(|t| render(t, map)).unwrap_or_default(),
    }
}

fn alias_symbol(alias: &TypeAlias, prefix: &str, map: &SourceMap) -> Symbol {
    Symbol::Alias {
        name: format!("{prefix}{}", alias.name),
        generics: alias.generics.iter().map(|g| g.name.clone()).collect(),
        ty: render(&alias.ty, map),
    }
}

/// Renders a type expression as one line of text.
///
/// Written structurally rather than by slicing the source text. A path segment's
/// span covers only its name -- `Pair<i64>` spans the four characters of `Pair`
/// -- so slicing gives the name and drops the arguments. Building the text from
/// the tree gives what the reader wrote, whatever the spans are.
fn render(ty: &TypeExpr, map: &SourceMap) -> String {
    match ty {
        TypeExpr::Path(path) => path_text(path),
        TypeExpr::Ref { mutable, inner, .. } => {
            format!("&{}{}", if *mutable { "mut " } else { "" }, render(inner, map))
        }
        TypeExpr::Ptr { mutable, inner, .. } => {
            format!("*{}{}", if *mutable { "mut " } else { "const " }, render(inner, map))
        }
        TypeExpr::Array { elem, len, .. } => {
            format!("[{}; {}]", render(elem, map), expr_text(len, map))
        }
        TypeExpr::Slice { elem, .. } => format!("[{}]", render(elem, map)),
        TypeExpr::Tuple { elems, .. } => {
            let listed: Vec<String> = elems.iter().map(|e| render(e, map)).collect();
            // A one-element tuple needs its trailing comma or it reads as a
            // parenthesised type.
            if listed.len() == 1 {
                format!("({},)", listed[0])
            } else {
                format!("({})", listed.join(", "))
            }
        }
        TypeExpr::Fn { params, ret, .. } => {
            let listed: Vec<String> = params.iter().map(|p| render(p, map)).collect();
            match ret {
                Some(ret) => format!("fn({}) -> {}", listed.join(", "), render(ret, map)),
                None => format!("fn({})", listed.join(", ")),
            }
        }
        TypeExpr::ImplTrait { bounds, .. } => {
            format!("impl {}", bounds.iter().map(path_text).collect::<Vec<_>>().join(" + "))
        }
        TypeExpr::DynTrait { bounds, .. } => {
            format!("dyn {}", bounds.iter().map(path_text).collect::<Vec<_>>().join(" + "))
        }
        TypeExpr::Infer { .. } => "_".to_string(),
        TypeExpr::Never { .. } => "!".to_string(),
    }
}

/// Array lengths are expressions, and only their source text is interesting
/// here, so they are rendered from the span rather than interpreted.
fn expr_text(expr: &crate::ast::Expr, map: &SourceMap) -> String {
    map.file(expr.span.file).span_text(expr.span).split_whitespace().collect::<Vec<_>>().join(" ")
}

/// `T` for an unconstrained parameter, `T: Size + Copy` for a bounded one.
///
/// The bound is part of what a caller needs to know: it says what a caller must
/// provide, and dropping it would describe a different function.
fn generic_text(param: &crate::ast::GenericParam) -> String {
    if param.bounds.is_empty() {
        return param.name.clone();
    }
    let bounds: Vec<String> = param.bounds.iter().map(path_text).collect();
    format!("{}: {}", param.name, bounds.join(" + "))
}

fn path_text(path: &crate::ast::Path) -> String {
    let mut out = String::new();
    for (i, segment) in path.segments.iter().enumerate() {
        if i > 0 {
            out.push_str("::");
        }
        out.push_str(&segment.name);
        if !segment.args.is_empty() || !segment.bindings.is_empty() {
            let mut listed: Vec<String> =
                segment.args.iter().map(|a| render(a, &SourceMap::new())).collect();
            listed.extend(segment.bindings.iter().map(|b| {
                format!("{} = {}", b.name, render(&b.ty, &SourceMap::new()))
            }));
            out.push('<');
            out.push_str(&listed.join(", "));
            out.push('>');
        }
    }
    out
}

/// Renders the symbols as one JSON document.
///
/// Written by hand rather than through a serializer so that the field order is
/// fixed and the output is stable enough to compare in a test.
pub fn to_json(symbols: &[Symbol]) -> String {
    let mut out = String::with_capacity(symbols.len() * 96 + 32);
    out.push_str("{\"version\":");
    out.push_str(&VERSION.to_string());
    out.push_str(",\"symbols\":[");
    for (i, symbol) in symbols.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(&symbol_json(symbol));
    }
    out.push_str("]}");
    out
}

fn symbol_json(symbol: &Symbol) -> String {
    match symbol {
        Symbol::Fn {
            name,
            owner,
            trait_name,
            generics,
            receiver,
            params,
            ret,
            is_extern,
            link_name,
        } => {
            let mut s = format!("{{\"kind\":\"fn\",\"name\":{}", json_str(name));
            s.push_str(",\"owner\":");
            s.push_str(&opt_json(owner.as_deref()));
            s.push_str(",\"trait\":");
            s.push_str(&opt_json(trait_name.as_deref()));
            s.push_str(",\"generics\":");
            s.push_str(&str_array(generics));
            s.push_str(",\"receiver\":");
            s.push_str(&opt_json(receiver.as_deref()));
            s.push_str(",\"params\":");
            s.push_str(&pair_array(params));
            s.push_str(",\"ret\":");
            s.push_str(&opt_json(ret.as_deref()));
            s.push_str(",\"extern\":");
            s.push_str(if *is_extern { "true" } else { "false" });
            s.push_str(",\"link_name\":");
            s.push_str(&opt_json(link_name.as_deref()));
            s.push('}');
            s
        }
        Symbol::Struct { name, generics, fields } => {
            let fields: Vec<(String, String)> =
                fields.iter().map(|(n, t, _)| (n.clone(), t.clone())).collect();
            format!(
                "{{\"kind\":\"struct\",\"name\":{},\"generics\":{},\"fields\":{}}}",
                json_str(name),
                str_array(generics),
                pair_array(&fields)
            )
        }
        Symbol::Enum { name, generics, variants } => {
            let listed: Vec<String> = variants
                .iter()
                .map(|v| {
                    format!(
                        "{{\"name\":{},\"tuple\":{},\"struct\":{}}}",
                        json_str(&v.name),
                        str_array(&v.tuple),
                        pair_array(&v.struct_fields)
                    )
                })
                .collect();
            format!(
                "{{\"kind\":\"enum\",\"name\":{},\"generics\":{},\"variants\":[{}]}}",
                json_str(name),
                str_array(generics),
                listed.join(",")
            )
        }
        Symbol::Trait { name, generics, methods } => format!(
            "{{\"kind\":\"trait\",\"name\":{},\"generics\":{},\"methods\":{}}}",
            json_str(name),
            str_array(generics),
            str_array(methods)
        ),
        Symbol::Impl { trait_name, owner, methods } => format!(
            "{{\"kind\":\"impl\",\"trait\":{},\"owner\":{},\"methods\":{}}}",
            opt_json(trait_name.as_deref()),
            json_str(owner),
            str_array(methods)
        ),
        Symbol::Const { name, ty } => {
            format!("{{\"kind\":\"const\",\"name\":{},\"type\":{}}}", json_str(name), json_str(ty))
        }
        Symbol::Alias { name, generics, ty } => format!(
            "{{\"kind\":\"alias\",\"name\":{},\"generics\":{},\"type\":{}}}",
            json_str(name),
            str_array(generics),
            json_str(ty)
        ),
        Symbol::Module { name } => {
            format!("{{\"kind\":\"mod\",\"name\":{}}}", json_str(name))
        }
    }
}

fn json_str(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn opt_json(text: Option<&str>) -> String {
    match text {
        Some(t) => json_str(t),
        None => "null".to_string(),
    }
}

fn str_array(items: &[String]) -> String {
    let listed: Vec<String> = items.iter().map(|s| json_str(s)).collect();
    format!("[{}]", listed.join(","))
}

fn pair_array(pairs: &[(String, String)]) -> String {
    let listed: Vec<String> = pairs
        .iter()
        .map(|(a, b)| format!("[{},{}]", json_str(a), json_str(b)))
        .collect();
    format!("[{}]", listed.join(","))
}

/// Renders the symbols as a table a person reads in a terminal.
pub fn to_text(symbols: &[Symbol]) -> String {
    let mut out = String::new();
    for symbol in symbols {
        match symbol {
            Symbol::Fn { name, generics, receiver, params, ret, is_extern, link_name, .. } => {
                out.push_str("fn ");
                out.push_str(name);
                if !generics.is_empty() {
                    out.push('<');
                    out.push_str(&generics.join(", "));
                    out.push('>');
                }
                out.push('(');
                let mut args: Vec<String> = Vec::new();
                if let Some(r) = receiver {
                    args.push(r.clone());
                }
                args.extend(params.iter().map(|(n, t)| format!("{n}: {t}")));
                out.push_str(&args.join(", "));
                out.push(')');
                if let Some(r) = ret {
                    out.push_str(" -> ");
                    out.push_str(r);
                }
                if *is_extern {
                    out.push_str(" [extern");
                    if let Some(symbol) = link_name {
                        out.push_str(" \"");
                        out.push_str(symbol);
                        out.push('"');
                    }
                    out.push(']');
                }
                out.push('\n');
            }
            Symbol::Struct { name, generics, fields } => {
                out.push_str("struct ");
                out.push_str(name);
                if !generics.is_empty() {
                    out.push('<');
                    out.push_str(&generics.join(", "));
                    out.push('>');
                }
                out.push_str(" { ");
                let listed: Vec<String> = fields
                    .iter()
                    .map(|(n, t, _)| format!("{n}: {t}"))
                    .collect();
                out.push_str(&listed.join(", "));
                out.push_str(" }\n");
            }
            Symbol::Enum { name, generics, variants } => {
                out.push_str("enum ");
                out.push_str(name);
                if !generics.is_empty() {
                    out.push('<');
                    out.push_str(&generics.join(", "));
                    out.push('>');
                }
                out.push_str(" { ");
                let listed: Vec<String> = variants
                    .iter()
                    .map(|v| {
                        if !v.tuple.is_empty() {
                            format!("{}({})", v.name, v.tuple.join(", "))
                        } else {
                            v.name.clone()
                        }
                    })
                    .collect();
                out.push_str(&listed.join(", "));
                out.push_str(" }\n");
            }
            Symbol::Trait { name, generics, methods } => {
                out.push_str("trait ");
                out.push_str(name);
                if !generics.is_empty() {
                    out.push('<');
                    out.push_str(&generics.join(", "));
                    out.push('>');
                }
                out.push_str(" { ");
                out.push_str(&methods.join(", "));
                out.push_str(" }\n");
            }
            Symbol::Impl { trait_name, owner, methods } => {
                out.push_str("impl ");
                if let Some(t) = trait_name {
                    out.push_str(t);
                    out.push_str(" for ");
                }
                out.push_str(owner);
                out.push_str(" { ");
                out.push_str(&methods.join(", "));
                out.push_str(" }\n");
            }
            Symbol::Const { name, ty } => {
                out.push_str("const ");
                out.push_str(name);
                out.push_str(": ");
                out.push_str(ty);
                out.push('\n');
            }
            Symbol::Module { name } => {
                out.push_str("mod ");
                out.push_str(name);
                out.push('\n');
            }
            Symbol::Alias { name, generics, ty } => {
                out.push_str("type ");
                out.push_str(name);
                if !generics.is_empty() {
                    out.push('<');
                    out.push_str(&generics.join(", "));
                    out.push('>');
                }
                out.push_str(" = ");
                out.push_str(ty);
                out.push('\n');
            }
        }
    }
    out
}
