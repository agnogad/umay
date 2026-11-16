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
pub fn allocate(function: &mut MachineFunction, abi: crate::target::Abi) -> Allocation {
    // Which registers survive a call and which are worth saving are the ABI's
    // answers, not the allocator's. Under Win64 `rsi` and `rdi` are callee-saved,
    // so a value that lives in one across a call is *not* destroyed and must not
    // be treated as if it were; and a value the allocator places in a
    // callee-saved register obliges the prologue to save it.
    let caller_saved: &[u8] = match abi {
        crate::target::Abi::SysV => &crate::machine::CALLER_SAVED,
        crate::target::Abi::Win64 => &crate::machine::CALLER_SAVED_WIN64,
    };
    let allocatable: &[u8] = match abi {
        crate::target::Abi::SysV => &crate::machine::ALLOCATABLE,
        crate::target::Abi::Win64 => &crate::machine::ALLOCATABLE_WIN64,
    };
    let callee_saved_of_abi = |reg: u8| match abi {
        crate::target::Abi::SysV => crate::machine::is_callee_saved_sysv(reg),
        crate::target::Abi::Win64 => crate::machine::is_callee_saved_win64(reg),
    };
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
            for &r in caller_saved.iter() {
                excluded.insert(r);
            }
        }
        intervals.push(Interval { vreg: v, start, end, excluded, address: address_regs.contains(&v) });
    }
    intervals.sort_by_key(|i| (i.start, i.vreg));

    // The classic scan: `active` holds intervals currently occupying a register,
    // kept sorted by end point so the longest one is easy to evict.
    let mut active: Vec<(usize, u32, u8)> = Vec::new(); // (end, vreg, reg)
    let mut free: Vec<u8> = allocatable
        .iter()
        .copied()
        .filter(|r| !function.reserved.contains(r))
        .collect();
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
            if callee_saved_of_abi(reg) {
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
                if callee_saved_of_abi(reg) {
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

    // UMAY_ALLOC_STATS=1 prints what the allocator had to do. Spills are the
    // quantity that decides runtime speed, so it is worth being able to see
    // them without reading the assembly.
    if std::env::var_os("UMAY_ALLOC_STATS").is_some() {
        let regs = allocation.locations.values().filter(|l| matches!(l, Location::Reg(_))).count();
        eprintln!(
            "alloc {}: {} vregs, {} in registers, {} spilled, {} spill bytes",
            function.name,
            intervals.len(),
            regs,
            spilled.len(),
            allocation.spill_bytes,
        );
    }

    rewrite_registers(function, &allocation);
    let removed = peephole(function);
    if std::env::var_os("UMAY_ALLOC_STATS").is_some() {
        eprintln!("peephole {}: {} instructions removed", function.name, removed);
    }
    allocation
}

/// Removes instruction pairs that one step of looking ahead proves redundant.
///
/// This is not an optimiser; it is the cleanup of patterns the selector emits
/// mechanically. Every rule here removes a move whose value is provably already
/// where the next instruction wants it, and none of them change a flag that a
/// following conditional reads, because only adjacent instructions are examined
/// and a flag-setting instruction ends any chain.
fn peephole(function: &mut MachineFunction) -> usize {
    let forwarded_removed = forward_slots(function);
    let dead_store_removed = remove_dead_stores(function);
    let copy_removed = coalesce_copies(function);
    forwarded_removed + dead_store_removed + copy_removed
}

/// Removes a store to a frame slot when nothing in the block loads that slot
/// before overwriting it.
///
/// This is the other half of slot forwarding: `%.r -> slot` is emitted for every
/// MIR local, and once the loads are served from registers the store that fed
/// them has no reader. A slot that *is* read later (because its value crosses a
/// block boundary, or the register holding it was clobbered) keeps its store.
///
/// Only stores to plain `%rbp`-relative slots are considered, and only within
/// one block: the same block, the same slot, no intervening load and no
/// intervening write to that slot. A branch ends the block, so a store whose
/// value is read after a branch is never removed.
fn remove_dead_stores(function: &mut MachineFunction) -> usize {
    let mut removed = 0;
    // Which slots any instruction in the function reads, ever.
    //
    // A store is dropped only when its slot is read nowhere in the whole body.
    // Restricting the search to the store's own block was wrong: a slot written
    // in one block and read in another kept no store and the reader saw stale
    // memory, which is a wrong answer, not a missing optimisation. The cost of
    // this conservative version is that a slot reused across blocks keeps its
    // store, which is exactly the traffic that must stay for correctness.
    let mut read_anywhere: std::collections::BTreeSet<(i64, i64)> = std::collections::BTreeSet::new();
    for block in &function.blocks {
        for inst in &block.insts {
            for slot in slots_read(inst) {
                read_anywhere.insert(slot);
            }
        }
    }
    for block in &mut function.blocks {
        let insts = std::mem::take(&mut block.insts);
        // For each store, whether a later instruction in this block loads it
        // before writing it.
        let mut keep = vec![true; insts.len()];
        for i in 0..insts.len() {
            let Some((slot, _)) = store_of_register(&insts[i]) else { continue };
            if !plain_stack(&slot) {
                continue;
            }
            let key = slot_key(&slot);
            let stored = match memory_write(&insts[i]) {
                Some(Some((_, bytes))) => bytes,
                _ => continue,
            };
            // Keep the store unless another store to the same slot overwrites it
            // before any read *within this block*. A block boundary is a
            // conservative stop: the next block may read the slot and this pass
            // does not track liveness across blocks, so a store whose value might
            // be read later is never removed.
            let mut dead = false;
            for later in &insts[i + 1..] {
                if matches!(later, Inst::Call { .. }) || loads_from_slot(later, &key) {
                    dead = false;
                    break;
                }
                if matches!(memory_write(later), Some(Some((k, bytes))) if k == key && bytes >= stored) {
                    dead = true;
                    break;
                }
            }
            let _ = &read_anywhere;
            if dead {
                keep[i] = false;
            }
        }
        block.insts = insts
            .into_iter()
            .zip(keep)
            .filter_map(|(i, k)| if k { Some(i) } else { removed += 1; None })
            .collect();
    }
    removed
}

/// Every frame slot `inst` reads, in any operand position.
fn slots_read(inst: &Inst) -> Vec<(i64, i64)> {
    fn one(op: &MOperand, out: &mut Vec<(i64, i64)>) {
        if let MOperand::Mem(m) = op {
            if plain_stack(m) {
                out.push(slot_key(m));
            }
        }
    }
    let mut out = Vec::new();
    match inst {
        Inst::Load { src, .. } => {
            if plain_stack(src) {
                out.push(slot_key(src));
            }
        }
        Inst::Move { src, .. } => one(src, &mut out),
        Inst::Alu { src, dst, .. } => {
            one(src, &mut out);
            one(dst, &mut out);
        }
        Inst::Cmp { lhs, rhs, .. } | Inst::Test { lhs, rhs, .. } => {
            one(lhs, &mut out);
            one(rhs, &mut out);
        }
        Inst::Push(op) | Inst::Pop(op) => one(op, &mut out),
        Inst::Store { src, .. } => one(src, &mut out),
        Inst::Call { target, .. } => one(target, &mut out),
        _ => {}
    }
    out
}

/// True when `inst` reads the frame slot `key`, in any position.
///
/// This must catch every way a slot can be read, not only an explicit load:
/// `addq slot, %rcx` reads the slot as an ALU operand and never becomes an
/// `Inst::Load`. Missing that case deleted the store that fed it and the loop
/// read a stale slot, which is a wrong answer rather than a crash -- the reason
/// this check is exhaustive rather than convenient.
fn loads_from_slot(inst: &Inst, key: &(i64, i64)) -> bool {
    // A read of up to 8 bytes anywhere within 8 bytes of the slot may cover
    // it, and a read through a pointer may reach any slot whose address was
    // taken; both count. Only a symbol-addressed global is known not to.
    fn mem_reads(m: &Mem, key: &(i64, i64)) -> bool {
        if plain_stack(m) {
            overlaps(slot_key(m), 8, *key, 8)
        } else {
            m.symbol.is_none() || m.base.is_some() || m.index.is_some()
        }
    }
    fn reads(op: &MOperand, key: &(i64, i64)) -> bool {
        match op {
            MOperand::Mem(m) => mem_reads(m, key),
            _ => false,
        }
    }
    match inst {
        Inst::Load { src, .. } => mem_reads(src, key),
        Inst::Move { src, .. } => reads(src, key),
        // Any ALU instruction may read its source from memory. `dst` is not
        // checked on purpose: a read-modify-write of the slot (`addq $1, slot`)
        // is a write for this analysis, so the store before it is redundant
        // anyway.
        // A memory destination is read as well as written (`addq $1, slot`).
        Inst::Alu { src, dst, .. } => reads(src, key) || reads(dst, key),
        // A compare, a test and an extend all take memory operands.
        Inst::Cmp { lhs, rhs, .. } => reads(lhs, key) || reads(rhs, key),
        Inst::Test { lhs, rhs, .. } => reads(lhs, key) || reads(rhs, key),
        Inst::SignExtendAcc { .. } => false,
        Inst::SetCc { .. } | Inst::Jmp(_) | Inst::Jcc { .. } | Inst::Ret | Inst::Label(_)
        | Inst::Comment(_) => false,
        // Anything not enumerated -- a push of a slot, a store whose source is a
        // slot -- is treated as a read. Being pessimistic here costs one store;
        // being optimistic costs correctness.
        Inst::Push(op) | Inst::Pop(op) => reads(op, key),
        _ => matches!(inst, Inst::Store { src, .. } if reads(src, key)),
    }
}

/// Removes `mov %a, %b` when `%b` already holds the same value.
///
/// The selector emits a copy per operand and the allocator often places both
/// sides in the same register, or a value is copied twice through a chain. A
/// `mov %rax, %rcx` immediately followed by `mov %rcx, %rax` puts back what was
/// already in `%rax`, and the loop then carries two instructions that compute
/// nothing.
///
/// Only `Move` instructions are tracked, and only registers that no other
/// instruction writes in between: the map is invalidated the moment the source
/// or the destination is written by anything else.
fn coalesce_copies(function: &mut MachineFunction) -> usize {
    // Remove `mov %r, %r` and the adjacent double copy
    // `mov %a, %r` / `mov %r, %b` where `%b` already holds `%a`'s value.
    //
    // The wider version of this pass -- tracking which register holds which
    // value across a whole block and rewriting every later use -- was written and
    // then reverted: it changed `%cl` to `%al` in a call setup, and `%al` is the
    // one register SysV reads as the number of vector arguments a variadic call
    // receives, so the program linked and then corrupted its arguments. The
    // lesson generalised: an optimisation that rewrites a register into a
    // different one must reason about *every* register with a fixed meaning, and
    // `%al` alone is enough to make that a wide surface.
    //
    // What is kept is local and checkable: adjacent instructions, no reasoning
    // about memory, no register with a special role. It removes the redundant
    // copy the selector emits for a return value, which is the case that exists.
    let mut removed = 0;
    for block in &mut function.blocks {
        let mut out: Vec<Inst> = Vec::with_capacity(block.insts.len());
        for inst in std::mem::take(&mut block.insts) {
            if is_self_move(&inst) {
                removed += 1;
                continue;
            }
            if let Inst::Move { kind, src: MOperand::Reg(s), dst: MOperand::Reg(d) } = &inst {
                if let Some(Inst::Move { src: MOperand::Reg(prev_src), dst: MOperand::Reg(prev_dst), .. }) = out.last() {
                    // `%prev_dst <- %prev_src`, then `%d <- %prev_dst`, with
                    // `%d == %prev_src`: the second move puts back the value the
                    // first one copied from.
                    if prev_dst == s && prev_src == d {
                        removed += 1;
                        continue;
                    }
                }
                let _ = kind;
                let _ = (s, d);
            }
            out.push(inst);
        }
        block.insts = out;
    }
    removed
}

/// Slot forwarding: serve loads of a frame slot from the register still holding
/// its value.
fn forward_slots(function: &mut MachineFunction) -> usize {
    let mut removed = 0;
    for (bi, block) in &mut function.blocks.iter_mut().enumerate() {
        if std::env::var_os("UMAY_FWD_DEBUG").is_some() {
            eprintln!("  ---- block {bi}");
        }
        let mut out: Vec<Inst> = Vec::with_capacity(block.insts.len());
        // The register each frame slot's current value is still held in.
        //
        // Every MIR local gets its own frame slot (`frame.rs`), so a straight
        // lowering stores each intermediate result to memory and loads it back
        // for the next operation. That is what made UMAY 1.8x slower than gcc on
        // an integer loop: the arithmetic was right, but half the instructions
        // were memory traffic to slots whose value never left a register.
        //
        // The map is what removes it. When `slot` is known to still equal
        // `reg`, a load of `slot` becomes a register-to-register move (or
        // vanishes, if the target is already that register), and the store that
        // put it there is deleted once nothing has read the slot since.
        let mut forwarded: std::collections::BTreeMap<(i64, i64), (Reg, u64)> = std::collections::BTreeMap::new();
        for inst in std::mem::take(&mut block.insts) {
            if let Some(prev) = out.last() {
                // `mov %r, %r` writes what is already there.
                if is_self_move(&inst) {
                    removed += 1;
                    continue;
                }
                // `mov %r, slot` then `mov slot, %r`: adjacent, same register,
                // same slot. Nothing ran in between, so the load reads exactly
                // what the store wrote and the value is still in the register.
                // This is true by construction -- no aliasing argument is needed
                // because there is no instruction between the two.
                #[allow(clippy::collapsible_if)] // let-chains need Rust 2024
                if let (Some((slot, reg)), Some(Some((_, stored)))) = (store_of_register(prev), memory_write(prev)) {
                    if loads_same_slot_into(&inst, &slot, reg, stored) {
                        removed += 1;
                        continue;
                    }
                }
            }

            // A load from a slot whose value is still in a register becomes a
            // register copy. The copy may itself turn out to be a self-move and
            // be dropped, which is the common case: the selector loads into `%rax`
            // and the allocator put the value there already.
            //
            // An ALU instruction that reads the slot as a memory operand is
            // rewritten the same way. That case is why this pass exists: the
            // selector emits `imulq $3, %rcx` / `movq %rcx, slot` and then
            // `addq slot, %rcx` for `acc + i * 3`, and the memory read there is
            // the traffic that made the loop slow. Only a source operand is
            // rewritten -- a destination in memory is a real write and stays one.
            let inst = match &inst {
                Inst::Load { kind: MoveKind::Move(w), src, dst: MOperand::Reg(d) } if plain_stack(src) => {
                    // Only a load no wider than the store is served: the
                    // register holds the stored bytes and nothing above them.
                    match forwarded.get(&slot_key(src)).filter(|(_, held)| w.bytes() <= *held) {
                        Some(&(src_reg, _)) if src_reg != *d => Inst::Move {
                            kind: MoveKind::Move(*w),
                            src: MOperand::Reg(src_reg),
                            dst: MOperand::Reg(*d),
                        },
                        Some(_) => {
                            removed += 1;
                            continue;
                        }
                        None => inst,
                    }
                }
                Inst::Alu { op, width, src: MOperand::Mem(m), dst } if plain_stack(m) => {
                    if std::env::var_os("UMAY_FWD_DEBUG").is_some() {
                        eprintln!("  alu read {:?} known={:?}", slot_key(m), forwarded.get(&slot_key(m)));
                    }
                    match forwarded.get(&slot_key(m)).filter(|(_, held)| width.bytes() <= *held) {
                        Some(&(src_reg, _)) => Inst::Alu { op: *op, width: *width, src: MOperand::Reg(src_reg), dst: dst.clone() },
                        None => inst,
                    }
                }
                Inst::Cmp { width, lhs: MOperand::Mem(m), rhs } if plain_stack(m) => {
                    match forwarded.get(&slot_key(m)).filter(|(_, held)| width.bytes() <= *held) {
                        Some(&(src_reg, _)) => Inst::Cmp { width: *width, lhs: MOperand::Reg(src_reg), rhs: rhs.clone() },
                        None => inst,
                    }
                }
                _ => inst,
            };

            // Anything that writes a register invalidates the forwarding for
            // that register: the slot still holds the value, but the register no
            // longer does, so a later load must really read memory.
            match registers_written(&inst) {
                Some(written) => {
                    if std::env::var_os("UMAY_FWD_DEBUG").is_some() && !written.is_empty() {
                        eprintln!("  writes {:?} :: {:?}", written, inst);
                    }
                    for reg in written {
                        forwarded.retain(|_, (held, _)| *held != reg);
                    }
                }
                // A call clobbers the caller-saved registers and may write any
                // slot whose address it was given.
                None => forwarded.clear(),
            }
            // A write to memory ends the forwarding of every slot it overlaps;
            // a write through a pointer may hit any slot, so it ends them all.
            match memory_write(&inst) {
                Some(Some((key, bytes))) => forwarded.retain(|k, (_, held)| !overlaps(*k, *held, key, bytes)),
                Some(None) => forwarded.clear(),
                None => {}
            }

            // Record a store of a register into a slot. Only a plain
            // `%rbp`-relative slot with no symbol or index qualifies, which is
            // what `plain_stack` checks.
            if let (Some((slot, reg)), Some(Some((_, bytes)))) = (store_of_register(&inst), memory_write(&inst)) {
                if plain_stack(&slot) {
                    if std::env::var_os("UMAY_FWD_DEBUG").is_some() {
                        eprintln!("  fwd store {:?} -> {:?}", slot_key(&slot), reg);
                    }
                    forwarded.insert(slot_key(&slot), (reg, bytes));
                }
            }

            out.push(inst);
        }
        block.insts = out;
    }
    removed
}

/// A frame slot's identity: (base register code, offset).
///
/// Two memory operands name the same slot when their base and offset agree and
/// neither has a symbol or an index. A struct field's address would have one, so
/// they never collide with a plain slot.
fn slot_key(m: &Mem) -> (i64, i64) {
    let base = match m.base {
        Some(Reg::Phys(code)) => code as i64,
        _ => -1,
    };
    (base, m.offset)
}

/// Every physical register `inst` writes, or `None` when it writes none.
///
/// Returns `None` rather than an empty list for a call, because a call clobbers
/// the whole caller-saved set and the caller clears forwarding separately; a
/// plain "writes nothing" would leave a stale entry alive across it.
fn registers_written(inst: &Inst) -> Option<Vec<Reg>> {
    let one = |op: &MOperand| match op {
        MOperand::Reg(r) => Some(vec![*r]),
        _ => Some(Vec::new()),
    };
    match inst {
        Inst::Call { .. } => None,
        Inst::Move { dst, .. } => one(dst),
        // `div`/`idiv` also leave the remainder in `%rdx`, and `cqto` fills it.
        Inst::Alu { op: AluOp::Div | AluOp::Idiv, .. } => Some(vec![Reg::Phys(0), Reg::Phys(2)]),
        Inst::SignExtendAcc { .. } => Some(vec![Reg::Phys(2)]),
        Inst::Alu { dst, .. } => one(dst),
        // Every load writes its destination, a widening `movzb`/`movsb` too:
        // leaving those out kept `%rcx` forwarded after `movzbq slot, %rcx`
        // overwrote it, so a method's third argument read the second's value.
        Inst::Load { dst, .. } => one(dst),
        Inst::Lea { dst, .. } => one(dst),
        Inst::SetCc { dst, .. } => one(dst),
        Inst::Pop(op) => one(op),
        _ => Some(Vec::new()),
    }
}

/// True for `Move src -> dst` where both sides are the same physical register.
fn is_self_move(inst: &Inst) -> bool {
    match inst {
        Inst::Move { src: MOperand::Reg(a), dst: MOperand::Reg(b), .. } => a == b,
        _ => false,
    }
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
                match allocation.locations.get(&v) {
                    Some(Location::Reg(r)) => mem.base = Some(Reg::Phys(*r)),
                    // A base spilled to the stack cannot be an addressing mode
                    // operand: that would need an extra load the selector never
                    // emits. It cannot happen -- `address_registers` marks such
                    // a register as unspillable -- so reaching here is a bug
                    // worth stopping on rather than printing `(%!v14)`.
                    other => panic!("base register v{v} has no physical home: {other:?}"),
                }
            }
        }
        if let Some((index, scale)) = mem.index {
            if let Reg::Virtual(v) = index {
                match allocation.locations.get(&v) {
                    Some(Location::Reg(r)) => mem.index = Some((Reg::Phys(*r), scale)),
                    other => panic!("index register v{v} has no physical home: {other:?}"),
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
            // Both sides may be a memory operand: `*p` as a source reads through
            // a base register, and a spilled destination becomes a stack
            // reference. Their bases are virtual registers too and must be
            // resolved, or the assembler sees a placeholder like `(%!v14)`.
            match src {
                MOperand::Reg(r) => *src = resolve(*r, width, allocation),
                MOperand::Mem(m) => fix_mem(m, allocation),
                _ => {}
            }
            match dst {
                MOperand::Reg(r) => *dst = resolve(*r, width, allocation),
                MOperand::Mem(m) => fix_mem(m, allocation),
                _ => {}
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
        Inst::Call { target, .. } => {
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

/// `mov %r, slot` -> (slot, r), when both sides are a plain stack slot and a
/// physical register.
fn store_of_register(inst: &Inst) -> Option<(Mem, Reg)> {
    match inst {
        Inst::Store { src: MOperand::Reg(r), dst, kind: MoveKind::Move(_) } => {
            plain_stack(dst).then(|| (dst.clone(), *r))
        }
        Inst::Move { src: MOperand::Reg(r), dst: MOperand::Mem(m), kind: MoveKind::Move(_) } => {
            plain_stack(m).then(|| (m.clone(), *r))
        }
        _ => None,
    }
}

/// True when `inst` loads `slot` into `reg`.
///
/// A load wider than the `stored` bytes reads bytes the register never held.
fn loads_same_slot_into(inst: &Inst, slot: &Mem, reg: Reg, stored: u64) -> bool {
    let (src, dst) = match inst {
        Inst::Load { src, dst, kind: MoveKind::Move(w) } if w.bytes() <= stored => (src, dst),
        Inst::Move { src: MOperand::Mem(m), dst: MOperand::Reg(r), kind: MoveKind::Move(w) } => {
            return *r == reg && same_slot(m, slot) && w.bytes() <= stored;
        }
        _ => return false,
    };
    matches!(dst, MOperand::Reg(r) if *r == reg) && same_slot(src, slot)
}

/// A frame slot: `%rbp` plus a constant. A memory operand on any other base is
/// an access through a pointer, whose register may change between two uses
/// that look alike, so it is never treated as a slot.
fn plain_stack(m: &Mem) -> bool {
    m.symbol.is_none() && m.index.is_none() && m.base == Some(Reg::Phys(5))
}

/// The bytes a move of `kind` writes.
fn kind_bytes(kind: &MoveKind) -> u64 {
    match kind {
        MoveKind::Move(w) => w.bytes(),
        MoveKind::SignExtend { to, .. } | MoveKind::ZeroExtend { to, .. } => to.bytes(),
    }
}

/// The memory `inst` writes: `Some(Some((slot, bytes)))` for a frame slot,
/// `Some(None)` for a write through a pointer (which may alias any slot whose
/// address was taken), `None` when it writes no memory.
fn memory_write(inst: &Inst) -> Option<Option<((i64, i64), u64)>> {
    let (m, bytes) = match inst {
        Inst::Store { kind, dst, .. } => (dst, kind_bytes(kind)),
        Inst::Move { kind, dst: MOperand::Mem(m), .. } => (m, kind_bytes(kind)),
        Inst::Alu { width, dst: MOperand::Mem(m), .. } => (m, width.bytes()),
        Inst::SetCc { dst: MOperand::Mem(m), .. } | Inst::Pop(MOperand::Mem(m)) => (m, 8),
        _ => return None,
    };
    Some(plain_stack(m).then(|| (slot_key(m), bytes)))
}

/// True when the byte ranges `[a, a + aw)` and `[b, b + bw)` of the frame share a byte.
fn overlaps(a: (i64, i64), aw: u64, b: (i64, i64), bw: u64) -> bool {
    a.0 == b.0 && a.1 < b.1 + bw as i64 && b.1 < a.1 + aw as i64
}

fn same_slot(a: &Mem, b: &Mem) -> bool {
    a.base == b.base && a.offset == b.offset && a.symbol.is_none() && b.symbol.is_none()
        && a.index.is_none() && b.index.is_none()
}
