//! A small, dependency-free CSV/TSV parser for pseudo-op scripts.
//!
//! Scope, per `plans/015-csv-parsing.md`: RFC 4180-style quoting (embedded
//! delimiters/newlines, `""` escaping a literal quote), a configurable
//! single-character delimiter, and blank-line skipping. Every field is a
//! plain string — no numeric coercion, no automatic trimming (§5.2), matching
//! how `xml_node.text` stays mechanical and leaves `.trimmed()` to the
//! script.
//!
//! This is Rust doing the byte-level work so a Rhai script only ever walks an
//! already-parsed table — see [`crate::xml`], which this module's cursor
//! shape (`rest`/`line`/`col`) deliberately mirrors.

use std::fmt;
use std::sync::Arc;

/// A parsed CSV/TSV document, as scripts see it (via `csv_table` in the
/// engine).
///
/// `Arc`-backed so cloning a handle — which Rhai does for every function
/// argument that is not the method receiver — is a refcount bump, matching
/// [`crate::xml::XmlNode`]'s guarantee. See the plan's §4 for why CSV gets
/// this treatment rather than JSON's eager conversion: `csv_row`'s dual
/// name/position indexing is per-row behavior no native Rhai map or array
/// offers together.
#[derive(Clone, Debug)]
pub struct CsvTable(Arc<CsvTableData>);

#[derive(Debug)]
struct CsvTableData {
    headers: Arc<Vec<String>>,
    rows: Vec<CsvRow>,
}

impl CsvTable {
    pub(crate) fn headers(&self) -> &[String] {
        &self.0.headers
    }

    pub(crate) fn rows(&self) -> &[CsvRow] {
        &self.0.rows
    }
}

impl IntoIterator for CsvTable {
    type Item = CsvRow;
    type IntoIter = std::vec::IntoIter<CsvRow>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.rows.clone().into_iter()
    }
}

/// One data row of a [`CsvTable`], indexable by column name or position.
///
/// `Arc`-backed for the same reason as `CsvTable`; `headers` is shared across
/// every row of the same table (one clone per row is a refcount bump, not a
/// copy of the column names).
#[derive(Clone, Debug)]
pub struct CsvRow(Arc<CsvRowData>);

#[derive(Debug)]
struct CsvRowData {
    headers: Arc<Vec<String>>,
    fields: Vec<String>,
}

impl CsvRow {
    /// The field under `name` — the first header with that name, if the
    /// header row has a duplicate — or an error naming the unknown column
    /// (§5.1: a bad index throws, since a row's columns are fixed by the
    /// table's own header).
    pub(crate) fn field_by_name(&self, name: &str) -> Result<&str, String> {
        let index = self
            .0
            .headers
            .iter()
            .position(|h| h == name)
            .ok_or_else(|| format!("unknown column {name:?}"))?;
        Ok(&self.0.fields[index])
    }

    /// The field at zero-based position `index`, or an error naming the
    /// index and the row's actual width.
    pub(crate) fn field_by_index(&self, index: i64) -> Result<&str, String> {
        usize::try_from(index)
            .ok()
            .and_then(|i| self.0.fields.get(i))
            .map(String::as_str)
            .ok_or_else(|| {
                format!(
                    "row index {index} out of range (0..{})",
                    self.0.fields.len()
                )
            })
    }
}

/// A parse failure, with the line and column the parser had reached.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CsvError {
    pub line: u32,
    pub col: u32,
    pub message: String,
}

impl fmt::Display for CsvError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}: {}", self.line, self.col, self.message)
    }
}

/// Parse `src` as CSV, using `,` as the field delimiter.
///
/// # Errors
/// See [`parse_with_delimiter`].
pub fn parse(src: &str) -> Result<CsvTable, CsvError> {
    parse_with_delimiter(src, ',')
}

/// Parse `src` as CSV/TSV, splitting fields on `delimiter`.
///
/// # Errors
/// Returns a [`CsvError`] naming the line and column of the failure: an
/// unterminated quoted field, or a data row whose field count disagrees with
/// the header row's.
pub fn parse_with_delimiter(src: &str, delimiter: char) -> Result<CsvTable, CsvError> {
    let src = src.strip_prefix('\u{feff}').unwrap_or(src);
    let mut p = Parser::new(src, delimiter);

    let Some(header_line) = p.read_row()? else {
        return Ok(CsvTable(Arc::new(CsvTableData {
            headers: Arc::new(Vec::new()),
            rows: Vec::new(),
        })));
    };
    let headers = Arc::new(header_line.fields);

    let mut rows = Vec::new();
    while let Some(line) = p.read_row()? {
        if line.fields.len() != headers.len() {
            let bad = if line.fields.len() < headers.len() {
                format!("missing {:?}", headers[line.fields.len()])
            } else {
                format!("no header for field {}", line.fields.len() + 1)
            };
            return Err(CsvError {
                line: line.line,
                col: 1,
                message: format!(
                    "row has {} field{}, expected {} ({bad})",
                    line.fields.len(),
                    if line.fields.len() == 1 { "" } else { "s" },
                    headers.len()
                ),
            });
        }
        rows.push(CsvRow(Arc::new(CsvRowData {
            headers: headers.clone(),
            fields: line.fields,
        })));
    }

    Ok(CsvTable(Arc::new(CsvTableData { headers, rows })))
}

/// One raw row, as read off the cursor before header/width validation.
struct Row {
    fields: Vec<String>,
    /// The line the row *started* on, for error messages.
    line: u32,
}

/// A cursor over the remaining source, tracking line/column as it advances —
/// the same shape as `xml::Parser`.
struct Parser<'a> {
    rest: &'a str,
    line: u32,
    col: u32,
    delimiter: char,
}

impl<'a> Parser<'a> {
    fn new(src: &'a str, delimiter: char) -> Self {
        Parser {
            rest: src,
            line: 1,
            col: 1,
            delimiter,
        }
    }

    fn peek(&self) -> Option<char> {
        self.rest.chars().next()
    }

    fn advance(&mut self) -> Option<char> {
        let mut chars = self.rest.chars();
        let c = chars.next()?;
        self.rest = chars.as_str();
        if c == '\n' {
            self.line += 1;
            self.col = 1;
        } else {
            self.col += 1;
        }
        Some(c)
    }

    /// Consume a single line ending (`\r\n` or `\n`) if present.
    fn eat_line_ending(&mut self) -> bool {
        if self.peek() == Some('\r') {
            self.advance();
            if self.peek() == Some('\n') {
                self.advance();
            }
            true
        } else if self.peek() == Some('\n') {
            self.advance();
            true
        } else {
            false
        }
    }

    fn error(&self, message: impl Into<String>) -> CsvError {
        CsvError {
            line: self.line,
            col: self.col,
            message: message.into(),
        }
    }

    /// Read one field, quoted or not, up to (not including) the next
    /// delimiter or line ending.
    fn read_field(&mut self) -> Result<String, CsvError> {
        if self.peek() == Some('"') {
            return self.read_quoted_field();
        }
        let mut out = String::new();
        loop {
            match self.peek() {
                None | Some('\r' | '\n') => return Ok(out),
                Some(c) if c == self.delimiter => return Ok(out),
                Some(c) => {
                    out.push(c);
                    self.advance();
                }
            }
        }
    }

    /// Read a quoted field (the opening quote is the current character):
    /// everything through the matching closing quote is literal, and `""`
    /// decodes to one literal `"`.
    fn read_quoted_field(&mut self) -> Result<String, CsvError> {
        let start = self.error("unterminated quoted field");
        self.advance(); // opening '"'
        let mut out = String::new();
        loop {
            match self.peek() {
                None => return Err(start),
                Some('"') => {
                    self.advance();
                    if self.peek() == Some('"') {
                        out.push('"');
                        self.advance();
                    } else {
                        return Ok(out);
                    }
                }
                Some(c) => {
                    out.push(c);
                    self.advance();
                }
            }
        }
    }

    /// Read one row: `None` at end of input, skipping any number of blank
    /// lines first (§3: a blank line is zero characters between line
    /// endings, not merely a line of all-empty fields).
    fn read_row(&mut self) -> Result<Option<Row>, CsvError> {
        loop {
            if self.peek().is_none() {
                return Ok(None);
            }
            if matches!(self.peek(), Some('\r' | '\n')) {
                self.eat_line_ending();
                continue;
            }
            break;
        }

        let line = self.line;
        let mut fields = vec![self.read_field()?];
        while self.peek() == Some(self.delimiter) {
            self.advance();
            fields.push(self.read_field()?);
        }
        self.eat_line_ending();
        Ok(Some(Row { fields, line }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_ok(src: &str) -> CsvTable {
        parse(src).unwrap_or_else(|e| panic!("expected {src:?} to parse, got {e}"))
    }

    fn row_fields(row: &CsvRow) -> Vec<&str> {
        (0..).map_while(|i| row.field_by_index(i).ok()).collect()
    }

    #[test]
    fn parses_headers_and_rows() {
        let t = parse_ok("a,b,c\n1,2,3\n4,5,6\n");
        assert_eq!(t.headers(), &["a", "b", "c"]);
        assert_eq!(t.rows().len(), 2);
        assert_eq!(row_fields(&t.rows()[0]), ["1", "2", "3"]);
        assert_eq!(row_fields(&t.rows()[1]), ["4", "5", "6"]);
    }

    #[test]
    fn rows_are_indexable_by_column_name_and_position() {
        let t = parse_ok("name,hp\nslime,10\n");
        let row = &t.rows()[0];
        assert_eq!(row.field_by_name("name").unwrap(), "slime");
        assert_eq!(row.field_by_name("hp").unwrap(), "10");
        assert_eq!(row.field_by_index(0).unwrap(), "slime");
        assert_eq!(row.field_by_index(1).unwrap(), "10");
    }

    #[test]
    fn an_unknown_column_name_is_an_error() {
        let t = parse_ok("a\n1\n");
        let err = t.rows()[0].field_by_name("nope").unwrap_err();
        assert!(err.contains("nope"), "{err}");
    }

    #[test]
    fn an_out_of_range_index_is_an_error() {
        let t = parse_ok("a\n1\n");
        let err = t.rows()[0].field_by_index(5).unwrap_err();
        assert!(err.contains('5'), "{err}");
        let err = t.rows()[0].field_by_index(-1).unwrap_err();
        assert!(err.contains("-1"), "{err}");
    }

    #[test]
    fn quoted_fields_may_embed_delimiters_and_newlines() {
        let t = parse_ok("a,b\n\"x,y\",\"line1\nline2\"\n");
        let row = &t.rows()[0];
        assert_eq!(row.field_by_index(0).unwrap(), "x,y");
        assert_eq!(row.field_by_index(1).unwrap(), "line1\nline2");
    }

    #[test]
    fn doubled_quotes_decode_to_one_literal_quote() {
        let t = parse_ok("a\n\"she said \"\"hi\"\"\"\n");
        assert_eq!(t.rows()[0].field_by_index(0).unwrap(), "she said \"hi\"");
    }

    #[test]
    fn an_unterminated_quoted_field_is_an_error() {
        let err = parse("a\n\"unterminated").unwrap_err();
        assert!(err.message.contains("unterminated"), "{err}");
    }

    #[test]
    fn blank_lines_are_skipped_not_turned_into_rows() {
        let t = parse_ok("a,b\n1,2\n\n\n3,4\n");
        assert_eq!(t.rows().len(), 2);
        assert_eq!(row_fields(&t.rows()[1]), ["3", "4"]);
    }

    #[test]
    fn a_stray_delimiter_line_is_a_row_of_empty_fields_not_blank() {
        let t = parse_ok("a,b\n,\n1,2\n");
        assert_eq!(t.rows().len(), 2);
        assert_eq!(row_fields(&t.rows()[0]), ["", ""]);
    }

    #[test]
    fn crlf_and_lf_line_endings_parse_identically() {
        let crlf = parse_ok("a,b\r\n1,2\r\n");
        let lf = parse_ok("a,b\n1,2\n");
        assert_eq!(row_fields(&crlf.rows()[0]), row_fields(&lf.rows()[0]));
    }

    #[test]
    fn a_trailing_newline_does_not_add_a_phantom_row() {
        let with = parse_ok("a\n1\n");
        let without = parse_ok("a\n1");
        assert_eq!(with.rows().len(), 1);
        assert_eq!(without.rows().len(), 1);
    }

    #[test]
    fn a_short_row_names_the_missing_header() {
        let err = parse("a,b,c\n1,2\n").unwrap_err();
        assert!(err.message.contains('"'), "{err}");
        assert!(err.message.contains('c'), "{err}");
        assert_eq!(err.line, 2, "{err}");
    }

    #[test]
    fn a_long_row_names_the_unheaded_field() {
        let err = parse("a\n1,2\n").unwrap_err();
        assert_eq!(err.line, 2, "{err}");
        assert!(err.message.contains('2'), "{err}");
    }

    #[test]
    fn a_tab_delimiter_is_honored() {
        let t = parse_with_delimiter("a\tb\n1\t2\n", '\t').unwrap();
        assert_eq!(t.headers(), &["a", "b"]);
        assert_eq!(row_fields(&t.rows()[0]), ["1", "2"]);
    }

    #[test]
    fn an_empty_document_has_no_headers_or_rows() {
        let t = parse_ok("");
        assert!(t.headers().is_empty());
        assert!(t.rows().is_empty());
    }

    #[test]
    fn a_byte_order_mark_is_stripped() {
        let t = parse_ok("\u{feff}a,b\n1,2\n");
        assert_eq!(t.headers(), &["a", "b"]);
    }

    #[test]
    fn fields_are_not_trimmed() {
        let t = parse_ok("a, b\n1, 2\n");
        assert_eq!(t.headers(), &["a", " b"]);
        assert_eq!(t.rows()[0].field_by_index(1).unwrap(), " 2");
    }

    #[test]
    fn duplicate_headers_resolve_by_name_to_the_first_match() {
        let t = parse_ok("a,a\n1,2\n");
        assert_eq!(t.rows()[0].field_by_name("a").unwrap(), "1");
    }

    #[test]
    fn cloning_a_table_or_row_is_cheap_and_shares_the_same_data() {
        let t = parse_ok("a\n1\n");
        let t2 = t.clone();
        assert!(Arc::ptr_eq(&t.0, &t2.0));
        let row = t.rows()[0].clone();
        let row2 = row.clone();
        assert!(Arc::ptr_eq(&row.0, &row2.0));
    }

    #[test]
    fn iterating_a_table_visits_every_row_in_order() {
        let t = parse_ok("a\n1\n2\n3\n");
        let all: Vec<String> = t
            .into_iter()
            .map(|r| r.field_by_index(0).unwrap().to_string())
            .collect();
        assert_eq!(all, ["1", "2", "3"]);
    }
}
