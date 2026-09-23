//! Source locations and human-readable compiler diagnostics.

use std::fmt;

/// A half-open byte range in one source file, with a one-based display location.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourceSpan {
    pub start: usize,
    pub end: usize,
    pub line: u32,
    pub column: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Label {
    pub span: SourceSpan,
    pub message: String,
}

impl SourceSpan {
    pub const fn new(start: usize, end: usize, line: u32, column: u32) -> Self {
        Self {
            start,
            end,
            line,
            column,
        }
    }
}

/// A primary compiler error location and its message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    pub severity: Severity,
    pub code: Option<String>,
    pub file: Option<String>,
    pub message: String,
    pub primary: SourceSpan,
    pub labels: Vec<Label>,
    pub notes: Vec<String>,
}

impl Diagnostic {
    pub fn error(primary: SourceSpan, message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Error,
            code: None,
            file: None,
            message: message.into(),
            primary,
            labels: Vec::new(),
            notes: Vec::new(),
        }
    }

    pub fn with_code(mut self, code: impl Into<String>) -> Self {
        self.code = Some(code.into());
        self
    }
    pub fn with_file(mut self, file: impl Into<String>) -> Self {
        self.file = Some(file.into());
        self
    }
    pub fn with_label(mut self, span: SourceSpan, message: impl Into<String>) -> Self {
        self.labels.push(Label {
            span,
            message: message.into(),
        });
        self
    }
    pub fn with_note(mut self, note: impl Into<String>) -> Self {
        self.notes.push(note.into());
        self
    }

    /// Stable, dependency-free editor integration format.
    pub fn to_json(&self) -> String {
        fn quote(value: &str) -> String {
            format!(
                "\"{}\"",
                value
                    .replace('\\', "\\\\")
                    .replace('"', "\\\"")
                    .replace('\n', "\\n")
            )
        }
        let code = self
            .code
            .as_deref()
            .map(quote)
            .unwrap_or_else(|| "null".into());
        let file = self
            .file
            .as_deref()
            .map(quote)
            .unwrap_or_else(|| "null".into());
        let labels = self
            .labels
            .iter()
            .map(|label| {
                format!(
                    "{{\"message\":{},\"span\":{{\"start\":{},\"end\":{},\"line\":{},\"column\":{}}}}}",
                    quote(&label.message),
                    label.span.start,
                    label.span.end,
                    label.span.line,
                    label.span.column
                )
            })
            .collect::<Vec<_>>()
            .join(",");
        let notes = self
            .notes
            .iter()
            .map(|note| quote(note))
            .collect::<Vec<_>>()
            .join(",");
        format!("{{\"severity\":\"{}\",\"code\":{},\"file\":{},\"message\":{},\"span\":{{\"start\":{},\"end\":{},\"line\":{},\"column\":{}}},\"labels\":[{}],\"notes\":[{}]}}", match self.severity { Severity::Error => "error", Severity::Warning => "warning" }, code, file, quote(&self.message), self.primary.start, self.primary.end, self.primary.line, self.primary.column, labels, notes)
    }

    pub fn render_excerpt(&self, source: &str) -> String {
        let line = source
            .lines()
            .nth(self.primary.line.saturating_sub(1) as usize)
            .unwrap_or("");
        format!(
            "{self}\n{}\n{}^",
            line,
            " ".repeat(self.primary.column.saturating_sub(1) as usize)
        )
    }
}

impl fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "line {}, column {}: {}{}",
            self.primary.line,
            self.primary.column,
            self.message,
            self.code
                .as_ref()
                .map(|code| format!(" [{code}]"))
                .unwrap_or_default()
        )
    }
}

impl std::error::Error for Diagnostic {}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn renders_human_and_machine_readable_diagnostics() {
        let diagnostic = Diagnostic::error(SourceSpan::new(3, 4, 2, 2), "bad token")
            .with_code("ELEX001")
            .with_note("use a valid token");
        assert_eq!(
            diagnostic.render_excerpt("a\n @"),
            "line 2, column 2: bad token [ELEX001]\n @\n ^"
        );
        assert!(diagnostic.to_json().contains("\"code\":\"ELEX001\""));
        assert!(diagnostic
            .to_json()
            .contains("\"notes\":[\"use a valid token\"]"));
        assert!(diagnostic.to_json().contains("\"file\":null"));
    }
}
