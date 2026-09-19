//! A deliberately small XML tree for the OOXML parts that this crate edits.
//!
//! This is not a general XML object model.  It keeps element and attribute
//! names verbatim (including namespace prefixes), rejects DTDs/entities that
//! could change interpretation, and serializes a normalized, safe document.

use quick_xml::{Reader, events::Event};
use std::collections::{BTreeMap, BTreeSet};

const MAX_DEPTH: usize = 128;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Element {
    pub name: String,
    pub attrs: BTreeMap<String, String>,
    pub children: Vec<Node>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Node {
    Element(Element),
    Text(String),
    /// XML declaration, processing instruction, or comment.  These are not
    /// interpreted by the mutation code, but retaining them prevents a patch
    /// from silently dropping workbook metadata.
    Raw(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Document {
    pub nodes: Vec<Node>,
}

impl Element {
    pub fn new(name: &str) -> Self {
        Self {
            name: name.to_owned(),
            attrs: BTreeMap::new(),
            children: Vec::new(),
        }
    }

    pub fn local_name(&self) -> &str {
        self.name
            .rsplit_once(':')
            .map_or(&self.name, |(_, name)| name)
    }

    pub fn child(&self, name: &str) -> Option<&Element> {
        self.elements()
            .find(|child| self.matches_child(child, name))
    }

    pub fn child_mut(&mut self, name: &str) -> Option<&mut Element> {
        let parent_name = self.name.clone();
        self.elements_mut()
            .find(|child| matches_child_name(&parent_name, child, name))
    }

    pub fn elements(&self) -> impl Iterator<Item = &Element> {
        self.children.iter().filter_map(|node| match node {
            Node::Element(element) => Some(element),
            _ => None,
        })
    }

    pub fn elements_mut(&mut self) -> impl Iterator<Item = &mut Element> {
        self.children.iter_mut().filter_map(|node| match node {
            Node::Element(element) => Some(element),
            _ => None,
        })
    }

    /// Concatenates textual descendants. Raw nodes intentionally do not
    /// contribute: comments and processing instructions are never cell text.
    pub fn text(&self) -> String {
        fn collect(nodes: &[Node], out: &mut String) {
            for node in nodes {
                match node {
                    Node::Text(text) => out.push_str(text),
                    Node::Element(element) => collect(&element.children, out),
                    Node::Raw(_) => {}
                }
            }
        }
        let mut text = String::new();
        collect(&self.children, &mut text);
        text
    }

    fn matches_child(&self, child: &Element, wanted: &str) -> bool {
        matches_child_name(&self.name, child, wanted)
    }
}

impl Document {
    pub fn root(&self) -> Result<&Element, String> {
        let mut roots = self.nodes.iter().filter_map(|node| match node {
            Node::Element(element) => Some(element),
            _ => None,
        });
        let root = roots
            .next()
            .ok_or_else(|| "XML document has no root element".to_owned())?;
        if roots.next().is_some() {
            return Err("XML document has multiple root elements".to_owned());
        }
        Ok(root)
    }

    pub fn root_mut(&mut self) -> Result<&mut Element, String> {
        let root_count = self
            .nodes
            .iter()
            .filter(|node| matches!(node, Node::Element(_)))
            .count();
        if root_count != 1 {
            return Err(if root_count == 0 {
                "XML document has no root element".to_owned()
            } else {
                "XML document has multiple root elements".to_owned()
            });
        }
        self.nodes
            .iter_mut()
            .find_map(|node| match node {
                Node::Element(element) => Some(element),
                _ => None,
            })
            .ok_or_else(|| "XML document has no root element".to_owned())
    }

    pub fn serialize(&self) -> String {
        let mut output = String::new();
        for node in &self.nodes {
            // Entity references are forbidden outside the document element.
            // Root-level Text has already been validated as whitespace.
            if let Node::Text(text) = node {
                output.push_str(text);
            } else {
                write_node(node, &mut output);
            }
        }
        output
    }
}

pub fn parse(source: &str) -> Result<Document, String> {
    // quick-xml accepts an encoding declaration and may subsequently decode
    // bytes through an encoding feature.  OOXML parts handled here are UTF-8;
    // reject declarations that would make a String input ambiguous.
    reject_non_utf8_declaration(source)?;

    let mut reader = Reader::from_str(source);
    reader.config_mut().trim_text(false);
    let mut document = Document { nodes: Vec::new() };
    let mut stack: Vec<Element> = Vec::new();
    let mut element_seen = false;

    loop {
        match reader.read_event() {
            Ok(Event::Start(start)) => {
                if stack.len() >= MAX_DEPTH {
                    return Err(format!("XML nesting exceeds {MAX_DEPTH}"));
                }
                if stack.is_empty() && element_seen {
                    return Err("XML document has multiple root elements".to_owned());
                }
                let element = element_from_start(&start, reader.decoder())?;
                element_seen = true;
                stack.push(element);
            }
            Ok(Event::Empty(start)) => {
                if stack.len() >= MAX_DEPTH {
                    return Err(format!("XML nesting exceeds {MAX_DEPTH}"));
                }
                if stack.is_empty() && element_seen {
                    return Err("XML document has multiple root elements".to_owned());
                }
                let element = element_from_start(&start, reader.decoder())?;
                element_seen = true;
                push_node(&mut document.nodes, &mut stack, Node::Element(element));
            }
            Ok(Event::End(end)) => {
                let element = stack
                    .pop()
                    .ok_or_else(|| "XML end tag without a start tag".to_owned())?;
                if end.name().as_ref() != element.name.as_bytes() {
                    return Err("XML end tag does not match start tag".to_owned());
                }
                push_node(&mut document.nodes, &mut stack, Node::Element(element));
            }
            Ok(Event::Text(text)) => {
                let value = text
                    .xml10_content()
                    .map_err(|error| format!("invalid XML text encoding: {error}"))?;
                let value = quick_xml::escape::unescape(&value)
                    .map_err(|error| format!("invalid XML text reference: {error}"))?;
                validate_xml_characters(&value)?;
                if stack.is_empty() && !value.trim().is_empty() {
                    return Err("non-whitespace text outside XML root element".to_owned());
                }
                push_node(
                    &mut document.nodes,
                    &mut stack,
                    Node::Text(value.into_owned()),
                );
            }
            Ok(Event::CData(cdata)) => {
                let value = cdata
                    .xml10_content()
                    .map_err(|error| format!("invalid XML CDATA encoding: {error}"))?;
                validate_xml_characters(&value)?;
                if stack.is_empty() && !value.trim().is_empty() {
                    return Err("CDATA outside XML root element".to_owned());
                }
                push_node(
                    &mut document.nodes,
                    &mut stack,
                    Node::Text(value.into_owned()),
                );
            }
            Ok(Event::Comment(comment)) => {
                let value = comment
                    .decode()
                    .map_err(|error| format!("invalid XML comment encoding: {error}"))?;
                push_node(
                    &mut document.nodes,
                    &mut stack,
                    Node::Raw(format!("<!--{}-->", value)),
                );
            }
            Ok(Event::PI(pi)) => {
                let value = std::str::from_utf8(pi.as_ref()).map_err(|error| {
                    format!("invalid XML processing instruction encoding: {error}")
                })?;
                push_node(
                    &mut document.nodes,
                    &mut stack,
                    Node::Raw(format!("<?{}?>", value)),
                );
            }
            Ok(Event::Decl(declaration)) => {
                if element_seen || !stack.is_empty() {
                    return Err("XML declaration must appear before the root element".to_owned());
                }
                let value = std::str::from_utf8(declaration.as_ref())
                    .map_err(|error| format!("invalid XML declaration encoding: {error}"))?;
                document.nodes.push(Node::Raw(format!("<?{}?>", value)));
            }
            Ok(Event::DocType(_)) => {
                return Err("DOCTYPE is not supported in OOXML parts".to_owned());
            }
            Ok(Event::GeneralRef(reference)) => {
                if stack.is_empty() {
                    return Err("entity reference outside XML root element".into());
                }
                let name = reference.decode().map_err(|error| error.to_string())?;
                let value = match name.as_ref() {
                    "lt" => '<',
                    "gt" => '>',
                    "amp" => '&',
                    "apos" => '\'',
                    "quot" => '"',
                    _ => reference
                        .resolve_char_ref()
                        .map_err(|error| format!("invalid XML character reference: {error}"))?
                        .ok_or_else(|| format!("unresolved XML entity reference: {name}"))?,
                };
                if !legal_xml_char(value) {
                    return Err(format!("illegal XML character reference: {name}"));
                }
                push_node(
                    &mut document.nodes,
                    &mut stack,
                    Node::Text(value.to_string()),
                );
            }
            Ok(Event::Eof) => break,
            Err(error) => return Err(format!("invalid XML: {error}")),
        }
    }

    if !stack.is_empty() {
        return Err("XML document ended before all elements closed".to_owned());
    }
    document.root()?;
    Ok(document)
}

fn element_from_start(
    start: &quick_xml::events::BytesStart<'_>,
    decoder: quick_xml::encoding::Decoder,
) -> Result<Element, String> {
    let name = start.name().as_ref().to_vec();
    let name = std::str::from_utf8(&name)
        .map_err(|error| format!("invalid XML element name: {error}"))?
        .to_owned();
    let mut attrs = BTreeMap::new();
    let mut seen = BTreeSet::new();
    for attribute in start.attributes().with_checks(true) {
        let attribute = attribute.map_err(|error| format!("invalid XML attribute: {error}"))?;
        let key = std::str::from_utf8(attribute.key.as_ref())
            .map_err(|error| format!("invalid XML attribute name: {error}"))?
            .to_owned();
        if !seen.insert(key.clone()) {
            return Err(format!("duplicate XML attribute: {key}"));
        }
        // XML attribute normalization applies to literal whitespace before
        // entity expansion; an explicit &#13; must remain a carriage return.
        let raw = decoder
            .decode(attribute.value.as_ref())
            .map_err(|e| e.to_string())?;
        let normalized = raw.replace("\r\n", "\n").replace(['\r', '\n', '\t'], " ");
        let value = quick_xml::escape::unescape(&normalized)
            .map_err(|e| e.to_string())?
            .into_owned();
        validate_xml_characters(&value)?;
        attrs.insert(key, value);
    }
    Ok(Element {
        name,
        attrs,
        children: Vec::new(),
    })
}

fn push_node(document: &mut Vec<Node>, stack: &mut [Element], node: Node) {
    if let Some(parent) = stack.last_mut() {
        push_or_merge_text(&mut parent.children, node);
    } else {
        push_or_merge_text(document, node);
    }
}

fn push_or_merge_text(nodes: &mut Vec<Node>, node: Node) {
    if let Node::Text(text) = node {
        if let Some(Node::Text(previous)) = nodes.last_mut() {
            previous.push_str(&text);
        } else {
            nodes.push(Node::Text(text));
        }
    } else {
        nodes.push(node);
    }
}

fn validate_xml_characters(value: &str) -> Result<(), String> {
    if value.chars().all(legal_xml_char) {
        Ok(())
    } else {
        Err("illegal XML 1.0 character".to_owned())
    }
}

fn legal_xml_char(character: char) -> bool {
    matches!(character as u32, 0x9 | 0xA | 0xD | 0x20..=0xD7FF | 0xE000..=0xFFFD | 0x10000..=0x10FFFF)
}

fn matches_child_name(parent_name: &str, child: &Element, wanted: &str) -> bool {
    let (wanted_prefix, wanted_local) = split_name(wanted);
    let (parent_prefix, _) = split_name(parent_name);
    let (child_prefix, child_local) = split_name(&child.name);
    child_local == wanted_local
        && child_prefix == parent_prefix
        && (!wanted.contains(':') || wanted_prefix == child_prefix)
}

fn split_name(name: &str) -> (&str, &str) {
    name.rsplit_once(':')
        .map_or(("", name), |(prefix, local)| (prefix, local))
}

fn reject_non_utf8_declaration(source: &str) -> Result<(), String> {
    let prefix = source.trim_start_matches(char::is_whitespace);
    if !prefix.starts_with("<?xml") {
        return Ok(());
    }
    let end = prefix
        .find("?>")
        .ok_or_else(|| "unterminated XML declaration".to_owned())?;
    let declaration = &prefix[..end + 2];
    let lower = declaration.to_ascii_lowercase();
    if let Some(position) = lower.find("encoding") {
        let rest = declaration[position + "encoding".len()..].trim_start();
        let Some(rest) = rest.strip_prefix('=') else {
            return Err("malformed XML encoding declaration".to_owned());
        };
        let rest = rest.trim_start();
        let quote = rest
            .chars()
            .next()
            .filter(|character| *character == '\'' || *character == '"')
            .ok_or_else(|| "malformed XML encoding declaration".to_owned())?;
        let end_quote = rest[1..]
            .find(quote)
            .ok_or_else(|| "unterminated XML encoding declaration".to_owned())?;
        let encoding = &rest[1..end_quote + 1];
        if !encoding.eq_ignore_ascii_case("utf-8") && !encoding.eq_ignore_ascii_case("utf8") {
            return Err(format!("unsupported XML encoding declaration: {encoding}"));
        }
    }
    Ok(())
}

fn write_node(node: &Node, output: &mut String) {
    match node {
        Node::Text(text) => escape_text(text, output),
        Node::Raw(raw) => output.push_str(raw),
        Node::Element(element) => {
            output.push('<');
            output.push_str(&element.name);
            for (key, value) in &element.attrs {
                output.push(' ');
                output.push_str(key);
                output.push_str("=\"");
                escape_attribute(value, output);
                output.push('"');
            }
            if element.children.is_empty() {
                output.push_str("/>");
                return;
            }
            output.push('>');
            for child in &element.children {
                write_node(child, output);
            }
            output.push_str("</");
            output.push_str(&element.name);
            output.push('>');
        }
    }
}

fn escape_text(value: &str, output: &mut String) {
    for character in value.chars() {
        match character {
            '&' => output.push_str("&amp;"),
            '<' => output.push_str("&lt;"),
            '>' => output.push_str("&gt;"),
            '\r' => output.push_str("&#13;"),
            _ => output.push(character),
        }
    }
}

fn escape_attribute(value: &str, output: &mut String) {
    for character in value.chars() {
        match character {
            '&' => output.push_str("&amp;"),
            '<' => output.push_str("&lt;"),
            '"' => output.push_str("&quot;"),
            '\'' => output.push_str("&apos;"),
            '\r' => output.push_str("&#13;"),
            '\n' => output.push_str("&#10;"),
            '\t' => output.push_str("&#9;"),
            _ => output.push(character),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_namespace_attributes_and_references() {
        let document = parse("<?xml version=\"1.0\" encoding=\"UTF-8\"?><x:root xmlns:x=\"urn:x\" value=\"a&amp;b\"><x:child>1 &lt; 2 &amp; 3</x:child><!--note--></x:root>").unwrap();
        let root = document.root().unwrap();
        assert_eq!(root.attrs["value"], "a&b");
        assert_eq!(root.child("x:child").unwrap().text(), "1 < 2 & 3");
        let reparsed = parse(&document.serialize()).unwrap();
        assert_eq!(reparsed, document);
    }

    #[test]
    fn declaration_whitespace_and_line_endings_remain_legal() {
        let doc = parse(
            "<?xml version=\"1.0\"?>\r\n<a value=\"x\r\ny&#13;z\">one\r\ntwo&#13;three</a>\r\n",
        )
        .unwrap();
        let output = doc.serialize();
        assert!(output.starts_with("<?xml version=\"1.0\"?>\n<a"));
        assert_eq!(doc.root().unwrap().text(), "one\ntwo\rthree");
        assert_eq!(doc.root().unwrap().attrs["value"], "x y\rz");
        assert_eq!(parse(&output).unwrap(), doc);
        assert!(parse("<?xml version=\"1.0\"?>&#13;<a/>").is_err());
    }

    #[test]
    fn rejects_unsafe_or_ambiguous_constructs() {
        for source in [
            "<!DOCTYPE a [<!ENTITY x 'y'>]><a>&x;</a>",
            "<a x=\"1\" x=\"2\"/>",
            "<a/><b/>",
            "<a><b></a></b>",
            "<?xml version=\"1.0\" encoding=\"UTF-16\"?><a/>",
        ] {
            assert!(parse(source).is_err(), "{source}");
        }
    }

    #[test]
    fn child_requires_the_parent_prefix() {
        let document = parse("<x:a><x:b/><y:b/></x:a>").unwrap();
        let root = document.root().unwrap();
        assert_eq!(root.child("b").unwrap().name, "x:b");
        assert!(root.elements().any(|element| element.name == "y:b"));
    }

    #[test]
    fn resolves_references_and_merges_cdata_into_text() {
        let document = parse("<a>before&#13;<![CDATA[ middle ]]>&#x41;&amp;after</a>").unwrap();
        let root = document.root().unwrap();
        assert_eq!(root.text(), "before\r middle A&after");
        assert_eq!(root.children.len(), 1, "adjacent text must be stable");
        assert!(
            document
                .serialize()
                .contains("before&#13; middle A&amp;after")
        );
        assert_eq!(parse(&document.serialize()).unwrap(), document);
    }

    #[test]
    fn rejects_unknown_and_illegal_character_references() {
        for source in ["<a>&notDefined;</a>", "<a>&#1;</a>", "<a>\u{0001}</a>"] {
            assert!(parse(source).is_err(), "{source:?}");
        }
    }
}
