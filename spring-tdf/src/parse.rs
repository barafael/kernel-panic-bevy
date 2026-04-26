//! Generic TDF parser.
//!
//! Produces a [`Tdf`] tree — a list of named [`Section`]s, each containing
//! key-value pairs and nested subsections.
//!
//! ## Conformance
//!
//! This is a port of Spring's reference parser
//! (`cont/base/springcontent/gamedata/parse_tdf.lua`, invoked from
//! [`rts/System/TdfParser.cpp`][cpp]). It mirrors that implementation
//! precisely:
//!
//! - whitespace, `//` line comments, and `/* … */` block comments (non-greedy,
//!   may span lines) are eaten between tokens via [`Parser::eat_white`]
//! - section headers are `[name]` with `]`-terminated names; any whitespace
//!   or comment may sit between `]` and the opening `{`
//! - keys match `[^\s=]+` and may carry inline tabs/spaces around the `=`
//! - values are either quoted (`"…";+`) or unquoted (`[^\n;]*;+`); both
//!   forms **require** a trailing `;`
//! - top-level `key=value;` pairs sitting outside any section are valid and
//!   land in [`Tdf::root_entries`]
//!
//! Strictness deliberately matches Spring: missing terminators, unterminated
//! comments, unterminated quotes, and stray closing braces all raise
//! [`ParseError`] rather than being silently skipped. Real upstream KP
//! files satisfy these rules.
//!
//! [cpp]: https://github.com/beyond-all-reason/RecoilEngine/blob/master/rts/System/TdfParser.cpp

use std::collections::BTreeMap;

use thiserror::Error;

/// A parsed TDF document.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Tdf {
    /// Top-level named sections, in source order.
    pub sections: Vec<Section>,
    /// Top-level `key=value;` pairs that appear outside any section.
    /// Spring stores these on the root table; for KP content this is
    /// always empty, but the field exists so callers passing arbitrary
    /// TDF can still see them.
    pub root_entries: BTreeMap<String, String>,
}

/// A named section containing key-value pairs and nested subsections.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Section {
    /// The section name as it appeared in `[Name]` (whitespace-trimmed).
    pub name: String,
    /// Key-value pairs (keys lowercased, values verbatim apart from
    /// stripped trailing inline whitespace).
    pub entries: BTreeMap<String, String>,
    /// Nested child sections, in source order.
    pub children: Vec<Section>,
}

impl Tdf {
    /// Parse a TDF document from its raw text.
    pub fn parse(text: &str) -> Result<Self, ParseError> {
        Parser::new(text).parse_document()
    }

    /// Look up a top-level section by name (case-insensitive).
    pub fn section(&self, name: &str) -> Option<&Section> {
        let needle = name.to_ascii_lowercase();
        self.sections
            .iter()
            .find(|s| s.name.to_ascii_lowercase() == needle)
    }
}

impl Section {
    /// Look up a value by key (case-insensitive).
    pub fn get(&self, key: &str) -> Option<&str> {
        self.entries
            .get(&key.to_ascii_lowercase())
            .map(|s| s.as_str())
    }

    /// Look up a child section by name (case-insensitive).
    pub fn child(&self, name: &str) -> Option<&Section> {
        let needle = name.to_ascii_lowercase();
        self.children
            .iter()
            .find(|s| s.name.to_ascii_lowercase() == needle)
    }

    /// Parse a value as `f32`, returning `0.0` for missing or malformed values.
    pub fn f32(&self, key: &str) -> f32 {
        self.f32_or(key, 0.0)
    }

    /// Parse a value as `f32`, returning `default` for missing or malformed
    /// values. Use when Spring's authored default for a tag is non-zero
    /// (e.g. `beamdecay` defaults to 1.0 — "no fade" — when omitted).
    pub fn f32_or(&self, key: &str, default: f32) -> f32 {
        self.get(key)
            .and_then(|v| v.trim().parse().ok())
            .unwrap_or(default)
    }

    /// Parse a value as `bool` (Spring convention: `1` = true, anything else = false).
    pub fn bool(&self, key: &str) -> bool {
        self.get(key).is_some_and(|v| v.trim() == "1")
    }

    /// Get a value as an owned `String`, returning `""` for missing keys.
    pub fn string(&self, key: &str) -> String {
        self.get(key).unwrap_or_default().to_string()
    }

    /// Get a value with inline `//` comments stripped and whitespace trimmed.
    ///
    /// Under the strict parser this is functionally equivalent to
    /// [`Section::string`] — `//` comments inside lines are consumed by the
    /// tokenizer's [`Parser::eat_white`] before they can reach a value. The
    /// helper is kept for callers that want the previous semantics
    /// explicitly.
    pub fn string_clean(&self, key: &str) -> String {
        let raw = self.get(key).unwrap_or_default();
        match raw.find("//") {
            Some(pos) => raw[..pos].trim().to_string(),
            None => raw.to_string(),
        }
    }

    /// Parse a space-separated RGB triplet (e.g. `"128 0 0"` or `"0.8 1 0.8"`).
    ///
    /// Missing or malformed components default to `0.0`.
    pub fn color3(&self, key: &str) -> [f32; 3] {
        let value = self.get(key).unwrap_or_default();
        let parts: Vec<f32> = value
            .split_whitespace()
            .filter_map(|s| s.parse().ok())
            .collect();
        [
            parts.first().copied().unwrap_or(0.0),
            parts.get(1).copied().unwrap_or(0.0),
            parts.get(2).copied().unwrap_or(0.0),
        ]
    }
}

/// Errors raised while parsing TDF text.
///
/// Variants intentionally cover every failure mode Spring's reference
/// parser also rejects, so error messages stay actionable.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum ParseError {
    #[error("line {line}: section header missing closing `]`")]
    UnclosedSectionHeader { line: usize },

    #[error("line {line}: section `{section}` missing opening `{{`")]
    MissingOpenBrace { line: usize, section: String },

    #[error("line {line}: section `{section}` missing closing `}}`")]
    UnclosedSection { line: usize, section: String },

    #[error("line {line}: unmatched `}}` outside any section")]
    UnmatchedCloseBrace { line: usize },

    #[error("line {line}: empty key")]
    EmptyKey { line: usize },

    #[error("line {line}: missing `=` after key `{key}`")]
    MissingEquals { line: usize, key: String },

    #[error("line {line}: missing `;` after value")]
    MissingSemicolon { line: usize },

    #[error("line {line}: unterminated quoted string")]
    UnterminatedString { line: usize },

    #[error("line {line}: unterminated block comment")]
    UnterminatedBlockComment { line: usize },
}

struct Parser<'a> {
    text: &'a str,
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Parser<'a> {
    fn new(text: &'a str) -> Self {
        Self {
            text,
            bytes: text.as_bytes(),
            pos: 0,
        }
    }

    fn line(&self) -> usize {
        self.line_at(self.pos)
    }

    fn line_at(&self, pos: usize) -> usize {
        let p = pos.min(self.bytes.len());
        self.bytes[..p].iter().filter(|&&b| b == b'\n').count() + 1
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    fn starts_with(&self, lit: &[u8]) -> bool {
        self.bytes
            .get(self.pos..)
            .is_some_and(|s| s.starts_with(lit))
    }

    /// Advance over whitespace, `//` line comments, and `/* … */` block
    /// comments. Loops until no progress is made — matches the
    /// `repeat … until ppos == pos` shape in Spring's `EatWhite`.
    fn eat_white(&mut self) -> Result<(), ParseError> {
        loop {
            let start = self.pos;

            // ASCII whitespace (space, tab, CR, LF, FF, VT).
            while let Some(b) = self.peek() {
                if b.is_ascii_whitespace() {
                    self.pos += 1;
                } else {
                    break;
                }
            }

            // Line comment: `//` to end of line. The newline itself is
            // left for the whitespace pass on the next iteration.
            if self.starts_with(b"//") {
                self.pos += 2;
                while let Some(b) = self.peek() {
                    if b == b'\n' {
                        break;
                    }
                    self.pos += 1;
                }
                continue;
            }

            // Block comment: `/*` … `*/`. Non-greedy: closes on the
            // first `*/`. May span multiple lines.
            if self.starts_with(b"/*") {
                let open_line = self.line();
                self.pos += 2;
                loop {
                    if self.pos >= self.bytes.len() {
                        return Err(ParseError::UnterminatedBlockComment { line: open_line });
                    }
                    if self.starts_with(b"*/") {
                        self.pos += 2;
                        break;
                    }
                    self.pos += 1;
                }
                continue;
            }

            if self.pos == start {
                return Ok(());
            }
        }
    }

    /// Parse one TDF document. Top-level elements are sections,
    /// `key=value;` pairs (stored in [`Tdf::root_entries`]), or trailing
    /// whitespace / comments. A stray `}` at the top level is an error.
    fn parse_document(&mut self) -> Result<Tdf, ParseError> {
        let mut sections = Vec::new();
        let mut root_entries = BTreeMap::new();

        loop {
            self.eat_white()?;
            match self.peek() {
                None => break,
                Some(b'}') => {
                    return Err(ParseError::UnmatchedCloseBrace { line: self.line() });
                }
                Some(b'[') => sections.push(self.parse_section()?),
                Some(_) => {
                    let (key, value) = self.parse_pair()?;
                    root_entries.insert(key, value);
                }
            }
        }

        Ok(Tdf {
            sections,
            root_entries,
        })
    }

    /// Parse a section starting at the leading `[`.
    fn parse_section(&mut self) -> Result<Section, ParseError> {
        debug_assert_eq!(self.peek(), Some(b'['));
        let header_line = self.line();
        self.pos += 1;

        let name_start = self.pos;
        while let Some(b) = self.peek() {
            match b {
                b']' => break,
                b'\n' => return Err(ParseError::UnclosedSectionHeader { line: header_line }),
                _ => self.pos += 1,
            }
        }
        if self.peek() != Some(b']') {
            return Err(ParseError::UnclosedSectionHeader { line: header_line });
        }
        let name = self.text[name_start..self.pos].trim().to_string();
        self.pos += 1; // consume ']'

        self.eat_white()?;
        if self.peek() != Some(b'{') {
            return Err(ParseError::MissingOpenBrace {
                line: self.line(),
                section: name,
            });
        }
        let body_line = self.line();
        self.pos += 1;

        let mut entries = BTreeMap::new();
        let mut children = Vec::new();

        loop {
            self.eat_white()?;
            match self.peek() {
                None => {
                    return Err(ParseError::UnclosedSection {
                        line: body_line,
                        section: name,
                    });
                }
                Some(b'}') => {
                    self.pos += 1;
                    break;
                }
                Some(b'[') => children.push(self.parse_section()?),
                Some(_) => {
                    let (k, v) = self.parse_pair()?;
                    entries.insert(k, v);
                }
            }
        }

        Ok(Section {
            name,
            entries,
            children,
        })
    }

    fn parse_pair(&mut self) -> Result<(String, String), ParseError> {
        let key = self.parse_key()?;
        let value = self.parse_value()?;
        Ok((key, value))
    }

    /// Parse a key plus the surrounding `=`. Mirrors
    /// `^([^%s=]+)[ \t]*=[ \t]*` from Spring's `ParseKey`.
    fn parse_key(&mut self) -> Result<String, ParseError> {
        let line = self.line();
        let start = self.pos;
        while let Some(b) = self.peek() {
            if b == b'=' || b.is_ascii_whitespace() {
                break;
            }
            self.pos += 1;
        }
        if self.pos == start {
            return Err(ParseError::EmptyKey { line });
        }
        let key = self.text[start..self.pos].to_ascii_lowercase();

        // Inline whitespace before `=` (tabs / spaces only — newlines
        // make the key invalid).
        self.eat_inline_ws();
        if self.peek() != Some(b'=') {
            return Err(ParseError::MissingEquals { line, key });
        }
        self.pos += 1;
        self.eat_inline_ws();
        Ok(key)
    }

    /// Parse a value plus its trailing `;`. Quoted values are recognised
    /// with the same regex as Spring's `ParseValue`.
    fn parse_value(&mut self) -> Result<String, ParseError> {
        let line = self.line();

        if self.peek() == Some(b'"') {
            self.pos += 1;
            let start = self.pos;
            while let Some(b) = self.peek() {
                if b == b'"' || b == b'\n' {
                    break;
                }
                self.pos += 1;
            }
            if self.peek() != Some(b'"') {
                return Err(ParseError::UnterminatedString { line });
            }
            let value = self.text[start..self.pos].to_string();
            self.pos += 1; // consume closing `"`
            self.eat_inline_ws();
            self.consume_semicolons(line)?;
            return Ok(value);
        }

        let start = self.pos;
        while let Some(b) = self.peek() {
            if b == b'\n' || b == b';' {
                break;
            }
            self.pos += 1;
        }
        if self.peek() != Some(b';') {
            return Err(ParseError::MissingSemicolon { line });
        }
        // Spring's regex captures up to `;` as-is; trim trailing inline
        // whitespace so `key = value ;` round-trips to `value` rather
        // than `value ` (the existing Section helpers already assume a
        // trimmed view of values via .trim() at the call site).
        let value = self.text[start..self.pos]
            .trim_end_matches([' ', '\t'])
            .to_string();
        self.consume_semicolons(line)?;
        Ok(value)
    }

    fn eat_inline_ws(&mut self) {
        while let Some(b) = self.peek() {
            if b == b' ' || b == b'\t' {
                self.pos += 1;
            } else {
                break;
            }
        }
    }

    /// Spring requires at least one `;` and consumes any chain of them.
    fn consume_semicolons(&mut self, line: usize) -> Result<(), ParseError> {
        if self.peek() != Some(b';') {
            return Err(ParseError::MissingSemicolon { line });
        }
        while self.peek() == Some(b';') {
            self.pos += 1;
        }
        Ok(())
    }
}
