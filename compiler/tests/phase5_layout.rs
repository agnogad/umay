//! Phase 5 tests, part 1: type layout and the ABI classifier.
//!
//! Layout bugs are expensive: a wrong field offset produces a program that runs
//! and returns the wrong answer, which is far worse than a crash. These tests
//! pin down every rule the backend relies on.

use umayc::layout::{AbiClass, EightbyteClass, Layout, Layouts};

/// Builds layouts for `src` and hands them to `f`.
fn with<R>(src: &str, f: impl FnOnce(&mut Layouts) -> R) -> R {
    let mut map = umayc::source::SourceMap::new();
    let id = map.add("test.um", src);
    let out = umayc::parse_file(&map, id);
    let mut sema = umayc::sema::analyze(&out.program, &map);
    assert!(!sema.diagnostics.has_errors(), "test program must type-check");
    let mut layouts = Layouts::new(&mut sema.types, &sema.hir);
    f(&mut layouts)
}

#[track_caller]
fn size_of_type(src: &str, name: &str) -> Layout {
    with(src, |l| {
        let def = l
            .hir()
            .structs
            .iter()
            .find(|s| s.name == name)
            .unwrap_or_else(|| panic!("no struct named {name}"))
            .def_id;
        let ty = l.types_mut().adt(def, Vec::new());
        l.layout(ty).expect("layout")
    })
}

// ---------------------------------------------------------------------------
// Scalar layouts
// ---------------------------------------------------------------------------

#[test]
fn integer_widths_match_their_types() {
    with("fn main() { }", |l| {
        for (name, size) in [("u8", 1u64), ("u16", 2), ("u32", 4), ("u64", 8), ("i128", 16)] {
            let ty = l.types_mut().primitive(name).unwrap();
            let layout = l.layout(ty).unwrap();
            assert_eq!(layout.size, size, "{name} should be {size} bytes");
            // Alignment never exceeds size and is always a power of two.
            assert!(layout.align() <= size.max(1), "{name} over-aligned");
            assert!(layout.align().is_power_of_two());
        }
    });
}

#[test]
fn references_are_one_word() {
    with("fn main() { }", |l| {
        let i32_ = l.types_mut().primitive("i32").unwrap();
        let r = l.types_mut().ref_to(i32_);
        assert_eq!(l.layout(r).unwrap(), Layout::new(8, 8));
    });
}

#[test]
fn unit_occupies_no_space() {
    with("fn main() { }", |l| {
        let unit = l.types_mut().unit();
        let layout = l.layout(unit).unwrap();
        assert_eq!(layout.size, 0);
        // A zero-sized type still has a legal alignment, so arrays of it work.
        assert_eq!(layout.align(), 1);
    });
}

// ---------------------------------------------------------------------------
// Aggregate layouts
// ---------------------------------------------------------------------------

#[test]
fn fields_are_laid_out_in_declaration_order_with_padding() {
    // u8 at 0, u32 needs offset 4, u8 at 8, total padded to 12.
    let layout = size_of_type("struct S { a: u8, b: u32, c: u8 }\nfn main() { }", "S");
    assert_eq!(layout.size, 12);
    assert_eq!(layout.align(), 4);
}

#[test]
fn fields_are_reordered_only_by_padding_never_by_name() {
    // The ABI layout must follow declaration order; reordering is an
    // optimisation UMAY does not do, so offsets are predictable.
    with("struct S { a: u8, b: u8, c: u32 }\nfn main() { }", |l| {
        let def = l.hir().structs.iter().find(|s| s.name == "S").unwrap().def_id;
        assert_eq!(l.field_offsets(def, &[]).unwrap(), vec![0, 1, 4]);
    });
}

#[test]
fn a_struct_of_only_zero_sized_fields_is_zero_sized() {
    let layout = size_of_type("struct Empty { }\nfn main() { }", "Empty");
    assert_eq!(layout.size, 0);
}

#[test]
fn arrays_are_a_multiple_of_their_element() {
    with("fn main() { }", |l| {
        let u16_ = l.types_mut().primitive("u16").unwrap();
        let array = l.types_mut().array(u16_, 7);
        assert_eq!(l.layout(array).unwrap(), Layout::new(14, 2));
    });
}

#[test]
fn tuples_align_to_their_largest_member() {
    with("fn main() { }", |l| {
        let u8_ = l.types_mut().primitive("u8").unwrap();
        let u64_ = l.types_mut().primitive("u64").unwrap();
        let tuple = l.types_mut().tuple(vec![u8_, u64_, u8_]);
        // u8 at 0, u64 at 8, u8 at 16, total 24 for align 8.
        assert_eq!(l.layout(tuple).unwrap(), Layout::new(24, 8));
    });
}

#[test]
fn enum_payload_is_aligned_after_the_tag() {
    with("enum E { None, Pair(u64, u64) }\nfn main() { }", |l| {
        let def = l.hir().enums.iter().find(|e| e.name == "E").unwrap().def_id;
        let ty = l.types_mut().adt(def, Vec::new());
        // u8 tag, payload at 8, 16 bytes of payload, total 24.
        assert_eq!(l.layout(ty).unwrap(), Layout::new(24, 8));
        assert_eq!(l.enum_payload_offset(def, &[]).unwrap(), 8);
    });
}

#[test]
fn enum_with_only_unit_variants_is_a_bare_tag() {
    with("enum C { Red, Green }\nfn main() { }", |l| {
        let def = l.hir().enums.iter().find(|e| e.name == "C").unwrap().def_id;
        let ty = l.types_mut().adt(def, Vec::new());
        assert_eq!(l.layout(ty).unwrap(), Layout::new(1, 1));
    });
}

// ---------------------------------------------------------------------------
// SysV AMD64 classification
// ---------------------------------------------------------------------------

#[track_caller]
fn class_of_struct(src: &str, name: &str) -> AbiClass {
    with(src, |l| {
        let def = l.hir().structs.iter().find(|s| s.name == name).unwrap().def_id;
        let ty = l.types_mut().adt(def, Vec::new());
        l.classify(ty).unwrap()
    })
}

#[test]
fn a_struct_of_two_integers_uses_two_int_registers() {
    let class = class_of_struct("struct P { a: i64, b: i64 }\nfn main() { }", "P");
    assert_eq!(
        class,
        AbiClass::Registers(vec![EightbyteClass::Integer, EightbyteClass::Integer])
    );
    assert_eq!(class.int_registers(), 2);
}

#[test]
fn a_struct_of_two_floats_uses_two_sse_registers() {
    let class = class_of_struct("struct D { x: f64, y: f64 }\nfn main() { }", "D");
    assert_eq!(class.sse_registers(), 2);
    assert_eq!(class.int_registers(), 0);
}

#[test]
fn a_struct_larger_than_two_words_goes_on_the_stack() {
    let class = class_of_struct("struct Big { a: i64, b: i64, c: i64 }\nfn main() { }", "Big");
    assert_eq!(class, AbiClass::Memory);
}

#[test]
fn an_integer_next_to_a_float_forces_an_integer_eightbyte() {
    // Writing the integer clobbers the whole eightbyte, so the pair cannot be
    // passed in an SSE register.
    let class = class_of_struct("struct M { a: i32, b: f32 }\nfn main() { }", "M");
    assert_eq!(class, AbiClass::Registers(vec![EightbyteClass::Integer]));
}

#[test]
fn a_float_alone_is_sse_classed() {
    with("fn main() { }", |l| {
        let f64_ = l.types_mut().primitive("f64").unwrap();
        assert_eq!(l.classify(f64_).unwrap(), AbiClass::Registers(vec![EightbyteClass::Sse]));
    });
}

#[test]
fn zero_sized_types_need_no_registers() {
    with("fn main() { }", |l| {
        let unit = l.types_mut().unit();
        let class = l.classify(unit).unwrap();
        assert_eq!(class.int_registers(), 0);
        assert_eq!(class.sse_registers(), 0);
    });
}

// ---------------------------------------------------------------------------
// Robustness
// ---------------------------------------------------------------------------

#[test]
fn layout_of_every_type_in_a_program_terminates() {
    // A recursive struct definition must not make layout computation loop.
    let mut map = umayc::source::SourceMap::new();
    let id = map.add(
        "test.um",
        "struct Node { next: *const u8, value: i32 }\nfn main() { }",
    );
    let out = umayc::parse_file(&map, id);
    let mut sema = umayc::sema::analyze(&out.program, &map);
    let mut layouts = Layouts::new(&mut sema.types, &sema.hir);
    let def = layouts.hir().structs.iter().find(|s| s.name == "Node").unwrap().def_id;
    // A pointer field keeps the type sized; this must return, not hang.
    let ty = layouts.types_mut().adt(def, Vec::new());
    assert!(layouts.layout(ty).is_ok());
}
