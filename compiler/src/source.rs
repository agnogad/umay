//! Source text management.
//!
//! Every byte of UMAY source that the compiler touches lives in exactly one
//! [`SourceFile`], owned by the [`SourceMap`]. All spans are byte offsets into
//! that buffer. Files are identified by an opaque [`FileId`] so that all later
//! stages never need to hold `&str` paths.

use std::fmt;

/// Opaque identifier for a source file inside a [`SourceMap`].
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct FileId(pub u32);

impl FileId {
    /// A sentinel used for synthetic spans (e.g. compiler-generated code).
    pub const DUMMY: FileId = FileId(u32::MAX);

    pub fn index(self) -> usize {
        self.0 as usize
    }
}

impl fmt::Display for FileId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if *self == FileId::DUMMY {
            f.write_str("<builtin>")
        } else {
            write!(f, "file#{}", self.0)
        }
    }
}

/// A half-open byte range `[start, end)` inside a single source file.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Span {
    pub file: FileId,
    pub start: u32,
    pub end: u32,
}

impl Span {
    pub const DUMMY: Span = Span { file: FileId::DUMMY, start: 0, end: 0 };

    pub fn new(file: FileId, start: u32, end: u32) -> Span {
        debug_assert!(start <= end, "span start must not exceed end");
        Span { file, start, end }
    }

    /// Builds a span covering both `self` and `other` if they are in the same file.
    pub fn to(self, other: Span) -> Span {
        if self.file != other.file {
            return self;
        }
        Span { file: self.file, start: self.start.min(other.start), end: self.end.max(other.end) }
    }

    pub fn len(self) -> u32 {
        self.end.saturating_sub(self.start)
    }

    pub fn is_empty(self) -> bool {
        self.start == self.end
    }

    /// Shrinks the span to its first `n` bytes.
    pub fn take(self, n: u32) -> Span {
        Span { file: self.file, start: self.start, end: (self.start + n).min(self.end) }
    }
}

impl fmt::Debug for Span {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if *self == Span::DUMMY {
            return f.write_str("Span::DUMMY");
        }
        write!(f, "{}..{}@{}", self.start, self.end, self.file.0)
    }
}

/// A human-usable position: 1-based line and column (in characters, not bytes).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct LineCol {
    pub line: u32,
    pub column: u32,
}

/// A single source file.
pub struct SourceFile {
    id: FileId,
    /// Name as presented to the user, e.g. `src/main.um`.
    name: String,
    /// Full path if the file came from disk.
    path: Option<std::path::PathBuf>,
    src: String,
    /// Byte offset of the start of each line.
    line_starts: Vec<u32>,
}

impl SourceFile {
    fn new(id: FileId, name: String, path: Option<std::path::PathBuf>, src: String) -> Self {
        let mut line_starts = vec![0u32];
        for (idx, byte) in src.bytes().enumerate() {
            if byte == b'\n' {
                line_starts.push(idx as u32 + 1);
            }
        }
        SourceFile { id, name, path, src, line_starts }
    }

    pub fn id(&self) -> FileId {
        self.id
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn path(&self) -> Option<&std::path::Path> {
        self.path.as_deref()
    }

    pub fn src(&self) -> &str {
        &self.src
    }

    pub fn len(&self) -> u32 {
        self.src.len() as u32
    }

    pub fn is_empty(&self) -> bool {
        self.src.is_empty()
    }

    /// Number of lines in the file (always at least 1).
    pub fn line_count(&self) -> u32 {
        self.line_starts.len() as u32
    }

    /// Source text of `span`, clamped to file bounds.
    pub fn span_text(&self, span: Span) -> &str {
        let start = (span.start as usize).min(self.src.len());
        let end = (span.end as usize).min(self.src.len());
        if start > end {
            return "";
        }
        // Slicing at non-boundary offsets cannot happen because the lexer only
        // produces spans at character boundaries.
        self.src.get(start..end).unwrap_or("")
    }

    /// Source text of line `line` (1-based), without the trailing newline.
    pub fn line_text(&self, line: u32) -> &str {
        if line == 0 || line > self.line_count() {
            return "";
        }
        let start = self.line_starts[(line - 1) as usize] as usize;
        let end = self
            .line_starts
            .get(line as usize)
            .map(|&e| e as usize)
            .unwrap_or(self.src.len());
        let text = &self.src[start..end];
        text.strip_suffix('\n').map(|t| t.strip_suffix('\r').unwrap_or(t)).unwrap_or(text)
    }

    /// Converts a byte offset into a 1-based [`LineCol`].
    pub fn line_col(&self, offset: u32) -> LineCol {
        let offset = offset.min(self.len());
        let line_idx = match self.line_starts.binary_search(&offset) {
            Ok(i) => i,
            Err(i) => i - 1,
        };
        let line_start = self.line_starts[line_idx] as usize;
        let prefix = &self.src[line_start..offset as usize];
        LineCol { line: line_idx as u32 + 1, column: prefix.chars().count() as u32 + 1 }
    }

    /// Byte offset of the start of `line` (1-based).
    pub fn line_start(&self, line: u32) -> u32 {
        self.line_starts.get((line - 1) as usize).copied().unwrap_or(self.len())
    }
}

/// Owns every source file loaded during a compilation.
#[derive(Default)]
pub struct SourceMap {
    files: Vec<SourceFile>,
}

impl SourceMap {
    pub fn new() -> Self {
        SourceMap::default()
    }

    /// Adds a file under an explicit display name.
    pub fn add(&mut self, name: impl Into<String>, src: impl Into<String>) -> FileId {
        self.add_impl(name.into(), None, src.into())
    }

    /// Adds a file read from `path`, using `name` for display.
    pub fn add_file(&mut self, name: impl Into<String>, path: std::path::PathBuf, src: impl Into<String>) -> FileId {
        self.add_impl(name.into(), Some(path), src.into())
    }

    fn add_impl(&mut self, name: String, path: Option<std::path::PathBuf>, src: String) -> FileId {
        let id = FileId(self.files.len() as u32);
        self.files.push(SourceFile::new(id, name, path, src));
        id
    }

    pub fn get(&self, id: FileId) -> Option<&SourceFile> {
        self.files.get(id.index())
    }

    /// Returns the file or panics. Internal invariant: all ids come from this map.
    pub fn file(&self, id: FileId) -> &SourceFile {
        &self.files[id.index()]
    }

    pub fn len(&self) -> usize {
        self.files.len()
    }

    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    /// Resolves a span to `(file_name, LineCol, LineCol)` for start and end.
    pub fn resolve(&self, span: Span) -> Option<ResolvedSpan<'_>> {
        let file = self.get(span.file)?;
        Some(ResolvedSpan {
            file_name: file.name(),
            start: file.line_col(span.start),
            end: file.line_col(span.end),
            text: file.span_text(span),
        })
    }
}

/// A span resolved against the source map, ready for rendering.
pub struct ResolvedSpan<'a> {
    pub file_name: &'a str,
    pub start: LineCol,
    pub end: LineCol,
    pub text: &'a str,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_col_is_one_based() {
        let mut map = SourceMap::new();
        let id = map.add("t.um", "fn main() {\n  let x = 1\n}\n");
        let f = map.file(id);
        assert_eq!(f.line_count(), 4);
        assert_eq!(f.line_col(0), LineCol { line: 1, column: 1 });
        assert_eq!(f.line_col(12), LineCol { line: 2, column: 1 });
        assert_eq!(f.line_col(14), LineCol { line: 2, column: 3 });
    }

    #[test]
    fn line_text_strips_newline() {
        let mut map = SourceMap::new();
        let id = map.add("t.um", "abc\ndef\r\nghi");
        let f = map.file(id);
        assert_eq!(f.line_text(1), "abc");
        assert_eq!(f.line_text(2), "def");
        assert_eq!(f.line_text(3), "ghi");
        assert_eq!(f.line_text(4), "");
    }

    #[test]
    fn line_col_counts_chars_not_bytes() {
        let mut map = SourceMap::new();
        // 'ç' and 'ü' are 2 bytes each in UTF-8.
        let id = map.add("t.um", "çü x");
        let f = map.file(id);
        assert_eq!(f.line_col(4), LineCol { line: 1, column: 3 });
    }

    #[test]
    fn span_text_clamps() {
        let mut map = SourceMap::new();
        let id = map.add("t.um", "hello");
        let f = map.file(id);
        assert_eq!(f.span_text(Span::new(id, 1, 3)), "el");
        assert_eq!(f.span_text(Span::new(id, 3, 999)), "lo");
    }

    #[test]
    fn span_to_merges_when_same_file() {
        let mut map = SourceMap::new();
        let a = map.add("a.um", "aaa");
        let b = map.add("b.um", "bbb");
        let s1 = Span::new(a, 0, 1);
        let s2 = Span::new(a, 2, 3);
        assert_eq!(s1.to(s2), Span::new(a, 0, 3));
        assert_eq!(s1.to(Span::new(b, 0, 1)), s1);
    }
}
