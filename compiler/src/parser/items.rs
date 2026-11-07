//! Parser: top-level items and type expressions. (`impl Parser` block.)

use crate::ast::*;
use crate::diagnostics::Diagnostic;
use crate::token::{Keyword, TokenKind};

use super::Parser;

impl Parser {
    pub fn parse_program(&mut self) -> Program {
        let start = self.span();
        let mut items = Vec::new();
        while !self.at_eof() {
            match self.parse_item() {
                Some(item) => items.push(item),
                None => {
                    self.recover_item();
                }
            }
        }
        let end = self.prev_span();
        Program { items, span: start.to(end) }
    }

    pub fn parse_item(&mut self) -> Option<Item> {
        let start = self.span();
        // Attributes.
        let mut attrs = Vec::new();
        while self.at(&TokenKind::Hash) {
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

        let item = match self.kind().clone() {
            TokenKind::Keyword(Keyword::Fn) => {
                let fn_span = self.span();
                self.advance();
                let mut f = self.parse_fn_body(false)?;
                f.is_extern = is_extern;
                f.fn_span = fn_span;
                f.extern_span = extern_span;
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

    // ---- functions ---------------------------------------------------------

    /// Parses `name<...>(params) -> Ret { body }` after `fn` is consumed.
    fn parse_fn_body(&mut self, method: bool) -> Option<FnDecl> {
        let (name, name_span) = self.expect_ident("E0110", "after `fn`")?;
        let generics = self.parse_generic_params();
        let mut self_param = None;
        let mut params = Vec::new();
        if self.eat(&TokenKind::LParen) {
            if !self.at(&TokenKind::RParen) {
                loop {
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
        Some(FnDecl {
            name,
            name_span,
            generics,
            params,
            self_param,
            ret,
            body,
            attrs: Vec::new(),
            is_extern: false,
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
        Some(StructDecl { name, name_span, generics, fields, attrs: Vec::new(), span: name_span.to(end) })
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
        Some(EnumDecl { name, name_span, generics, variants, attrs: Vec::new(), span: name_span.to(end) })
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
                if self.at_keyword(Keyword::Fn) {
                    self.advance();
                    match self.parse_fn_body(true) {
                        Some(f) => methods.push(f),
                        None => break,
                    }
                } else {
                    self.error("E0152", "expected `fn` in trait body", self.span());
                    self.recover_item();
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
                if self.at_keyword(Keyword::Fn) {
                    self.advance();
                    match self.parse_fn_body(true) {
                        Some(f) => methods.push(f),
                        None => break,
                    }
                } else {
                    self.error("E0162", "expected `fn` in impl body", self.span());
                    self.recover_item();
                }
            }
            self.expect(TokenKind::RBrace, "E0163", "impl body");
        }
        let end = self.prev_span();
        Some(ImplBlock { generics, trait_path, self_ty, methods, span: start.to(end) })
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
        Some(ConstDecl { name, name_span, ty, value, attrs: Vec::new(), span: name_span.to(end) })
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
                    Some((name, sp)) => segs.push(PathSegment { name, span: sp, args: Vec::new() }),
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
            while !self.at(&TokenKind::RBrace) {
                if self.at_eof() {
                    break;
                }
                match self.parse_item() {
                    Some(i) => list.push(i),
                    None => {
                        self.recover_item();
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
                let span = if self.eat(&TokenKind::Semi) {
                    let len = self.parse_expr();
                    let end = self.expect(TokenKind::RBracket, "E0220", "array type").unwrap_or(len.span);
                    TypeExpr::Array { elem, len: Box::new(len), span: start.to(end) }
                } else {
                    let end = self.expect(TokenKind::RBracket, "E0221", "slice type").unwrap_or(elem.span());
                    TypeExpr::Slice { elem, span: start.to(end) }
                };
                span
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
            if self.at(&TokenKind::Lt) {
                self.advance();
                while !self.at(&TokenKind::Gt) {
                    if self.at_eof() {
                        break;
                    }
                    args.push(self.parse_type());
                    if !self.eat(&TokenKind::Comma) {
                        break;
                    }
                }
                self.expect(TokenKind::Gt, "E0226", "generic arguments");
            }
            segments.push(PathSegment { name, span: sp, args });
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
            if self.at(&TokenKind::Lt) {
                self.advance();
                while !self.at(&TokenKind::Gt) {
                    if self.at_eof() {
                        break;
                    }
                    args.push(self.parse_type());
                    if !self.eat(&TokenKind::Comma) {
                        break;
                    }
                }
                self.expect(TokenKind::Gt, "E0230", "generic arguments");
            }
            segments.push(PathSegment { name, span: sp, args });
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

/// Attaches attributes and visibility onto an item. For Phase 1 the presence
/// of attributes/`pub` is validated but not semantically meaningful yet.
fn apply_attrs(item: &mut Item, attrs: Vec<Attribute>, pub_keyword: bool, p: &mut Parser) {
    let _ = pub_keyword; // visibility semantics arrive with HIR (Phase 2)
    // Phase 1 recognises attribute syntax but implements no attributes yet.
    // Reporting W0010 keeps builds honest instead of silently dropping them.
    for attr in &attrs {
        p.diagnostics.push(
            Diagnostic::warning(
                "W0010",
                format!("attribute `#[{}]` is not implemented yet and will be ignored", attr.name),
            )
            .at(attr.span),
        );
    }
    match item {
        Item::Fn(f) => f.attrs = attrs,
        Item::Struct(s) => s.attrs = attrs,
        Item::Enum(e) => e.attrs = attrs,
        Item::Const(c) => c.attrs = attrs,
        _ => {}
    }
}
