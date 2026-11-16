//! Deterministic random UMAY program generator, reference evaluator, and
//! AST-based shrinker for differential testing of the x86_64 backend.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

// ---------------------------------------------------------------------------
// PRNG
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct Rng {
    state: u64,
}

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self { state: seed.wrapping_add(0x9e37_79b9_7f4a_7c15) }
    }

    pub fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    pub fn gen_range(&mut self, upper: usize) -> usize {
        if upper <= 1 {
            return 0;
        }
        (self.next_u64() % (upper as u64)) as usize
    }

    pub fn gen_bool(&mut self, pct_true: usize) -> bool {
        self.gen_range(100) < pct_true
    }
}

// ---------------------------------------------------------------------------
// Types and Values
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PrimTy {
    I8,
    I16,
    I32,
    I64,
    U8,
    U16,
    U32,
    U64,
    Usize,
    Bool,
}

pub const INT_TYS: [PrimTy; 9] = [
    PrimTy::I8,
    PrimTy::I16,
    PrimTy::I32,
    PrimTy::I64,
    PrimTy::U8,
    PrimTy::U16,
    PrimTy::U32,
    PrimTy::U64,
    PrimTy::Usize,
];

impl PrimTy {
    pub fn is_int(self) -> bool {
        self != PrimTy::Bool
    }

    pub fn is_signed(self) -> bool {
        matches!(self, PrimTy::I8 | PrimTy::I16 | PrimTy::I32 | PrimTy::I64)
    }

    pub fn bits(self) -> u32 {
        match self {
            PrimTy::I8 | PrimTy::U8 => 8,
            PrimTy::I16 | PrimTy::U16 => 16,
            PrimTy::I32 | PrimTy::U32 => 32,
            PrimTy::I64 | PrimTy::U64 | PrimTy::Usize => 64,
            PrimTy::Bool => 1,
        }
    }

    pub fn umay_name(self) -> &'static str {
        match self {
            PrimTy::I8 => "i8",
            PrimTy::I16 => "i16",
            PrimTy::I32 => "i32",
            PrimTy::I64 => "i64",
            PrimTy::U8 => "u8",
            PrimTy::U16 => "u16",
            PrimTy::U32 => "u32",
            PrimTy::U64 => "u64",
            PrimTy::Usize => "usize",
            PrimTy::Bool => "bool",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ty {
    Prim(PrimTy),
    Struct(usize),
    Array(PrimTy, usize),
}

impl Ty {
    pub fn prim(self) -> Option<PrimTy> {
        match self {
            Ty::Prim(p) => Some(p),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Val {
    I8(i8),
    I16(i16),
    I32(i32),
    I64(i64),
    U8(u8),
    U16(u16),
    U32(u32),
    U64(u64),
    Usize(u64),
    Bool(bool),
    Struct(usize, Vec<Val>),
    Array(PrimTy, Vec<Val>),
}

impl Val {
    pub fn ty(&self) -> Ty {
        match self {
            Val::I8(_) => Ty::Prim(PrimTy::I8),
            Val::I16(_) => Ty::Prim(PrimTy::I16),
            Val::I32(_) => Ty::Prim(PrimTy::I32),
            Val::I64(_) => Ty::Prim(PrimTy::I64),
            Val::U8(_) => Ty::Prim(PrimTy::U8),
            Val::U16(_) => Ty::Prim(PrimTy::U16),
            Val::U32(_) => Ty::Prim(PrimTy::U32),
            Val::U64(_) => Ty::Prim(PrimTy::U64),
            Val::Usize(_) => Ty::Prim(PrimTy::Usize),
            Val::Bool(_) => Ty::Prim(PrimTy::Bool),
            Val::Struct(id, _) => Ty::Struct(*id),
            Val::Array(elem, items) => Ty::Array(*elem, items.len()),
        }
    }

    pub fn zero(p: PrimTy) -> Self {
        match p {
            PrimTy::I8 => Val::I8(0),
            PrimTy::I16 => Val::I16(0),
            PrimTy::I32 => Val::I32(0),
            PrimTy::I64 => Val::I64(0),
            PrimTy::U8 => Val::U8(0),
            PrimTy::U16 => Val::U16(0),
            PrimTy::U32 => Val::U32(0),
            PrimTy::U64 => Val::U64(0),
            PrimTy::Usize => Val::Usize(0),
            PrimTy::Bool => Val::Bool(false),
        }
    }

    pub fn one(p: PrimTy) -> Self {
        match p {
            PrimTy::I8 => Val::I8(1),
            PrimTy::I16 => Val::I16(1),
            PrimTy::I32 => Val::I32(1),
            PrimTy::I64 => Val::I64(1),
            PrimTy::U8 => Val::U8(1),
            PrimTy::U16 => Val::U16(1),
            PrimTy::U32 => Val::U32(1),
            PrimTy::U64 => Val::U64(1),
            PrimTy::Usize => Val::Usize(1),
            PrimTy::Bool => Val::Bool(true),
        }
    }

    pub fn from_u64_trunc(p: PrimTy, raw: u64) -> Self {
        match p {
            PrimTy::I8 => Val::I8(raw as i8),
            PrimTy::I16 => Val::I16(raw as i16),
            PrimTy::I32 => Val::I32(raw as i32),
            PrimTy::I64 => Val::I64(raw as i64),
            PrimTy::U8 => Val::U8(raw as u8),
            PrimTy::U16 => Val::U16(raw as u16),
            PrimTy::U32 => Val::U32(raw as u32),
            PrimTy::U64 => Val::U64(raw),
            PrimTy::Usize => Val::Usize(raw),
            PrimTy::Bool => Val::Bool((raw & 1) != 0),
        }
    }

    /// Sign-extends signed types and zero-extends unsigned types to i128.
    pub fn to_i128(&self) -> Option<i128> {
        match *self {
            Val::I8(v) => Some(v as i128),
            Val::I16(v) => Some(v as i128),
            Val::I32(v) => Some(v as i128),
            Val::I64(v) => Some(v as i128),
            Val::U8(v) => Some(v as i128),
            Val::U16(v) => Some(v as i128),
            Val::U32(v) => Some(v as i128),
            Val::U64(v) => Some(v as i128),
            Val::Usize(v) => Some(v as i128),
            Val::Bool(_) | Val::Struct(_, _) | Val::Array(_, _) => None,
        }
    }

    /// Two's-complement bit pattern as u64 (sign-extended for signed types).
    pub fn to_u64_sext(&self) -> Option<u64> {
        self.to_i128().map(|x| x as u64)
    }

    pub fn cast_to(&self, to: PrimTy) -> Option<Val> {
        if let Val::Bool(b) = self {
            return if to == PrimTy::Bool { Some(Val::Bool(*b)) } else { None };
        }
        let raw = self.to_u64_sext()?;
        if to == PrimTy::Bool {
            return None;
        }
        Some(Val::from_u64_trunc(to, raw))
    }
}

// ---------------------------------------------------------------------------
// AST
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnOp {
    Neg,
    Not,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    BitAnd,
    BitOr,
    BitXor,
    Shl,
    Shr,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    And,
    Or,
}

#[derive(Clone, Debug)]
pub enum Expr {
    Lit(Val),
    Var(String),
    Cast {
        expr: Box<Expr>,
        to: PrimTy,
    },
    Unary {
        op: UnOp,
        expr: Box<Expr>,
    },
    Binary {
        op: BinOp,
        lhs: Box<Expr>,
        rhs: Box<Expr>,
    },
    FieldAccess {
        var: String,
        field: usize,
    },
    ArrayIndex {
        var: String,
        index: Box<Expr>,
    },
    Call {
        func: usize,
        /// For `&mut` parameters, `Expr::Var(name)` is passed as `&mut name`.
        args: Vec<Expr>,
    },
    MethodCall {
        receiver: String,
        struct_id: usize,
        method: usize,
        args: Vec<Expr>,
    },
    StructLit {
        struct_id: usize,
        fields: Vec<Expr>,
    },
    ArrayLit {
        elem: PrimTy,
        len: usize,
        init: Box<Expr>,
    },
}

#[derive(Clone, Debug)]
pub enum LValue {
    Var(String),
    Field(String, usize),
    Index(String, Expr),
}

#[derive(Clone, Debug)]
pub enum Stmt {
    Let {
        name: String,
        ty: Ty,
        is_mut: bool,
        init: Expr,
    },
    Assign {
        target: LValue,
        value: Expr,
    },
    If {
        cond: Expr,
        then_body: Vec<Stmt>,
        else_body: Vec<Stmt>,
    },
    While {
        /// Loop counter variable name, initialized to 0 before the loop,
        /// incremented at the start of each iteration, bounded by `max_iters`.
        counter: String,
        max_iters: i32,
        cond: Expr,
        body: Vec<Stmt>,
    },
    Return(Expr),
    Print(Expr),
    Expr(Expr),
}

#[derive(Clone, Debug)]
pub struct Param {
    pub name: String,
    pub ty: Ty,
    pub is_mut_ref: bool,
}

#[derive(Clone, Debug)]
pub struct MethodDef {
    pub name: String,
    pub is_mut_self: bool,
    pub params: Vec<Param>,
    pub ret: PrimTy,
    pub body: Vec<Stmt>,
}

#[derive(Clone, Debug)]
pub struct StructDef {
    pub name: String,
    pub fields: Vec<(String, PrimTy)>,
    pub methods: Vec<MethodDef>,
}

#[derive(Clone, Debug)]
pub struct FuncDef {
    pub name: String,
    pub params: Vec<Param>,
    pub ret: PrimTy,
    pub body: Vec<Stmt>,
}

#[derive(Clone, Debug)]
pub struct Program {
    pub seed: u64,
    pub structs: Vec<StructDef>,
    pub funcs: Vec<FuncDef>,
    pub main_stmts: Vec<Stmt>,
}

// ---------------------------------------------------------------------------
// Generator
// ---------------------------------------------------------------------------

struct ScopeVar {
    name: String,
    ty: Ty,
    is_mut: bool,
    /// True when the variable is a `&mut` parameter inside a function.
    is_mut_ref: bool,
}

struct GenCx<'a> {
    rng: &'a mut Rng,
    structs: Vec<StructDef>,
    funcs: Vec<FuncDef>,
    next_id: usize,
    /// Variables currently borrowed `&mut` in an enclosing call's argument list.
    borrowed: BTreeSet<String>,
}

impl<'a> GenCx<'a> {
    fn fresh_name(&mut self, prefix: &str) -> String {
        let id = self.next_id;
        self.next_id += 1;
        format!("{prefix}_{id}")
    }

    fn rand_prim(&mut self) -> PrimTy {
        if self.rng.gen_bool(12) {
            PrimTy::Bool
        } else {
            self.rand_int()
        }
    }

    fn rand_int(&mut self) -> PrimTy {
        INT_TYS[self.rng.gen_range(INT_TYS.len())]
    }

    fn rand_lit(&mut self, ty: PrimTy) -> Val {
        if ty == PrimTy::Bool {
            return Val::Bool(self.rng.gen_bool(50));
        }
        let special = self.rng.gen_range(12);
        let raw: u64 = match special {
            0 => 0,
            1 => 1,
            2 => u64::MAX, // -1 for signed, MAX for unsigned
            3 => {
                // MIN for signed, or high-bit set for unsigned
                match ty.bits() {
                    8 => 0x80,
                    16 => 0x8000,
                    32 => 0x8000_0000,
                    _ => 0x8000_0000_0000_0000,
                }
            }
            4 => {
                // MAX for signed
                match ty.bits() {
                    8 => 0x7f,
                    16 => 0x7fff,
                    32 => 0x7fff_ffff,
                    _ => 0x7fff_ffff_ffff_ffff,
                }
            }
            5 => {
                // Large i64/u64 outside imm32
                0x1234_5678_9abc_def0u64 ^ (self.rng.next_u64() & 0xffff)
            }
            _ => self.rng.next_u64(),
        };
        Val::from_u64_trunc(ty, raw)
    }

    fn gen_expr(
        &mut self,
        ty: PrimTy,
        vars: &[ScopeVar],
        depth: usize,
        allow_calls: bool,
    ) -> Expr {
        if depth == 0 {
            return self.gen_leaf_expr(ty, vars);
        }

        let choice = self.rng.gen_range(100);
        if choice < 20 {
            return self.gen_leaf_expr(ty, vars);
        }

        // Calls and method calls
        if allow_calls && choice < 35 {
            if let Some(call) = self.try_gen_call_expr(ty, vars, depth) {
                return call;
            }
        }

        if ty == PrimTy::Bool {
            match self.rng.gen_range(4) {
                0 => {
                    let e = self.gen_expr(PrimTy::Bool, vars, depth - 1, allow_calls);
                    Expr::Unary { op: UnOp::Not, expr: Box::new(e) }
                }
                1 => {
                    let op = if self.rng.gen_bool(50) { BinOp::And } else { BinOp::Or };
                    let l = self.gen_expr(PrimTy::Bool, vars, depth - 1, allow_calls);
                    let r = self.gen_expr(PrimTy::Bool, vars, depth - 1, allow_calls);
                    Expr::Binary { op, lhs: Box::new(l), rhs: Box::new(r) }
                }
                _ => {
                    let cmp_ty = if self.rng.gen_bool(15) {
                        PrimTy::Bool
                    } else {
                        self.rand_int()
                    };
                    let ops = [BinOp::Eq, BinOp::Ne, BinOp::Lt, BinOp::Le, BinOp::Gt, BinOp::Ge];
                    let op = ops[self.rng.gen_range(ops.len())];
                    let l = self.gen_expr(cmp_ty, vars, depth - 1, allow_calls);
                    let r = self.gen_expr(cmp_ty, vars, depth - 1, allow_calls);
                    Expr::Binary { op, lhs: Box::new(l), rhs: Box::new(r) }
                }
            }
        } else {
            // Integer expression
            let kind = self.rng.gen_range(100);
            if kind < 18 {
                // Cast from another integer type
                let src_ty = self.rand_int();
                let inner = self.gen_expr(src_ty, vars, depth - 1, allow_calls);
                Expr::Cast { expr: Box::new(inner), to: ty }
            } else if kind < 26 {
                // Unary Neg or Bitwise Not
                let op = if self.rng.gen_bool(50) { UnOp::Neg } else { UnOp::Not };
                let inner = self.gen_expr(ty, vars, depth - 1, allow_calls);
                Expr::Unary { op, expr: Box::new(inner) }
            } else {
                // Binary op
                let ops = [
                    BinOp::Add,
                    BinOp::Sub,
                    BinOp::Mul,
                    BinOp::Div,
                    BinOp::Rem,
                    BinOp::BitAnd,
                    BinOp::BitOr,
                    BinOp::BitXor,
                    BinOp::Shl,
                    BinOp::Shr,
                ];
                let op = ops[self.rng.gen_range(ops.len())];
                let lhs = self.gen_expr(ty, vars, depth - 1, allow_calls);
                let rhs = match op {
                    BinOp::Div | BinOp::Rem => {
                        // Safe non-zero, non-(-1) divisor:
                        // either ((e & 63) + 1) in [1..=64]
                        // or for signed types, -((e & 63) + 2) in [-65..=-2].
                        let raw = self.gen_expr(ty, vars, depth - 1, allow_calls);
                        let mask = Val::from_u64_trunc(ty, 63);
                        let masked = Expr::Binary {
                            op: BinOp::BitAnd,
                            lhs: Box::new(raw),
                            rhs: Box::new(Expr::Lit(mask)),
                        };
                        if ty.is_signed() && self.rng.gen_bool(40) {
                            let two = Val::from_u64_trunc(ty, 2);
                            let pos = Expr::Binary {
                                op: BinOp::Add,
                                lhs: Box::new(masked),
                                rhs: Box::new(Expr::Lit(two)),
                            };
                            Expr::Unary { op: UnOp::Neg, expr: Box::new(pos) }
                        } else {
                            Expr::Binary {
                                op: BinOp::Add,
                                lhs: Box::new(masked),
                                rhs: Box::new(Expr::Lit(Val::one(ty))),
                            }
                        }
                    }
                    BinOp::Shl | BinOp::Shr => {
                        // Safe shift count in 0..ty.bits()
                        let raw = self.gen_expr(ty, vars, depth - 1, allow_calls);
                        let mask = Val::from_u64_trunc(ty, (ty.bits() - 1) as u64);
                        Expr::Binary {
                            op: BinOp::BitAnd,
                            lhs: Box::new(raw),
                            rhs: Box::new(Expr::Lit(mask)),
                        }
                    }
                    _ => self.gen_expr(ty, vars, depth - 1, allow_calls),
                };
                Expr::Binary { op, lhs: Box::new(lhs), rhs: Box::new(rhs) }
            }
        }
    }

    fn gen_leaf_expr(&mut self, ty: PrimTy, vars: &[ScopeVar]) -> Expr {
        let mut candidates: Vec<Expr> = Vec::new();
        for v in vars {
            if self.borrowed.contains(&v.name) {
                continue;
            }
            match v.ty {
                Ty::Prim(p) if p == ty => {
                    candidates.push(Expr::Var(v.name.clone()));
                }
                Ty::Struct(sid) => {
                    for (fi, (_, fty)) in self.structs[sid].fields.iter().enumerate() {
                        if *fty == ty {
                            candidates.push(Expr::FieldAccess {
                                var: v.name.clone(),
                                field: fi,
                            });
                        }
                    }
                }
                Ty::Array(elem, len) if elem == ty => {
                    let idx_raw = self.gen_leaf_int(PrimTy::Usize, vars);
                    let mask = Expr::Lit(Val::Usize((len - 1) as u64));
                    let idx = Expr::Binary {
                        op: BinOp::BitAnd,
                        lhs: Box::new(idx_raw),
                        rhs: Box::new(mask),
                    };
                    candidates.push(Expr::ArrayIndex {
                        var: v.name.clone(),
                        index: Box::new(idx),
                    });
                }
                _ => {}
            }
        }
        if !candidates.is_empty() && self.rng.gen_bool(75) {
            let idx = self.rng.gen_range(candidates.len());
            return candidates.swap_remove(idx);
        }
        Expr::Lit(self.rand_lit(ty))
    }

    fn gen_leaf_int(&mut self, ty: PrimTy, vars: &[ScopeVar]) -> Expr {
        let mut matches = Vec::new();
        for v in vars {
            if !self.borrowed.contains(&v.name) && v.ty == Ty::Prim(ty) {
                matches.push(v.name.clone());
            }
        }
        if !matches.is_empty() && self.rng.gen_bool(70) {
            let idx = self.rng.gen_range(matches.len());
            Expr::Var(matches.swap_remove(idx))
        } else {
            Expr::Lit(self.rand_lit(ty))
        }
    }

    fn try_gen_call_expr(&mut self, ret: PrimTy, vars: &[ScopeVar], depth: usize) -> Option<Expr> {
        let mut options: Vec<EitherCall> = Vec::new();
        for (fi, f) in self.funcs.iter().enumerate() {
            if f.ret == ret && self.can_bind_params(&f.params, vars) {
                options.push(EitherCall::Func(fi));
            }
        }
        for v in vars {
            if self.borrowed.contains(&v.name) {
                continue;
            }
            if let Ty::Struct(sid) = v.ty {
                for (mi, m) in self.structs[sid].methods.iter().enumerate() {
                    if m.ret == ret && (!m.is_mut_self || v.is_mut) && self.can_bind_method_params(&v.name, &m.params, vars) {
                        options.push(EitherCall::Method(v.name.clone(), sid, mi));
                    }
                }
            }
        }
        if options.is_empty() {
            return None;
        }
        let pick = options.swap_remove(self.rng.gen_range(options.len()));
        match pick {
            EitherCall::Func(fi) => {
                let params = self.funcs[fi].params.clone();
                let mut args = Vec::with_capacity(params.len());
                let mut newly_borrowed = Vec::new();
                for p in &params {
                    if p.is_mut_ref {
                        let vname = self.pick_mut_var(p.ty, vars)?;
                        self.borrowed.insert(vname.clone());
                        newly_borrowed.push(vname.clone());
                        args.push(Expr::Var(vname));
                    } else {
                        let pty = p.ty.prim().expect("value param is prim");
                        args.push(self.gen_expr(pty, vars, depth - 1, false));
                    }
                }
                for b in newly_borrowed {
                    self.borrowed.remove(&b);
                }
                Some(Expr::Call { func: fi, args })
            }
            EitherCall::Method(recv, sid, mi) => {
                let method = self.structs[sid].methods[mi].clone();
                let mut newly_borrowed = Vec::new();
                if method.is_mut_self {
                    self.borrowed.insert(recv.clone());
                    newly_borrowed.push(recv.clone());
                }
                let mut args = Vec::with_capacity(method.params.len());
                for p in &method.params {
                    let pty = p.ty.prim().expect("method param is prim");
                    args.push(self.gen_expr(pty, vars, depth - 1, false));
                }
                for b in newly_borrowed {
                    self.borrowed.remove(&b);
                }
                Some(Expr::MethodCall {
                    receiver: recv,
                    struct_id: sid,
                    method: mi,
                    args,
                })
            }
        }
    }

    fn can_bind_params(&self, params: &[Param], vars: &[ScopeVar]) -> bool {
        let mut used = self.borrowed.clone();
        for p in params {
            if p.is_mut_ref {
                // A `&mut` parameter is already a reference: emitting `&mut v`
                // for it would take a reference to a reference. The compiler
                // reborrows `&mut T` directly, so those variables are not
                // candidates for a fresh `&mut` borrow here.
                let found = vars.iter().find(|v| {
                    v.is_mut && !v.is_mut_ref && v.ty == p.ty && !used.contains(&v.name)
                });
                if let Some(v) = found {
                    used.insert(v.name.clone());
                } else {
                    return false;
                }
            }
        }
        true
    }

    fn can_bind_method_params(&self, recv: &str, params: &[Param], vars: &[ScopeVar]) -> bool {
        let mut used = self.borrowed.clone();
        used.insert(recv.to_string());
        for p in params {
            if p.is_mut_ref {
                // A `&mut` parameter is already a reference: emitting `&mut v`
                // for it would take a reference to a reference. The compiler
                // reborrows `&mut T` directly, so those variables are not
                // candidates for a fresh `&mut` borrow here.
                let found = vars.iter().find(|v| {
                    v.is_mut && !v.is_mut_ref && v.ty == p.ty && !used.contains(&v.name)
                });
                if let Some(v) = found {
                    used.insert(v.name.clone());
                } else {
                    return false;
                }
            }
        }
        true
    }

    fn pick_mut_var(&mut self, ty: Ty, vars: &[ScopeVar]) -> Option<String> {
        let mut cands: Vec<String> = vars
            .iter()
            .filter(|v| v.is_mut && !v.is_mut_ref && v.ty == ty && !self.borrowed.contains(&v.name))
            .map(|v| v.name.clone())
            .collect();
        if cands.is_empty() {
            None
        } else {
            Some(cands.swap_remove(self.rng.gen_range(cands.len())))
        }
    }

    fn gen_block(
        &mut self,
        vars: &mut Vec<ScopeVar>,
        len: usize,
        nest_depth: usize,
        in_main: bool,
        ret_ty: Option<PrimTy>,
    ) -> Vec<Stmt> {
        let initial_len = vars.len();
        let mut stmts = Vec::new();

        for _ in 0..len {
            let kind = self.rng.gen_range(100);
            if kind < 30 || vars.len() < 3 {
                // Let binding
                let ty = if self.rng.gen_bool(78) {
                    Ty::Prim(self.rand_prim())
                } else if self.rng.gen_bool(50) && !self.structs.is_empty() {
                    Ty::Struct(self.rng.gen_range(self.structs.len()))
                } else {
                    let elem = self.rand_int();
                    let alen = if self.rng.gen_bool(50) { 4 } else { 8 };
                    Ty::Array(elem, alen)
                };
                let init = self.gen_init_for_ty(ty, vars, 2, true);
                let name = self.fresh_name("v");
                let is_mut = self.rng.gen_bool(75);
                vars.push(ScopeVar {
                    name: name.clone(),
                    ty,
                    is_mut,
                    is_mut_ref: false,
                });
                stmts.push(Stmt::Let { name, ty, is_mut, init });
            } else if kind < 58 {
                // Reassignment (var, struct field, or array element)
                if let Some(stmt) = self.try_gen_assign(vars) {
                    stmts.push(stmt);
                }
            } else if kind < 73 && nest_depth > 0 {
                // If / else
                let cond = self.gen_expr(PrimTy::Bool, vars, 2, true);
                let then_len = 1 + self.rng.gen_range(3);
                let else_len = 1 + self.rng.gen_range(3);
                // UMAY types `if`/`else` as an expression, so both arms must
                // produce the same value. No early returns inside an arm, and a
                // trailing print makes each arm's type `()`.
                let mut then_body = self.gen_block(vars, then_len, nest_depth - 1, in_main, None);
                let mut else_body = self.gen_block(vars, else_len, nest_depth - 1, in_main, None);
                let unit = || Stmt::Print(Expr::Lit(Val::I64(0)));
                then_body.push(unit());
                else_body.push(unit());
                stmts.push(Stmt::If { cond, then_body, else_body });
            } else if kind < 84 && nest_depth > 0 {
                // Bounded while loop
                let counter = self.fresh_name("lc");
                let max_iters = (2 + self.rng.gen_range(6)) as i32;
                let cond = self.gen_expr(PrimTy::Bool, vars, 1, false);
                let body_len = 1 + self.rng.gen_range(3);
                let body = self.gen_block(vars, body_len, nest_depth - 1, in_main, ret_ty);
                stmts.push(Stmt::While { counter, max_iters, cond, body });
            } else if kind < 92 && !in_main {
                // Early return inside helper function / method
                if let Some(rt) = ret_ty {
                    let val = self.gen_expr(rt, vars, 2, true);
                    stmts.push(Stmt::Return(val));
                }
            } else if in_main {
                // Print an integer value in main
                let pty = self.rand_int();
                let val = self.gen_expr(pty, vars, 2, true);
                stmts.push(Stmt::Print(val));
            } else {
                // Expression statement (e.g. call)
                let pty = self.rand_prim();
                if let Some(call) = self.try_gen_call_expr(pty, vars, 2) {
                    stmts.push(Stmt::Expr(call));
                }
            }
        }

        vars.truncate(initial_len);
        stmts
    }

    fn gen_init_for_ty(&mut self, ty: Ty, vars: &[ScopeVar], depth: usize, allow_calls: bool) -> Expr {
        match ty {
            Ty::Prim(p) => self.gen_expr(p, vars, depth, allow_calls),
            Ty::Struct(sid) => {
                let field_tys: Vec<PrimTy> = self.structs[sid].fields.iter().map(|f| f.1).collect();
                let fields = field_tys
                    .into_iter()
                    .map(|ft| self.gen_expr(ft, vars, 1, false))
                    .collect();
                Expr::StructLit { struct_id: sid, fields }
            }
            Ty::Array(elem, len) => {
                let init = self.gen_expr(elem, vars, 1, false);
                Expr::ArrayLit { elem, len, init: Box::new(init) }
            }
        }
    }

    fn try_gen_assign(&mut self, vars: &[ScopeVar]) -> Option<Stmt> {
        let mut targets: Vec<(LValue, PrimTy)> = Vec::new();
        for v in vars {
            if !v.is_mut {
                continue;
            }
            match v.ty {
                Ty::Prim(p) if !v.is_mut_ref => {
                    targets.push((LValue::Var(v.name.clone()), p));
                }
                Ty::Struct(sid) => {
                    for (fi, (_, fty)) in self.structs[sid].fields.iter().enumerate() {
                        targets.push((LValue::Field(v.name.clone(), fi), *fty));
                    }
                }
                Ty::Array(elem, len) => {
                    let idx_raw = self.gen_leaf_int(PrimTy::Usize, vars);
                    let idx = Expr::Binary {
                        op: BinOp::BitAnd,
                        lhs: Box::new(idx_raw),
                        rhs: Box::new(Expr::Lit(Val::Usize((len - 1) as u64))),
                    };
                    targets.push((LValue::Index(v.name.clone(), idx), elem));
                }
                _ => {}
            }
        }
        if targets.is_empty() {
            return None;
        }
        let (target, ty) = targets.swap_remove(self.rng.gen_range(targets.len()));
        let value = self.gen_expr(ty, vars, 2, true);
        Some(Stmt::Assign { target, value })
    }
}

enum EitherCall {
    Func(usize),
    Method(String, usize, usize),
}

pub fn gen_program(seed: u64) -> Program {
    let mut rng = Rng::new(seed);
    let mut cx = GenCx {
        rng: &mut rng,
        structs: Vec::new(),
        funcs: Vec::new(),
        next_id: 0,
        borrowed: BTreeSet::new(),
    };

    // 1. Generate 1-2 structs with mixed-width fields.
    let num_structs = 1 + cx.rng.gen_range(2);
    for si in 0..num_structs {
        let num_fields = 2 + cx.rng.gen_range(4);
        let mut fields = Vec::with_capacity(num_fields);
        for fi in 0..num_fields {
            fields.push((format!("f{fi}"), cx.rand_prim()));
        }
        cx.structs.push(StructDef {
            name: format!("G_S{si}"),
            fields,
            methods: Vec::new(),
        });
    }

    // Populate methods on each struct (takes &self or &mut self + 1..4 extra mixed-width params).
    for si in 0..num_structs {
        let num_methods = 1 + cx.rng.gen_range(3);
        let mut methods = Vec::with_capacity(num_methods);
        for mi in 0..num_methods {
            let is_mut_self = cx.rng.gen_bool(45);
            let nparams = 1 + cx.rng.gen_range(4);
            let mut params = Vec::with_capacity(nparams);
            let mut vars = vec![ScopeVar {
                name: "self".to_string(),
                ty: Ty::Struct(si),
                is_mut: is_mut_self,
                is_mut_ref: true,
            }];
            for pi in 0..nparams {
                let pty = cx.rand_prim();
                let pname = format!("p{pi}");
                params.push(Param {
                    name: pname.clone(),
                    ty: Ty::Prim(pty),
                    is_mut_ref: false,
                });
                vars.push(ScopeVar {
                    name: pname,
                    ty: Ty::Prim(pty),
                    is_mut: false,
                    is_mut_ref: false,
                });
            }
            let ret = cx.rand_prim();
            let body_len = 2 + cx.rng.gen_range(4);
            let mut body = cx.gen_block(&mut vars, body_len, 1, false, Some(ret));
            let ret_expr = cx.gen_expr(ret, &vars, 2, false);
            body.push(Stmt::Return(ret_expr));
            methods.push(MethodDef {
                name: format!("m{mi}"),
                is_mut_self,
                params,
                ret,
                body,
            });
        }
        cx.structs[si].methods = methods;
    }

    // 2. Generate 3-5 functions with 1-8 parameters of mixed widths (including >6 for stack args,
    //    and some &mut struct / &mut array parameters).
    let num_funcs = 3 + cx.rng.gen_range(3);
    for fi in 0..num_funcs {
        let nparams = if fi == 0 {
            // Always at least one function with 7-8 args to stress stack arguments
            7 + cx.rng.gen_range(2)
        } else {
            1 + cx.rng.gen_range(8)
        };
        let mut params = Vec::with_capacity(nparams);
        let mut vars = Vec::new();
        for pi in 0..nparams {
            let pname = format!("p{pi}");
            if pi == 0 && cx.rng.gen_bool(45) {
                let ty = if cx.rng.gen_bool(50) {
                    Ty::Struct(cx.rng.gen_range(num_structs))
                } else {
                    let elem = cx.rand_int();
                    let alen = if cx.rng.gen_bool(50) { 4 } else { 8 };
                    Ty::Array(elem, alen)
                };
                params.push(Param { name: pname.clone(), ty, is_mut_ref: true });
                vars.push(ScopeVar { name: pname, ty, is_mut: true, is_mut_ref: true });
            } else {
                let pty = cx.rand_prim();
                params.push(Param { name: pname.clone(), ty: Ty::Prim(pty), is_mut_ref: false });
                vars.push(ScopeVar { name: pname, ty: Ty::Prim(pty), is_mut: false, is_mut_ref: false });
            }
        }
        let ret = cx.rand_prim();
        let body_len = 2 + cx.rng.gen_range(5);
        let mut body = cx.gen_block(&mut vars, body_len, 1, false, Some(ret));
        let ret_expr = cx.gen_expr(ret, &vars, 2, true);
        body.push(Stmt::Return(ret_expr));
        cx.funcs.push(FuncDef {
            name: format!("g_f{fi}"),
            params,
            ret,
            body,
        });
    }

    // 3. Generate main body.
    let mut main_vars = Vec::new();
    // Seed main with one mutable struct and one mutable array so calls/methods always have targets.
    let s0_init = cx.gen_init_for_ty(Ty::Struct(0), &main_vars, 1, false);
    let mut main_stmts = vec![Stmt::Let {
        name: "g_s".to_string(),
        ty: Ty::Struct(0),
        is_mut: true,
        init: s0_init,
    }];
    main_vars.push(ScopeVar {
        name: "g_s".to_string(),
        ty: Ty::Struct(0),
        is_mut: true,
        is_mut_ref: false,
    });

    let arr_ty = Ty::Array(cx.rand_int(), 8);
    let arr_init = cx.gen_init_for_ty(arr_ty, &main_vars, 1, false);
    main_stmts.push(Stmt::Let {
        name: "g_a".to_string(),
        ty: arr_ty,
        is_mut: true,
        init: arr_init,
    });
    main_vars.push(ScopeVar {
        name: "g_a".to_string(),
        ty: arr_ty,
        is_mut: true,
        is_mut_ref: false,
    });

    let body_len = 10 + cx.rng.gen_range(10);
    let mut generated = cx.gen_block(&mut main_vars, body_len, 2, true, None);
    main_stmts.append(&mut generated);

    // Ensure main prints a summary of live integer locals/fields/elements so dead-store
    // or forwarding bugs in any variable become observable in stdout.
    for v in &main_vars {
        match v.ty {
            Ty::Prim(p) if p.is_int() => {
                main_stmts.push(Stmt::Print(Expr::Var(v.name.clone())));
            }
            Ty::Struct(sid) => {
                for (fi, (_, fty)) in cx.structs[sid].fields.iter().enumerate() {
                    if fty.is_int() {
                        main_stmts.push(Stmt::Print(Expr::FieldAccess {
                            var: v.name.clone(),
                            field: fi,
                        }));
                    }
                }
            }
            Ty::Array(_, len) => {
                for idx in 0..len {
                    main_stmts.push(Stmt::Print(Expr::ArrayIndex {
                        var: v.name.clone(),
                        index: Box::new(Expr::Lit(Val::Usize(idx as u64))),
                    }));
                }
            }
            _ => {}
        }
    }

    Program {
        seed,
        structs: cx.structs,
        funcs: cx.funcs,
        main_stmts,
    }
}

// ---------------------------------------------------------------------------
// Emitter
// ---------------------------------------------------------------------------

pub fn emit_program(prog: &Program) -> String {
    let mut out = String::new();
    for s in &prog.structs {
        writeln!(out, "struct {} {{", s.name).unwrap();
        for (fname, fty) in &s.fields {
            writeln!(out, "    {fname}: {},", fty.umay_name()).unwrap();
        }
        writeln!(out, "}}").unwrap();
        if !s.methods.is_empty() {
            writeln!(out, "impl {} {{", s.name).unwrap();
            for m in &s.methods {
                let recv = if m.is_mut_self { "&mut self" } else { "&self" };
                write!(out, "    fn {}({recv}", m.name).unwrap();
                for p in &m.params {
                    write!(out, ", {}: {}", p.name, emit_param_ty(p, &prog.structs)).unwrap();
                }
                writeln!(out, ") -> {} {{", m.ret.umay_name()).unwrap();
                for st in &m.body {
                    emit_stmt(st, &prog.structs, &prog.funcs, 2, &mut out);
                }
                writeln!(out, "    }}").unwrap();
            }
            writeln!(out, "}}").unwrap();
        }
    }

    for f in &prog.funcs {
        write!(out, "fn {}(", f.name).unwrap();
        for (i, p) in f.params.iter().enumerate() {
            if i > 0 {
                out.push_str(", ");
            }
            write!(out, "{}: {}", p.name, emit_param_ty(p, &prog.structs)).unwrap();
        }
        writeln!(out, ") -> {} {{", f.ret.umay_name()).unwrap();
        for st in &f.body {
            emit_stmt(st, &prog.structs, &prog.funcs, 1, &mut out);
        }
        writeln!(out, "}}").unwrap();
    }

    writeln!(out, "fn main() -> i32 {{").unwrap();
    for st in &prog.main_stmts {
        emit_stmt(st, &prog.structs, &prog.funcs, 1, &mut out);
    }
    writeln!(out, "    return 0").unwrap();
    writeln!(out, "}}").unwrap();
    out
}

fn emit_ty(ty: Ty, structs: &[StructDef]) -> String {
    match ty {
        Ty::Prim(p) => p.umay_name().to_string(),
        Ty::Struct(sid) => structs[sid].name.clone(),
        Ty::Array(elem, len) => format!("[{}; {len}]", elem.umay_name()),
    }
}

fn emit_param_ty(p: &Param, structs: &[StructDef]) -> String {
    let base = emit_ty(p.ty, structs);
    if p.is_mut_ref {
        format!("&mut {base}")
    } else {
        base
    }
}

fn indent(level: usize, out: &mut String) {
    for _ in 0..level {
        out.push_str("    ");
    }
}

fn emit_stmt(
    stmt: &Stmt,
    structs: &[StructDef],
    funcs: &[FuncDef],
    level: usize,
    out: &mut String,
) {
    match stmt {
        Stmt::Let { name, ty, is_mut, init } => {
            indent(level, out);
            let mut_kw = if *is_mut { "mut " } else { "" };
            let ty_str = emit_ty(*ty, structs);
            let init_str = emit_expr(init, structs, funcs);
            writeln!(out, "let {mut_kw}{name}: {ty_str} = {init_str}").unwrap();
        }
        Stmt::Assign { target, value } => {
            indent(level, out);
            let lhs = match target {
                LValue::Var(v) => v.clone(),
                LValue::Field(v, fi) => {
                    // Field name lookup requires knowing struct, or we can find it by index?
                    // Since fields are always named f0, f1, ..., we can emit `v.f{fi}` directly!
                    format!("{v}.f{fi}")
                }
                LValue::Index(v, idx) => format!("{v}[{}]", emit_expr(idx, structs, funcs)),
            };
            writeln!(out, "{lhs} = {}", emit_expr(value, structs, funcs)).unwrap();
        }
        Stmt::If { cond, then_body, else_body } => {
            indent(level, out);
            writeln!(out, "if {} {{", emit_expr(cond, structs, funcs)).unwrap();
            for st in then_body {
                emit_stmt(st, structs, funcs, level + 1, out);
            }
            indent(level, out);
            writeln!(out, "}} else {{").unwrap();
            for st in else_body {
                emit_stmt(st, structs, funcs, level + 1, out);
            }
            indent(level, out);
            writeln!(out, "}}").unwrap();
        }
        Stmt::While { counter, max_iters, cond, body } => {
            indent(level, out);
            writeln!(out, "let mut {counter}: i32 = 0i32").unwrap();
            indent(level, out);
            writeln!(
                out,
                "while ({counter} < {max_iters}i32) && ({}) {{",
                emit_expr(cond, structs, funcs)
            )
            .unwrap();
            indent(level + 1, out);
            writeln!(out, "{counter} = {counter} + 1i32").unwrap();
            for st in body {
                emit_stmt(st, structs, funcs, level + 1, out);
            }
            indent(level, out);
            writeln!(out, "}}").unwrap();
        }
        Stmt::Return(e) => {
            indent(level, out);
            writeln!(out, "return {}", emit_expr(e, structs, funcs)).unwrap();
        }
        Stmt::Print(e) => {
            indent(level, out);
            writeln!(out, "println_i64(({}) as i64)", emit_expr(e, structs, funcs)).unwrap();
        }
        Stmt::Expr(e) => {
            indent(level, out);
            writeln!(out, "{}", emit_expr(e, structs, funcs)).unwrap();
        }
    }
}

fn emit_val(v: &Val) -> String {
    match *v {
        Val::I8(x) => {
            if x == i8::MIN {
                "(-127i8 - 1i8)".to_string()
            } else if x < 0 {
                format!("({x}i8)")
            } else {
                format!("{x}i8")
            }
        }
        Val::I16(x) => {
            if x == i16::MIN {
                "(-32767i16 - 1i16)".to_string()
            } else if x < 0 {
                format!("({x}i16)")
            } else {
                format!("{x}i16")
            }
        }
        Val::I32(x) => {
            if x == i32::MIN {
                "(-2147483647i32 - 1i32)".to_string()
            } else if x < 0 {
                format!("({x}i32)")
            } else {
                format!("{x}i32")
            }
        }
        Val::I64(x) => {
            if x == i64::MIN {
                "(-9223372036854775807i64 - 1i64)".to_string()
            } else if x < 0 {
                format!("({x}i64)")
            } else {
                format!("{x}i64")
            }
        }
        Val::U8(x) => format!("{x}u8"),
        Val::U16(x) => format!("{x}u16"),
        Val::U32(x) => format!("{x}u32"),
        Val::U64(x) => format!("{x}u64"),
        Val::Usize(x) => format!("{x}usize"),
        Val::Bool(b) => if b { "true" } else { "false" }.to_string(),
        Val::Struct(_, _) | Val::Array(_, _) => unreachable!("composite literal"),
    }
}

fn emit_expr(expr: &Expr, structs: &[StructDef], funcs: &[FuncDef]) -> String {
    match expr {
        Expr::Lit(v) => emit_val(v),
        Expr::Var(n) => n.clone(),
        Expr::Cast { expr, to } => format!("({} as {})", emit_expr(expr, structs, funcs), to.umay_name()),
        Expr::Unary { op, expr } => {
            let s = match op {
                UnOp::Neg => "-",
                UnOp::Not => "!",
            };
            format!("({s}{})", emit_expr(expr, structs, funcs))
        }
        Expr::Binary { op, lhs, rhs } => {
            let s = match op {
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
                BinOp::And => "&&",
                BinOp::Or => "||",
            };
            format!(
                "({} {s} {})",
                emit_expr(lhs, structs, funcs),
                emit_expr(rhs, structs, funcs)
            )
        }
        Expr::FieldAccess { var, field } => format!("{var}.f{field}"),
        Expr::ArrayIndex { var, index } => format!("{var}[{}]", emit_expr(index, structs, funcs)),
        Expr::Call { func, args } => {
            let f = &funcs[*func];
            let mut s = format!("{}(", f.name);
            for (i, (a, p)) in args.iter().zip(f.params.iter()).enumerate() {
                if i > 0 {
                    s.push_str(", ");
                }
                if p.is_mut_ref {
                    if let Expr::Var(v) = a {
                        write!(s, "&mut {v}").unwrap();
                    } else {
                        s.push_str(&emit_expr(a, structs, funcs));
                    }
                } else {
                    s.push_str(&emit_expr(a, structs, funcs));
                }
            }
            s.push(')');
            s
        }
        Expr::MethodCall { receiver, struct_id, method, args } => {
            let m = &structs[*struct_id].methods[*method];
            let mut s = format!("{receiver}.{}(", m.name);
            for (i, a) in args.iter().enumerate() {
                if i > 0 {
                    s.push_str(", ");
                }
                s.push_str(&emit_expr(a, structs, funcs));
            }
            s.push(')');
            s
        }
        Expr::StructLit { struct_id, fields } => {
            let st = &structs[*struct_id];
            let mut s = format!("{} {{ ", st.name);
            for (i, ((fname, _), fexpr)) in st.fields.iter().zip(fields.iter()).enumerate() {
                if i > 0 {
                    s.push_str(", ");
                }
                write!(s, "{fname}: {}", emit_expr(fexpr, structs, funcs)).unwrap();
            }
            s.push_str(" }");
            s
        }
        Expr::ArrayLit { len, init, .. } => {
            format!("[{}; {len}]", emit_expr(init, structs, funcs))
        }
    }
}

// ---------------------------------------------------------------------------
// Reference Evaluator (Oracle)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EvalErr {
    Undefined,
}

struct EvalCx<'a> {
    prog: &'a Program,
    /// Heap/slot table for all variables so `&mut` aliasing is exact.
    slots: Vec<Val>,
    stdout: String,
    steps: usize,
}

const MAX_STEPS: usize = 50_000;

type Env = BTreeMap<String, usize>;

pub fn eval_program(prog: &Program) -> Result<String, EvalErr> {
    let mut cx = EvalCx {
        prog,
        slots: Vec::new(),
        stdout: String::new(),
        steps: 0,
    };
    let mut env = Env::new();
    let _ = cx.eval_stmts(&prog.main_stmts, &mut env)?;
    Ok(cx.stdout)
}

impl<'a> EvalCx<'a> {
    fn tick(&mut self) -> Result<(), EvalErr> {
        self.steps += 1;
        if self.steps > MAX_STEPS {
            Err(EvalErr::Undefined)
        } else {
            Ok(())
        }
    }

    fn alloc(&mut self, val: Val) -> usize {
        let id = self.slots.len();
        self.slots.push(val);
        id
    }

    fn eval_stmts(&mut self, stmts: &[Stmt], env: &mut Env) -> Result<Option<Val>, EvalErr> {
        for st in stmts {
            self.tick()?;
            match st {
                Stmt::Let { name, init, .. } => {
                    let v = self.eval_expr(init, env)?;
                    let slot = self.alloc(v);
                    env.insert(name.clone(), slot);
                }
                Stmt::Assign { target, value } => {
                    let v = self.eval_expr(value, env)?;
                    match target {
                        LValue::Var(name) => {
                            let slot = *env.get(name).ok_or(EvalErr::Undefined)?;
                            self.slots[slot] = v;
                        }
                        LValue::Field(name, fi) => {
                            let slot = *env.get(name).ok_or(EvalErr::Undefined)?;
                            if let Val::Struct(_, ref mut fields) = self.slots[slot] {
                                if *fi >= fields.len() {
                                    return Err(EvalErr::Undefined);
                                }
                                fields[*fi] = v;
                            } else {
                                return Err(EvalErr::Undefined);
                            }
                        }
                        LValue::Index(name, idx_expr) => {
                            let idx_val = self.eval_expr(idx_expr, env)?;
                            let idx = idx_val.to_i128().ok_or(EvalErr::Undefined)?;
                            let slot = *env.get(name).ok_or(EvalErr::Undefined)?;
                            if let Val::Array(_, ref mut items) = self.slots[slot] {
                                if idx < 0 || (idx as usize) >= items.len() {
                                    return Err(EvalErr::Undefined);
                                }
                                items[idx as usize] = v;
                            } else {
                                return Err(EvalErr::Undefined);
                            }
                        }
                    }
                }
                Stmt::If { cond, then_body, else_body } => {
                    let cv = self.eval_expr(cond, env)?;
                    let b = match cv {
                        Val::Bool(b) => b,
                        _ => return Err(EvalErr::Undefined),
                    };
                    let mut sub_env = env.clone();
                    let branch = if b { then_body } else { else_body };
                    if let Some(ret) = self.eval_stmts(branch, &mut sub_env)? {
                        return Ok(Some(ret));
                    }
                }
                Stmt::While { max_iters, cond, body, .. } => {
                    let mut iters = 0i32;
                    loop {
                        self.tick()?;
                        if iters >= *max_iters {
                            break;
                        }
                        let cv = self.eval_expr(cond, env)?;
                        let b = match cv {
                            Val::Bool(b) => b,
                            _ => return Err(EvalErr::Undefined),
                        };
                        if !b {
                            break;
                        }
                        iters += 1;
                        let mut sub_env = env.clone();
                        if let Some(ret) = self.eval_stmts(body, &mut sub_env)? {
                            return Ok(Some(ret));
                        }
                    }
                }
                Stmt::Return(e) => {
                    let v = self.eval_expr(e, env)?;
                    return Ok(Some(v));
                }
                Stmt::Print(e) => {
                    let v = self.eval_expr(e, env)?;
                    let iv = v.cast_to(PrimTy::I64).ok_or(EvalErr::Undefined)?;
                    if let Val::I64(n) = iv {
                        writeln!(self.stdout, "{n}").unwrap();
                    } else {
                        return Err(EvalErr::Undefined);
                    }
                }
                Stmt::Expr(e) => {
                    let _ = self.eval_expr(e, env)?;
                }
            }
        }
        Ok(None)
    }

    fn eval_expr(&mut self, expr: &Expr, env: &mut Env) -> Result<Val, EvalErr> {
        self.tick()?;
        match expr {
            Expr::Lit(v) => Ok(v.clone()),
            Expr::Var(name) => {
                let slot = *env.get(name).ok_or(EvalErr::Undefined)?;
                Ok(self.slots[slot].clone())
            }
            Expr::Cast { expr, to } => {
                let v = self.eval_expr(expr, env)?;
                v.cast_to(*to).ok_or(EvalErr::Undefined)
            }
            Expr::Unary { op, expr } => {
                let v = self.eval_expr(expr, env)?;
                match (op, v) {
                    (UnOp::Not, Val::Bool(b)) => Ok(Val::Bool(!b)),
                    (UnOp::Not, other) => {
                        let p = other.ty().prim().ok_or(EvalErr::Undefined)?;
                        let raw = other.to_u64_sext().ok_or(EvalErr::Undefined)?;
                        Ok(Val::from_u64_trunc(p, !raw))
                    }
                    (UnOp::Neg, other) => {
                        let p = other.ty().prim().ok_or(EvalErr::Undefined)?;
                        if !p.is_int() {
                            return Err(EvalErr::Undefined);
                        }
                        let raw = other.to_u64_sext().ok_or(EvalErr::Undefined)?;
                        Ok(Val::from_u64_trunc(p, 0u64.wrapping_sub(raw)))
                    }
                }
            }
            Expr::Binary { op, lhs, rhs } => {
                if *op == BinOp::And {
                    let lv = self.eval_expr(lhs, env)?;
                    return match lv {
                        Val::Bool(false) => Ok(Val::Bool(false)),
                        Val::Bool(true) => self.eval_expr(rhs, env),
                        _ => Err(EvalErr::Undefined),
                    };
                }
                if *op == BinOp::Or {
                    let lv = self.eval_expr(lhs, env)?;
                    return match lv {
                        Val::Bool(true) => Ok(Val::Bool(true)),
                        Val::Bool(false) => self.eval_expr(rhs, env),
                        _ => Err(EvalErr::Undefined),
                    };
                }
                let lv = self.eval_expr(lhs, env)?;
                let rv = self.eval_expr(rhs, env)?;
                eval_binop(*op, lv, rv)
            }
            Expr::FieldAccess { var, field } => {
                let slot = *env.get(var).ok_or(EvalErr::Undefined)?;
                if let Val::Struct(_, ref fields) = self.slots[slot] {
                    fields.get(*field).cloned().ok_or(EvalErr::Undefined)
                } else {
                    Err(EvalErr::Undefined)
                }
            }
            Expr::ArrayIndex { var, index } => {
                let iv = self.eval_expr(index, env)?;
                let idx = iv.to_i128().ok_or(EvalErr::Undefined)?;
                let slot = *env.get(var).ok_or(EvalErr::Undefined)?;
                if let Val::Array(_, ref items) = self.slots[slot] {
                    if idx < 0 || (idx as usize) >= items.len() {
                        return Err(EvalErr::Undefined);
                    }
                    Ok(items[idx as usize].clone())
                } else {
                    Err(EvalErr::Undefined)
                }
            }
            Expr::Call { func, args } => {
                let f = self.prog.funcs.get(*func).ok_or(EvalErr::Undefined)?;
                let mut fn_env = Env::new();
                for (p, a) in f.params.iter().zip(args.iter()) {
                    if p.is_mut_ref {
                        if let Expr::Var(vname) = a {
                            let slot = *env.get(vname).ok_or(EvalErr::Undefined)?;
                            fn_env.insert(p.name.clone(), slot);
                        } else {
                            return Err(EvalErr::Undefined);
                        }
                    } else {
                        let v = self.eval_expr(a, env)?;
                        let slot = self.alloc(v);
                        fn_env.insert(p.name.clone(), slot);
                    }
                }
                self.eval_stmts(&f.body, &mut fn_env)?
                    .ok_or(EvalErr::Undefined)
            }
            Expr::MethodCall { receiver, struct_id, method, args } => {
                let m = self
                    .prog
                    .structs
                    .get(*struct_id)
                    .and_then(|s| s.methods.get(*method))
                    .ok_or(EvalErr::Undefined)?;
                let mut m_env = Env::new();
                let recv_slot = *env.get(receiver).ok_or(EvalErr::Undefined)?;
                if m.is_mut_self {
                    m_env.insert("self".to_string(), recv_slot);
                } else {
                    let cloned = self.slots[recv_slot].clone();
                    let copy_slot = self.alloc(cloned);
                    m_env.insert("self".to_string(), copy_slot);
                }
                for (p, a) in m.params.iter().zip(args.iter()) {
                    let v = self.eval_expr(a, env)?;
                    let slot = self.alloc(v);
                    m_env.insert(p.name.clone(), slot);
                }
                self.eval_stmts(&m.body, &mut m_env)?
                    .ok_or(EvalErr::Undefined)
            }
            Expr::StructLit { struct_id, fields } => {
                let mut vals = Vec::with_capacity(fields.len());
                for f in fields {
                    vals.push(self.eval_expr(f, env)?);
                }
                Ok(Val::Struct(*struct_id, vals))
            }
            Expr::ArrayLit { elem, len, init } => {
                let v = self.eval_expr(init, env)?;
                Ok(Val::Array(*elem, vec![v; *len]))
            }
        }
    }
}

fn eval_binop(op: BinOp, lv: Val, rv: Val) -> Result<Val, EvalErr> {
    if let (Val::Bool(a), Val::Bool(b)) = (&lv, &rv) {
        return match op {
            BinOp::Eq => Ok(Val::Bool(a == b)),
            BinOp::Ne => Ok(Val::Bool(a != b)),
            BinOp::Lt => Ok(Val::Bool(a < b)),
            BinOp::Le => Ok(Val::Bool(a <= b)),
            BinOp::Gt => Ok(Val::Bool(a > b)),
            BinOp::Ge => Ok(Val::Bool(a >= b)),
            _ => Err(EvalErr::Undefined),
        };
    }

    let p = lv.ty().prim().ok_or(EvalErr::Undefined)?;
    if !p.is_int() || lv.ty() != rv.ty() {
        return Err(EvalErr::Undefined);
    }

    let li = lv.to_i128().ok_or(EvalErr::Undefined)?;
    let ri = rv.to_i128().ok_or(EvalErr::Undefined)?;

    match op {
        BinOp::Eq => return Ok(Val::Bool(li == ri)),
        BinOp::Ne => return Ok(Val::Bool(li != ri)),
        BinOp::Lt => return Ok(Val::Bool(li < ri)),
        BinOp::Le => return Ok(Val::Bool(li <= ri)),
        BinOp::Gt => return Ok(Val::Bool(li > ri)),
        BinOp::Ge => return Ok(Val::Bool(li >= ri)),
        _ => {}
    }

    let lu = lv.to_u64_sext().ok_or(EvalErr::Undefined)?;
    let ru = rv.to_u64_sext().ok_or(EvalErr::Undefined)?;

    let out_u64 = match op {
        BinOp::Add => lu.wrapping_add(ru),
        BinOp::Sub => lu.wrapping_sub(ru),
        BinOp::Mul => lu.wrapping_mul(ru),
        BinOp::BitAnd => lu & ru,
        BinOp::BitOr => lu | ru,
        BinOp::BitXor => lu ^ ru,
        BinOp::Div | BinOp::Rem => {
            if ri == 0 {
                return Err(EvalErr::Undefined);
            }
            if p.is_signed() {
                let min_val = -(1i128 << (p.bits() - 1));
                if li == min_val && ri == -1 {
                    return Err(EvalErr::Undefined);
                }
                let res = if op == BinOp::Div { li / ri } else { li % ri };
                res as u64
            } else {
                let mask = if p.bits() == 64 { u64::MAX } else { (1u64 << p.bits()) - 1 };
                let a = lu & mask;
                let b = ru & mask;
                if b == 0 {
                    return Err(EvalErr::Undefined);
                }
                if op == BinOp::Div { a / b } else { a % b }
            }
        }
        BinOp::Shl | BinOp::Shr => {
            if ri < 0 || ri >= (p.bits() as i128) {
                return Err(EvalErr::Undefined);
            }
            let shift = ri as u32;
            if op == BinOp::Shl {
                lu.wrapping_shl(shift)
            } else if p.is_signed() {
                (li >> shift) as u64
            } else {
                let mask = if p.bits() == 64 { u64::MAX } else { (1u64 << p.bits()) - 1 };
                (lu & mask).wrapping_shr(shift)
            }
        }
        _ => return Err(EvalErr::Undefined),
    };

    Ok(Val::from_u64_trunc(p, out_u64))
}

// ---------------------------------------------------------------------------
// Shrinker
// ---------------------------------------------------------------------------

/// Shrinks a failing `Program` by repeatedly removing statements and
/// simplifying expressions while preserving definedness (`eval_program` = `Ok`)
/// and the failure predicate `is_bad(&candidate, &oracle_stdout)`.
pub fn shrink_program(
    prog: &Program,
    mut is_bad: impl FnMut(&Program, &str) -> bool,
) -> Program {
    let mut best = prog.clone();
    let mut changed = true;

    while changed {
        changed = false;

        // 1. Try deleting statements from main_stmts (except keep at least 1 Print if possible,
        //    actually is_bad will tell us if the failure persists).
        let mut i = best.main_stmts.len();
        while i > 0 {
            i -= 1;
            let mut cand = best.clone();
            cand.main_stmts.remove(i);
            if let Ok(out) = eval_program(&cand) {
                if is_bad(&cand, &out) {
                    best = cand;
                    changed = true;
                }
            }
        }

        // 2. Try deleting statements from each function body (except final Return).
        for fi in 0..best.funcs.len() {
            let mut i = best.funcs[fi].body.len();
            while i > 0 {
                i -= 1;
                if matches!(best.funcs[fi].body[i], Stmt::Return(_))
                    && i + 1 == best.funcs[fi].body.len()
                {
                    continue;
                }
                let mut cand = best.clone();
                cand.funcs[fi].body.remove(i);
                if let Ok(out) = eval_program(&cand) {
                    if is_bad(&cand, &out) {
                        best = cand;
                        changed = true;
                    }
                }
            }
        }

        // 3. Try deleting statements from each method body (except final Return).
        for si in 0..best.structs.len() {
            for mi in 0..best.structs[si].methods.len() {
                let mut i = best.structs[si].methods[mi].body.len();
                while i > 0 {
                    i -= 1;
                    if matches!(best.structs[si].methods[mi].body[i], Stmt::Return(_))
                        && i + 1 == best.structs[si].methods[mi].body.len()
                    {
                        continue;
                    }
                    let mut cand = best.clone();
                    cand.structs[si].methods[mi].body.remove(i);
                    if let Ok(out) = eval_program(&cand) {
                        if is_bad(&cand, &out) {
                            best = cand;
                            changed = true;
                        }
                    }
                }
            }
        }

        // 4. Try flattening If/While statements in main_stmts and function bodies.
        for i in 0..best.main_stmts.len() {
            let replacement = match &best.main_stmts[i] {
                Stmt::If { then_body, else_body, .. } => {
                    vec![then_body.clone(), else_body.clone()]
                }
                Stmt::While { body, .. } => vec![body.clone()],
                _ => continue,
            };
            for rep in replacement {
                let mut cand = best.clone();
                cand.main_stmts.splice(i..=i, rep);
                if let Ok(out) = eval_program(&cand) {
                    if is_bad(&cand, &out) {
                        best = cand;
                        changed = true;
                        break;
                    }
                }
            }
            if changed {
                break;
            }
        }

        // 5. Try simplifying expressions inside main_stmts (replacing complex
        //    expressions with their sub-expressions or a constant).
        for i in 0..best.main_stmts.len() {
            let mut candidates = Vec::new();
            collect_stmt_expr_simplifications(&best.main_stmts[i], &mut candidates);
            for rep_stmt in candidates {
                let mut cand = best.clone();
                cand.main_stmts[i] = rep_stmt;
                if let Ok(out) = eval_program(&cand) {
                    if is_bad(&cand, &out) {
                        best = cand;
                        changed = true;
                        break;
                    }
                }
            }
            if changed {
                break;
            }
        }
    }

    best
}

fn collect_stmt_expr_simplifications(stmt: &Stmt, out: &mut Vec<Stmt>) {
    match stmt {
        Stmt::Let { name, ty: Ty::Prim(p), is_mut, init } => {
            for e in simplify_expr(init, Some(*p)) {
                out.push(Stmt::Let { name: name.clone(), ty: Ty::Prim(*p), is_mut: *is_mut, init: e });
            }
        }
        Stmt::Assign { target, value } => {
            for e in simplify_expr(value, None) {
                out.push(Stmt::Assign { target: target.clone(), value: e });
            }
        }
        Stmt::Print(e) => {
            for se in simplify_expr(e, None) {
                out.push(Stmt::Print(se));
            }
        }
        _ => {}
    }
}

fn simplify_expr(expr: &Expr, known_ty: Option<PrimTy>) -> Vec<Expr> {
    let mut out = Vec::new();
    match expr {
        Expr::Lit(_) | Expr::Var(_) => {}
        Expr::Cast { expr: inner, .. } => {
            out.push((**inner).clone());
            if let Some(p) = known_ty {
                out.push(Expr::Lit(Val::zero(p)));
            }
        }
        Expr::Unary { expr: inner, .. } => {
            out.push((**inner).clone());
            if let Some(p) = known_ty {
                out.push(Expr::Lit(Val::zero(p)));
            }
        }
        Expr::Binary { lhs, rhs, .. } => {
            out.push((**lhs).clone());
            out.push((**rhs).clone());
            if let Some(p) = known_ty {
                out.push(Expr::Lit(Val::zero(p)));
            }
        }
        _ => {
            if let Some(p) = known_ty {
                out.push(Expr::Lit(Val::zero(p)));
            }
        }
    }
    out
}
