//! Generic TDF parser.
//!
//! Produces a [`Tdf`] tree — a list of named [`Section`]s, each containing
//! key-value pairs and nested subsections.

use std::collections::BTreeMap;

use thiserror::Error;

/// A parsed TDF document: a sequence of top-level sections.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Tdf {
    pub sections: Vec<Section>,
}

/// A named section containing key-value pairs and nested subsections.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Section {
    /// The section name as it appeared in `[Name]`.
    pub name: String,
    /// Key-value pairs (keys lowercased, values as-is).
    pub entries: BTreeMap<String, String>,
    /// Nested child sections.
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
    /// Some TDF values contain trailing comments (e.g. `Weapon1=BuildLaser;//Unused`).
    /// The line-level comment stripping in the parser doesn't catch these because
    /// the `//` is inside the value portion of a `key=value;` pair.
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

#[derive(Debug, Error)]
pub enum ParseError {
    #[error("line {line}: unexpected closing brace without matching section")]
    UnmatchedCloseBrace { line: usize },

    #[error("line {line}: unexpected end of input inside section `{section}`")]
    UnclosedSection { line: usize, section: String },
}

struct Parser<'a> {
    lines: Vec<(usize, &'a str)>,
    pos: usize,
}

impl<'a> Parser<'a> {
    fn new(text: &'a str) -> Self {
        let lines: Vec<(usize, &str)> = text
            .lines()
            .enumerate()
            .map(|(i, line)| (i + 1, strip_comment(line).trim()))
            .filter(|(_, line)| !line.is_empty())
            .collect();
        Self { lines, pos: 0 }
    }

    fn peek(&self) -> Option<(usize, &'a str)> {
        self.lines.get(self.pos).copied()
    }

    fn advance(&mut self) {
        self.pos += 1;
    }

    fn parse_document(&mut self) -> Result<Tdf, ParseError> {
        let mut sections = Vec::new();
        while let Some((line_num, line)) = self.peek() {
            if line == "}" {
                return Err(ParseError::UnmatchedCloseBrace { line: line_num });
            }
            if let Some(name) = section_name(line) {
                self.advance();
                let section = self.parse_section(name.to_string(), line_num)?;
                sections.push(section);
            } else {
                // Stray key=value outside any section — skip.
                self.advance();
            }
        }
        Ok(Tdf { sections })
    }

    fn parse_section(&mut self, name: String, open_line: usize) -> Result<Section, ParseError> {
        // Expect `{` on the next non-empty line.
        if let Some((_, line)) = self.peek()
            && line == "{"
        {
            self.advance();
        }
        // Some files put `{` on the same line as `[Name]` — already consumed.

        let mut entries = BTreeMap::new();
        let mut children = Vec::new();

        loop {
            let Some((line_num, line)) = self.peek() else {
                return Err(ParseError::UnclosedSection {
                    line: open_line,
                    section: name,
                });
            };

            if line == "}" {
                self.advance();
                break;
            }

            if let Some(child_name) = section_name(line) {
                self.advance();
                let child = self.parse_section(child_name.to_string(), line_num)?;
                children.push(child);
                continue;
            }

            if let Some((key, value)) = parse_key_value(line) {
                entries.insert(key, value);
            }
            self.advance();
        }

        Ok(Section {
            name,
            entries,
            children,
        })
    }
}

/// Strip `//` line comments.
fn strip_comment(line: &str) -> &str {
    match line.find("//") {
        Some(pos) => &line[..pos],
        None => line,
    }
}

/// Extract the section name from a `[Name]` header line.
fn section_name(line: &str) -> Option<&str> {
    let line = line.trim();
    if line.starts_with('[') {
        let end = line.find(']')?;
        Some(line[1..end].trim())
    } else {
        None
    }
}

/// Parse `key=value;` into (lowercased_key, trimmed_value).
fn parse_key_value(line: &str) -> Option<(String, String)> {
    let line = line.trim_end_matches(';').trim();
    let eq_pos = line.find('=')?;
    let key = line[..eq_pos].trim().to_ascii_lowercase();
    let value = line[eq_pos + 1..].trim().to_string();
    if key.is_empty() {
        return None;
    }
    Some((key, value))
}
