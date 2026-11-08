//! Compiler cache location and housekeeping.
//!
//! UMAY never writes compilation temporaries into the project directory. All
//! cached artifacts live under the operating system's user cache directory:
//!
//! * Linux:   `$XDG_CACHE_HOME/umay` or `~/.cache/umay`
//! * macOS:   `~/Library/Caches/umay`
//! * Windows: `%LOCALAPPDATA%\umay\cache`
//!
//! See `docs/build-system.md` for the cache key derivation.

use std::path::{Path, PathBuf};

use crate::source::SourceMap;

/// Returns the root of the UMAY cache, without creating it.
pub fn cache_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("UMAY_CACHE_DIR") {
        return PathBuf::from(dir);
    }
    let base = if cfg!(target_os = "windows") {
        std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."))
    } else if cfg!(target_os = "macos") {
        std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Library/Caches")).unwrap_or_else(|| PathBuf::from("."))
    } else {
        std::env::var_os("XDG_CACHE_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))
            .unwrap_or_else(|| PathBuf::from("."))
    };
    base.join("umay")
}

/// Creates the cache directory if it does not exist.
pub fn ensure_cache_dir() -> std::io::Result<PathBuf> {
    let dir = cache_dir();
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Removes every entry inside the cache directory, returning the count deleted.
pub fn clear_cache() -> std::io::Result<usize> {
    let dir = cache_dir();
    if !Path::new(&dir).exists() {
        return Ok(0);
    }
    // Count the files removed, not the top-level directories: the cache is one
    // tree under `v1/`, so counting the root reported "cleared 1 cache entry"
    // while removing two hundred. `doctor` counts files, and the two numbers
    // have to agree or the pair is useless.
    let mut count = 0;
    for entry in std::fs::read_dir(&dir)? {
        let entry = entry?;
        let path = entry.path();
        count += count_files(&path);
        if path.is_dir() {
            std::fs::remove_dir_all(&path)?;
        } else {
            std::fs::remove_file(&path)?;
        }
    }
    Ok(count)
}

/// How many files are under `path`, counting `path` itself if it is a file.
fn count_files(path: &Path) -> usize {
    if !path.is_dir() {
        return 1;
    }
    let Ok(entries) = std::fs::read_dir(path) else { return 0 };
    entries.flatten().map(|e| count_files(&e.path())).sum()
}

// ---------------------------------------------------------------------------
// AST JSON rendering (used by `umay ast`)
// ---------------------------------------------------------------------------

fn esc(s: &str) -> String {
    crate::diagnostics::json_escape(s)
}

fn jstr(s: &str) -> String {
    format!("\"{}\"", esc(s))
}

/// Serializes the syntax tree to JSON. This is a debugging aid (and a stable
/// input for tooling); its shape is not part of the diagnostic schema contract.
pub fn ast_json(program: &crate::ast::Program, map: &SourceMap) -> String {
    let mut out = String::new();
    out.push_str("{\"kind\":\"Program\",\"items\":[");
    for (i, item) in program.items.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(&item_json(item, map));
    }
    out.push_str("]}");
    out
}

fn span_json(span: crate::source::Span, map: &SourceMap) -> String {
    match map.get(span.file) {
        Some(f) => {
            let a = f.line_col(span.start);
            let b = f.line_col(span.end);
            format!(
                "\"{}..{}@{}:{}:{}-{}:{}\"",
                span.start,
                span.end,
                f.name(),
                a.line,
                a.column,
                b.line,
                b.column
            )
        }
        None => "null".to_string(),
    }
}

fn item_json(item: &crate::ast::Item, map: &SourceMap) -> String {
    use crate::ast::Item;
    match item {
        Item::Fn(f) => format!(
            "{{\"kind\":\"Fn\",\"name\":{},\"params\":[{}],\"span\":{}}}",
            jstr(&f.name),
            f.params
                .iter()
                .map(|p| format!(
                    "{{\"name\":{},\"type\":{}}}",
                    jstr(p.name.as_deref().unwrap_or("_")),
                    jstr(&format!("{:?}", p.ty))
                ))
                .collect::<Vec<_>>()
                .join(","),
            span_json(f.span, map)
        ),
        Item::Struct(s) => format!(
            "{{\"kind\":\"Struct\",\"name\":{},\"fields\":[{}],\"span\":{}}}",
            jstr(&s.name),
            s.fields
                .iter()
                .map(|f| format!("{{\"name\":{},\"type\":{}}}", jstr(&f.name), jstr(&format!("{:?}", f.ty))))
                .collect::<Vec<_>>()
                .join(","),
            span_json(s.span, map)
        ),
        Item::Enum(e) => format!(
            "{{\"kind\":\"Enum\",\"name\":{},\"variants\":[{}],\"span\":{}}}",
            jstr(&e.name),
            e.variants.iter().map(|v| jstr(&v.name)).collect::<Vec<_>>().join(","),
            span_json(e.span, map)
        ),
        Item::Trait(t) => format!(
            "{{\"kind\":\"Trait\",\"name\":{},\"methods\":[{}],\"span\":{}}}",
            jstr(&t.name),
            t.methods.iter().map(|m| jstr(&m.name)).collect::<Vec<_>>().join(","),
            span_json(t.span, map)
        ),
        Item::Impl(i) => format!(
            "{{\"kind\":\"Impl\",\"trait\":{},\"self_type\":{},\"methods\":[{}],\"span\":{}}}",
            match &i.trait_path {
                Some(p) => jstr(&p.as_string()),
                None => "null".to_string(),
            },
            jstr(&format!("{:?}", i.self_ty)),
            i.methods.iter().map(|m| jstr(&m.name)).collect::<Vec<_>>().join(","),
            span_json(i.span, map)
        ),
        Item::Const(c) => format!(
            "{{\"kind\":\"Const\",\"name\":{},\"span\":{}}}",
            jstr(&c.name),
            span_json(c.span, map)
        ),
        Item::Use(u) => format!(
            "{{\"kind\":\"Use\",\"path\":{},\"span\":{}}}",
            jstr(&u.path.as_string()),
            span_json(u.span, map)
        ),
        Item::Mod(m) => format!(
            "{{\"kind\":\"Mod\",\"name\":{},\"inline\":{},\"span\":{}}}",
            jstr(&m.name),
            m.items.is_some(),
            span_json(m.span, map)
        ),
        Item::TypeAlias(t) => format!(
            "{{\"kind\":\"TypeAlias\",\"name\":{},\"span\":{}}}",
            jstr(&t.name),
            span_json(t.span, map)
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_dir_honours_env_override() {
        std::env::set_var("UMAY_CACHE_DIR", "/tmp/umay-test-cache");
        assert_eq!(cache_dir(), PathBuf::from("/tmp/umay-test-cache"));
        std::env::remove_var("UMAY_CACHE_DIR");
    }

    #[test]
    fn ast_json_contains_item_names() {
        let (out, map) = crate::parse_source("t.um", "fn add(a: i32) -> i32 { return a }");
        let json = ast_json(&out.program, &map);
        assert!(json.contains("\"name\":\"add\""), "{json}");
        assert!(json.starts_with("{\"kind\":\"Program\""), "{json}");
    }
}
