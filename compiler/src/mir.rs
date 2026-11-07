//! MIR: the typed control-flow graph between type checking and code generation.
//!
//! MIR is deliberately small. Every construct is one of:
//!
//! * **places** — a local plus a projection path (`_3.1[2]`),
//! * **rvalues** — computations that produce a value,
//! * **statements** — assign an rvalue to a place, or a storage marker,
//! * **terminators** — the end of a block: a jump, a branch, a call, a return.
//!
//! Two properties matter for later phases:
//!
//! 1. **Every operand is a place.** Nested expressions are flattened into
//!    temporaries during lowering, so nothing downstream has to walk an
//!    expression tree. This is what makes the dataflow analyses in Phase 4
//!    local and linear.
//! 2. **Every jump target is an explicit block.** Control flow is never
//!    implicit, which is what makes ownership a simple forward dataflow
//!    problem rather than a tree walk that must reason about early exits.
//!
//! Sugar is already gone: `for` is a `loop` over an induction variable, `?` is
//! a discriminant test plus an early return, `match` is a decision tree of
//! comparisons, and string interpolation is a sequence of calls.

use crate::hir::{ConstId, FnId, LocalId};
use crate::source::Span;
use crate::types::TyId;

/// Index of a basic block within a [`Body`].
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash, PartialOrd, Ord)]
pub struct BlockId(pub u32);

/// A lowering unit: one function, method or constant initialiser.
#[derive(Debug, Clone)]
pub struct Body {
    /// Human-readable name, e.g. `main` or `Counter::get`.
    pub name: String,
    /// The function this body belongs to, when lowering a function.
    pub fn_id: Option<FnId>,
    /// Declared return type.
    pub ret_ty: TyId,
    /// Every local slot: parameters first, then `let` bindings and temporaries.
    pub locals: Vec<LocalDecl>,
    /// One entry per local: whether it has a destructor and so needs a drop
    /// flag. Indexed by `LocalId`.
    pub arg_count: u32,
    pub blocks: Vec<BasicBlock>,
    /// Where execution starts. Always `BlockId(0)` today, but stored so that
    /// transformations may add a prologue.
    pub entry: BlockId,
    /// Span of the function this body came from.
    pub span: Span,
}

impl Body {
    pub fn block(&self, id: BlockId) -> &BasicBlock {
        &self.blocks[id.0 as usize]
    }

    pub fn block_mut(&mut self, id: BlockId) -> &mut BasicBlock {
        &mut self.blocks[id.0 as usize]
    }

    pub fn local(&self, id: LocalId) -> &LocalDecl {
        &self.locals[id.0 as usize]
    }

    /// All reachable blocks, in a deterministic depth-first preorder.
    ///
    /// Unreachable blocks are still present (they are simpler to keep than to
    /// remove during lowering) but excluded here so analyses do not report
    /// facts about code that never runs.
    pub fn reachable(&self) -> Vec<BlockId> {
        let mut seen = vec![false; self.blocks.len()];
        let mut order = Vec::new();
        let mut stack = vec![self.entry];
        while let Some(b) = stack.pop() {
            if seen[b.0 as usize] {
                continue;
            }
            seen[b.0 as usize] = true;
            order.push(b);
            let mut succ = self.blocks[b.0 as usize].successors();
            // Reverse so that the natural order (then-branch first) is preserved
            // when popped from the stack.
            succ.reverse();
            stack.extend(succ);
        }
        order
    }

    /// True if `b` is reachable from the entry block.
    pub fn is_reachable(&self, b: BlockId) -> bool {
        self.reachable().contains(&b)
    }
}

/// A local slot.
#[derive(Debug, Clone)]
pub struct LocalDecl {
    pub name: String,
    pub ty: TyId,
    /// `let mut` (or a `&mut self` receiver). Rebinding an immutable local is
    /// rejected during checking, so this is informational by the time MIR
    /// exists, but it is kept for diagnostics and for the borrow checker.
    pub mutable: bool,
    /// Span of the declaration, for diagnostics that point at a binding.
    pub span: Span,
    /// True when this local is a temporary introduced by lowering.
    pub is_temp: bool,
}

/// A storage location: a local plus a projection path.
///
/// `_1.0[2]` is local 1, field 0, then index 2.
/// Note: no `Eq`/`Hash`, because an `Index` projection can hold a float
/// operand. Places are compared structurally with `PartialEq`.
#[derive(Debug, Clone, PartialEq)]
pub struct Place {
    pub local: LocalId,
    pub projection: Vec<Projection>,
}

impl Place {
    pub fn local(local: LocalId) -> Self {
        Place { local, projection: Vec::new() }
    }

    pub fn field(mut self, index: u32) -> Self {
        self.projection.push(Projection::Field(index));
        self
    }

    pub fn index(mut self, by: Operand) -> Self {
        self.projection.push(Projection::Index(by));
        self
    }

    pub fn deref(mut self) -> Self {
        self.projection.push(Projection::Deref);
        self
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Projection {
    /// Named field or tuple element, by resolved index.
    Field(u32),
    /// Array or slice element, by a runtime index.
    Index(Operand),
    /// `*place`
    Deref,
}

/// A value held directly, as opposed to a place that stores it.
#[derive(Debug, Clone, PartialEq)]
pub enum Operand {
    Copy(Place),
    Move(Place),
    Constant(Constant),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Constant {
    Int { value: i128, ty: TyId },
    Uint { value: u128, ty: TyId },
    Float { value: f64, ty: TyId },
    Bool(bool),
    Char(char),
    /// A string literal, identified by index into the program's string table.
    Str(StringId),
    /// A byte-length-plus-pointer string slice, used for `&"literal"`.
    Unit,
    /// A function pointer.
    Fn(FnId),
    /// A constant, referenced by id so that its initialiser is not duplicated.
    Const(ConstId),
    /// A unit-only enum variant, e.g. `Color::Red`. The variant index is the
    /// payload; the discriminant value is assigned by the backend.
    UnitVariant { def: crate::types::DefId, index: u32 },
}

/// Index into a program-wide string table.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash, PartialOrd, Ord)]
pub struct StringId(pub u32);

/// A computation that produces a value.
#[derive(Debug, Clone, PartialEq)]
pub enum Rvalue {
    Use(Operand),
    /// Unary or binary operation on already-evaluated operands.
    Binary { op: BinOp, lhs: Operand, rhs: Operand },
    Unary { op: UnOp, operand: Operand },
    /// `&place` / `&mut place`
    Ref { mutable: bool, place: Place },
    /// `&*place` — reborrow, which keeps the original borrow alive.
    Reborrow { mutable: bool, place: Place },
    /// `operand as Ty`
    Cast { operand: Operand, to: TyId },
    /// An aggregate: struct, tuple, array, or enum variant with payload.
    Aggregate { kind: AggregateKind, operands: Vec<Operand> },
    /// Array repeat, `[v; n]`. `n` is a compile-time constant.
    Repeat { operand: Operand, count: u64 },
    /// `Place::Discriminant` for a tag comparison.
    Discriminant(Place),
    /// `len(place)` for an array or slice.
    Len(Place),
    /// A raw pointer's address, for `as usize` on pointers.
    PtrToInt(Operand),
    /// `ptr.offset(count)` — a pointer moved by whole elements.
    ///
    /// The count is in elements and the scaling by the pointee's size happens in
    /// code generation, where the machine's addressing modes can do it for free.
    PtrOffset { ptr: Operand, count: Operand },
}

#[derive(Debug, Clone, PartialEq)]
pub enum AggregateKind {
    /// Struct construction: fields in declaration order.
    Struct(crate::types::DefId),
    /// Tuple construction.
    Tuple,
    /// Fixed array construction.
    Array,
    /// Enum variant construction.
    Variant { def: crate::types::DefId, index: u32 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
    /// Short-circuit `&&` and `||` never appear here: lowering turns them into
    /// control flow so that the right operand is not evaluated eagerly.
    LogicalAnd,
    LogicalOr,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnOp {
    Neg,
    Not,
}

/// One basic block: straight-line statements and one terminator.
#[derive(Debug, Clone)]
pub struct BasicBlock {
    pub statements: Vec<Statement>,
    /// Always present: a block with no terminator would not be a block.
    pub terminator: Terminator,
    /// True when no other block jumps here and it is not the entry.
    pub is_cleanup: bool,
}

impl BasicBlock {
    pub fn new(terminator: Terminator) -> Self {
        BasicBlock { statements: Vec::new(), terminator, is_cleanup: false }
    }

    /// Blocks this block can transfer control to, excluding unwind edges.
    pub fn successors(&self) -> Vec<BlockId> {
        self.terminator.successors()
    }
}

#[derive(Debug, Clone)]
pub enum Statement {
    /// `place = rvalue`
    Assign { place: Place, value: Rvalue, span: Span },
    /// The local becomes uninitialised; used after a move and after a drop.
    StorageDead(LocalId),
    /// The local's storage begins. Kept for symmetry and for debug output.
    StorageLive(LocalId),
    /// Marks the local as needing a drop at scope exit. Set when a local is
    /// initialised; cleared when it is conditionally moved out.
    SetDropFlag { local: LocalId, value: bool },
    /// No-op carrying a span so that source positions survive lowering.
    Nop,
}

#[derive(Debug, Clone)]
pub enum Terminator {
    Goto(BlockId),
    /// Two-way branch on a `bool`.
    SwitchInt { discr: Operand, targets: Vec<(i128, BlockId)>, otherwise: BlockId },
    /// Call a function or method. `destination` is where the result is stored.
    Call {
        callee: Operand,
        args: Vec<Operand>,
        destination: Option<Place>,
        target: BlockId,
        /// Blocks that run when the callee unwinds. Empty for now: UMAY has no
        /// exceptions and panics abort until Phase 6.
        unwind: Vec<BlockId>,
        span: Span,
    },
    /// Drop a place, then continue at `target`.
    Drop { place: Place, target: BlockId },
    Return,
    /// Reached only if the function body ended without a value; the type
    /// checker rejects this, so the terminator exists for defensive lowering.
    Unreachable,
}

impl Terminator {
    pub fn successors(&self) -> Vec<BlockId> {
        match self {
            Terminator::Goto(b) => vec![*b],
            Terminator::SwitchInt { targets, otherwise, .. } => {
                let mut v: Vec<BlockId> = targets.iter().map(|(_, b)| *b).collect();
                v.push(*otherwise);
                v
            }
            Terminator::Call { target, unwind, .. } => {
                let mut v = vec![*target];
                v.extend(unwind.iter().copied());
                v
            }
            Terminator::Drop { target, .. } => vec![*target],
            Terminator::Return | Terminator::Unreachable => Vec::new(),
        }
    }

    /// The single successor of a block, if it has exactly one.
    pub fn single_successor(&self) -> Option<BlockId> {
        match self {
            Terminator::Goto(b) => Some(*b),
            Terminator::Drop { target, .. } => Some(*target),
            Terminator::Call { target, unwind, .. } if unwind.is_empty() => Some(*target),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Program
// ---------------------------------------------------------------------------

/// The whole program after lowering.
#[derive(Debug, Clone, Default)]
pub struct MirProgram {
    pub bodies: Vec<Body>,
    /// String literals, in a deterministic order.
    pub strings: Vec<String>,
}

impl MirProgram {
    pub fn string(&self, id: StringId) -> &str {
        &self.strings[id.0 as usize]
    }

    /// Interns a string literal, returning its table index.
    pub fn intern_string(&mut self, s: &str) -> StringId {
        if let Some(i) = self.strings.iter().position(|x| x == s) {
            return StringId(i as u32);
        }
        self.strings.push(s.to_string());
        StringId((self.strings.len() - 1) as u32)
    }

    pub fn body_named(&self, name: &str) -> Option<&Body> {
        self.bodies.iter().find(|b| b.name == name)
    }
}
