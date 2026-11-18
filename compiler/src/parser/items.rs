//! Parser: top-level items and type expressions. (`impl Parser` block.)

use crate::ast::*;
use crate::diagnostics::Diagnostic;
use crate::source::Span;
use crate::token::{Keyword, TokenKind};

use super::Parser;

impl Parser {
    pub fn parse_program(&mut self) -> Program {
        let start = self.span();
        let mut features = Vec::new();
        // Crate-level inner attributes must precede every item. Several
        // `#![feature(...)]` attributes, and several names in each, are one
        // enabled set; each name is stored separately so a gate diagnostic can
        // point at the exact spelling.
        while self.at(&TokenKind::Hash) && self.nth_kind(1) == &TokenKind::Bang {
            match self.parse_feature_attr() {
                Some(list) => features.extend(list),
                None => continue, // the malformed attribute still consumed its `#!`
            }
        }
        let mut items = Vec::new();
        loop {
            if self.at_eof() && self.pending_items.is_empty() {
                break;
            }
            if !self.pending_items.is_empty() {
                items.append(&mut self.pending_items);
                continue;
            }
            match self.parse_item() {
                Some(item) => items.push(item),
                None => {
                    self.recover_item();
                }
            }
        }
        let end = self.prev_span();
        Program { items, features, span: start.to(end) }
    }

    /// Parses one `#![feature(name, ...)]` at the crate root.
    ///
    /// Malformed input produces a diagnostic and consumes up to the next `]`
    /// (or item boundary), so a missing bracket never desynchronises the item
    /// loop. Unknown names are deliberately *not* validated here: gate
    /// enforcement is a later pass and owns the `E0154`/`E0155`/`E0156` rules.
    fn parse_feature_attr(&mut self) -> Option<Vec<FeatureAttr>> {
        let hash = self.advance(); // #
        self.advance(); // !
        let start = hash.span;
        if !self.eat(&TokenKind::LBracket) {
            self.diagnostics.push(
                Diagnostic::error("E0120", "expected `[` after `#!`")
                    .at(self.span())
                    .with_cause("a crate-level attribute is written `#![feature(name)]`"),
            );
            self.skip_inner_attr();
            return None;
        }
        let (_name, _name_span) = match self.expect_ident("E0121", "for the inner attribute") {
            Some(v) => v,
            None => {
                self.skip_inner_attr();
                return None;
            }
        };
        if !self.eat(&TokenKind::LParen) {
            self.diagnostics.push(
                Diagnostic::error("E0123", "expected `(` after `feature`")
                    .at(self.span())
                    .with_cause("write the gate name as `#![feature(name)]`"),
            );
            self.skip_inner_attr();
            return None;
        }
        let mut out = Vec::new();
        while !self.at(&TokenKind::RParen) {
            if self.at_eof() || self.at(&TokenKind::RBracket) || self.at(&TokenKind::LBrace) {
                break;
            }
            match self.expect_ident("E0121", "for a feature name") {
                Some((fname, fspan)) => out.push(FeatureAttr {
                    name: fname,
                    name_span: fspan,
                    span: start.to(fspan),
                }),
                None => break,
            }
            if !self.eat(&TokenKind::Comma) {
                break;
            }
        }
        self.expect(TokenKind::RParen, "E0123", "feature name list");
        if self.eat(&TokenKind::RBracket) {
            let end = self.prev_span();
            for f in &mut out {
                f.span = start.to(end);
            }
        } else {
            self.diagnostics.push(
                Diagnostic::error("E0124", "malformed inner attribute: expected `]`")
                    .at(self.span())
                    .with_cause("the feature attribute was not closed with `]`"),
            );
            self.skip_inner_attr();
        }
        // An attribute with no names still parsed; nothing to enable.
        Some(out)
    }

    /// Recovers from a broken `#![...]` by advancing to the next closing `]`,
    /// `}` or EOF, whichever comes first.
    fn skip_inner_attr(&mut self) {
        while !self.at_eof() {
            if self.at(&TokenKind::RBracket) {
                self.advance();
                return;
            }
            // Do not swallow the next attribute or item while looking for this
            // one's `]`.
            if self.at(&TokenKind::RBrace) || self.at(&TokenKind::LBrace) || self.at(&TokenKind::Hash) {
                return;
            }
            if matches!(
                self.kind(),
                TokenKind::Keyword(
                    Keyword::Fn
                        | Keyword::Struct
                        | Keyword::Enum
                        | Keyword::Trait
                        | Keyword::Impl
                        | Keyword::Const
                        | Keyword::Use
                        | Keyword::Mod
                        | Keyword::Type
                        | Keyword::Extern
                )
            ) {
                return;
            }
            self.advance();
        }
    }

    /// Consumes a contextual `async` when it is immediately followed by `fn`.
    ///
    /// `async` is deliberately *not* in [`Keyword`]: every other use of it stays
    /// an ordinary identifier, so v1 programs like `let async = 1` keep working.
    fn eat_contextual_async(&mut self) -> bool {
        if matches!(self.kind(), TokenKind::Ident(s) if s == "async")
            && matches!(self.nth_kind(1), TokenKind::Keyword(Keyword::Fn))
        {
            self.advance();
            true
        } else {
            false
        }
    }

    pub fn parse_item(&mut self) -> Option<Item> {
        let start = self.span();
        // Attributes. An inner `#![...]` is only legal at the crate root, where
        // `parse_program` consumes it before reaching this loop.
        let mut attrs = Vec::new();
        while self.at(&TokenKind::Hash) {
            if self.nth_kind(1) == &TokenKind::Bang {
                self.error(
                    "E0100",
                    "an inner attribute is only allowed at the crate root",
                    self.span(),
                );
                self.parse_feature_attr();
                continue;
            }
            match self.parse_attribute() {
                Some(a) => attrs.push(a),
                None => break,
            }
        }
        let pub_keyword = self.eat_keyword(Keyword::Pub);
        // `extern fn name(...) -> T` declares a function implemented elsewhere.
        // The keyword is required rather than inferred from a missing body, so
        // that forgetting a body is reported instead of silently becoming a
        // declaration of a symbol that does not exist.
        let extern_span = if self.at_keyword(Keyword::Extern) {
            let span = self.span();
            self.advance();
            Some(span)
        } else {
            None
        };
        let is_extern = extern_span.is_some();

        // An ABI string may precede either a grouped block or a single
        // declaration: `extern "C" { ... }` and `extern "C" fn f();`.
        let abi_name = if is_extern {
            if let TokenKind::Str(s) = self.kind().clone() {
                let next_is_block = self.nth_kind(1) == &TokenKind::LBrace;
                let next_is_fn = matches!(self.nth_kind(1), TokenKind::Keyword(Keyword::Fn));
                if next_is_block || next_is_fn {
                    self.advance();
                    Some(s)
                } else {
                    None
                }
            } else {
                None
            }
        } else {
            None
        };

        // v2 `extern "C" { ... }` (the ABI may be omitted, defaulting to "C").
        // The block expands to several ordinary items; only the first is
        // returned, the rest wait in `pending_items` for the item loop.
        if is_extern && self.at(&TokenKind::LBrace) {
            return self.parse_extern_block(extern_span, abi_name.unwrap_or_else(|| "C".into()), attrs, pub_keyword);
        }

        let is_async = self.eat_contextual_async();

        let item = match self.kind().clone() {
            TokenKind::Keyword(Keyword::Fn) => {
                let fn_span = self.span();
                self.advance();
                // `extern fn "symbol" name(..)` names the linker symbol separately
                // from the UMAY name. The prelude needs this: it declares `read` and
                // `write` to reach the C library, and a program is free to have its
                // own `read`. Without a separate symbol the prelude would occupy
                // those names for every program.
                let link_name = if is_extern {
                    if let TokenKind::Str(symbol) = self.kind().clone() {
                        let span = self.span();
                        self.advance();
                        Some((symbol, span))
                    } else {
                        None
                    }
                } else {
                    None
                };
                let mut f = self.parse_fn_body(false, is_extern)?;
                f.is_extern = is_extern;
                f.is_async = is_async;
                f.fn_span = fn_span;
                f.extern_span = extern_span;
                if let Some((symbol, span)) = link_name.clone() {
                    f.link_name = Some(symbol);
                    f.link_name_span = Some(span);
                }
                Item::Fn(f)
            }
            _ if is_extern => {
                let span = self.span();
                self.error("E0110", "`extern` must be followed by an `fn` declaration", span);
                return None;
            }
            TokenKind::Keyword(Keyword::Struct) => {
                self.advance();
                let s = self.parse_struct()?;
                Item::Struct(s)
            }
            TokenKind::Keyword(Keyword::Enum) => {
                self.advance();
                let e = self.parse_enum()?;
                Item::Enum(e)
            }
            TokenKind::Keyword(Keyword::Trait) => {
                self.advance();
                let t = self.parse_trait()?;
                Item::Trait(t)
            }
            TokenKind::Keyword(Keyword::Impl) => {
                self.advance();
                let i = self.parse_impl()?;
                Item::Impl(i)
            }
            TokenKind::Keyword(Keyword::Const) => {
                self.advance();
                let c = self.parse_const()?;
                Item::Const(c)
            }
            TokenKind::Keyword(Keyword::Use) => {
                self.advance();
                let u = self.parse_use()?;
                Item::Use(u)
            }
            TokenKind::Keyword(Keyword::Mod) => {
                self.advance();
                let m = self.parse_mod()?;
                Item::Mod(m)
            }
            TokenKind::Keyword(Keyword::Type) => {
                self.advance();
                let t = self.parse_type_alias()?;
                Item::TypeAlias(t)
            }
            TokenKind::Eof => return None,
            other => {
                let found = other.describe();
                let sp = self.span();
                self.diagnostics.push(
                    Diagnostic::error("E0100", format!("expected an item (fn, struct, enum, trait, ...), found {found}"))
                        .at(sp)
                        .with_cause("a declaration must start with a keyword such as `fn`, `struct`, `enum` or `const`"),
                );
                return None;
            }
        };

        let mut item = item;
        // Attach attributes/visibility to the item that supports them.
        apply_attrs(&mut item, attrs, pub_keyword, self);
        if let (Item::Fn(f), Some(abi)) = (&mut item, abi_name) {
            f.attrs.push(Attribute {
                name: "abi".into(),
                args: vec![abi],
                span: start,
            });
        }
        let _ = start;
        Some(item)
    }

    fn parse_attribute(&mut self) -> Option<Attribute> {
        // consume '#'
        self.advance();
        let start = self.prev_span();
        if !self.eat(&TokenKind::LBracket) {
            self.diagnostics.push(
                Diagnostic::error("E0120", "expected `[` after `#`")
                    .at(self.span())
                    .with_cause("attributes look like `#[name]` or `#[name(arg)]`"),
            );
            return None;
        }
        let (name, _name_span) = self.expect_ident("E0121", "for the attribute")?;
        let mut args = Vec::new();
        if self.eat(&TokenKind::LParen) {
            while !self.at(&TokenKind::RParen) {
                if self.at_eof() {
                    break;
                }
                match self.kind().clone() {
                    TokenKind::Ident(s) => {
                        args.push(s);
                        self.advance();
                    }
                    TokenKind::Str(s) => {
                        args.push(s);
                        self.advance();
                    }
                    TokenKind::Int { value, .. } => {
                        args.push(value.to_string());
                        self.advance();
                    }
                    other => {
                        self.error("E0122", format!("unexpected {} in attribute arguments", other.describe()), self.span());
                        self.advance();
                    }
                }
                if !self.eat(&TokenKind::Comma) {
                    break;
                }
            }
            self.expect(TokenKind::RParen, "E0123", "attribute arguments");
        }
        let end = self.expect(TokenKind::RBracket, "E0124", "attribute")?;
        Some(Attribute { name, args, span: start.to(end) })
    }

    // ---- extern blocks -----------------------------------------------------

    /// Parses the body of `extern "abi" { ... }` into ordinary items.
    ///
    /// `parse_item` returns one item, so the first declaration is returned and
    /// the rest are queued on the parser for the enclosing item loop.
    fn parse_extern_block(
        &mut self,
        extern_span: Option<Span>,
        abi: String,
        attrs: Vec<Attribute>,
        pub_keyword: bool,
    ) -> Option<Item> {
        self.expect(TokenKind::LBrace, "E0110", "extern block");
        let mut decls = Vec::new();
        while !self.at(&TokenKind::RBrace) {
            if self.at_eof() {
                self.error("E0110", "unterminated extern block", self.span());
                break;
            }
            match self.parse_extern_item(&abi, extern_span, &attrs, pub_keyword) {
                Some(item) => decls.push(item),
                None => self.recover_item_in_body(),
            }
        }
        self.expect(TokenKind::RBrace, "E0110", "extern block");
        if decls.is_empty() {
            return None;
        }
        let first = decls.remove(0);
        self.pending_items.extend(decls);
        Some(first)
    }

    /// One declaration inside an extern block, mapped onto an existing item.
    fn parse_extern_item(
        &mut self,
        abi: &str,
        extern_span: Option<Span>,
        attrs: &[Attribute],
        pub_keyword: bool,
    ) -> Option<Item> {
        let start = self.span();
        match self.kind().clone() {
            TokenKind::Keyword(Keyword::Fn) => {
                let fn_span = self.span();
                self.advance();
                let mut f = self.parse_fn_body(false, true)?;
                f.is_extern = true;
                f.fn_span = fn_span;
                f.extern_span = extern_span;
                f.is_pub = pub_keyword;
                f.attrs.extend(attrs.iter().cloned());
                f.attrs.push(Attribute {
                    name: "abi".into(),
                    args: vec![abi.into()],
                    span: start,
                });
                Some(Item::Fn(f))
            }
            TokenKind::Keyword(Keyword::Type) => {
                self.advance();
                let (name, name_span) = self.expect_ident("E0195", "after `type` in an extern block")?;
                self.eat_semi();
                let end = self.prev_span();
                // v1 has no opaque-type item. A fieldless struct is the
                // smallest existing representation that analysis registers as a
                // named type, which is all a pointer to it needs.
                Some(Item::Struct(StructDecl {
                    name,
                    name_span,
                    generics: Vec::new(),
                    fields: Vec::new(),
                    attrs: attrs.to_vec(),
                    is_pub: pub_keyword,
                    span: name_span.to(end),
                }))
            }
            TokenKind::Keyword(Keyword::Static) => {
                self.advance();
                self.eat_keyword(Keyword::Mut);
                let _ = self.expect_ident("E0110", "after `extern static`");
                if self.eat(&TokenKind::Colon) {
                    let _ = self.parse_type();
                }
                self.eat_semi();
                self.error("E0110", "`extern static` declarations are not supported yet", start);
                None
            }
            other => {
                self.error("E0110", format!("unexpected {} in extern block", other.describe()), self.span());
                None
            }
        }
    }

    // ---- functions ---------------------------------------------------------

    /// Parses `name<...>(params) -> Ret { body }` after `fn` is consumed.
    fn parse_fn_body(&mut self, method: bool, allow_variadic: bool) -> Option<FnDecl> {
        let (name, name_span) = self.expect_ident("E0110", "after `fn`")?;
        let generics = self.parse_generic_params();
        let mut self_param = None;
        let mut params = Vec::new();
        let mut variadic = false;
        if self.eat(&TokenKind::LParen) {
            if !self.at(&TokenKind::RParen) {
                loop {
                    // `...` lexes as `..` + `.`. It is terminal in an extern
                    // declaration and needs at least one fixed parameter.
                    if allow_variadic
                        && self.at(&TokenKind::DotDot)
                        && self.nth_kind(1) == &TokenKind::Dot
                    {
                        self.advance();
                        self.advance();
                        variadic = true;
                        break;
                    }
                    if self.at_keyword(Keyword::SelfValue) && self.nth_kind(1) == &TokenKind::RParen || self.at_keyword(Keyword::SelfValue) && self.nth_kind(1) == &TokenKind::Comma {
                        // `self`, `&self`, `&mut self`
                        let sp = self.span();
                        self.advance();
                        self_param = Some(SelfParam { mutable: false, by_ref: None, span: sp });
                    } else if self.at(&TokenKind::Amp) && self.nth_kind(1) == &TokenKind::Keyword(Keyword::Mut) && self.nth_kind(2) == &TokenKind::Keyword(Keyword::SelfValue) {
                        let sp = self.span().to(self.nth(2).span);
                        self.advance();
                        self.advance();
                        self.advance();
                        self_param = Some(SelfParam { mutable: true, by_ref: Some(true), span: sp });
                    } else if self.at(&TokenKind::Amp) && self.nth_kind(1) == &TokenKind::Keyword(Keyword::SelfValue) {
                        let sp = self.span().to(self.nth(1).span);
                        self.advance();
                        self.advance();
                        self_param = Some(SelfParam { mutable: false, by_ref: Some(false), span: sp });
                    } else {
                        match self.parse_param() {
                            Some(p) => params.push(p),
                            None => break,
                        }
                    }
                    if !self.eat(&TokenKind::Comma) {
                        break;
                    }
                    if self.at(&TokenKind::RParen) {
                        break;
                    }
                }
            }
            self.expect(TokenKind::RParen, "E0111", "parameter list");
        }
        let mut ret = None;
        if self.eat(&TokenKind::Arrow) {
            ret = Some(self.parse_type());
        }
        let body = if self.at(&TokenKind::LBrace) {
            Some(self.parse_block())
        } else if !method {
            self.eat_semi();
            None
        } else {
            None
        };
        let end = if body.is_some() { self.prev_span() } else { self.span() };
        let mut attrs = Vec::new();
        if variadic {
            attrs.push(Attribute { name: "variadic".into(), args: Vec::new(), span: name_span });
        }
        Some(FnDecl {
            name,
            name_span,
            generics,
            params,
            self_param,
            ret,
            body,
            attrs,
            is_extern: false,
            is_pub: false, // free functions are set by `apply_attrs`, methods by the impl parser
            is_async: false, // set by the caller when a contextual `async` preceded `fn`
            link_name: None,
            link_name_span: None,
            fn_span: name_span,
            extern_span: None,
            span: name_span.to(end),
        })
    }

    fn parse_param(&mut self) -> Option<Param> {
        let start = self.span();
        let (name, name_span) = match self.kind().clone() {
            TokenKind::Ident(name) => {
                let sp = self.advance().span;
                (Some(name), sp)
            }
            TokenKind::Underscore => {
                let sp = self.advance().span;
                (None, sp)
            }
            other => {
                let found = other.describe();
                self.diagnostics.push(
                    Diagnostic::error("E0112", format!("expected a parameter name, found {found}"))
                        .at(self.span())
                        .with_cause("function parameters are written `name: Type`"),
                );
                return None;
            }
        };
        self.expect(TokenKind::Colon, "E0113", "parameter type annotation");
        let ty = self.parse_type();
        let end = ty.span();
        Some(Param { name, name_span, ty, span: start.to(end) })
    }

    fn parse_generic_params(&mut self) -> Vec<GenericParam> {
        let mut out = Vec::new();
        if !self.eat(&TokenKind::Lt) {
            return out;
        }
        self.split_closing_angles();
        while !self.at(&TokenKind::Gt) {
            if self.at_eof() {
                break;
            }
            match self.kind().clone() {
                TokenKind::Ident(name) => {
                    let sp = self.advance().span;
                    let bounds = if self.eat(&TokenKind::Colon) {
                        self.parse_trait_bounds()
                    } else {
                        Vec::new()
                    };
                    out.push(GenericParam { name, span: sp, bounds });
                }
                other => {
                    self.error("E0114", format!("expected a generic parameter name, found {}", other.describe()), self.span());
                    self.advance();
                }
            }
            if !self.eat(&TokenKind::Comma) {
                break;
            }
        }
        self.expect(TokenKind::Gt, "E0115", "generic parameter list");
        out
    }

    fn parse_trait_bounds(&mut self) -> Vec<Path> {
        let mut bounds = Vec::new();
        loop {
            if let Some(p) = self.parse_path() {
                bounds.push(p);
            }
            if !self.eat(&TokenKind::Plus) {
                break;
            }
        }
        bounds
    }

    // ---- struct / enum / trait / impl --------------------------------------

    fn parse_struct(&mut self) -> Option<StructDecl> {
        let (name, name_span) = self.expect_ident("E0130", "after `struct`")?;
        let generics = self.parse_generic_params();
        let mut fields = Vec::new();
        if self.eat(&TokenKind::LBrace) {
            while !self.at(&TokenKind::RBrace) {
                if self.at_eof() {
                    self.error("E0131", "unexpected end of file in struct body", self.span());
                    break;
                }
                match self.parse_field() {
                    Some(f) => fields.push(f),
                    None => break,
                }
                if !self.eat(&TokenKind::Comma) {
                    break;
                }
            }
            self.expect(TokenKind::RBrace, "E0132", "struct body");
        }
        let end = self.prev_span();
        Some(StructDecl { name, name_span, generics, fields, attrs: Vec::new(), is_pub: false, span: name_span.to(end) })
    }

    fn parse_field(&mut self) -> Option<FieldDecl> {
        let start = self.span();
        let mutable = self.eat_keyword(Keyword::Mut);
        let (name, name_span) = self.expect_ident("E0133", "for the field")?;
        self.expect(TokenKind::Colon, "E0134", "field type annotation");
        let ty = self.parse_type();
        let end = ty.span();
        Some(FieldDecl { name, name_span, ty, mutable, span: start.to(end) })
    }

    fn parse_enum(&mut self) -> Option<EnumDecl> {
        let (name, name_span) = self.expect_ident("E0140", "after `enum`")?;
        let generics = self.parse_generic_params();
        let mut variants = Vec::new();
        if self.eat(&TokenKind::LBrace) {
            while !self.at(&TokenKind::RBrace) {
                if self.at_eof() {
                    self.error("E0141", "unexpected end of file in enum body", self.span());
                    break;
                }
                match self.parse_enum_variant() {
                    Some(v) => variants.push(v),
                    None => break,
                }
                if !self.eat(&TokenKind::Comma) {
                    break;
                }
            }
            self.expect(TokenKind::RBrace, "E0142", "enum body");
        }
        let end = self.prev_span();
        Some(EnumDecl { name, name_span, generics, variants, attrs: Vec::new(), is_pub: false, span: name_span.to(end) })
    }

    fn parse_enum_variant(&mut self) -> Option<EnumVariant> {
        let (name, name_span) = self.expect_ident("E0143", "for the enum variant")?;
        let mut tuple_fields = None;
        let mut struct_fields = None;
        let mut discriminant = None;
        if self.at(&TokenKind::LParen) {
            self.advance();
            let mut fields = Vec::new();
            while !self.at(&TokenKind::RParen) {
                if self.at_eof() {
                    break;
                }
                fields.push(self.parse_type());
                if !self.eat(&TokenKind::Comma) {
                    break;
                }
            }
            self.expect(TokenKind::RParen, "E0144", "variant tuple fields");
            tuple_fields = Some(fields);
        } else if self.at(&TokenKind::LBrace) {
            self.advance();
            let mut fields = Vec::new();
            while !self.at(&TokenKind::RBrace) {
                if self.at_eof() {
                    break;
                }
                match self.parse_field() {
                    Some(f) => fields.push(f),
                    None => break,
                }
                if !self.eat(&TokenKind::Comma) {
                    break;
                }
            }
            self.expect(TokenKind::RBrace, "E0145", "variant struct fields");
            struct_fields = Some(fields);
        }
        if self.eat(&TokenKind::Eq) {
            discriminant = Some(self.parse_expr());
        }
        let end = self.prev_span();
        Some(EnumVariant { name, name_span, tuple_fields, struct_fields, discriminant, span: name_span.to(end) })
    }

    fn parse_trait(&mut self) -> Option<TraitDecl> {
        let (name, name_span) = self.expect_ident("E0150", "after `trait`")?;
        let generics = self.parse_generic_params();
        let mut methods = Vec::new();
        if self.eat(&TokenKind::LBrace) {
            while !self.at(&TokenKind::RBrace) {
                if self.at_eof() {
                    self.error("E0151", "unexpected end of file in trait body", self.span());
                    break;
                }
                // A method may carry a visibility and attributes, the same as a
                // free function.
                let is_pub = self.skip_method_decorations();
                let is_async = self.eat_contextual_async();
                if self.at_keyword(Keyword::Fn) {
                    self.advance();
                    match self.parse_fn_body(true, false) {
                        Some(mut f) => {
                            f.is_pub = is_pub;
                            f.is_async = is_async;
                            methods.push(f);
                        }
                        None => break,
                    }
                } else {
                    self.error("E0152", "expected `fn` in trait body", self.span());
                    self.recover_item_in_body();
                }
            }
            self.expect(TokenKind::RBrace, "E0153", "trait body");
        }
        let end = self.prev_span();
        Some(TraitDecl { name, name_span, generics, methods, span: name_span.to(end) })
    }

    fn parse_impl(&mut self) -> Option<ImplBlock> {
        let start = self.span();
        let generics = self.parse_generic_params();
        let mut trait_path = None;
        let self_ty;
        if let Some(path) = self.parse_path() {
            if self.eat_keyword(Keyword::For) {
                trait_path = Some(path);
                self_ty = self.parse_type();
            } else {
                self_ty = TypeExpr::Path(path);
            }
        } else {
            self.error("E0160", "expected a type after `impl`", self.span());
            return None;
        }
        let mut methods = Vec::new();
        if self.eat(&TokenKind::LBrace) {
            while !self.at(&TokenKind::RBrace) {
                if self.at_eof() {
                    self.error("E0161", "unexpected end of file in impl body", self.span());
                    break;
                }
                // A method may carry a visibility and attributes, the same as a
                // free function. They meant nothing to the impl body until now,
                // so `pub fn` inside an `impl` was a parse error -- in a module,
                // where a method is normally written `pub`, that made the
                // ordinary way to write an `impl` unusable.
                let is_pub = self.skip_method_decorations();
                let is_async = self.eat_contextual_async();
                if self.at_keyword(Keyword::Fn) {
                    self.advance();
                    match self.parse_fn_body(true, false) {
                        Some(mut f) => {
                            f.is_pub = is_pub;
                            f.is_async = is_async;
                            methods.push(f);
                        }
                        None => break,
                    }
                } else {
                    self.error("E0162", "expected `fn` in impl body", self.span());
                    self.recover_item_in_body();
                }
            }
            self.expect(TokenKind::RBrace, "E0163", "impl body");
        }
        let end = self.prev_span();
        Some(ImplBlock { generics, trait_path, self_ty, methods, span: start.to(end) })
    }

    /// Consumes the attributes and visibility that may precede a method,
    /// returning whether it was `pub`.
    fn skip_method_decorations(&mut self) -> bool {
        // `parse_attribute` consumes a `#` before looking, so it is only called
        // when one is there -- otherwise every non-attribute token after a method
        // became "expected `[` after `#`".
        while self.at(&TokenKind::Hash) && self.parse_attribute().is_some() {}
        self.eat_keyword(Keyword::Pub)
    }

    fn parse_const(&mut self) -> Option<ConstDecl> {
        let (name, name_span) = self.expect_ident("E0170", "after `const`")?;
        let mut ty = None;
        if self.eat(&TokenKind::Colon) {
            ty = Some(self.parse_type());
        }
        self.expect(TokenKind::Eq, "E0171", "const initializer");
        let value = self.parse_expr();
        self.eat_semi();
        let end = self.prev_span();
        Some(ConstDecl { name, name_span, ty, value, attrs: Vec::new(), is_pub: false, span: name_span.to(end) })
    }

    fn parse_use(&mut self) -> Option<UseDecl> {
        let start = self.span();
        let mut subtree = false;
        let mut alias = None;
        let mut path = match self.parse_path() {
            Some(p) => p,
            None => {
                self.error("E0180", "expected a path after `use`", self.span());
                return None;
            }
        };
        if self.eat(&TokenKind::ColonColon) && self.at(&TokenKind::LBrace) {
            // use a::b::{c, d}
            self.advance(); // {
            let mut segs = Vec::new();
            while !self.at(&TokenKind::RBrace) {
                if self.at_eof() {
                    break;
                }
                match self.expect_ident("E0181", "in `use` list") {
                    Some((name, sp)) => segs.push(PathSegment { name, span: sp, args: Vec::new(), bindings: Vec::new() }),
                    None => break,
                }
                if !self.eat(&TokenKind::Comma) {
                    break;
                }
            }
            self.expect(TokenKind::RBrace, "E0182", "`use` list");
            // A grouped use is simplified into a single segment path for now.
            path.segments.extend(segs);
            subtree = true;
        }
        if self.eat_keyword(Keyword::As) {
            alias = Some(self.expect_ident("E0183", "after `as`")?.0);
        }
        let end = self.prev_span();
        Some(UseDecl { path, alias, subtree, span: start.to(end) })
    }

    fn parse_mod(&mut self) -> Option<ModDecl> {
        let (name, name_span) = self.expect_ident("E0190", "after `mod`")?;
        let mut items = None;
        if self.at(&TokenKind::LBrace) {
            self.advance();
            let mut list = Vec::new();
            loop {
                if !self.pending_items.is_empty() {
                    list.append(&mut self.pending_items);
                    continue;
                }
                if self.at(&TokenKind::RBrace) || self.at_eof() {
                    break;
                }
                match self.parse_item() {
                    Some(i) => list.push(i),
                    None => {
                        self.recover_item_in_body();
                    }
                }
            }
            self.expect(TokenKind::RBrace, "E0191", "module body");
            items = Some(list);
        } else {
            self.eat_semi();
        }
        let end = self.prev_span();
        Some(ModDecl { name, name_span, items, span: name_span.to(end) })
    }

    fn parse_type_alias(&mut self) -> Option<TypeAlias> {
        let (name, name_span) = self.expect_ident("E0195", "after `type`")?;
        let generics = self.parse_generic_params();
        self.expect(TokenKind::Eq, "E0196", "type alias");
        let ty = self.parse_type();
        self.eat_semi();
        let end = self.prev_span();
        Some(TypeAlias { name, name_span, generics, ty, span: name_span.to(end) })
    }

    // ---- type expressions ---------------------------------------------------

    pub fn parse_type(&mut self) -> TypeExpr {
        let start = self.span();
        let t = match self.kind().clone() {
            TokenKind::Amp => {
                self.advance();
                let mutable = self.eat_keyword(Keyword::Mut);
                let inner = Box::new(self.parse_type());
                let end = inner.span();
                TypeExpr::Ref { mutable, inner, span: start.to(end) }
            }
            TokenKind::Star => {
                self.advance();
                // `*mut T` and `*const T`. The qualifier is required: a bare `*T`
                // would leave the pointer's mutability unstated, and the two forms
                // print differently in diagnostics.
                let mutable = if self.eat_keyword(Keyword::Mut) {
                    true
                } else if self.eat_keyword(Keyword::Const) {
                    false
                } else {
                    let span = self.span();
                    self.error(
                        "E0224",
                        "a raw pointer needs `mut` or `const`, as in `*mut u8`",
                        span,
                    );
                    false
                };
                let inner = Box::new(self.parse_type());
                let end = inner.span();
                TypeExpr::Ptr { mutable, inner, span: start.to(end) }
            }
            TokenKind::LBracket => {
                self.advance();
                let elem = Box::new(self.parse_type());
                if self.eat(&TokenKind::Semi) {
                    let len = self.parse_expr();
                    let end = self.expect(TokenKind::RBracket, "E0220", "array type").unwrap_or(len.span);
                    TypeExpr::Array { elem, len: Box::new(len), span: start.to(end) }
                } else {
                    let end = self.expect(TokenKind::RBracket, "E0221", "slice type").unwrap_or(elem.span());
                    TypeExpr::Slice { elem, span: start.to(end) }
                }
            }
            TokenKind::LParen => {
                self.advance();
                let mut elems = Vec::new();
                if !self.at(&TokenKind::RParen) {
                    loop {
                        let t = self.parse_type();
                        let end = t.span();
                        elems.push(t);
                        if !self.eat(&TokenKind::Comma) {
                            break;
                        }
                        let _ = end;
                        if self.at(&TokenKind::RParen) {
                            break;
                        }
                    }
                }
                let end = self.expect(TokenKind::RParen, "E0222", "tuple type").unwrap_or(start);
                TypeExpr::Tuple { elems, span: start.to(end) }
            }
            TokenKind::Keyword(Keyword::Fn) => {
                self.advance();
                let params = if self.eat(&TokenKind::LParen) {
                    let mut ps = Vec::new();
                    if !self.at(&TokenKind::RParen) {
                        loop {
                            ps.push(self.parse_type());
                            if !self.eat(&TokenKind::Comma) {
                                break;
                            }
                            if self.at(&TokenKind::RParen) {
                                break;
                            }
                        }
                    }
                    self.expect(TokenKind::RParen, "E0223", "function type parameters");
                    ps
                } else {
                    Vec::new()
                };
                let ret = if self.eat(&TokenKind::Arrow) {
                    Some(Box::new(self.parse_type()))
                } else {
                    None
                };
                let end = ret.as_ref().map(|r| r.span()).unwrap_or(start);
                TypeExpr::Fn { params, ret, span: start.to(end) }
            }
            TokenKind::Keyword(Keyword::Impl) => {
                self.advance();
                let bounds = self.parse_trait_bounds();
                let end = bounds.last().map(|p| p.span).unwrap_or(start);
                TypeExpr::ImplTrait { bounds, span: start.to(end) }
            }
            TokenKind::Keyword(Keyword::Dyn) => {
                self.advance();
                let bounds = self.parse_trait_bounds();
                let end = bounds.last().map(|p| p.span).unwrap_or(start);
                TypeExpr::DynTrait { bounds, span: start.to(end) }
            }
            TokenKind::Underscore => {
                let sp = self.advance().span;
                TypeExpr::Infer { span: sp }
            }
            TokenKind::Bang => {
                let sp = self.advance().span;
                TypeExpr::Never { span: sp }
            }
            TokenKind::Ident(_) | TokenKind::Keyword(Keyword::SelfType) | TokenKind::ColonColon => self.parse_type_path(),
            other => {
                let found = other.describe();
                let sp = self.span();
                self.diagnostics.push(
                    Diagnostic::error("E0224", format!("expected a type, found {found}"))
                        .at(sp)
                        .with_cause("a type expression was required here"),
                );
                TypeExpr::Path(Path { segments: Vec::new(), span: sp })
            }
        };
        t
    }

    /// Parses the contents of the `<…>` after a path segment name. The caller has
    /// already consumed `<` and split any `>>`. Each entry is either a type
    /// argument (`i64`) or an associated-type binding (`Item = u8`); both are
    /// kept so neither is silently dropped.
    fn parse_generic_args(&mut self, err_code: &'static str) -> (Vec<TypeExpr>, Vec<AssocTypeBinding>) {
        let mut args = Vec::new();
        let mut bindings = Vec::new();
        while !self.at(&TokenKind::Gt) {
            if self.at_eof() {
                break;
            }
            if let TokenKind::Ident(name) = self.kind().clone() {
                if self.nth_kind(1) == &TokenKind::Eq {
                    let name_span = self.advance().span;
                    self.advance(); // `=`
                    let ty = self.parse_type();
                    let span = name_span.to(ty.span());
                    bindings.push(AssocTypeBinding { name, name_span, ty, span });
                    if !self.eat(&TokenKind::Comma) {
                        break;
                    }
                    continue;
                }
            }
            args.push(self.parse_type());
            if !self.eat(&TokenKind::Comma) {
                break;
            }
        }
        self.expect(TokenKind::Gt, err_code, "generic arguments");
        (args, bindings)
    }

    fn parse_type_path(&mut self) -> TypeExpr {
        let start = self.span();
        // Leading '::'
        if self.at(&TokenKind::ColonColon) {
            self.advance();
        }
        let mut segments = Vec::new();
        loop {
            let (name, sp) = match self.kind().clone() {
                TokenKind::Ident(name) => (name, self.advance().span),
                TokenKind::Keyword(Keyword::SelfType) => ("Self".to_string(), self.advance().span),
                TokenKind::Keyword(Keyword::SelfValue) => ("self".to_string(), self.advance().span),
                other => {
                    if segments.is_empty() {
                        let found = other.describe();
                        let sp = self.span();
                        self.diagnostics.push(
                            Diagnostic::error("E0225", format!("expected a type name, found {found}"))
                                .at(sp)
                                .with_cause("a type path must start with a name"),
                        );
                        return TypeExpr::Path(Path { segments, span: start });
                    }
                    break;
                }
            };
            let mut args = Vec::new();
            let mut bindings = Vec::new();
            if self.at(&TokenKind::Lt) {
                self.advance();
                self.split_closing_angles();
                (args, bindings) = self.parse_generic_args("E0226");
            }
            segments.push(PathSegment { name, span: sp, args, bindings });
            if self.eat(&TokenKind::ColonColon) {
                continue;
            }
            break;
        }
        let end = segments.last().map(|s| s.span).unwrap_or(start);
        TypeExpr::Path(Path { segments, span: start.to(end) })
    }

    pub fn parse_path(&mut self) -> Option<Path> {
        let start = self.span();
        if self.at(&TokenKind::ColonColon) {
            self.advance();
        }
        let mut segments = Vec::new();
        loop {
            let (name, sp) = match self.kind().clone() {
                TokenKind::Ident(name) => (name, self.advance().span),
                TokenKind::Keyword(Keyword::SelfType) => ("Self".to_string(), self.advance().span),
                TokenKind::Keyword(Keyword::SelfValue) => ("self".to_string(), self.advance().span),
                _ => break,
            };
            let mut args = Vec::new();
            let mut bindings = Vec::new();
            if self.at(&TokenKind::Lt) {
                self.advance();
                self.split_closing_angles();
                (args, bindings) = self.parse_generic_args("E0230");
            }
            segments.push(PathSegment { name, span: sp, args, bindings });
            if self.eat(&TokenKind::ColonColon) {
                continue;
            }
            break;
        }
        if segments.is_empty() {
            None
        } else {
            let end = segments.last().map(|s| s.span).unwrap_or(start);
            Some(Path { segments, span: start.to(end) })
        }
    }
}

/// A `derive` with only implemented traits is a real attribute, not a stub.
/// Only `Copy` has semantic effect today; there are no `Send`/`Sync` traits in
/// the UMAY source language, so they are deliberately not recognised here.
fn recognized_derive(attr: &Attribute) -> bool {
    attr.name == "derive" && !attr.args.is_empty() && attr.args.iter().all(|a| a == "Copy")
}

/// Attaches attributes and visibility onto an item. Visibility is kept so that
/// semantic analysis can refuse a cross-module reference to a private item.
fn apply_attrs(item: &mut Item, attrs: Vec<Attribute>, pub_keyword: bool, p: &mut Parser) {
    // Implemented attributes are silent; everything else still warns so builds
    // stay honest instead of silently dropping unknown attributes.
    for attr in &attrs {
        if recognized_derive(attr) {
            continue;
        }
        p.diagnostics.push(
            Diagnostic::warning(
                "W0010",
                format!("attribute `#[{}]` is not implemented yet and will be ignored", attr.name),
            )
            .at(attr.span),
        );
    }
    match item {
        Item::Fn(f) => {
            f.attrs = attrs;
            f.is_pub = pub_keyword;
        }
        Item::Struct(s) => {
            s.attrs = attrs;
            s.is_pub = pub_keyword;
        }
        Item::Enum(e) => {
            e.attrs = attrs;
            e.is_pub = pub_keyword;
        }
        Item::Const(c) => {
            c.attrs = attrs;
            c.is_pub = pub_keyword;
        }
        _ => {}
    }
}
