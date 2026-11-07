//! Linear-scan register allocation.
//!
//! The allocator walks each function's virtual registers in order of first use,
//! keeps a list of live intervals, and when all registers are taken spills the
//! interval that ends last. That is the classic linear-scan algorithm; it is not
//! optimal, but it is linear, predictable, and easy to reason about, which is
//! what a first backend needs.
//!
//! Two details matter for correctness and are handled explicitly:
//!
//! * **Live ranges across calls.** Every caller-saved register is clobbered by a
//!   call, so an interval that spans a call may not be assigned one. Its allowed
//!   registers are narrowed instead of spilling everywhere.
//! * **Fixed registers.** Division needs `rax`/`rdx` and shifts need `cl`. The
//!   selector emits those as physical registers directly, so the allocator never
//!   sees them as virtual and cannot collide with them.

use crate::machine::*;

/// The machine-level operand, distinct from `mir::Operand`.
use crate::machine::Operand as MOperand;
use std::collections::{BTreeMap, BTreeSet};

/// Where a virtual register ended up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Location {
    Reg(u8),
    /// A spill slot, offset from `rbp`.
    Stack(i64),
}

/// The result of allocating one function.
#[derive(Debug, Clone)]
pub struct Allocation {
    pub locations: BTreeMap<u32, Location>,
    /// Bytes of spill space needed, added to the frame.
    pub spill_bytes: u64,
    /// Callee-saved registers used, which the prologue must preserve.
    pub used_callee_saved: Vec<u8>,
}

/// A live interval for one virtual register.
#[derive(Debug, Clone)]
struct Interval {
    vreg: u32,
    start: usize,
    end: usize,
    /// Physical registers this interval must not be placed in, because its live
    /// range crosses a call that clobbers them.
    excluded: BTreeSet<u8>,
    /// Set when the value is used as an address. A spilled base register would
    /// need an extra load before every access that uses it, and the selector
    /// never emits one, so these intervals must get a real register.
    address: bool,
}

/// Allocates registers for `function`.
///
/// Point numbers are assigned in a single pass over the blocks, so an interval
/// is the range from its first definition to its last use. Blocks are treated as
/// linear, which is correct for the structured control flow UMAY lowers to but
/// would be pessimistic for arbitrary graphs; that is a deliberate trade.
pub fn allocate(function: &mut MachineFunction) -> Allocation {
    let mut point = 0usize;
    // Virtual registers numbered contiguously, so the interval table can be an
    // array indexed by vreg number.
    let mut first: BTreeMap<u32, usize> = BTreeMap::new();
    let mut last: BTreeMap<u32, usize> = BTreeMap::new();
    let mut max_vreg = 0u32;
    // Points at which a call appears, so live ranges that span one are found.
    let mut call_points: Vec<usize> = Vec::new();
    // Virtual registers used as a base or index, which cannot be spilled.
    let mut address_regs: BTreeSet<u32> = BTreeSet::new();

    for block in &function.blocks {
        for inst in &block.insts {
            point += 1;
            if matches!(inst, Inst::Call { .. }) {
                call_points.push(point);
            }
            for reg in inst.address_registers() {
                if let Reg::Virtual(v) = reg {
                    address_regs.insert(v);
                }
            }
            for reg in inst.reads().into_iter().chain(inst.writes()) {
                if let Reg::Virtual(v) = reg {
                    max_vreg = max_vreg.max(v);
                    first.entry(v).or_insert(point);
                    last.insert(v, point);
                }
            }
        }
    }

    let mut intervals: Vec<Interval> = Vec::new();
    for v in 0..=max_vreg {
        let (Some(&start), Some(&end)) = (first.get(&v), last.get(&v)) else { continue };
        let mut excluded = BTreeSet::new();
        // A live range that spans a call loses every caller-saved register.
        if call_points.iter().any(|&c| start < c && c < end) {
            for &r in CALLER_SAVED.iter() {
                excluded.insert(r);
            }
        }
        intervals.push(Interval { vreg: v, start, end, excluded, address: address_regs.contains(&v) });
    }
    intervals.sort_by_key(|i| (i.start, i.vreg));

    // The classic scan: `active` holds intervals currently occupying a register,
    // kept sorted by end point so the longest one is easy to evict.
    let mut active: Vec<(usize, u32, u8)> = Vec::new(); // (end, vreg, reg)
    let mut free: Vec<u8> = ALLOCATABLE.to_vec();
    let mut locations: BTreeMap<u32, Location> = BTreeMap::new();
    let mut spilled: Vec<u32> = Vec::new();
    let mut used_callee_saved: BTreeSet<u8> = BTreeSet::new();

    for interval in &intervals {
        // Retire intervals that ended before this one starts.
        active.retain(|(end, _, reg)| {
            if *end < interval.start {
                free.push(*reg);
                false
            } else {
                true
            }
        });
        free.sort();

        // Prefer a register this interval is allowed to use.
        if let Some(&reg) = free.iter().find(|r| !interval.excluded.contains(r)) {
            free.retain(|r| *r != reg);
            locations.insert(interval.vreg, Location::Reg(reg));
            if is_callee_saved(reg) {
                used_callee_saved.insert(reg);
            }
            active.push((interval.end, interval.vreg, reg));
            active.sort_by_key(|(end, _, _)| *end);
            continue;
        }

        // No register is free. If an active interval ends later than this one,
        // evict it and take its register; otherwise spill this interval. An
        // address interval may take a later-ending register even if that means
        // evicting it, because it cannot be spilled at all; an ordinary interval
        // may not evict an address one.
        let evictable = active
            .iter()
            .enumerate()
            .filter(|(_, (_, v, reg))| {
                !interval.excluded.contains(reg)
                    && *v != interval.vreg
                    && (!address_regs.contains(v) || interval.address)
            })
            .max_by_key(|(_, (end, _, _))| *end);
        match evictable {
            Some((index, (end, victim, reg)))
                if (*end > interval.end || interval.address)
                    && !interval.excluded.contains(reg) =>
            {
                let _ = (end, victim);
                let (_, victim_vreg, reg) = active.remove(index);
                spilled.push(victim_vreg);
                locations.remove(&victim_vreg);
                locations.insert(interval.vreg, Location::Reg(reg));
                if is_callee_saved(reg) {
                    used_callee_saved.insert(reg);
                }
                active.push((interval.end, interval.vreg, reg));
                active.sort_by_key(|(end, _, _)| *end);
            }
            _ => spilled.push(interval.vreg),
        }
    }

    // Spill slots, one eight-byte slot each.
    let mut spill_bytes = 0u64;
    for vreg in &spilled {
        spill_bytes += 8;
        locations.insert(*vreg, Location::Stack(-(spill_bytes as i64)));
    }

    let allocation = Allocation {
        locations,
        // Spill space is aligned so the frame stays 16-byte aligned.
        spill_bytes: spill_bytes.div_ceil(16) * 16,
        used_callee_saved: used_callee_saved.into_iter().collect(),
    };

    rewrite_registers(function, &allocation);
    allocation
}

fn is_callee_saved(reg: u8) -> bool {
    matches!(reg, 3 | 12 | 13 | 14 | 15)
}

/// Rewrites every virtual register in the function to its assigned location.
///
/// A spilled register becomes a memory operand; an allocated one becomes the
/// physical register.
fn rewrite_registers(function: &mut MachineFunction, allocation: &Allocation) {
    for block in &mut function.blocks {
        let mut out = Vec::with_capacity(block.insts.len());
        for mut inst in std::mem::take(&mut block.insts) {
            rewrite_inst(&mut inst, allocation);
            // x86 cannot copy memory to memory, and a store source must be a
            // register or an immediate. When a spilled value is the source, load
            // it into a scratch register first.
            if let Inst::Store { kind: MoveKind::Move(w), src: MOperand::Mem(m), dst } = &inst {
                let width = *w;
                let scratch = Reg::Phys(crate::machine::SCRATCH);
                let source = m.clone();
                let target = dst.clone();
                out.push(Inst::Load {
                    kind: MoveKind::Move(width),
                    src: source,
                    dst: MOperand::Reg(scratch),
                });
                out.push(Inst::Store {
                    kind: MoveKind::Move(width),
                    src: MOperand::Reg(scratch),
                    dst: target,
                });
                continue;
            }
            if let Inst::Move { kind: MoveKind::Move(w), src: MOperand::Mem(_), dst: MOperand::Mem(_) } = &inst {
                let width = *w;
                let scratch = Reg::Phys(crate::machine::SCRATCH);
                let (src, dst) = match &inst {
                    Inst::Move { src, dst, .. } => (src.clone(), dst.clone()),
                    _ => unreachable!(),
                };
                out.push(Inst::Load {
                    kind: MoveKind::Move(width),
                    src: match src {
                        MOperand::Mem(m) => m,
                        _ => unreachable!(),
                    },
                    dst: MOperand::Reg(scratch),
                });
                out.push(Inst::Store {
                    kind: MoveKind::Move(width),
                    src: MOperand::Reg(scratch),
                    dst: match dst {
                        MOperand::Mem(m) => m,
                        _ => unreachable!(),
                    },
                });
                continue;
            }
            out.push(inst);
        }
        block.insts = out;
    }
}

fn rewrite_inst(inst: &mut Inst, allocation: &Allocation) {
    fn place(loc: Location) -> MOperand {
        match loc {
            Location::Reg(r) => MOperand::Reg(Reg::Phys(r)),
            // The width is carried by the instruction, not by the operand, so a
            // spilled value is just its stack slot.
            Location::Stack(offset) => MOperand::Mem(Mem::stack(offset)),
        }
    }
    let resolve = |reg: Reg, _width: RegWidth, allocation: &Allocation| -> MOperand {
        match reg {
            Reg::Phys(_) => MOperand::Reg(reg),
            Reg::Virtual(v) => match allocation.locations.get(&v) {
                Some(loc) => place(*loc),
                // An unallocated virtual register means the analysis missed it.
                // Falling back to a register would silently read a wrong value,
                // so the printer reports this instead; see `unallocated`.
                None => MOperand::Reg(Reg::Phys(0)),
            },
        }
    };
    let width_of = |inst: &Inst| -> RegWidth {
        match inst {
            Inst::Move { kind, .. } => match kind {
                MoveKind::Move(w) | MoveKind::SignExtend { to: w, .. } | MoveKind::ZeroExtend { to: w, .. } => *w,
            },
            Inst::Alu { width, .. } | Inst::Cmp { width, .. } | Inst::Test { width, .. } => *width,
            _ => RegWidth::W64,
        }
    };
    let width = width_of(inst);

    // Resolve registers inside memory operands too: a base or index may be a
    // virtual register that was spilled, in which case the address has to be
    // materialised by two loads, which the selector never produces. In practice
    // bases are always physical or allocated to a register.
    let fix_mem = |mem: &mut Mem, allocation: &Allocation| {
        if let Some(base) = mem.base {
            if let Reg::Virtual(v) = base {
                if let Some(Location::Reg(r)) = allocation.locations.get(&v) {
                    mem.base = Some(Reg::Phys(*r));
                }
            }
        }
        if let Some((index, scale)) = mem.index {
            if let Reg::Virtual(v) = index {
                if let Some(Location::Reg(r)) = allocation.locations.get(&v) {
                    mem.index = Some((Reg::Phys(*r), scale));
                }
            }
        }
    };

    match inst {
        Inst::Move { src, dst, kind: _ } => {
            if let MOperand::Reg(r) = src {
                *src = resolve(*r, width, allocation);
            }
            match dst {
                MOperand::Reg(r) => *dst = resolve(*r, width, allocation),
                MOperand::Mem(m) => fix_mem(m, allocation),
                _ => {}
            }
        }
        Inst::Alu { src, dst, .. } => {
            if let MOperand::Reg(r) = src {
                *src = resolve(*r, width, allocation);
            }
            if let MOperand::Reg(r) = dst {
                *dst = resolve(*r, width, allocation);
            }
        }
        Inst::Cmp { lhs, rhs, .. } | Inst::Test { lhs, rhs, .. } => {
            if let MOperand::Reg(r) = lhs {
                *lhs = resolve(*r, width, allocation);
            }
            if let MOperand::Reg(r) = rhs {
                *rhs = resolve(*r, width, allocation);
            }
        }
        Inst::SetCc { dst, .. } => {
            if let MOperand::Reg(r) = dst {
                *dst = resolve(*r, RegWidth::W8, allocation);
            }
        }
        Inst::Lea { src, dst } => {
            fix_mem(src, allocation);
            if let MOperand::Reg(r) = dst {
                *dst = resolve(*r, RegWidth::W64, allocation);
            }
        }
        Inst::Load { src, dst, .. } => {
            fix_mem(src, allocation);
            if let MOperand::Reg(r) = dst {
                *dst = resolve(*r, width, allocation);
            }
        }
        Inst::Store { src, dst, .. } => {
            // The source may itself be a memory operand when the value was
            // spilled, in which case its base is an address too.
            if let MOperand::Mem(m) = src {
                fix_mem(m, allocation);
            }
            if let MOperand::Reg(r) = src {
                *src = resolve(*r, width, allocation);
            }
            fix_mem(dst, allocation);
        }
        Inst::Call { target } => {
            if let MOperand::Reg(r) = target {
                *target = resolve(*r, RegWidth::W64, allocation);
            }
        }
        Inst::Push(op) | Inst::Pop(op) => {
            if let MOperand::Reg(r) = op {
                *op = resolve(*r, RegWidth::W64, allocation);
            }
        }
        Inst::Jmp(_) | Inst::Jcc { .. } | Inst::Ret | Inst::SignExtendAcc { .. } | Inst::Label(_)
        | Inst::Comment(_) => {}
    }
}
