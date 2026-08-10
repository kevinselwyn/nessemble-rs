//! A small, dependency-free XML parser for pseudo-op scripts.
//!
//! Scope is deliberately narrow — elements, attributes, text, and entities, per
//! `plans/013-structured-data-parsing.md` §2.2: no namespaces, no `XPath`, no
//! schema validation, and **no DTD processing**. A `<!DOCTYPE` is a hard parse
//! error rather than something this parser could be talked into expanding, since
//! an assembler that resolves external entities on a script's behalf has an XXE
//! problem no asset pipeline needs.
//!
//! This is Rust doing the byte-level work so a Rhai script only ever walks an
//! already-parsed tree — see the crate's top-level doc comment and the timing
//! table in the plan this module implements.

use std::fmt;
use std::sync::Arc;

/// A parsed XML element, as scripts see it (via `xml_node` in the engine).
///
/// `Arc`-backed so cloning a handle — which Rhai does for every function
/// argument that is not the method receiver — is a refcount bump, matching
/// [`crate::Image`]'s existing guarantee.
#[derive(Clone, Debug)]
pub struct XmlNode(pub(crate) Arc<XmlNodeData>);

#[derive(Debug)]
pub(crate) struct XmlNodeData {
    pub(crate) name: String,
    /// Source order. `.attrs` (the Rhai-visible map) sorts these by key on the
    /// way out, since `rhai::Map` is a `BTreeMap` and cannot preserve insertion
    /// order — see plan §2.5. `.attr(name)` (a linear scan here) is unaffected.
    pub(crate) attrs: Vec<(String, String)>,
    /// Child **elements** only; text is collected separately into `text`.
    pub(crate) children: Vec<XmlNode>,
    /// Concatenated direct text content (entities decoded), or `None` if the
    /// element has no text between its tags at all. Whitespace used purely for
    /// indentation between child elements is *not* filtered out — the host
    /// stays mechanical, and a script that cares can call `.trimmed()`.
    pub(crate) text: Option<String>,
}

impl XmlNode {
    pub(crate) fn name(&self) -> &str {
        &self.0.name
    }

    pub(crate) fn attr(&self, name: &str) -> Option<&str> {
        self.0
            .attrs
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }

    pub(crate) fn attrs(&self) -> &[(String, String)] {
        &self.0.attrs
    }

    pub(crate) fn children(&self) -> &[XmlNode] {
        &self.0.children
    }

    pub(crate) fn text(&self) -> Option<&str> {
        self.0.text.as_deref()
    }

    pub(crate) fn find(&self, name: &str) -> Option<XmlNode> {
        self.0.children.iter().find(|c| c.name() == name).cloned()
    }

    pub(crate) fn find_all(&self, name: &str) -> Vec<XmlNode> {
        self.0
            .children
            .iter()
            .filter(|c| c.name() == name)
            .cloned()
            .collect()
    }
}

/// A parse failure, with the line and column the parser had reached.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XmlError {
    pub line: u32,
    pub col: u32,
    pub message: String,
}

impl fmt::Display for XmlError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}: {}", self.line, self.col, self.message)
    }
}

/// Parse `src` as a single XML document, returning its root element.
///
/// # Errors
/// Returns an [`XmlError`] naming the line and column of the failure — a syntax
/// error, an unterminated construct, a `<!DOCTYPE` (unsupported by design), or an
/// unresolved entity reference.
pub fn parse(src: &str) -> Result<XmlNode, XmlError> {
    let src = src.strip_prefix('\u{feff}').unwrap_or(src);
    let mut p = Parser::new(src);
    skip_misc(&mut p)?;
    match p.peek() {
        Some('<') => parse_element(&mut p),
        Some(c) => Err(p.error(format!("expected the root element, found {c:?}"))),
        None => Err(p.error("expected the root element, found end of input")),
    }
}

/// A cursor over the remaining source, tracking line/column as it advances.
struct Parser<'a> {
    rest: &'a str,
    line: u32,
    col: u32,
}

impl<'a> Parser<'a> {
    fn new(src: &'a str) -> Self {
        Parser {
            rest: src,
            line: 1,
            col: 1,
        }
    }

    fn peek(&self) -> Option<char> {
        self.rest.chars().next()
    }

    fn starts_with(&self, s: &str) -> bool {
        self.rest.starts_with(s)
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

    /// Advance past a literal we already confirmed with `starts_with`.
    fn consume(&mut self, literal: &str) {
        for _ in literal.chars() {
            self.advance();
        }
    }

    fn skip_ws(&mut self) {
        while matches!(self.peek(), Some(c) if c.is_whitespace()) {
            self.advance();
        }
    }

    fn error(&self, message: impl Into<String>) -> XmlError {
        XmlError {
            line: self.line,
            col: self.col,
            message: message.into(),
        }
    }
}

/// Skip whitespace, comments, and processing instructions; error on `<!DOCTYPE`.
fn skip_misc(p: &mut Parser) -> Result<(), XmlError> {
    loop {
        p.skip_ws();
        if p.starts_with("<!--") {
            skip_delimited(p, "<!--", "-->")?;
        } else if p.starts_with("<?") {
            skip_delimited(p, "<?", "?>")?;
        } else if starts_with_ci(p, "<!DOCTYPE") {
            return Err(p.error(
                "<!DOCTYPE is not supported: no DTD processing, no external entities \
                 (see plans/013-structured-data-parsing.md \u{a7}2.2)",
            ));
        } else {
            return Ok(());
        }
    }
}

/// Case-insensitive prefix check, for `<!DOCTYPE`/`<!doctype` alike. `str::get`
/// (rather than slicing) avoids a panic when `literal.len()` does not land on a
/// UTF-8 character boundary in `p.rest`.
fn starts_with_ci(p: &Parser, literal: &str) -> bool {
    p.rest
        .get(..literal.len())
        .is_some_and(|s| s.eq_ignore_ascii_case(literal))
}

/// Consume `open` (already confirmed present) through the matching `close`,
/// discarding the content, e.g. a comment or a processing instruction.
fn skip_delimited(p: &mut Parser, open: &str, close: &str) -> Result<(), XmlError> {
    read_delimited(p, open, close).map(|_| ())
}

/// Consume `open` (already confirmed present) through the matching `close`,
/// returning everything between them verbatim (no entity decoding).
fn read_delimited(p: &mut Parser, open: &str, close: &str) -> Result<String, XmlError> {
    let start = p.error(format!("unterminated {open} ... {close}"));
    p.consume(open);
    let mut out = String::new();
    loop {
        if p.starts_with(close) {
            p.consume(close);
            return Ok(out);
        }
        match p.advance() {
            Some(c) => out.push(c),
            None => return Err(start),
        }
    }
}

fn is_name_start(c: char) -> bool {
    c.is_alphabetic() || c == '_' || c == ':'
}

fn is_name_char(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '_' | ':' | '-' | '.')
}

fn parse_name(p: &mut Parser) -> Result<String, XmlError> {
    let mut name = String::new();
    match p.peek() {
        Some(c) if is_name_start(c) => {
            name.push(c);
            p.advance();
        }
        Some(c) => return Err(p.error(format!("expected a name, found {c:?}"))),
        None => return Err(p.error("expected a name, found end of input")),
    }
    while let Some(c) = p.peek() {
        if is_name_char(c) {
            name.push(c);
            p.advance();
        } else {
            break;
        }
    }
    Ok(name)
}

/// Read one entity reference starting at `&`, returning the character it
/// decodes to. Only the five predefined entities and numeric character
/// references (`&#10;`, `&#x41;`) are recognized — there is no DTD to define
/// anything else.
fn read_entity(p: &mut Parser) -> Result<char, XmlError> {
    let at = p.error("");
    p.advance(); // '&'
    let mut name = String::new();
    loop {
        match p.peek() {
            Some(';') => {
                p.advance();
                break;
            }
            Some(c) if name.len() < 32 => {
                name.push(c);
                p.advance();
            }
            _ => {
                return Err(XmlError {
                    message: format!("unterminated entity reference '&{name}'"),
                    ..at
                })
            }
        }
    }
    decode_entity_name(&name).ok_or(XmlError {
        message: format!("unknown entity '&{name};'"),
        ..at
    })
}

fn decode_entity_name(name: &str) -> Option<char> {
    match name {
        "amp" => return Some('&'),
        "lt" => return Some('<'),
        "gt" => return Some('>'),
        "quot" => return Some('"'),
        "apos" => return Some('\''),
        _ => {}
    }
    let digits = name
        .strip_prefix("#x")
        .or_else(|| name.strip_prefix("#X"))
        .map(|d| (d, 16))
        .or_else(|| name.strip_prefix('#').map(|d| (d, 10)))?;
    let code = u32::from_str_radix(digits.0, digits.1).ok()?;
    char::from_u32(code)
}

/// Read text content up to (not including) the next `<`, decoding entities.
fn read_text(p: &mut Parser) -> Result<String, XmlError> {
    let mut out = String::new();
    loop {
        match p.peek() {
            None | Some('<') => return Ok(out),
            Some('&') => out.push(read_entity(p)?),
            Some(c) => {
                out.push(c);
                p.advance();
            }
        }
    }
}

/// Read a quoted attribute value (the opening quote is the current character),
/// decoding entities. A literal `<` inside an attribute value is rejected, as
/// in real XML.
fn read_attr_value(p: &mut Parser, attr_name: &str) -> Result<String, XmlError> {
    let quote = p.peek().expect("caller checked a quote is present");
    let start = p.error(format!("unterminated value for attribute '{attr_name}'"));
    p.advance();
    let mut out = String::new();
    loop {
        match p.peek() {
            Some(c) if c == quote => {
                p.advance();
                return Ok(out);
            }
            None => return Err(start),
            Some('&') => out.push(read_entity(p)?),
            Some('<') => {
                return Err(p.error(format!(
                    "'<' is not allowed in the value of attribute '{attr_name}'"
                )))
            }
            Some(c) => {
                out.push(c);
                p.advance();
            }
        }
    }
}

fn parse_attr(p: &mut Parser) -> Result<(String, String), XmlError> {
    let name = parse_name(p)?;
    p.skip_ws();
    if p.peek() != Some('=') {
        return Err(p.error(format!("expected '=' after attribute name '{name}'")));
    }
    p.advance();
    p.skip_ws();
    match p.peek() {
        Some('"' | '\'') => {}
        Some(c) => {
            return Err(p.error(format!(
                "expected a quoted value for attribute '{name}', found {c:?}"
            )))
        }
        None => {
            return Err(p.error(format!(
                "expected a quoted value for attribute '{name}', found end of input"
            )))
        }
    }
    let value = read_attr_value(p, &name)?;
    Ok((name, value))
}

/// Parse one element, starting at its opening `<` (already confirmed present).
fn parse_element(p: &mut Parser) -> Result<XmlNode, XmlError> {
    p.advance(); // '<'
    let name = parse_name(p)?;
    let mut attrs = Vec::new();
    loop {
        p.skip_ws();
        match p.peek() {
            Some('/') => {
                p.advance();
                if p.peek() != Some('>') {
                    return Err(p.error("expected '>' after '/'"));
                }
                p.advance();
                return Ok(XmlNode(Arc::new(XmlNodeData {
                    name,
                    attrs,
                    children: Vec::new(),
                    text: None,
                })));
            }
            Some('>') => {
                p.advance();
                break;
            }
            Some(c) if is_name_start(c) => attrs.push(parse_attr(p)?),
            Some(c) => {
                return Err(p.error(format!(
                    "unexpected {c:?} in start tag <{name}>, expected an attribute, '/', or '>'"
                )))
            }
            None => return Err(p.error(format!("unexpected end of input in start tag <{name}>"))),
        }
    }

    let mut children = Vec::new();
    let mut text = String::new();
    loop {
        if p.starts_with("</") {
            let close_start = p.error("");
            p.consume("</");
            let end_name = parse_name(p)?;
            p.skip_ws();
            if p.peek() != Some('>') {
                return Err(p.error(format!("expected '>' closing </{end_name}")));
            }
            p.advance();
            if end_name != name {
                return Err(XmlError {
                    message: format!(
                        "mismatched closing tag: expected </{name}>, found </{end_name}>"
                    ),
                    ..close_start
                });
            }
            break;
        } else if p.starts_with("<!--") {
            skip_delimited(p, "<!--", "-->")?;
        } else if p.starts_with("<![CDATA[") {
            // CDATA content is literal: no entity decoding inside it.
            text.push_str(&read_delimited(p, "<![CDATA[", "]]>")?);
        } else if p.starts_with("<?") {
            skip_delimited(p, "<?", "?>")?;
        } else if starts_with_ci(p, "<!DOCTYPE") {
            return Err(
                p.error("<!DOCTYPE is not supported: no DTD processing, no external entities")
            );
        } else if p.peek() == Some('<') {
            children.push(parse_element(p)?);
        } else if p.peek().is_none() {
            return Err(p.error(format!("unexpected end of input inside <{name}>")));
        } else {
            text.push_str(&read_text(p)?);
        }
    }

    Ok(XmlNode(Arc::new(XmlNodeData {
        name,
        attrs,
        children,
        text: (!text.is_empty()).then_some(text),
    })))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_ok(src: &str) -> XmlNode {
        parse(src).unwrap_or_else(|e| panic!("expected {src:?} to parse, got {e}"))
    }

    #[test]
    fn parses_a_self_closing_element() {
        let root = parse_ok(r#"<row id="1"/>"#);
        assert_eq!(root.name(), "row");
        assert_eq!(root.attr("id"), Some("1"));
        assert!(root.children().is_empty());
        assert_eq!(root.text(), None);
    }

    #[test]
    fn parses_nested_elements_and_text() {
        let root = parse_ok("<map><row>a,b,c</row><row>d,e,f</row></map>");
        assert_eq!(root.name(), "map");
        let rows = root.find_all("row");
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].text(), Some("a,b,c"));
        assert_eq!(rows[1].text(), Some("d,e,f"));
        assert_eq!(root.find("row").unwrap().text(), Some("a,b,c"));
        assert!(root.find("missing").is_none());
    }

    #[test]
    fn attrs_are_readable_by_name_regardless_of_declaration_order() {
        let root = parse_ok(r#"<t z="1" a="2" m="3"/>"#);
        assert_eq!(
            root.attrs(),
            &[
                ("z".to_string(), "1".to_string()),
                ("a".to_string(), "2".to_string()),
                ("m".to_string(), "3".to_string()),
            ]
        );
        assert_eq!(root.attr("a"), Some("2"));
        assert_eq!(root.attr("missing"), None);
    }

    #[test]
    fn decodes_predefined_and_numeric_entities() {
        let root = parse_ok("<t a=\"&amp;&lt;&gt;&quot;&apos;\">&#65;&#x42;</t>");
        assert_eq!(root.attr("a"), Some("&<>\"'"));
        assert_eq!(root.text(), Some("AB"));
    }

    #[test]
    fn an_unknown_entity_is_an_error_naming_its_position() {
        let err = parse("<t>&nope;</t>").unwrap_err();
        assert!(err.message.contains("&nope;"), "{err}");
        assert_eq!((err.line, err.col), (1, 4));
    }

    #[test]
    fn cdata_is_literal_and_not_entity_decoded() {
        let root = parse_ok("<t><![CDATA[a & b < c]]></t>");
        assert_eq!(root.text(), Some("a & b < c"));
    }

    #[test]
    fn comments_and_processing_instructions_are_skipped() {
        let root = parse_ok("<?xml version=\"1.0\"?>\n<!-- a comment --><t><!-- inner --><a/></t>");
        assert_eq!(root.name(), "t");
        assert_eq!(root.children().len(), 1);
    }

    #[test]
    fn doctype_is_rejected_outright() {
        let err = parse("<!DOCTYPE foo><t/>").unwrap_err();
        assert!(err.message.contains("DOCTYPE"), "{err}");

        // Also inside content, not only before the root.
        let err = parse("<t><!DOCTYPE foo></t>").unwrap_err();
        assert!(err.message.contains("DOCTYPE"), "{err}");
    }

    #[test]
    fn mismatched_closing_tags_are_rejected() {
        let err = parse("<a><b></c></a>").unwrap_err();
        assert!(
            err.message.contains("</b>") && err.message.contains("</c>"),
            "{err}"
        );
    }

    #[test]
    fn errors_report_line_and_column() {
        let err = parse("<a>\n  <b\n").unwrap_err();
        assert_eq!(err.line, 3, "{err}");
    }

    #[test]
    fn a_byte_order_mark_is_stripped() {
        let root = parse_ok("\u{feff}<a/>");
        assert_eq!(root.name(), "a");
    }

    #[test]
    fn whitespace_only_indentation_text_is_still_reported() {
        // The host stays mechanical: pretty-printed whitespace between child
        // elements is not filtered out. A script that only wants meaningful
        // text calls `.trimmed()`.
        let root = parse_ok("<a>\n  <b/>\n</a>");
        assert_eq!(root.text(), Some("\n  \n"));
    }

    #[test]
    fn empty_element_has_no_text() {
        let root = parse_ok("<a></a>");
        assert_eq!(root.text(), None);
    }

    #[test]
    fn element_names_may_contain_colons_with_no_namespace_splitting() {
        let root = parse_ok(r#"<ns:tag xmlns:ns="http://example.com"/>"#);
        assert_eq!(root.name(), "ns:tag");
        assert_eq!(root.attr("xmlns:ns"), Some("http://example.com"));
    }

    #[test]
    fn cloning_a_node_is_cheap_and_shares_the_same_data() {
        let root = parse_ok("<a><b/></a>");
        let child = root.find("b").unwrap();
        let child2 = child.clone();
        assert!(Arc::ptr_eq(&child.0, &child2.0));
    }
}
