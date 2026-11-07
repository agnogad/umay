#[test]
fn sema_never_panics_on_truncated_programs() {
    let src = r#"
enum Shape { Circle(f64), Rect { w: f64, h: f64 } }
struct Canvas { area: f64 }
impl Canvas { fn area(self) -> f64 { self.area } }
fn describe(s: Shape) -> str {
    match s {
        Shape::Circle(r) => "circle",
        Shape::Rect { w, h } => "rect",
    }
}
fn main() {
    let c = Canvas { area: 1.0 }
    let n: u8 = 200
    let mut total = 0
    for i in 0..10 { total = total + i }
    let half = n as f64 / 2.0
    let _ = (c.area(), describe(Shape::Circle(1.0)), total, half)
}
"#;
    for i in 0..src.len() {
        let prefix = &src[..i];
        let mut map = umayc::source::SourceMap::new();
        let id = map.add("<fuzz>", prefix);
        let (tokens, errs) = umayc::lexer::tokenize(prefix, id);
        let parsed = umayc::parser::parse(tokens, errs, id);
        // Must not panic.
        let _ = umayc::sema::analyze(&parsed.program, &map);
    }
}

#[test]
fn sema_never_panics_on_mutated_programs() {
    let src = "fn f(x: i32) -> i32 { let y = x + 1\n if y > 0 { y } else { 0 } }";
    let mut state: u64 = 0x2545F4914F6CDD1D;
    for _ in 0..500 {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        let mut bytes = src.as_bytes().to_vec();
        if bytes.is_empty() { continue }
        let idx = (state as usize) % bytes.len();
        const ALPHABET: &[u8] = b"{}()[]<>,.;:+*-/&|!=_\"'0aA \n";
        bytes[idx] = ALPHABET[((state >> 32) as usize) % ALPHABET.len()];
        let Ok(text) = String::from_utf8(bytes) else { continue };
        let mut map = umayc::source::SourceMap::new();
        let id = map.add("<mut>", text.as_str());
        let (tokens, errs) = umayc::lexer::tokenize(&text, id);
        let parsed = umayc::parser::parse(tokens, errs, id);
        let _ = umayc::sema::analyze(&parsed.program, &map);
    }
}
