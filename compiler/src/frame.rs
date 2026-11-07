//! Stack frame layout for one function.
//!
//! Every MIR local gets a dedicated stack slot. That is generous with stack
//! space and makes instruction selection simple: a `Place` is always an address
//! the selector can compute from `rbp` plus a constant, with no register
//! pressure to reason about. Register allocation then removes the traffic that
//! this creates between a slot and the temporary that reads it.
//!
//! Slots are assigned in declaration order, each aligned to its own type, which
//! keeps offsets stable and makes the emitted assembly readable.

use crate::layout::{Layout, LayoutError, Layouts};
use crate::mir::Body;
use crate::types::TyId;

/// One local's home in the frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Slot {
    /// Offset from `rbp`. Negative, because the stack grows down.
    pub offset: i64,
    /// The type stored there, so the selector knows how wide a load or store is.
    pub ty: TyId,
    pub size: u64,
    pub align: u64,
}

/// The frame of one function.
#[derive(Debug, Clone)]
pub struct Frame {
    /// One slot per MIR local, indexed by `LocalId`.
    pub slots: Vec<Slot>,
    /// Total size of the locals area, before outgoing arguments.
    pub local_bytes: u64,
    /// The slot holding the hidden return pointer, when the function returns an
    /// aggregate larger than two eightbytes. `None` otherwise.
    pub sret: Option<Slot>,
}

impl Frame {
    pub fn slot(&self, local: crate::hir::LocalId) -> Slot {
        self.slots[local.0 as usize]
    }

    /// The address of a local as a memory operand: `offset(%rbp)`.
    pub fn place_mem(&self, local: crate::hir::LocalId) -> crate::machine::Mem {
        crate::machine::Mem::stack(self.slot(local).offset)
    }
}

/// Assigns a slot to every local in `body`.
///
/// Zero-sized locals get a slot too, so that `slot()` never has to special-case
/// them; the slot costs nothing because it is zero bytes and the next one
/// overlaps it harmlessly (no load or store of a zero-sized value is emitted).
pub fn build_frame(body: &Body, layouts: &mut Layouts) -> Result<Frame, LayoutError> {
    let mut slots = Vec::with_capacity(body.locals.len());
    let mut offset: i64 = 0;
    for local in &body.locals {
        let layout = match layouts.layout(local.ty) {
            Ok(l) => l,
            // A zero-sized or unsized type still needs a slot entry so that the
            // mapping is total. An unsized local cannot be loaded or stored, so
            // a placeholder slot is safe.
            Err(LayoutError::Unsized(_)) => Layout::new(0, 1),
            Err(e) => return Err(e),
        };
        let size = layout.size;
        let align = layout.align();
        // The slot's start must be aligned, so the *current* offset is aligned up
        // first and only then advanced by the size. Advancing before aligning
        // gives the same result for power-of-two sizes, but a zero-sized local
        // would then share an offset with the local before it, and writing
        // through it would overwrite that local's first byte.
        let start = round_up(offset, align.max(1) as i64);
        // A zero-sized local is given a real byte so that distinct locals never
        // share an address.
        let reserved = size.max(1);
        offset = start + reserved as i64;
        slots.push(Slot { offset: -offset, ty: local.ty, size, align });
    }
    // A function that returns a large aggregate receives a pointer to the
    // caller's storage in `rdi`.
    let sret = if body.ret_is_sret {
        offset = round_up(offset, 8) + 8;
        Some(Slot { offset: -offset, ty: body.ret_ty, size: 8, align: 8 })
    } else {
        None
    };
    // The frame as a whole is kept 16-byte aligned so that calls can reuse the
    // same alignment without a second adjustment.
    let local_bytes = round_up(offset, 16).max(0) as u64;
    Ok(Frame { slots, local_bytes, sret })
}

/// Rounds `n` up to a multiple of `align`.
///
/// Written by hand rather than with `div_ceil`, which is not stable for signed
/// integers.
fn round_up(n: i64, align: i64) -> i64 {
    if align <= 1 {
        return n;
    }
    let rem = n % align;
    if rem == 0 {
        n
    } else {
        n + (align - rem)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame_for(src: &str) -> Frame {
        let mut map = crate::source::SourceMap::new();
        let id = map.add("test.um", src);
        let out = crate::parse_file(&map, id);
        let mut sema = crate::sema::analyze(&out.program, &map);
        let mut lowered = crate::lower::lower(&sema.hir, &mut sema.types);
        crate::drop_elab::elaborate(&mut lowered.program, &sema.types);
        let mut layouts = Layouts::new(&mut sema.types, &sema.hir);
        build_frame(&lowered.program.bodies[0], &mut layouts).expect("frame")
    }

    #[test]
    fn every_local_gets_a_slot() {
        let frame = frame_for("fn f(a: i32, b: i32) -> i32 { a + b }");
        assert!(frame.slots.len() >= 3, "args and return slot need slots");
        assert!(frame.local_bytes >= 12);
    }

    #[test]
    fn slots_do_not_overlap() {
        let frame = frame_for("fn f(a: i64, b: i64) -> i64 { a + b }");
        // Slots are assigned downward from rbp, so walking back up they must
        // never re-use bytes.
        let mut ranges: Vec<(i64, i64)> = frame
            .slots
            .iter()
            .filter(|s| s.size > 0)
            .map(|s| (s.offset, s.offset + s.size as i64))
            .collect();
        ranges.sort();
        for pair in ranges.windows(2) {
            assert!(
                pair[0].1 <= pair[1].0,
                "slots {:?} and {:?} overlap",
                pair[0],
                pair[1]
            );
        }
    }

    #[test]
    fn alignment_is_respected() {
        let frame = frame_for("struct S { a: u8, b: u64 }\nfn f(s: S) -> u64 { s.b }");
        for slot in &frame.slots {
            if slot.size >= 8 {
                assert_eq!(
                    (slot.offset.unsigned_abs()) % 8,
                    0,
                    "an 8-byte slot must be 8-aligned: {slot:?}"
                );
            }
        }
    }

    #[test]
    fn frame_size_is_sixteen_aligned() {
        let frame = frame_for("fn f(a: i32) -> i32 { a }");
        assert_eq!(frame.local_bytes % 16, 0);
    }
}
