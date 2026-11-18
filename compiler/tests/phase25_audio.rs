//! Phase 7: pure-UMAY PCM WAV generator (`stdlib/audio_wav.um`).
//!
//! SDL3 audio needs a struct-by-value `SDL_AudioSpec` and C function-pointer
//! callbacks, which bindgen cannot express, and miniaudio is absent. This slice
//! is the honest local increment: a pure-UMAY writer that produces a valid,
//! playable mono 16-bit PCM file, giving the PCM/mixer primitives a real test
//! surface. Device playback stays deferred.
//!
//! `audio_wav.um` is spliced at crate root above the splice-only app body
//! `examples/v2/audio_tone.um` (the compiler prepends the prelude, which already
//! contains `stdlib/fs.um` with `write_file_bytes`). The first group feeds that
//! real spliced text into the in-process lexer -> parser -> prelude -> sema
//! harness and the `umay check` CLI. The rest builds and runs whole programs on
//! the host, then validates the produced WAV with Python's `wave` module and
//! checks byte-for-byte determinism across two runs. Runtime tests skip (rather
//! than fail) when `as`, `cc` or `python3` is unavailable.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

use umayc::diagnostics::Diagnostics;
use umayc::lexer;
use umayc::parser;
use umayc::sema;
use umayc::source::SourceMap;
use umayc::target::Target;

const AUDIO_WAV_UM: &str = include_str!("../../stdlib/audio_wav.um");
const AUDIO_TONE_UM: &str = include_str!("../../examples/v2/audio_tone.um");

static NEXT_DIR: AtomicUsize = AtomicUsize::new(0);

fn compiler() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_umay"))
}

/// A temp dir that cleans itself up, so a failed assertion still leaves no
/// scratch behind.
struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        let n = NEXT_DIR.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "umay-phase25-audio-{tag}-{}-{n}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create scratch dir");
        Scratch(dir)
    }

    fn path(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn have_tool(name: &str) -> bool {
    Command::new(name)
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn native_toolchain() -> bool {
    !cfg!(windows) && have_tool("as") && have_tool("cc")
}

fn assert_ok(output: &std::process::Output, what: &str) {
    assert!(
        output.status.success(),
        "{what} failed (status {:?}):\nstdout:\n{}\nstderr:\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

/// lex -> parse -> prelude -> `sema::analyze`, mirroring phase20_gui_core.rs.
/// The prelude is injected because the core and app use `write_byte`/`alloc`/
/// `write_file_bytes`/`println_i64`, which every spliced program gets free.
fn analyze(src: &str) -> Diagnostics {
    let mut map = SourceMap::new();
    let file = map.add("<audio>", src);
    let (tokens, lex_errors) = lexer::tokenize(src, file);
    assert!(lex_errors.is_empty(), "lex errors: {lex_errors:?}\n---\n{src}");
    let parsed = parser::parse(tokens, Vec::new(), file);
    let parse_errors: Vec<_> = parsed
        .diagnostics
        .iter()
        .filter(|d| d.severity.is_fatal())
        .map(|d| format!("{}: {}", d.code, d.message))
        .collect();
    assert!(parse_errors.is_empty(), "parse errors: {parse_errors:?}\n---\n{src}");
    let (program, prelude_diags) =
        umayc::prelude::with_prelude(parsed.program, &mut map, Target::LINUX);
    assert!(!prelude_diags.has_errors(), "prelude errors: {prelude_diags:?}");
    let out = sema::analyze(&program, &map);
    if out.diagnostics.has_errors() {
        eprintln!("{}", umayc::render_all(&out.diagnostics, &map, false));
    }
    out.diagnostics
}

#[track_caller]
fn ok(src: &str) {
    assert!(!analyze(src).has_errors(), "expected no type errors\n---\n{src}");
}

/// The real `audio_wav.um` text followed by the splice-only app body.
fn spliced(body: &str) -> String {
    format!("{AUDIO_WAV_UM}\n{body}")
}

/// `umay check` on the spliced source in a temp directory.
fn cli_check(name: &str, src: &str) -> bool {
    let dir = Scratch::new(name);
    let file = dir.path("main.um");
    std::fs::write(&file, src).expect("write temp source");
    let out = Command::new(compiler())
        .arg("check")
        .arg(&file)
        .current_dir(&dir.0)
        .output()
        .expect("run umay check");
    let passed = out.status.success();
    if !passed {
        eprintln!(
            "umay check failed:\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
    passed
}

/// Builds and runs `src` inside `dir`, returning trimmed stdout. The produced
/// `tone.wav` stays in `dir` for the caller to inspect.
fn build_and_run(dir: &Scratch, src: &str) -> String {
    let file = dir.path("main.um");
    std::fs::write(&file, src).expect("write temp source");
    let build = Command::new(compiler())
        .arg("build")
        .arg(&file)
        .current_dir(&dir.0)
        .output()
        .expect("run umay build");
    assert_ok(&build, "umay build");
    let exe = dir.path("dist").join(if cfg!(windows) { "main.exe" } else { "main" });
    let out = Command::new(&exe).current_dir(&dir.0).output().expect("run built program");
    assert_ok(&out, "run built program");
    String::from_utf8_lossy(&out.stdout).trim_end().to_string()
}

/// Runs `python3 -c SCRIPT <path>` and returns trimmed stdout.
fn python(script: &str, path: &Path) -> String {
    let out = Command::new("python3")
        .arg("-c")
        .arg(script)
        .arg(path)
        .output()
        .expect("run python3");
    assert_ok(&out, "python3");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

// (a) The real spliced program (core + app body) type-checks in-process.
#[test]
fn audio_wav_spliced_typechecks_in_process() {
    ok(&spliced(AUDIO_TONE_UM));
}

// (a) The same splice is accepted by the `umay check` CLI.
#[test]
fn cli_check_accepts_spliced_audio_wav() {
    assert!(cli_check("check", &spliced(AUDIO_TONE_UM)));
}

// (b) Build and run: the app prints the exact deterministic length.
#[test]
fn audio_tone_prints_expected_length() {
    if !native_toolchain() {
        eprintln!("skipping runtime check: no `as` or `cc`");
        return;
    }
    let dir = Scratch::new("run");
    let stdout = build_and_run(&dir, &spliced(AUDIO_TONE_UM));
    assert_eq!(stdout, "WAV_OK 1644");
    assert!(dir.path("tone.wav").is_file(), "tone.wav was not written");
}

// Python's `wave` module is the independent reader: it validates the RIFF/WAVE
// header as a side effect of opening the file. We also check the raw size fields
// and that the PCM payload holds a nonzero, in-range sine.
const PY_WAVE_CHECK: &str = r#"
import sys, wave
path = sys.argv[1]
w = wave.open(path, "rb")
n = w.getnframes()
raw = w.readframes(n)
vals = [int.from_bytes(raw[i:i+2], "little", signed=True) for i in range(0, len(raw), 2)]
hdr = open(path, "rb").read()
print(w.getnchannels(), w.getsampwidth(), w.getframerate(), n,
      sum(v != 0 for v in vals), max(abs(v) for v in vals),
      hdr[:4].decode(), int.from_bytes(hdr[4:8], "little"), len(hdr) - 8,
      hdr[8:12].decode(), hdr[36:40].decode(), int.from_bytes(hdr[40:44], "little"))
"#;

// (c) The produced file is mono, 16-bit, 8000 Hz, 800 frames; RIFF sizes agree
// with the file and at least some samples are nonzero and within amplitude.
#[test]
fn tone_wav_is_valid_pcm_and_has_signal() {
    if !native_toolchain() || !have_tool("python3") {
        eprintln!("skipping WAV validation: no `as`, `cc` or `python3`");
        return;
    }
    let dir = Scratch::new("wave");
    build_and_run(&dir, &spliced(AUDIO_TONE_UM));
    let report = python(PY_WAVE_CHECK, &dir.path("tone.wav"));
    let f: Vec<&str> = report.split_whitespace().collect();
    assert_eq!(f.len(), 12, "unexpected wave report: {report}");
    assert_eq!(f[0], "1", "nchannels");
    assert_eq!(f[1], "2", "sampwidth");
    assert_eq!(f[2], "8000", "framerate");
    assert_eq!(f[3], "800", "nframes");

    let nonzero: i64 = f[4].parse().expect("nonzero count");
    let max_abs: i64 = f[5].parse().expect("max abs sample");
    assert!(nonzero > 0, "all samples were zero: {report}");
    assert!(max_abs > 0 && max_abs <= 8000, "sample out of range: {report}");

    assert_eq!(f[6], "RIFF", "RIFF tag");
    let riff_size: i64 = f[7].parse().expect("riff size");
    let file_size: i64 = f[8].parse().expect("file size - 8");
    assert_eq!(riff_size, file_size, "RIFF size != filesize - 8: {report}");
    assert_eq!(f[9], "WAVE", "WAVE tag");
    assert_eq!(f[10], "data", "data tag");
    let data_size: i64 = f[11].parse().expect("data size");
    assert_eq!(data_size, 1600, "data chunk size");
}

const PY_SHA256: &str = "import hashlib, sys; print(hashlib.sha256(open(sys.argv[1], 'rb').read()).hexdigest())";

// (d) Two independent runs produce byte-identical WAV files.
#[test]
fn tone_wav_is_deterministic() {
    if !native_toolchain() || !have_tool("python3") {
        eprintln!("skipping determinism check: no `as`, `cc` or `python3`");
        return;
    }
    let a = Scratch::new("det-a");
    let b = Scratch::new("det-b");
    build_and_run(&a, &spliced(AUDIO_TONE_UM));
    build_and_run(&b, &spliced(AUDIO_TONE_UM));
    let hash_a = python(PY_SHA256, &a.path("tone.wav"));
    let hash_b = python(PY_SHA256, &b.path("tone.wav"));
    assert_eq!(hash_a, hash_b, "two runs produced different WAV bytes");
    assert_eq!(hash_a.len(), 64, "expected a SHA-256 hex digest");
}
