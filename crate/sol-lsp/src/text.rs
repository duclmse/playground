//! Position/range helpers shared by every feature module. `crates/sol`'s AST
//! only carries 1-based *line* numbers (`ast::Function`/`ast::Stmt`/`ast::Expr`
//! all have a `line: u32`, not a column) except for `ast::Function::source_span`,
//! which does carry a byte range. Everywhere else we resolve columns directly
//! from the document text instead of the AST, which keeps this crate decoupled
//! from AST layout for anything finer than "which line".

use tower_lsp::lsp_types::{Position, Range};

/// Byte offset -> LSP `Position` (0-based line/character, UTF-16 code units
/// per the LSP spec). Sol source is byte-oriented and not guaranteed to be
/// UTF-8, but LSP positions are UTF-16-based; for the ASCII-dominant Lua/Sol
/// surface this crate targets, byte/UTF-16 offsets coincide except inside
/// non-ASCII text, which is an accepted approximation for v1.
pub fn offset_to_position(text: &str, offset: usize) -> Position {
    let offset = offset.min(text.len());
    let mut line = 0u32;
    let mut line_start = 0usize;
    for (idx, byte) in text.as_bytes()[..offset].iter().enumerate() {
        if *byte == b'\n' {
            line += 1;
            line_start = idx + 1;
        }
    }
    let character = text[line_start..offset].encode_utf16().count() as u32;
    Position::new(line, character)
}

/// 1-based source line -> that line's full-line `Range` (0-based line index,
/// column 0 through the line's length). Used for diagnostics/definitions that
/// only have a line number, not a column, to point at.
pub fn line_range(text: &str, line: u32) -> Range {
    let line0 = line.saturating_sub(1);
    let content = text.lines().nth(line0 as usize).unwrap_or("");
    let end_char = content.encode_utf16().count() as u32;
    Range::new(Position::new(line0, 0), Position::new(line0, end_char))
}

fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// The identifier word touching `pos`, plus its exact range, if `pos` sits on
/// or immediately after one. Used by hover/definition/signature-help/rename
/// to figure out "what identifier is the cursor on" without needing AST
/// column data.
pub fn word_at_position(text: &str, pos: Position) -> Option<(String, Range)> {
    let line0 = pos.line as usize;
    let line = text.lines().nth(line0)?;
    let bytes = line.as_bytes();
    // `pos.character` is a UTF-16 offset; map it to a byte offset in `line`.
    let mut utf16_count = 0u32;
    let mut byte_off = bytes.len();
    for (idx, ch) in line.char_indices() {
        if utf16_count >= pos.character {
            byte_off = idx;
            break;
        }
        utf16_count += ch.len_utf16() as u32;
    }
    let mut start = byte_off;
    while start > 0 && is_ident_byte(bytes[start - 1]) {
        start -= 1;
    }
    let mut end = byte_off;
    while end < bytes.len() && is_ident_byte(bytes[end]) {
        end += 1;
    }
    if start == end {
        return None;
    }
    let word = line[start..end].to_string();
    let start_char = line[..start].encode_utf16().count() as u32;
    let end_char = line[..end].encode_utf16().count() as u32;
    Some((
        word,
        Range::new(
            Position::new(pos.line, start_char),
            Position::new(pos.line, end_char),
        ),
    ))
}

/// Every whole-word (identifier-boundary) occurrence of `word` in `text`.
/// This is a textual scan, not a scope-aware resolution: it will match a
/// same-named local in an unrelated function or an unrelated global. That
/// trade-off is documented in `docs/sol-lsp.md` - real scope resolution would
/// need a proper binder shared with `typeck.rs`, which this first version
/// does not build.
pub fn find_word_occurrences(text: &str, word: &str) -> Vec<Range> {
    if word.is_empty() {
        return Vec::new();
    }
    let mut result = Vec::new();
    for (line_idx, line) in text.lines().enumerate() {
        let bytes = line.as_bytes();
        let wlen = word.len();
        let mut i = 0usize;
        while i + wlen <= bytes.len() {
            if &bytes[i..i + wlen] == word.as_bytes()
                && (i == 0 || !is_ident_byte(bytes[i - 1]))
                && (i + wlen == bytes.len() || !is_ident_byte(bytes[i + wlen]))
            {
                let start_char = line[..i].encode_utf16().count() as u32;
                let end_char = line[..i + wlen].encode_utf16().count() as u32;
                result.push(Range::new(
                    Position::new(line_idx as u32, start_char),
                    Position::new(line_idx as u32, end_char),
                ));
                i += wlen;
            } else {
                i += 1;
            }
        }
    }
    result
}
