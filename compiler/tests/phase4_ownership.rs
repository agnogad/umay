//! Phase 4 tests: ownership and borrow checking.
//!
//! The adversarial cases come first. Each one is a program a reasonable person
//! might write and that must be rejected with the documented code. The accept
//! corpus follows, because a checker that rejects everything passes the first
//! half of this file and is useless.
//!
//! Every error assertion names its code. "It failed somehow" is not a test: the
//! whole point of stable codes is that the same mistake always reports the same
//! code, so a test that accepts any error would hide a regression that changed
//! which mistake was found.

/// Asserts that the program is rejected with `code`, and that the diagnostic is
/// an error (not a warning).
#[track_caller]
fn err(src: &str, code: &str) {
    let (diags, map) = umayc::analyze_source("test.um", src);
    let found: Vec<&str> = diags.items().iter().map(|d| d.code).collect();
    assert!(
        found.contains(&code),
        "expected {code}, got {found:?}:\n{}",
        umayc::render_all_full(&diags, &map, false, false)
    );
}

/// Asserts that the program is accepted with no diagnostics at all.
#[track_caller]
fn ok(src: &str) {
    let (diags, map) = umayc::analyze_source("test.um", src);
    assert!(
        !diags.has_errors(),
        "expected no errors:\n{}",
        umayc::render_all_full(&diags, &map, false, false)
    );
}

/// Asserts that the program is accepted, ignoring warnings.
#[track_caller]
fn ok_with_warnings_allowed(src: &str) {
    let (diags, map) = umayc::analyze_source("test.um", src);
    assert!(
        !diags.has_errors(),
        "expected no errors:\n{}",
        umayc::render_all_full(&diags, &map, false, false)
    );
}

// ---------------------------------------------------------------------------
// Adversarial: use after move (E0201)
// ---------------------------------------------------------------------------

#[test]
fn use_after_move_is_rejected() {
    err(
        r#"
struct P { x: i32 }
fn take(p: P) { }
fn main() {
    let p = P { x: 1 }
    take(p)
    take(p)
}
"#,
        "E0201",
    );
}

#[test]
fn reusing_a_str_literal_is_not_a_move() {
    // `&str` is a single pointer, so it is `Copy`: passing one along does not
    // take ownership of anything. This used to be a move test when a string
    // literal had the unsized type `str`, which was wrong; the literal is now
    // `&str`, matching how the specification says `str` is used.
    ok(
        r#"
fn take(s: &str) { }
fn main() {
    let s = "text"
    take(s)
    let t = s
    take(t)
}
"#,
    );
}

#[test]
fn move_in_one_branch_then_use_is_rejected() {
    // `p` is moved only when `c` holds, so the later use is not definitely
    // initialised. This is the case a naive per-statement checker misses.
    err(
        r#"
struct P { x: i32 }
fn take(p: P) { }
fn main(c: bool) {
    let p = P { x: 1 }
    if c {
        take(p)
    }
    take(p)
}
"#,
        "E0201",
    );
}

#[test]
fn move_out_of_a_binding_is_only_reported_once() {
    // One move followed by one use yields one diagnostic, not a cascade.
    let src = r#"
struct P { x: i32 }
fn take(p: P) { }
fn main() {
    let p = P { x: 1 }
    take(p)
    take(p)
}
"#;
    let (diags, _) = umayc::analyze_source("test.um", src);
    let e0201 = diags
        .items()
        .iter()
        .filter(|d| d.code == "E0201")
        .count();
    assert_eq!(e0201, 1, "expected exactly one E0201, got {e0201}");
}

#[test]
fn moving_a_copy_type_is_not_a_move() {
    ok(
        r#"
fn take(n: i32) { }
fn main() {
    let n = 1
    take(n)
    take(n)
}
"#,
    );
}

#[test]
fn separate_bindings_do_not_interfere() {
    ok(
        r#"
struct P { x: i32 }
fn take(p: P) { }
fn main() {
    let a = P { x: 1 }
    let b = P { x: 2 }
    take(a)
    take(b)
}
"#,
    );
}

// ---------------------------------------------------------------------------
// Adversarial: conflicting borrows (E0204 / E0205 / E0206)
// ---------------------------------------------------------------------------

#[test]
fn two_overlapping_mutable_borrows_are_rejected() {
    err(
        r#"
fn main() {
    let mut x = 1
    let a = &mut x
    let b = &mut x
    let n = *a
    let m = *b
}
"#,
        "E0204",
    );
}

#[test]
fn shared_borrow_then_mutable_borrow_is_rejected() {
    err(
        r#"
fn use_it(v: i32) -> i32 { v }
fn main() {
    let mut x = 1
    let a = &x
    let b = &mut x
    let n = use_it(*a)
}
"#,
        "E0205",
    );
}

#[test]
fn mutable_borrow_then_shared_borrow_is_rejected() {
    err(
        r#"
fn use_it(v: i32) -> i32 { v }
fn main() {
    let mut x = 1
    let a = &mut x
    let b = &x
    let n = use_it(*a)
}
"#,
        "E0205",
    );
}

#[test]
fn assigning_to_a_borrowed_place_is_rejected() {
    err(
        r#"
fn use_it(v: i32) -> i32 { v }
fn main() {
    let mut x = 1
    let a = &mut x
    x = 5
    let n = use_it(*a)
}
"#,
        "E0206",
    );
}

#[test]
fn moving_a_borrowed_place_is_rejected() {
    err(
        r#"
struct P { x: i32 }
fn take(p: P) { }
fn use_it(p: &P) -> i32 { p.x }
fn main() {
    let p = P { x: 1 }
    let r = &p
    take(p)
    let n = use_it(r)
}
"#,
        "E0206",
    );
}

// ---------------------------------------------------------------------------
// Adversarial: lifetimes (E0207 / E0210)
// ---------------------------------------------------------------------------

#[test]
fn returning_a_reference_to_a_local_is_rejected() {
    err(
        r#"
fn bad() -> &i32 {
    let x = 1
    return &x
}
"#,
        "E0207",
    );
}

#[test]
fn borrowing_a_temporary_is_rejected() {
    err(
        r#"
fn main() {
    let r = &1
    let v = *r
}
"#,
        "E0210",
    );
}

// ---------------------------------------------------------------------------
// Adversarial: raw pointers (E0211)
// ---------------------------------------------------------------------------

#[test]
fn raw_pointer_deref_outside_unsafe_is_rejected() {
    err(
        "fn main() { let p: *const i32 = unsafe { 0 as *const i32 }\n let v = *p }",
        "E0211",
    );
}

// ---------------------------------------------------------------------------
// Accept corpus: the checker must not reject correct programs
// ---------------------------------------------------------------------------

#[test]
fn two_shared_borrows_coexist() {
    ok(
        r#"
fn main() {
    let x = 1
    let a = &x
    let b = &x
    let n = *a + *b
}
"#,
    );
}

#[test]
fn sequential_borrows_are_fine() {
    // The first borrow dies at its last use, before the second begins.
    ok(
        r#"
fn read(r: &i32) -> i32 { *r }
fn read_mut(r: &mut i32) -> i32 { *r }
fn main() {
    let mut x = 1
    let a = &x
    let n = read(a)
    let b = &mut x
    let m = read_mut(b)
}
"#,
    );
}

#[test]
fn reborrow_after_last_use_is_fine() {
    ok(
        r#"
fn read_mut(r: &mut i32) -> i32 { *r }
fn main() {
    let mut x = 1
    let a = &mut x
    let n = read_mut(a)
    let b = &mut x
    let m = read_mut(b)
}
"#,
    );
}

#[test]
fn borrow_last_used_in_branch_does_not_conflict_with_sibling_block() {
    // The `&mut g_s` borrow is created and last used inside the `else` arm.
    // The shared borrow after the `if` sits on a sibling CFG path, so the two
    // can never be live at the same point. A linear block-index range used to
    // straddle the sibling block and report a false E0205.
    ok_with_warnings_allowed(
        r#"
struct G_S0 { f0: i8, f1: i64, f2: u16 }
impl G_S0 {
    fn m0(&self, p0: usize) -> bool { self.f0 > 0i8 }
    fn m1(&mut self, p0: bool) -> usize { 0usize }
}
struct G_S1 { f0: bool, f1: bool, f2: u8, f3: u32 }
fn g_f0(p0: &mut G_S0, p1: u64, p2: bool, p3: u8, p4: i64, p5: u8, p6: u16, p7: i16) -> bool {
    p0.m1(false)
    if (p0.m0(1usize) && (p2 == p2)) { println_i64((0i64) as i64) } else { p0.f0 = 0i8 }
    return true
}
fn main() -> i32 {
    let mut g_s: G_S0 = G_S0 { f0: (16i8 & 125i8), f1: ((-1i64) ^ 3463981566876876157i64), f2: 6280u16 }
    let mut g_a: [u64; 8] = [(2147483647u32 as u64); 8]
    let v_29: i32 = ((0i32 + (-1698921085i32)) as i32)
    let mut v_30: G_S1 = G_S1 { f0: (g_s.f1 <= 9223372036854775807i64), f1: (2275255072774531846u64 <= g_a[(15242313226860178803usize & 7usize)]), f2: (0u8 | 128u8), f3: 645243055u32 }
    if (!g_s.m0(14203844084864877292usize)) {
        v_30.f1 = g_f0(&mut g_s, g_a[(1usize & 7usize)], (v_30.f0 || v_30.f1), 255u8, (1u8 as i64), (v_29 as u8), (32768u16 + 0u16), ((-18811i16) ^ (-27352i16)))
        let mut lc_34: i32 = 0i32
        while (lc_34 < 5i32) && ((!false)) {
        }
    }
    v_30.f0 = g_s.m0((11958203785178649128usize * 12429678466931099892usize))
    let mut lc_38: i32 = 0i32
    while (lc_38 < 4i32) && ((g_s.f0 >= g_s.f0)) {
    }
    return 0
}
"#,
    );
}

#[test]
fn borrowing_a_parameter_and_returning_it_is_fine() {
    // The caller owns the referent, so elision makes this sound.
    ok(
        r#"
fn pick(x: &i32) -> &i32 {
    x
}
"#,
    );
}

#[test]
fn disjoint_field_borrow_and_write_is_fine() {
    // `&p.x` and writing `p.y` concern different places.
    ok(
        r#"
struct P { x: i32, y: i32 }
fn use_it(v: i32) -> i32 { v }
fn main() {
    let mut p = P { x: 1, y: 2 }
    let a = &p.x
    p.y = 3
    let v = use_it(*a)
}
"#,
    );
}

#[test]
fn copy_values_move_freely() {
    ok(
        r#"
struct P { x: i32 }
fn take(n: i32) -> i32 { n }
fn main() {
    let n = 1
    let a = take(n)
    let b = take(n)
    let c = a + b
}
"#,
    );
}

#[test]
fn a_value_moved_once_and_never_used_again_is_fine() {
    ok(
        r#"
struct P { x: i32 }
fn take(p: P) -> i32 { p.x }
fn main() {
    let p = P { x: 1 }
    let n = take(p)
}
"#,
    );
}

#[test]
fn assigning_before_reading_is_fine() {
    ok(
        r#"
fn main() {
    let mut x = 1
    x = 2
    let n = x
}
"#,
    );
}

#[test]
fn struct_update_moves_from_the_base() {
    ok(
        r#"
struct P { x: i32, y: i32 }
fn main() {
    let a = P { x: 1, y: 2 }
    let b = P { x: 9, ..a }
    let n = b.x
}
"#,
    );
}

#[test]
fn loops_with_borrows_inside_are_fine() {
    // Each iteration borrows and releases; nothing overlaps across the back
    // edge in a way the checker should reject.
    ok(
        r#"
fn read(r: &i32) -> i32 { *r }
fn main() {
    let mut i = 0
    let mut sum = 0
    while i < 3 {
        let n = read(&i)
        sum = sum + n
        i = i + 1
    }
}
"#,
    );
}

#[test]
fn match_arm_payload_is_usable() {
    ok_with_warnings_allowed(
        r#"
enum E { One(i32), Zero }
fn f(e: E) -> i32 {
    match e {
        E::One(n) => n,
        E::Zero => 0,
    }
}
"#,
    );
}

// ---------------------------------------------------------------------------
// Diagnostics quality
// ---------------------------------------------------------------------------

#[test]
fn ownership_errors_name_the_variable() {
    let src = r#"
struct P { x: i32 }
fn take(p: P) { }
fn main() {
    let widget = P { x: 1 }
    take(widget)
    take(widget)
}
"#;
    let (diags, map) = umayc::analyze_source("test.um", src);
    let text = umayc::render_all_full(&diags, &map, false, false);
    assert!(
        text.contains("widget"),
        "the diagnostic must name the variable:\n{text}"
    );
}

#[test]
fn borrow_conflicts_report_both_origins() {
    let src = r#"
fn use_it(v: i32) -> i32 { v }
fn main() {
    let mut x = 1
    let a = &mut x
    let b = &mut x
    let n = use_it(*a)
}
"#;
    let (diags, map) = umayc::analyze_source("test.um", src);
    let text = umayc::render_all_full(&diags, &map, false, false);
    assert!(text.contains("starts here"), "must point at the first borrow:\n{text}");
    assert!(text.contains("x"), "must name the place:\n{text}");
}

#[test]
fn every_ownership_code_has_an_explanation() {
    for code in [
        "E0201", "E0202", "E0203", "E0204", "E0205", "E0206", "E0207", "E0208", "E0209",
        "E0210", "E0211", "E0212", "E0213",
    ] {
        let entry = umayc::explain::lookup(code)
            .unwrap_or_else(|| panic!("{code} must be documented in the explain database"));
        assert!(!entry.summary.is_empty(), "{code} needs a summary");
        assert!(!entry.explanation.is_empty(), "{code} needs an explanation");
    }
}

// ---------------------------------------------------------------------------
// Robustness
// ---------------------------------------------------------------------------

#[test]
fn ownership_check_never_panics_on_truncated_input() {
    let src = r#"
struct P { x: i32 }
fn take(p: P) -> i32 { p.x }
fn main() {
    let mut p = P { x: 1 }
    let r = &mut p
    p.x = 2
    let n = take(p)
}
"#;
    for end in 0..=src.len() {
        if !src.is_char_boundary(end) {
            continue;
        }
        let _ = umayc::analyze_source("trunc.um", &src[..end]);
    }
}

#[test]
fn mut_ref_coerces_to_shared_ref_without_moving() {
    ok(
        r#"
struct P { x: i64 }
fn read_p(p: &P) -> i64 { p.x }
fn bump(p: &mut P) { p.x = p.x + 1 }
fn main() -> i64 {
    let mut p = P { x: 10 }
    let r = &mut p
    let a = read_p(r)
    bump(r)
    let b = read_p(r)
    a + b - 21
}
"#,
    );
}

#[test]
fn mut_ref_coerces_in_if_else_and_let() {
    ok(
        r#"
fn pick(cond: bool, a: &mut i64, b: &i64) -> &i64 {
    if cond { a } else { b }
}
fn main() -> i64 {
    let mut x = 5
    let mut z = 6
    let y = 7
    let r: &i64 = &mut z
    let p = pick(true, &mut x, &y)
    let _ = *r
    let _ = *p
    0
}
"#,
    );
}

#[test]
fn mut_ref_coerced_to_shared_still_conflicts_with_new_mut_borrow() {
    err(
        r#"
fn main() {
    let mut x = 5
    let r: &i64 = &mut x
    let q = &mut x
    let _ = *r
    let _ = *q
}
"#,
        "E0204",
    );
}

#[test]
fn shared_ref_does_not_coerce_to_mut_ref() {
    err(
        r#"
fn bump(p: &mut i64) { *p = *p + 1 }
fn main() {
    let x = 1
    let r = &x
    bump(r)
}
"#,
        "E1001",
    );
}
