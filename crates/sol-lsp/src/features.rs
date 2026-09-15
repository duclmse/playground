//! Feature implementations, each a plain function over a `Document` plus
//! whatever LSP request data it needs. `backend.rs` is just the
//! `LanguageServer` trait wiring onto these.

use sol::parser::SourceMode;
use tower_lsp::lsp_types::{
    CompletionItem, CompletionItemKind, DocumentSymbol, Location, ParameterInformation,
    ParameterLabel, Position, Range, SignatureHelp, SignatureInformation, SymbolKind, Url,
};

use crate::document::Document;
use crate::index::SymbolIndex;
use crate::text::{find_word_occurrences, offset_to_position, word_at_position};

const LUA_KEYWORDS: &[&str] = &[
    "and", "break", "do", "else", "elseif", "end", "false", "for", "function", "goto", "if",
    "in", "local", "nil", "not", "or", "repeat", "return", "then", "true", "until", "while",
];

const SOL_KEYWORDS: &[&str] = &[
    "struct", "extern", "import", "export", "is", "as", "const", "i64", "f64", "bool", "string",
    "any", "Array", "Map",
];

fn identifier_range_on_line(text: &str, line: u32, name: &str) -> Range {
    let full = find_word_occurrences(text, name);
    full.into_iter()
        .find(|r| r.start.line == line.saturating_sub(1))
        .unwrap_or_else(|| crate::text::line_range(text, line))
}

fn hover_markdown(mode: SourceMode, code: String) -> String {
    let lang = if mode == SourceMode::Lua { "lua" } else { "sol" };
    format!("```{lang}\n{code}\n```")
}

pub fn hover(doc: &Document, pos: Position) -> Option<String> {
    let (word, _range) = word_at_position(&doc.text, pos)?;

    if let Some(f) = doc.index.find_function(&word) {
        let params = f
            .params
            .iter()
            .map(|(n, t)| format!("{n}: {t}"))
            .collect::<Vec<_>>()
            .join(", ");
        let ret = f
            .return_type
            .as_ref()
            .map(|r| format!(": {r}"))
            .unwrap_or_default();
        let kw = if f.is_extern { "extern function" } else { "function" };
        return Some(hover_markdown(
            doc.mode,
            format!("{kw} {}({params}){ret}", f.name),
        ));
    }

    if let Some(s) = doc.index.find_struct(&word) {
        let fields = s
            .fields
            .iter()
            .map(|(n, t)| format!("    {n}: {t}"))
            .collect::<Vec<_>>()
            .join(",\n");
        return Some(hover_markdown(
            doc.mode,
            format!("struct {} {{\n{fields}\n}}", s.name),
        ));
    }

    let enclosing = doc.index.enclosing_function_at(&doc.text, pos.line + 1);
    if let Some(local) = doc.index.find_local(&word, &enclosing, pos.line + 1) {
        let ty = local.ty.clone().unwrap_or_else(|| "any".to_string());
        return Some(hover_markdown(doc.mode, format!("local {}: {ty}", local.name)));
    }

    None
}

pub fn definition(doc: &Document, uri: &Url, pos: Position) -> Option<Location> {
    let (word, _range) = word_at_position(&doc.text, pos)?;

    if let Some(f) = doc.index.find_function(&word) {
        let range = identifier_range_on_line(&doc.text, f.line, &f.name);
        return Some(Location::new(uri.clone(), range));
    }
    if let Some(s) = doc.index.find_struct(&word) {
        let range = identifier_range_on_line(&doc.text, s.line, &s.name);
        return Some(Location::new(uri.clone(), range));
    }
    let enclosing = doc.index.enclosing_function_at(&doc.text, pos.line + 1);
    if let Some(local) = doc.index.find_local(&word, &enclosing, pos.line + 1) {
        let range = identifier_range_on_line(&doc.text, local.line, &local.name);
        return Some(Location::new(uri.clone(), range));
    }
    None
}

/// Every occurrence of the identifier under `pos`, in this document only -
/// see `text::find_word_occurrences` for why this is textual rather than
/// scope-aware. Used for both `textDocument/references` (wrapped in
/// `Location`s) and `textDocument/documentHighlight` (ranges only).
pub fn word_occurrences_at(doc: &Document, pos: Position) -> Vec<Range> {
    let Some((word, _)) = word_at_position(&doc.text, pos) else {
        return Vec::new();
    };
    find_word_occurrences(&doc.text, &word)
}

#[allow(deprecated)]
pub fn document_symbols(doc: &Document) -> Vec<DocumentSymbol> {
    let mut out = Vec::new();
    for f in &doc.index.functions {
        if f.is_extern {
            continue;
        }
        let start = offset_to_position(&doc.text, f.start_byte);
        let end = offset_to_position(&doc.text, f.end_byte);
        let range = Range::new(start, end);
        let selection_range = identifier_range_on_line(&doc.text, f.line, &f.name);
        let params = f
            .params
            .iter()
            .map(|(n, t)| format!("{n}: {t}"))
            .collect::<Vec<_>>()
            .join(", ");
        out.push(DocumentSymbol {
            name: f.name.clone(),
            detail: Some(format!("({params})")),
            kind: SymbolKind::FUNCTION,
            tags: None,
            deprecated: None,
            range,
            selection_range,
            children: None,
        });
    }
    for s in &doc.index.structs {
        let range = crate::text::line_range(&doc.text, s.line);
        out.push(DocumentSymbol {
            name: s.name.clone(),
            detail: None,
            kind: SymbolKind::STRUCT,
            tags: None,
            deprecated: None,
            range,
            selection_range: range,
            children: None,
        });
    }
    out
}

pub fn completions(index: &SymbolIndex, mode: SourceMode, enclosing: &str, line: u32) -> Vec<CompletionItem> {
    let mut items = Vec::new();
    for f in &index.functions {
        let params = f
            .params
            .iter()
            .map(|(n, t)| format!("{n}: {t}"))
            .collect::<Vec<_>>()
            .join(", ");
        items.push(CompletionItem {
            label: f.name.clone(),
            kind: Some(CompletionItemKind::FUNCTION),
            detail: Some(format!("({params})")),
            ..Default::default()
        });
    }
    for s in &index.structs {
        items.push(CompletionItem {
            label: s.name.clone(),
            kind: Some(CompletionItemKind::STRUCT),
            ..Default::default()
        });
    }
    for l in &index.locals {
        if l.enclosing_function == enclosing && l.line <= line {
            items.push(CompletionItem {
                label: l.name.clone(),
                kind: Some(CompletionItemKind::VARIABLE),
                detail: l.ty.clone(),
                ..Default::default()
            });
        }
    }
    let keywords: &[&str] = if mode == SourceMode::Lua {
        LUA_KEYWORDS
    } else {
        SOL_KEYWORDS
    };
    for kw in keywords {
        items.push(CompletionItem {
            label: kw.to_string(),
            kind: Some(CompletionItemKind::KEYWORD),
            ..Default::default()
        });
    }
    items
}

fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// Scans backward from the cursor on its own line for the nearest enclosing
/// `(`, returning the callee name immediately before it and how many
/// top-level commas separate that `(` from the cursor (the active parameter
/// index). Single-line only - multi-line call arguments are an accepted v1
/// limitation, see `docs/sol-lsp.md`.
fn call_context(line: &str, byte_pos: usize) -> Option<(String, usize)> {
    let bytes = line.as_bytes();
    let mut depth = 0i32;
    let mut active_param = 0usize;
    let mut i = byte_pos.min(bytes.len());
    while i > 0 {
        i -= 1;
        match bytes[i] {
            b')' => depth += 1,
            b'(' => {
                if depth == 0 {
                    let mut end = i;
                    while end > 0 && bytes[end - 1].is_ascii_whitespace() {
                        end -= 1;
                    }
                    let mut start = end;
                    while start > 0 && is_ident_byte(bytes[start - 1]) {
                        start -= 1;
                    }
                    if start == end {
                        return None;
                    }
                    return Some((line[start..end].to_string(), active_param));
                }
                depth -= 1;
            }
            b',' if depth == 0 => active_param += 1,
            _ => {}
        }
    }
    None
}

pub fn signature_help(doc: &Document, pos: Position) -> Option<SignatureHelp> {
    let line = doc.text.lines().nth(pos.line as usize)?;
    // Map the UTF-16 `pos.character` to a byte offset in `line`.
    let mut utf16_count = 0u32;
    let mut byte_off = line.len();
    for (idx, ch) in line.char_indices() {
        if utf16_count >= pos.character {
            byte_off = idx;
            break;
        }
        utf16_count += ch.len_utf16() as u32;
    }
    let (name, active_param) = call_context(line, byte_off)?;
    let f = doc.index.find_function(&name)?;
    let params: Vec<ParameterInformation> = f
        .params
        .iter()
        .map(|(n, t)| ParameterInformation {
            label: ParameterLabel::Simple(format!("{n}: {t}")),
            documentation: None,
        })
        .collect();
    let label = format!(
        "{}({})",
        f.name,
        f.params
            .iter()
            .map(|(n, t)| format!("{n}: {t}"))
            .collect::<Vec<_>>()
            .join(", ")
    );
    Some(SignatureHelp {
        signatures: vec![SignatureInformation {
            label,
            documentation: None,
            parameters: Some(params),
            active_parameter: Some(active_param as u32),
        }],
        active_signature: Some(0),
        active_parameter: Some(active_param as u32),
    })
}
