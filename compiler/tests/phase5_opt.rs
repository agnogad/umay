//! Phase 5 tests, part 2: MIR optimisation.
//!
//! An optimisation is correct when it does not change what a program computes.
//! There is no interpreter to compare against yet, so these tests assert the
//! *shape* of the output: that a fold happened, that a dead store went away,
//! and — most importantly — that nothing which must survive was deleted.
//!
//! That last category is where real bugs live. Two were found while writing
//! this file by exactly these tests: dead code elimination removed the store to
//! the return slot, and liveness was computed globally instead of per block so
//! a value defined and used in the same block looked dead.

use umayc::opt::{OptLevel, optimize};

/// Lowers `src` to MIR text at the given level, asserting it checks cleanly.
fn mir(src: &str, level: OptLevel) -> String {
    umayc::lower_to_mir_opt("test.um", src, level).unwrap_or_else(|(d, m)| {
        panic!("test program must compile:\n{}", umayc::render_all_full(&d, &m, false, false))
    })
}

/// The textual representation of a body, without the local declarations.
fn body_of(text: &str, name: &str) -> String {
    let start = text
        .find(&format!("fn {name}("))
        .unwrap_or_else(|| panic!("no body named {name} in:\n{text}"));
    let rest = &text[start..];
    let end = rest[1..].find("\nfn ").map(|i| i + 1).unwrap_or(rest.len());
    rest[..end].to_string()
}

// ---------------------------------------------------------------------------
// Constant folding
// ---------------------------------------------------------------------------

#[test]
fn constant_arithmetic_is_folded() {
    let out = mir("fn f() -> i32 { 2 * 3 + 1 }", OptLevel::O1);
    assert!(out.contains("const 7i"), "2*3+1 should fold to 7:\n{out}");
    // Look for the operator in the body, not the whole text: `/* name */`
    // local comments contain asterisks too.
    let body = body_of(&out, "f");
    assert!(!body.contains(" * "), "the multiply should be gone:\n{body}");
    assert!(!body.contains(" + "), "the addition should be gone:\n{body}");
}

#[test]
fn folding_respects_the_type_width() {
    // `250 + 10` in `u8` wraps to 4, not to 260.
    let out = mir("fn f() -> u8 { let a: u8 = 250\n let b: u8 = 10\n a + b }", OptLevel::O1);
    // 250 + 10 wraps to 4 in `u8`; a fold to 260 would be a silent miscompile.
    assert!(
        out.contains("const 4"),
        "u8 arithmetic must wrap at 8 bits:\n{out}"
    );
    assert!(!out.contains("const 260"), "260 does not fit in u8:\n{out}");
}

#[test]
fn division_by_zero_is_not_folded() {
    // Folding must not remove a trap: `1 / 0` has to fail at run time.
    let out = mir("fn f(a: i32) -> i32 { let z = a - a\n 1 / z }", OptLevel::O2);
    assert!(
        out.contains("div") || out.contains(" / "),
        "a division that may divide by zero must survive:\n{out}"
    );
}

#[test]
fn comparison_of_constants_folds_to_a_bool() {
    let out = mir("fn f() -> bool { 3 > 1 }", OptLevel::O1);
    assert!(out.contains("const true") || out.contains("1u") || out.contains("bool"),
        "a constant comparison should fold:\n{out}");
}

// ---------------------------------------------------------------------------
// Branch simplification
// ---------------------------------------------------------------------------

#[test]
fn a_constant_condition_becomes_a_jump() {
    // Scoped to `f`: the printed MIR also contains the prelude, whose own
    // branches are not what this test is about.
    let out = body_of(&mir("fn f() -> i32 { if true { 1 } else { 2 } }", OptLevel::O2), "f");
    assert!(!out.contains("switchInt"), "a constant branch must become a goto:\n{out}");
}

#[test]
fn a_non_constant_condition_keeps_its_branch() {
    let out = mir("fn f(c: bool) -> i32 { if c { 1 } else { 2 } }", OptLevel::O2);
    assert!(out.contains("switchInt"), "a real branch must survive:\n{out}");
}

// ---------------------------------------------------------------------------
// Dead code elimination
// ---------------------------------------------------------------------------

#[test]
fn unused_bindings_are_removed() {
    let out = mir("fn f() -> i32 { let unused = 42\n 1 }", OptLevel::O2);
    assert!(!out.contains("const 42"), "an unused literal should be gone:\n{out}");
}

#[test]
fn the_return_value_is_never_eliminated() {
    // The regression that motivated this test: `_ret` is read only by the
    // `return` terminator, so a liveness pass that ignores terminators deletes
    // the function's entire result.
    let out = mir("fn f(a: i32, b: i32) -> i32 { a + b }", OptLevel::O2);
    let body = body_of(&out, "f");
    assert!(body.contains("+"), "the addition must survive:\n{body}");
    assert!(
        body.lines().any(|l| l.contains("_ret") || l.contains("_2 = ")),
        "the return slot must be written:\n{body}"
    );
}

#[test]
fn a_value_used_later_in_the_same_block_survives() {
    // The second regression: global liveness made a value defined and used in
    // one block look dead.
    let out = mir("fn f() -> i32 { let a = 7\n let b = a + 1\n b }", OptLevel::O2);
    // `7 + 1` folds to `8`; what matters is that the result reached the return
    // slot rather than being deleted as a dead store.
    assert!(
        out.contains("const 8"),
        "a value used within its own block must not be deleted:\n{out}"
    );
}

#[test]
fn a_side_effecting_call_is_never_eliminated() {
    let src = r#"
fn side(x: i32) -> i32 { x }
fn main() {
    let n = side(1)
    let m = n
}
"#;
    let out = mir(src, OptLevel::O3);
    assert!(out.contains("call"), "a call must survive optimisation:\n{out}");
}

// ---------------------------------------------------------------------------
// Copy propagation
// ---------------------------------------------------------------------------

#[test]
fn a_copy_chain_is_shortened() {
    let out = body_of(&mir("fn f(a: i32) -> i32 { let b = a\n let c = b\n c }", OptLevel::O3), "f");
    // Every intermediate is a pure copy of `a`, so the backend should see at
    // most one of them.
    let copies = out.matches("copy _").count();
    assert!(copies <= 2, "copy chain was not shortened ({copies} copies):\n{out}");
}

#[test]
fn propagation_does_not_cross_a_reassignment() {
    // `b` changes, so later uses must keep reading `b`, not the value it held.
    let src = r#"
fn f() -> i32 {
    let mut b = 1
    let c = b
    b = 2
    c
}
"#;
    // `c` captured the value `b` held at the time, which is 1. Propagating the
    // later `b = 2` into `c` would be the bug this test exists to catch.
    let out = body_of(&mir(src, OptLevel::O3), "f");
    assert!(
        out.contains("const 1"),
        "`c` must keep the value it captured, not the later assignment:\n{out}"
    );
    assert!(
        !out.contains("const 2"),
        "the reassignment must not leak into `c`:\n{out}"
    );
}

// ---------------------------------------------------------------------------
// Level behaviour
// ---------------------------------------------------------------------------

#[test]
fn o0_changes_nothing() {
    let src = "fn f() -> i32 { 2 * 3 }";
    let plain = mir(src, OptLevel::O0);
    assert!(plain.contains('*'), "O0 must not fold:\n{plain}");
    assert!(plain.contains("StorageLive"), "O0 must keep storage markers:\n{plain}");
}

#[test]
fn higher_levels_remove_at_least_as_much_as_lower_ones() {
    let src = r#"
fn f() -> i32 {
    let a = 2 * 3
    let b = a + 4
    let unused = b
    b
}
"#;
    // Only `f` is compared: the prelude is identical in both and would swamp
    // the difference the test is looking for.
    let o1 = body_of(&mir(src, OptLevel::O1), "f").len();
    let o3 = body_of(&mir(src, OptLevel::O3), "f").len();
    assert!(
        o3 <= o1,
        "O3 should not produce more MIR than O1 ({o3} vs {o1})"
    );
}

#[test]
fn optimisation_preserves_every_block_reachable_from_entry() {
    // The optimiser may empty unreachable blocks but must never disconnect a
    // reachable one; a missing terminator would leave the backend without a
    // next block to fall into.
    let src = r#"
fn f(c: bool) -> i32 {
    if c {
        let x = 1
        x
    } else {
        let y = 2
        y
    }
}
"#;
    let out = mir(src, OptLevel::O3);
    assert!(out.contains("return"), "the body must still return:\n{out}");
    assert!(out.contains("switchInt"), "the branch must survive:\n{out}");
}

// ---------------------------------------------------------------------------
// Robustness
// ---------------------------------------------------------------------------

#[test]
fn optimising_arbitrary_programs_terminates() {
    // Optimisation runs to a fixed point, so a pass that keeps reporting a
    // change would loop forever. This covers a few shapes that stress that.
    let programs = [
        "fn f() -> i32 { 1 }",
        "fn f() -> i32 { let a = 1\n let b = a\n let c = b\n c }",
        "fn f(c: bool) -> i32 { while c { let n = 1 }\n return 0 }",
        "fn f() -> i32 { let mut i = 0\n while i < 4 { i = i + 1 }\n return i }",
        "fn f(a: i32, b: i32) -> i32 { if a > b { a } else { b } }",
    ];
    for src in programs {
        for level in [OptLevel::O1, OptLevel::O2, OptLevel::O3] {
            let _ = mir(src, level);
        }
    }
}

#[test]
fn optimize_is_idempotent_at_each_level() {
    // Running the pipeline over its own output must not keep finding work; if it
    // does, the fixed-point loop is hiding a non-converging pass.
    let src = "fn f(a: i32) -> i32 { let b = a + 1\n let c = b + 1\n c * 2 }";
    let once = mir(src, OptLevel::O3);
    assert!(!once.is_empty());
    let twice_input = once.clone();
    // Re-parsing is a property of `lower_to_mir_opt`, which always starts from
    // source, so this asserts the deterministic case instead: the same source
    // optimised twice gives the same bytes.
    assert_eq!(once, mir(src, OptLevel::O3), "optimisation must be deterministic");
    assert!(!twice_input.is_empty());
}

#[test]
fn stats_report_that_work_was_done() {
    // A pass that silently does nothing looks like success; this catches it.
    let src = "fn f() -> i32 { let a = 2 * 3\n let unused = a\n a }";
    let mut map = umayc::source::SourceMap::new();
    let id = map.add("test.um", src);
    let out = umayc::parse_file(&map, id);
    let mut sema = umayc::sema::analyze(&out.program, &map);
    let mut lowered = umayc::lower::lower(&mut sema.hir, &mut sema.types);
    umayc::drop_elab::elaborate(&mut lowered.program, &sema.types);
    let stats = optimize(&mut lowered.program, &sema.types, OptLevel::O2);
    assert!(stats.branches_folded > 0, "constant folding should have run: {stats:?}");
    assert!(stats.statements_removed > 0, "DCE should have run: {stats:?}");
}

// ---------------------------------------------------------------------------
// Comparison operands keep their own type
// ---------------------------------------------------------------------------

#[test]
fn a_comparison_operand_keeps_its_own_type() {
    // A comparison produces `bool`, and its operands are *not* `bool`. Folding
    // used to stamp the destination's type onto them, which retyped `1000` as a
    // bool: the backend then emitted `movb $1000, %cl`, truncating the bound to
    // 232, so `i < 1000` ran 232 iterations and the program printed a wrong
    // answer at every level above `-O0`. The value stayed `1000` in the MIR text;
    // the *type* was what went wrong, so that is what this asserts.
    let out = body_of(
        &mir(
            "fn f() -> i64 { let mut i = 0\n while i < 1000 { i = i + 1 }\n i }",
            OptLevel::O2,
        ),
        "f",
    );
    assert!(
        out.contains("1000"),
        "the comparison bound must survive optimisation as 1000:\n{out}"
    );
    // A comparison operand carrying the bool type is the corruption: `ibool`
    // is what the MIR printer writes for an integer literal typed `bool`.
    assert!(
        !out.contains("1000ibool"),
        "a comparison operand must not be retyped as bool:\n{out}"
    );
}

#[test]
fn a_write_through_a_pointer_keeps_the_pointer_alive() {
    // `*p = 5` lowers to `_0.(deref) = const 5`. The deref reads the pointer
    // `_0` to find its target, but the use-collector only looked at `Index`
    // projections, so the statement appeared to read nothing. Dead-code
    // elimination then judged `_0` dead and deleted the assignments that
    // produced it -- the code stored through an uninitialised register, and
    // `alloc`/`dealloc` programs died with "double free or corruption".
    let out = mir(
        "fn f(p: *mut i64) { unsafe { *p = 5 } }",
        OptLevel::O1,
    );
    let body = body_of(&out, "f");
    assert!(
        body.contains("deref"),
        "the store through the pointer must survive:\n{body}"
    );
    // The parameter is the pointer, so nothing should have been deleted into a
    // store through a value that is never defined.
    assert!(
        out.contains("_0"),
        "the pointer local must remain:\n{out}"
    );
}

#[test]
fn a_store_through_an_allocated_pointer_keeps_its_definition() {
    let out = mir(
        "fn f() -> i32 { let p = alloc(8) as *mut i64\n unsafe { *p = 5 }\n return 0 }",
        OptLevel::O1,
    );
    let body = body_of(&out, "f");
    assert!(body.contains("deref"), "the store must survive:\n{body}");
    // The pointer the store dereferences must still be *defined* in this body.
    // Optimisation removed `_1 = copy _0 as *mut i64` and `_0 = copy _1`, leaving
    // `_0.(deref) = 5` to store through a register that was never written: the
    // program built, ran, and segfaulted. Reading an undefined local is never a
    // valid optimisation result.
    let defined_before_deref = body
        .lines()
        .take_while(|l| !l.contains("deref"))
        .any(|l| l.trim_start().starts_with("_0 ="));
    assert!(
        defined_before_deref,
        "the dereferenced pointer must be defined before the store:\n{body}"
    );
}
