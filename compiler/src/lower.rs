//! Lowering from HIR to MIR.
//!
//! The lowering is a straight-line translation from an expression tree into a
//! control-flow graph. Two rules keep it simple:
//!
//! 1. **Every block is open while lowering.** [`Lowering::open`] allocates a
//!    block and records it as the current insertion point; [`Lowering::seal`]
//!    attaches a terminator. Statements are appended to the current block, so
//!    an expression always lowers into exactly one block unless it contains
//!    control flow.
//! 2. **Every sub-expression becomes a temporary.** `a + b * c` produces a
//!    temporary for `b * c`, then one for the sum. Nothing downstream ever has
//!    to re-walk an expression tree, which is what makes the ownership dataflow
//!    in Phase 4 local to a single block.
//!
//! Sugar is removed here rather than in HIR, because the desugaring needs the
//! CFG: `&&` must branch, `?` must branch and return, and `match` becomes a
//! decision tree. Doing it in MIR means one representation for all of them.

use std::collections::HashMap;

use crate::hir::*;
use crate::mir::*;
use crate::source::Span;
use crate::types::TyId;

/// The result of lowering a program.
pub struct Lowered {
    pub program: MirProgram,
}

/// Lowers every function with a body into MIR.
pub fn lower(hir: &Hir, types: &mut crate::types::TyCtxt) -> Lowered {
    let mut program = MirProgram::default();
    for f in &hir.fns {
        let Some(body) = &f.body else { continue };
        let l = Lowering::new(hir, types, f, &mut program);
        let mir = l.lower_fn(body);
        program.bodies.push(mir);
    }
    // Constants are lowered into their own bodies so that the backend has one
    // uniform thing to compile.
    for c in hir.consts.iter() {
        let l = Lowering::new_const(hir, types, c, &mut program);
        let mir = l.lower_const(&c.value);
        program.bodies.push(mir);
    }
    Lowered { program }
}

/// Per-function lowering state.
pub(crate) struct Lowering<'a> {
    pub(crate) hir: &'a Hir,
    pub(crate) types: &'a mut crate::types::TyCtxt,
    pub(crate) program: &'a mut MirProgram,
    /// Block currently being filled.
    pub(crate) current: BlockId,
    pub(crate) blocks: Vec<BasicBlock>,
    pub(crate) locals: Vec<LocalDecl>,
    /// Maps a HIR local to its MIR local. They coincide today, but the map
    /// keeps lowering independent of that accident.
    pub(crate) local_map: HashMap<LocalId, LocalId>,
    /// HIR local names, for MIR debug output.
    #[allow(dead_code)]
    pub(crate) hir_locals: Vec<LocalInfo>,
    /// The function's return type.
    pub(crate) ret_ty: TyId,
    pub(crate) ret_local: LocalId,
    pub(crate) fn_name: String,
    pub(crate) fn_id: Option<FnId>,
    pub(crate) arg_count: u32,
    pub(crate) span: Span,
    /// Next temporary index, used to name temps deterministically.
    pub(crate) next_temp: u32,
    /// Loop targets: `(break, continue)` blocks.
    pub(crate) loop_stack: Vec<LoopTargets>,
    /// Deferred blocks that run when the scope exits.
    pub(crate) defer_stack: Vec<Vec<HirBlock>>,
}

#[derive(Clone, Copy)]
pub(crate) struct LoopTargets {
    /// Where `break` jumps.
    pub(crate) exit: BlockId,
    /// Where `continue` jumps.
    pub(crate) head: BlockId,
    /// Local holding the loop's value, if it produces one.
    pub(crate) value: Option<(LocalId, TyId)>,
}

impl<'a> Lowering<'a> {
    fn new(hir: &'a Hir, types: &'a mut crate::types::TyCtxt, f: &HirFn, program: &'a mut MirProgram) -> Self {
        let mut locals = Vec::new();
        let mut local_map = HashMap::new();
        for (i, l) in f.locals.iter().enumerate() {
            let id = LocalId(i as u32);
            local_map.insert(id, id);
            locals.push(LocalDecl {
                name: l.name.clone(),
                ty: l.ty,
                mutable: l.mutable,
                span: l.span,
                is_temp: i >= f.params.len(),
            });
        }
        // The return slot is a dedicated local so that `return` lowers to a
        // single assignment plus a jump instead of duplicating the epilogue.
        let ret_local = LocalId(locals.len() as u32);
        locals.push(LocalDecl {
            name: "_ret".to_string(),
            ty: f.ret,
            mutable: true,
            span: f.span,
            is_temp: true,
        });
        Lowering {
            hir,
            types,
            program,
            current: BlockId(0),
            blocks: vec![BasicBlock::new(Terminator::Unreachable)],
            locals,
            local_map,
            hir_locals: f.locals.clone(),
            ret_ty: f.ret,
            ret_local,
            fn_name: f.name.clone(),
            fn_id: Some(f.id),
            arg_count: f.params.len() as u32,
            span: f.span,
            next_temp: 0,
            loop_stack: Vec::new(),
            defer_stack: Vec::new(),
        }
    }

    fn new_const(
        hir: &'a Hir,
        types: &'a mut crate::types::TyCtxt,
        c: &HirConst,
        program: &'a mut MirProgram,
    ) -> Self {
        let ret_local = LocalId(0);
        Lowering {
            hir,
            types,
            program,
            current: BlockId(0),
            blocks: vec![BasicBlock::new(Terminator::Unreachable)],
            locals: vec![LocalDecl {
                name: "_ret".to_string(),
                ty: c.ty,
                mutable: true,
                span: c.span,
                is_temp: true,
            }],
            local_map: HashMap::new(),
            hir_locals: Vec::new(),
            ret_ty: c.ty,
            ret_local,
            fn_name: format!("const {}", c.name),
            fn_id: None,
            arg_count: 0,
            span: c.span,
            next_temp: 0,
            loop_stack: Vec::new(),
            defer_stack: Vec::new(),
        }
    }

    // ---- block management -------------------------------------------------

    /// True when the current block still needs a terminator.
    /// `bool` type handle.
    pub(crate) fn bool_ty(&mut self) -> TyId {
        match self.types.primitive("bool") {
            Some(t) => t,
            None => self.types.unit(),
        }
    }

    /// The canonical integer type used for indices and discriminants.
    pub(crate) fn index_ty(&mut self) -> TyId {
        match self.types.primitive("usize") {
            Some(t) => t,
            None => self.types.unit(),
        }
    }

    pub(crate) fn block_is_open(&self) -> bool {
        matches!(self.blocks[self.current.0 as usize].terminator, Terminator::Unreachable)
    }

    pub(crate) fn new_block(&mut self) -> BlockId {
        let id = BlockId(self.blocks.len() as u32);
        self.blocks.push(BasicBlock::new(Terminator::Unreachable));
        id
    }

    pub(crate) fn emit(&mut self, s: Statement) {
        let cur = self.current;
        self.blocks[cur.0 as usize].statements.push(s);
    }

    pub(crate) fn assign(&mut self, place: Place, value: Rvalue, span: Span) {
        self.emit(Statement::Assign { place, value, span });
    }

    /// Attaches a terminator to the current block and moves to `next`.
    pub(crate) fn terminate(&mut self, t: Terminator, next: BlockId) {
        let cur = self.current;
        if matches!(self.blocks[cur.0 as usize].terminator, Terminator::Unreachable) {
            self.blocks[cur.0 as usize].terminator = t;
        }
        self.current = next;
    }

    pub(crate) fn goto(&mut self, target: BlockId) {
        self.terminate(Terminator::Goto(target), target);
    }

    // ---- locals and temporaries ------------------------------------------

    pub(crate) fn temp(&mut self, ty: TyId) -> LocalId {
        let id = LocalId(self.locals.len() as u32);
        self.next_temp += 1;
        self.locals.push(LocalDecl {
            name: format!("_t{}", self.next_temp),
            ty,
            mutable: false,
            span: self.span,
            is_temp: true,
        });
        self.emit(Statement::StorageLive(id));
        id
    }

    pub(crate) fn temp_place(&mut self, ty: TyId) -> Place {
        let id = self.temp(ty);
        Place::local(id)
    }

    pub(crate) fn map_local(&self, l: LocalId) -> LocalId {
        *self.local_map.get(&l).unwrap_or(&l)
    }

    pub(crate) fn place_of(&self, l: LocalId) -> Place {
        Place::local(self.map_local(l))
    }

    // ---- entry points -----------------------------------------------------

    fn lower_fn(mut self, body: &HirBlock) -> Body {
        let start = self.current;
        // Parameters are live from the first block.
        for i in 0..(self.arg_count as usize).min(self.locals.len()) {
            let id = LocalId(i as u32);
            self.emit(Statement::StorageLive(id));
            self.emit(Statement::SetDropFlag { local: id, value: true });
        }
        let value = self.lower_block(body);
        // The body's value, if any, becomes the return value.
        if let Some(op) = value {
            self.assign(Place::local(self.ret_local), Rvalue::Use(op), self.span);
        }
        self.terminate(Terminator::Return, start);
        Body {
            name: self.fn_name,
            fn_id: self.fn_id,
            ret_ty: self.ret_ty,
            locals: self.locals,
            arg_count: self.arg_count,
            blocks: self.blocks,
            entry: BlockId(0),
            span: self.span,
            ret_is_sret: false,
        }
    }

    /// Lowers a constant's initialiser into its own body.
    ///
    /// A constant body takes no arguments and returns the value, so the backend
    /// can treat it like any other function. Uses of the constant are calls to
    /// it until a later pass inlines them.
    fn lower_const(mut self, value: &HirExpr) -> Body {
        let start = self.current;
        let out = self.lower_expr(value);
        self.assign(Place::local(self.ret_local), Rvalue::Use(out), self.span);
        self.terminate(Terminator::Return, start);
        Body {
            name: self.fn_name,
            fn_id: self.fn_id,
            ret_ty: self.ret_ty,
            locals: self.locals,
            arg_count: 0,
            blocks: self.blocks,
            entry: BlockId(0),
            span: self.span,
            ret_is_sret: false,
        }
    }
}
