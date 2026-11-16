//! The build cache: content-addressed compilation outputs.
//!
//! UMAY never writes a `target/` into a project (that is one of the rules that
//! does not relax). Artifacts live under the user cache directory, named by a
//! hash of everything that went into producing them. The design is in
//! `docs/build-system.md`; this module is the implementation of sections 5 and 6.
//!
//! The key is the whole correctness argument. If it omits an input, a stale
//! artifact is served and the build is wrong in a way nobody will notice until
//! much later. So [the key](CacheKey) covers the compiler version, the
//! optimisation level, the target, and the exact bytes of the source -- and the
//! `CacheKey::derivation` list is printed by `umay doctor` so the inputs can be
//! read rather than inferred.
//!
//! Nothing here trusts the filesystem. A cache entry that exists but cannot be
//! read is treated as absent, so a half-written entry costs a rebuild rather than
//! a broken link step.

use std::path::{Path, PathBuf};

use crate::sha256;

/// What a cache entry holds.
///
/// The levels are separate directories so that a change to one does not discard
/// the work of the other: an optimisation level never affects the assembly a
/// program's definition of a symbol is, and a source edit never affects the
/// runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Artifact {
    /// The assembly text a program compiles to.
    ProgramAsm,
    /// The assembly text the runtime compiles to.
    RuntimeAsm,
    /// The linked executable.
    LinkedExe,
}

impl Artifact {
    fn tag(&self) -> &'static str {
        match self {
            Artifact::ProgramAsm => "program.s",
            Artifact::RuntimeAsm => "runtime.s",
            Artifact::LinkedExe => "exe",
        }
    }
}

/// Everything that determines one build output.
///
/// Constructed from the inputs rather than hashed field by field at the call
/// site, so that adding an input is a change to one function and every caller
/// gets it.
#[derive(Debug, Clone)]
pub struct CacheKey {
    /// The hex digest.
    pub hash: String,
}

impl CacheKey {
    /// Derives the key for one artifact.
    ///
    /// Every input is written into the hash with its name, so two different
    /// inputs cannot produce the same bytes by concatenation -- `("ab", "c")` and
    /// `("a", "bc")` hash differently because the lengths are hashed too.
    pub fn derive(artifact: Artifact, opt_level: &str, source: &[u8], extra: &[(&str, &str)]) -> CacheKey {
        CacheKey::derive_for(artifact, opt_level, source, extra, crate::target::Target::host())
    }

    /// Derives the key for one artifact of one target.
    ///
    /// The target is part of the key rather than a detail of the build: an
    /// assembly file for macOS and one for Linux are different bytes produced
    /// from the same source, and sharing a cache entry between them would serve
    /// a Mach-O object to an ELF link.
    pub fn derive_for(
        artifact: Artifact,
        opt_level: &str,
        source: &[u8],
        extra: &[(&str, &str)],
        target: crate::target::Target,
    ) -> CacheKey {
        let mut blob = Vec::new();
        let mut field = |name: &str, value: &[u8]| {
            // The name and the length go in before the value, so the encoding is
            // unambiguous however the value is shaped.
            blob.extend_from_slice(name.as_bytes());
            blob.push(0);
            blob.extend_from_slice(&(value.len() as u64).to_be_bytes());
            blob.extend_from_slice(value);
        };

        // The compiler's own identity. A change to the backend must not reuse the
        // previous backend's output, and a cache shared between two compiler
        // versions would do exactly that.
        //
        // The semantic version alone is not enough: editing the backend without
        // bumping the version left the two sharing every key, so a working
        // compiler served the broken assembly of the one before it. That is
        // exactly the failure this field exists to prevent, and it happened
        // because a `CARGO_PKG_VERSION` string only changes when someone
        // remembers to change it. Folding in a fingerprint of the running binary
        // makes any code change observable, whether or not the version moved.
        field("compiler", env!("CARGO_PKG_VERSION").as_bytes());
        field("compiler_binary", compiler_fingerprint().as_bytes());
        field("artifact", artifact.tag().as_bytes());
        field("target", target.triple.as_bytes());
        field("opt_level", opt_level.as_bytes());
        // The language edition: a source that means one thing in one edition and
        // another in a later one must not share a cache entry.
        field("edition", "2026".as_bytes());
        field("source", source);
        for (name, value) in extra {
            field(name, value.as_bytes());
        }

        CacheKey { hash: sha256::hex(&blob) }
    }

    /// The inputs this key is derived from, for `umay doctor` to print.
    pub fn derivation() -> Vec<&'static str> {
        vec![
            "compiler version",
            "compiler binary fingerprint",
            "artifact kind",
            "target triple",
            "optimisation level",
            "language edition",
            "source bytes",
            "named extra inputs (the runtime's source, for a link)",
        ]
    }
}

/// A fingerprint of the running compiler binary.
///
/// The semantic version is a claim about what the compiler is; this is what it
/// actually is on disk. Two builds of the same version number that differ in
/// code must not share a cache entry, and they will not if their binaries
/// differ in length or modification time. The value is computed once and
/// cached: `derive` runs for every artifact of every build, and hashing the
/// whole binary each time would cost more than the build it protects.
fn compiler_fingerprint() -> &'static str {
    use std::sync::OnceLock;
    static FINGERPRINT: OnceLock<String> = OnceLock::new();
    FINGERPRINT.get_or_init(|| {
        // The running executable is the compiler: the library and the binary are
        // built together, so the binary's identity is the code's identity. If the
        // path cannot be read (an unusual embedding), fall back to the version
        // string's own bytes with a marker so the value is still stable and
        // distinct from a real fingerprint.
        let path = match std::env::current_exe() {
            Ok(path) => path,
            Err(_) => return format!("version-only:{}", env!("CARGO_PKG_VERSION")),
        };
        match std::fs::metadata(&path) {
            Ok(meta) => {
                // Length and modification time, as nanoseconds since the epoch.
                // Either changing means the binary was relinked, which after any
                // edit to the compiler's source it was.
                let len = meta.len();
                let mtime = meta
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_nanos())
                    .unwrap_or(0);
                format!("{}:{}", len, mtime)
            }
            Err(_) => format!("version-only:{}", env!("CARGO_PKG_VERSION")),
        }
    })
}

/// The on-disk cache.
pub struct Cache {
    root: PathBuf,
}

impl Cache {
    /// The cache at the configured location.
    pub fn open() -> Cache {
        Cache { root: crate::cache::cache_dir() }
    }

    /// The cache at `root`, for a test that must not touch the user's cache.
    pub fn at(root: impl Into<PathBuf>) -> Cache {
        Cache { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Where an entry lives: `<root>/v1/<first 2 hex>/<full hash>/<artifact>`.
    ///
    /// The two-character directory keeps a single directory from holding tens of
    /// thousands of entries, which slows down every lookup on some filesystems.
    /// The full hash is still the directory name inside it, so two keys sharing
    /// a prefix is a lookup, never a collision.
    pub fn entry_path(&self, key: &CacheKey, artifact: Artifact) -> PathBuf {
        let prefix = &key.hash[..2];
        self.root.join("v1").join(prefix).join(&key.hash).join(artifact.tag())
    }

    /// Reads an entry, or `None` if it is absent or unusable.
    ///
    /// An empty entry is treated as a miss, not as an artifact. A build killed
    /// between opening the temporary file and writing it leaves a zero-length
    /// file, and a zero-length assembly links to nothing: the program builds and
    /// then fails to find `main`. Checking the size costs one `stat` and turns
    /// that into a rebuild.
    pub fn read(&self, key: &CacheKey, artifact: Artifact) -> Option<Vec<u8>> {
        let bytes = std::fs::read(self.entry_path(key, artifact)).ok()?;
        if bytes.is_empty() {
            return None;
        }
        Some(bytes)
    }

    /// Stores an entry.
    ///
    /// Written to a temporary name and renamed, so a build interrupted halfway
    /// through leaves either the old entry or none -- never half a file that the
    /// next build would happily link.
    pub fn write(&self, key: &CacheKey, artifact: Artifact, bytes: &[u8]) -> std::io::Result<PathBuf> {
        let path = self.entry_path(key, artifact);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let temp = path.with_extension(format!("tmp{}", std::process::id()));
        std::fs::write(&temp, bytes)?;
        std::fs::rename(&temp, &path)?;
        Ok(path)
    }

    /// The number of entries and their total size, for `umay doctor`.
    pub fn stats(&self) -> (usize, u64) {
        let mut entries = 0;
        let mut bytes = 0u64;
        let v1 = self.root.join("v1");
        let Ok(prefixes) = std::fs::read_dir(&v1) else {
            return (0, 0);
        };
        for prefix in prefixes.flatten() {
            let Ok(keys) = std::fs::read_dir(prefix.path()) else { continue };
            for key in keys.flatten() {
                let Ok(files) = std::fs::read_dir(key.path()) else { continue };
                for file in files.flatten() {
                    entries += 1;
                    bytes += file.metadata().map(|m| m.len()).unwrap_or(0);
                }
            }
        }
        (entries, bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("umay_cache_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn the_same_inputs_give_the_same_key() {
        let a = CacheKey::derive(Artifact::ProgramAsm, "O2", b"fn main() {}", &[]);
        let b = CacheKey::derive(Artifact::ProgramAsm, "O2", b"fn main() {}", &[]);
        assert_eq!(a.hash, b.hash);
    }

    #[test]
    fn a_different_source_gives_a_different_key() {
        let a = CacheKey::derive(Artifact::ProgramAsm, "O2", b"fn main() { return 0 }", &[]);
        let b = CacheKey::derive(Artifact::ProgramAsm, "O2", b"fn main() { return 1 }", &[]);
        assert_ne!(a.hash, b.hash, "a change to the source must miss the cache");
    }

    #[test]
    fn a_different_optimisation_level_gives_a_different_key() {
        let a = CacheKey::derive(Artifact::ProgramAsm, "O0", b"fn main() {}", &[]);
        let b = CacheKey::derive(Artifact::ProgramAsm, "O3", b"fn main() {}", &[]);
        assert_ne!(a.hash, b.hash, "different optimisation must not share output");
    }

    #[test]
    fn a_different_artifact_gives_a_different_key() {
        let a = CacheKey::derive(Artifact::ProgramAsm, "O2", b"same", &[]);
        let b = CacheKey::derive(Artifact::RuntimeAsm, "O2", b"same", &[]);
        assert_ne!(a.hash, b.hash);
    }

    #[test]
    fn the_compiler_binary_fingerprint_is_present_and_stable() {
        // The semantic version alone left two builds of the same version sharing
        // every cache key even when their code differed, so a fixed compiler
        // served the broken assembly of the one before it. The fingerprint is
        // what makes a code change observable. It must be non-empty and the same
        // across calls within one process.
        let first = compiler_fingerprint();
        let second = compiler_fingerprint();
        assert!(!first.is_empty(), "the fingerprint must not be empty");
        assert_eq!(first, second, "the fingerprint must be stable within a process");
        assert!(
            CacheKey::derivation().contains(&"compiler binary fingerprint"),
            "doctor must list the fingerprint among the key's inputs"
        );
    }

    #[test]
    fn two_inputs_cannot_stand_in_for_one() {
        // With naive concatenation `("ab","c")` and `("a","bc")` would hash the
        // same bytes and share an entry.
        let a = CacheKey::derive(Artifact::ProgramAsm, "O2", b"x", &[("a", "bc")]);
        let b = CacheKey::derive(Artifact::ProgramAsm, "O2", b"x", &[("a", "b"), ("c", "")]);
        assert_ne!(a.hash, b.hash);
    }

    #[test]
    fn a_named_extra_input_changes_the_key() {
        // The runtime's source goes in as an extra input for the link step, so a
        // runtime change must rebuild the executable.
        let a = CacheKey::derive(Artifact::LinkedExe, "O2", b"program", &[("runtime", "v1")]);
        let b = CacheKey::derive(Artifact::LinkedExe, "O2", b"program", &[("runtime", "v2")]);
        assert_ne!(a.hash, b.hash);
    }

    #[test]
    fn an_entry_round_trips_through_the_filesystem() {
        let root = scratch("round_trip");
        let cache = Cache::at(&root);
        let key = CacheKey::derive(Artifact::ProgramAsm, "O2", b"fn main() {}", &[]);
        assert!(cache.read(&key, Artifact::ProgramAsm).is_none(), "nothing stored yet");
        cache.write(&key, Artifact::ProgramAsm, b".text\n").expect("write");
        assert_eq!(cache.read(&key, Artifact::ProgramAsm).as_deref(), Some(&b".text\n"[..]));
    }

    #[test]
    fn a_missing_entry_is_a_miss_not_an_error() {
        let root = scratch("missing");
        let cache = Cache::at(&root);
        let key = CacheKey::derive(Artifact::ProgramAsm, "O2", b"fn f() {}", &[]);
        assert!(cache.read(&key, Artifact::ProgramAsm).is_none());
    }

    #[test]
    fn an_empty_entry_is_a_miss() {
        // A build interrupted mid-write leaves a zero-length file. Serving it
        // would link an assembly with no text in it, and the failure would show
        // up as an undefined `main` much later.
        let root = scratch("empty_entry");
        let cache = Cache::at(&root);
        let key = CacheKey::derive(Artifact::ProgramAsm, "O2", b"src", &[]);
        cache.write(&key, Artifact::ProgramAsm, b"").expect("write");
        assert!(cache.read(&key, Artifact::ProgramAsm).is_none());
    }

    #[test]
    fn an_entry_path_is_under_the_cache_root() {
        let cache = Cache::at("/tmp/umay-cache-probe");
        let key = CacheKey::derive(Artifact::ProgramAsm, "O2", b"x", &[]);
        let path = cache.entry_path(&key, Artifact::ProgramAsm);
        assert!(path.starts_with("/tmp/umay-cache-probe"), "got {}", path.display());
        // The path ends with the full key and the artifact's name, so a reader
        // can tell what an entry is from its location alone.
        assert!(path.to_string_lossy().ends_with(&format!("{}/program.s", key.hash)));
    }

    #[test]
    fn the_cache_never_writes_into_a_project() {
        // The rule that does not relax: no `target/` in the project. The cache
        // root comes from the user cache directory, never the working directory.
        let cache = Cache::open();
        let cwd = std::env::current_dir().expect("a working directory");
        assert!(
            !cache.root().starts_with(&cwd),
            "cache at {} is inside the project at {}",
            cache.root().display(),
            cwd.display()
        );
    }

    #[test]
    fn stats_counts_what_was_stored() {
        let root = scratch("stats");
        let cache = Cache::at(&root);
        assert_eq!(cache.stats(), (0, 0), "an empty cache holds nothing");
        let key = CacheKey::derive(Artifact::ProgramAsm, "O2", b"a", &[]);
        cache.write(&key, Artifact::ProgramAsm, b"12345").expect("write");
        let key2 = CacheKey::derive(Artifact::RuntimeAsm, "O2", b"b", &[]);
        cache.write(&key2, Artifact::RuntimeAsm, b"678").expect("write");
        assert_eq!(cache.stats(), (2, 8));
    }

    #[test]
    fn the_key_comes_from_the_inputs_and_not_from_the_clock() {
        // A key derived twice in the same process must agree; if anything time
        // dependent were in the hash this would eventually fail.
        let first = CacheKey::derive(Artifact::LinkedExe, "O1", b"source", &[("runtime", "r")]);
        std::thread::sleep(std::time::Duration::from_millis(2));
        let second = CacheKey::derive(Artifact::LinkedExe, "O1", b"source", &[("runtime", "r")]);
        assert_eq!(first.hash, second.hash);
    }
}
