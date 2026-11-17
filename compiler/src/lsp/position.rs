//! Converting between LSP positions and byte offsets.
//!
//! Three coordinate systems meet here and they disagree.
//!
//! The compiler counts bytes from 0 -- every span, every diagnostic.
//! The protocol counts lines from 0 and *columns in UTF-16 code units*.
//! A person reading a diagnostic counts lines and columns from 1.
//!
//! The UTF-16 detail is the one that bites. A column means "the 5th unit of
//! UTF-16", not "the 5th byte" and not "the 5th character": an emoji is two
//! units, a `é` is one, and both are more than one byte. Getting it wrong puts
//! the caret in the wrong place on exactly the lines where it matters most --
//! the ones with a non-ASCII character in them.
//!
//! Everything below goes through `char`, so a position always lands on a
//! character boundary and never inside a multi-byte sequence.

/// A 0-based line and UTF-16 column, as the protocol defines a position.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Position {
    pub line: u32,
    character: u32,
}

impl Position {
    pub fn new(line: u32, character: u32) -> Position {
        Position { line, character }
    }

    pub fn character(&self) -> u32 {
        self.character
    }
}

/// A 0-based line and UTF-16 column span.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Range {
    pub start: Position,
    pub end: Position,
}

impl Range {
    pub fn new(start: Position, end: Position) -> Range {
        Range { start, end }
    }
}

/// Converts between byte offsets and protocol positions for one source text.
pub struct LineIndex {
    /// Byte offset where each line starts. Line 0 always starts at 0.
    line_starts: Vec<u32>,
    len: u32,
}

impl LineIndex {
    pub fn new(text: &str) -> LineIndex {
        let mut line_starts = vec![0u32];
        for (i, byte) in text.bytes().enumerate() {
            if byte == b'\n' {
                line_starts.push(i as u32 + 1);
            }
        }
        LineIndex { line_starts, len: text.len() as u32 }
    }

    /// The byte offset a protocol position names, clamped to the text.
    ///
    /// A client may name a position past the end of a line -- after the last
    /// character, or on a line that no longer exists because the document was
    /// edited. Both clamp rather than fail: a completion request one character
    /// beyond the line should complete the line, not error.
    pub fn offset(&self, text: &str, position: Position) -> u32 {
        let line = position.line as usize;
        if line >= self.line_starts.len() {
            return self.len;
        }
        let start = self.line_starts[line] as usize;
        let end = self
            .line_starts
            .get(line + 1)
            .map(|&e| e as usize)
            .unwrap_or(text.len())
            // A line's end is its newline, which is not part of it.
            .min(text.len());
        // The newline ends the line rather than belonging to it, so a column
        // past the last character clamps to the break, not past it.
        let line_end = if end > start && text.as_bytes().get(end - 1) == Some(&b'\n') {
            end - 1
        } else {
            end
        };
        let line_text = &text[start.min(line_end)..line_end];
        let mut units = 0u32;
        for (offset, c) in line_text.char_indices() {
            if units >= position.character {
                return (start + offset) as u32;
            }
            units += c.len_utf16() as u32;
        }
        line_end as u32
    }

    /// The protocol position of a byte offset.
    pub fn position(&self, text: &str, offset: u32) -> Position {
        let offset = offset.min(self.len);
        let line = match self.line_starts.binary_search(&offset) {
            Ok(i) => i,
            Err(i) => i.saturating_sub(1),
        };
        let start = self.line_starts[line] as usize;
        let end = (offset as usize).min(text.len());
        let mut character = 0u32;
        if start <= end {
            // `get` is used because an offset from an older edit may not be a
            // character boundary in the current text.
            if let Some(prefix) = text.get(start..end) {
                character = prefix.chars().map(|c| c.len_utf16() as u32).sum();
            }
        }
        Position { line: line as u32, character }
    }

    /// The protocol range of a byte span.
    pub fn range(&self, text: &str, start: u32, end: u32) -> Range {
        Range {
            start: self.position(text, start),
            end: self.position(text, end),
        }
    }

}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_position_is_the_first_byte() {
        let text = "abc\ndef\n";
        let index = LineIndex::new(text);
        assert_eq!(index.offset(text, Position::new(0, 0)), 0);
        assert_eq!(index.position(text, 0), Position::new(0, 0));
    }

    #[test]
    fn a_newline_starts_the_next_line() {
        let text = "abc\ndef\n";
        let index = LineIndex::new(text);
        assert_eq!(index.offset(text, Position::new(1, 0)), 4);
        assert_eq!(index.position(text, 4), Position::new(1, 0));
        assert_eq!(index.position(text, 5), Position::new(1, 1));
    }

    #[test]
    fn columns_count_utf16_units_not_bytes() {
        // `é` is one char, one UTF-16 unit, and two bytes. The byte offset of
        // `b` is 3; its column is 2.
        let text = "é=1\n";
        let index = LineIndex::new(text);
        assert_eq!(index.position(text, 3), Position::new(0, 2));
        assert_eq!(index.offset(text, Position::new(0, 2)), 3);
    }

    #[test]
    fn a_supplementary_character_is_two_units() {
        // An emoji is one char, two UTF-16 units, four bytes.
        let text = "😀x\n";
        let index = LineIndex::new(text);
        assert_eq!(index.offset(text, Position::new(0, 2)), 4, "the emoji occupies two units");
        assert_eq!(index.position(text, 4), Position::new(0, 2));
    }

    #[test]
    fn a_position_past_the_end_of_a_line_clamps_to_it() {
        let text = "abc\ndef\n";
        let index = LineIndex::new(text);
        assert_eq!(index.offset(text, Position::new(0, 99)), 3);
    }

    #[test]
    fn a_position_past_the_end_of_the_document_clamps_to_it() {
        let text = "abc\n";
        let index = LineIndex::new(text);
        assert_eq!(index.offset(text, Position::new(99, 0)), text.len() as u32);
        assert_eq!(
            index.position(text, 999),
            Position::new(1, 0),
            "one line, so the end is the start of line 1"
        );
    }

    #[test]
    fn an_empty_document_has_one_position() {
        let text = "";
        let index = LineIndex::new(text);
        assert_eq!(index.offset(text, Position::new(0, 0)), 0);
        assert_eq!(index.position(text, 0), Position::new(0, 0));
    }

    #[test]
    fn a_range_covers_the_span_it_was_given() {
        let text = "let x = 1\nlet y = 2\n";
        let index = LineIndex::new(text);
        let range = index.range(text, 8, 9);
        assert_eq!(range.start, Position::new(0, 8));
        assert_eq!(range.end, Position::new(0, 9));
    }
}
