//! XMP sidecars (`cat.png.xmp`): reading BooruCraft's fields and updating them in place.
//!
//! Other apps (darktable, digiKam, Lightroom…) keep their own data in the same file. [`update`]
//! parses the whole document into a small tree, removes only the fields BooruCraft manages,
//! adds them back in an `rdf:Description` of their own, and writes every other byte it read
//! back out unchanged.
//!
//! Managed fields: `dc:subject` (all tags), `lr:hierarchicalSubject` (namespaced tags as
//! `artist|foo`), and `boorucraft:Rating`, `boorucraft:Source`, `boorucraft:Hash`.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable))]

use std::collections::BTreeSet;

use boorucraft_core::Rating;
use quick_xml::Reader;
use quick_xml::Writer;
use quick_xml::XmlVersion;
use quick_xml::escape::resolve_predefined_entity;
use quick_xml::events::attributes::Attribute;
use quick_xml::events::{BytesStart, BytesText, Event};
use thiserror::Error;

pub const NS_X: &str = "adobe:ns:meta/";
pub const NS_RDF: &str = "http://www.w3.org/1999/02/22-rdf-syntax-ns#";
pub const NS_DC: &str = "http://purl.org/dc/elements/1.1/";
pub const NS_LR: &str = "http://ns.adobe.com/lightroom/1.0/";
/// BooruCraft's own XMP namespace. It only has to be unique and stable; it is never fetched.
pub const NS_BOORUCRAFT: &str = "urn:boorucraft:xmp:1/";

/// Biggest sidecar we read. Real ones are a few KiB; darktable's edit histories can reach a few
/// hundred.
pub const MAX_SIDECAR_BYTES: usize = 16 * 1024 * 1024;
/// Deepest element nesting we accept.
const MAX_DEPTH: usize = 256;

#[derive(Debug, Error)]
pub enum XmpError {
    #[error("sidecar is larger than {MAX_SIDECAR_BYTES} bytes")]
    TooLarge,
    #[error("sidecar is not UTF-8 text")]
    NotUtf8,
    #[error("sidecar is not valid XML: {0}")]
    Xml(String),
    #[error("sidecar nests elements more than {MAX_DEPTH} deep")]
    TooDeep,
    #[error("sidecar has no rdf:RDF element")]
    NoRdf,
}

/// The fields BooruCraft keeps in a sidecar.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SidecarData {
    /// Full tag names, e.g. `cat`, `artist:foo`.
    pub tags: BTreeSet<String>,
    pub rating: Option<Rating>,
    pub source: Option<String>,
    /// Hex content hash of the file the sidecar belongs to.
    pub hash: Option<String>,
}

impl SidecarData {
    pub fn is_empty(&self) -> bool {
        self.tags.is_empty() && self.rating.is_none() && self.source.is_none()
    }
}

// ---------------------------------------------------------------------------------------------
// A minimal document tree that keeps every event it read.

#[derive(Debug)]
enum Node {
    Element(Element),
    /// Text, comments, declarations, entity references…: written back as read.
    Other(Event<'static>),
}

#[derive(Debug)]
struct Element {
    start: BytesStart<'static>,
    children: Vec<Node>,
    /// Was written as `<a/>`; written the same way again if it still has no children.
    empty: bool,
}

fn xml_err(e: impl std::fmt::Display) -> XmpError {
    XmpError::Xml(e.to_string())
}

fn parse(bytes: &[u8]) -> Result<Vec<Node>, XmpError> {
    if bytes.len() > MAX_SIDECAR_BYTES {
        return Err(XmpError::TooLarge);
    }
    let text = std::str::from_utf8(bytes).map_err(|_| XmpError::NotUtf8)?;
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut reader = Reader::from_str(text);
    reader.config_mut().check_end_names = true;

    let mut top: Vec<Node> = Vec::new();
    let mut stack: Vec<Element> = Vec::new();
    loop {
        let event = reader.read_event().map_err(xml_err)?.into_owned();
        let node = match event {
            Event::Eof => break,
            Event::Start(start) => {
                if stack.len() >= MAX_DEPTH {
                    return Err(XmpError::TooDeep);
                }
                stack.push(Element { start, children: Vec::new(), empty: false });
                continue;
            }
            Event::End(_) => {
                let Some(done) = stack.pop() else { return Err(XmpError::Xml("unexpected end tag".into())) };
                Node::Element(done)
            }
            Event::Empty(start) => Node::Element(Element { start, children: Vec::new(), empty: true }),
            other => Node::Other(other),
        };
        match stack.last_mut() {
            Some(parent) => parent.children.push(node),
            None => top.push(node),
        }
    }
    if !stack.is_empty() {
        return Err(XmpError::Xml("document ends inside an element".into()));
    }
    Ok(top)
}

fn write_nodes(nodes: &[Node], w: &mut Writer<Vec<u8>>) -> Result<(), XmpError> {
    for node in nodes {
        match node {
            Node::Other(e) => w.write_event(e.borrow()).map_err(xml_err)?,
            Node::Element(el) => {
                if el.empty && el.children.is_empty() {
                    w.write_event(Event::Empty(el.start.borrow())).map_err(xml_err)?;
                } else {
                    w.write_event(Event::Start(el.start.borrow())).map_err(xml_err)?;
                    write_nodes(&el.children, w)?;
                    w.write_event(Event::End(el.start.to_end())).map_err(xml_err)?;
                }
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Namespaces: a stack of (prefix, uri) bindings; `""` is the default namespace.

#[derive(Default, Clone)]
struct Scope(Vec<(String, String)>);

impl Scope {
    /// The scope inside `start`: the outer bindings plus the element's own `xmlns` attributes.
    fn enter(&self, start: &BytesStart) -> Scope {
        let mut inner = self.clone();
        for attr in start.attributes().with_checks(false).flatten() {
            let key = attr.key.0;
            let prefix = if key == "xmlns" {
                ""
            } else if let Some(p) = key.strip_prefix("xmlns:") {
                p
            } else {
                continue;
            };
            let uri = attr_value(&attr);
            inner.0.push((prefix.to_string(), uri));
        }
        inner
    }

    fn uri(&self, prefix: &str) -> Option<&str> {
        self.0.iter().rev().find(|(p, _)| p == prefix).map(|(_, u)| u.as_str())
    }

    /// Resolves an element name to `(namespace uri, local name)`.
    fn element(&self, qname: &str) -> (Option<&str>, String) {
        match qname.split_once(':') {
            Some((p, local)) => (self.uri(p), local.to_string()),
            None => (self.uri(""), qname.to_string()),
        }
    }

    /// Resolves an attribute name; unprefixed attributes have no namespace.
    fn attribute(&self, qname: &str) -> (Option<&str>, String) {
        match qname.split_once(':') {
            Some((p, local)) if p != "xmlns" => (self.uri(p), local.to_string()),
            _ => (None, qname.to_string()),
        }
    }
}

/// An attribute's value with references resolved; malformed values read as empty.
fn attr_value(attr: &Attribute) -> String {
    attr.normalized_value(XmlVersion::Implicit1_0).map(|v| v.into_owned()).unwrap_or_default()
}

fn is(resolved: &(Option<&str>, String), ns: &str, local: &str) -> bool {
    resolved.0 == Some(ns) && resolved.1 == local
}

/// A property BooruCraft manages (element or attribute form).
fn is_managed(resolved: &(Option<&str>, String)) -> bool {
    is(resolved, NS_DC, "subject") || is(resolved, NS_LR, "hierarchicalSubject") || resolved.0 == Some(NS_BOORUCRAFT)
}

/// The concatenated text inside a node, with entity and character references resolved.
fn text_of(nodes: &[Node], out: &mut String) {
    for node in nodes {
        match node {
            Node::Element(el) => text_of(&el.children, out),
            Node::Other(Event::Text(t)) => out.push_str(&t.xml10_content()),
            Node::Other(Event::CData(t)) => out.push_str(&t.xml10_content()),
            Node::Other(Event::GeneralRef(r)) => {
                if let Ok(Some(c)) = r.resolve_char_ref() {
                    out.push(c);
                } else if let Some(s) = resolve_predefined_entity(r) {
                    out.push_str(s);
                }
            }
            Node::Other(_) => {}
        }
    }
}

/// Texts of the `rdf:li` items inside a bag/seq property element.
fn list_items(el: &Element, scope: &Scope, out: &mut Vec<String>) {
    for child in &el.children {
        let Node::Element(c) = child else { continue };
        let inner = scope.enter(&c.start);
        if is(&inner.element(c.start.name().0), NS_RDF, "li") {
            let mut s = String::new();
            text_of(&c.children, &mut s);
            let s = s.trim();
            if !s.is_empty() {
                out.push(s.to_string());
            }
        } else {
            list_items(c, &inner, out);
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Reading

#[derive(Default)]
struct Found {
    subjects: Vec<String>,
    hierarchical: Vec<String>,
    rating: Option<String>,
    source: Option<String>,
    hash: Option<String>,
}

fn collect(nodes: &[Node], scope: &Scope, in_description: bool, found: &mut Found) {
    for node in nodes {
        let Node::Element(el) = node else { continue };
        let inner = scope.enter(&el.start);
        let name = inner.element(el.start.name().0);
        let is_description = is(&name, NS_RDF, "Description");
        if is_description {
            for attr in el.start.attributes().with_checks(false).flatten() {
                let key = inner.attribute(attr.key.0);
                if key.0 != Some(NS_BOORUCRAFT) {
                    continue;
                }
                let value = attr_value(&attr).trim().to_string();
                set_simple(found, &key.1, value);
            }
        }
        if in_description && is(&name, NS_DC, "subject") {
            list_items(el, &inner, &mut found.subjects);
        } else if in_description && is(&name, NS_LR, "hierarchicalSubject") {
            list_items(el, &inner, &mut found.hierarchical);
        } else if in_description && name.0 == Some(NS_BOORUCRAFT) {
            let mut s = String::new();
            text_of(&el.children, &mut s);
            set_simple(found, &name.1, s.trim().to_string());
        } else {
            collect(&el.children, &inner, is_description, found);
        }
    }
}

fn set_simple(found: &mut Found, local: &str, value: String) {
    if value.is_empty() {
        return;
    }
    match local {
        "Rating" => found.rating = Some(value),
        "Source" => found.source = Some(value),
        "Hash" => found.hash = Some(value),
        _ => {}
    }
}

/// Keywords in a sidecar, split into BooruCraft tags and keywords that belong to other apps.
#[derive(Debug, Default)]
struct Keywords {
    tags: BTreeSet<String>,
    /// `dc:subject` entries that aren't tags: darktable's own keywords and the parent levels of
    /// keyword trees (Lightroom writes `Places`, `Europe` and `France` for `Places|Europe|France`).
    foreign_subjects: Vec<String>,
    /// `lr:hierarchicalSubject` entries other than BooruCraft's `namespace|name`.
    foreign_hierarchy: Vec<String>,
}

fn classify(found: &Found, is_namespace: &dyn Fn(&str) -> bool) -> Keywords {
    let mut ignored: BTreeSet<&str> = BTreeSet::new();
    let mut namespaced: Vec<(&str, String)> = Vec::new();
    let mut foreign_hierarchy = Vec::new();
    for h in &found.hierarchical {
        let parts: Vec<&str> = h.split('|').map(str::trim).filter(|p| !p.is_empty()).collect();
        match parts.as_slice() {
            [] => {}
            [ns, name] if is_namespace(&ns.to_lowercase()) => namespaced.push((name, format!("{}:{name}", ns.to_lowercase()))),
            [first, ..] => {
                foreign_hierarchy.push(h.clone());
                // darktable's own tree (`darktable|format|jpg`, `darktable|exported`) is never a tag.
                let skip = if first.eq_ignore_ascii_case("darktable") { parts.len() } else { parts.len().saturating_sub(1) };
                ignored.extend(parts.iter().take(skip));
            }
        }
    }
    let mut kw = Keywords { foreign_hierarchy, ..Keywords::default() };
    for s in &found.subjects {
        if ignored.contains(s.as_str()) {
            if !kw.foreign_subjects.contains(s) {
                kw.foreign_subjects.push(s.clone());
            }
        } else {
            kw.tags.insert(s.clone());
        }
    }
    for (leaf, full) in namespaced {
        kw.tags.remove(leaf);
        kw.tags.insert(full);
    }
    kw
}

/// Reads BooruCraft's fields from a sidecar, including tags other apps wrote.
///
/// Tags come from `dc:subject`. A two-level `lr:hierarchicalSubject` entry whose first level is
/// a namespace (`artist|foo`) becomes `artist:foo`, replacing the plain `foo`. Parent levels of
/// other keyword trees and darktable's own keywords are not tags (see [`Keywords`]). Keywords
/// are returned as written (trimmed); the caller normalises them.
pub fn read(bytes: &[u8], is_namespace: impl Fn(&str) -> bool) -> Result<SidecarData, XmpError> {
    let nodes = parse(bytes)?;
    let mut found = Found::default();
    collect(&nodes, &Scope::default(), false, &mut found);
    let tags = classify(&found, &is_namespace).tags;
    Ok(SidecarData { tags, rating: found.rating.as_deref().and_then(Rating::parse), source: found.source, hash: found.hash })
}

// ---------------------------------------------------------------------------------------------
// Writing

/// Removes managed properties from every `rdf:Description`, and drops descriptions left with
/// nothing in them (BooruCraft's own from the last write). Returns the qualified name of the
/// `rdf:RDF` element if one was found at this level or below.
fn strip(nodes: &mut Vec<Node>, scope: &Scope, rdf_name: &mut Option<String>) {
    for node in nodes.iter_mut() {
        let Node::Element(el) = node else { continue };
        let inner = scope.enter(&el.start);
        let name = inner.element(el.start.name().0);
        if is(&name, NS_RDF, "RDF") && rdf_name.is_none() {
            *rdf_name = Some(el.start.name().0.to_string());
        }
        if is(&name, NS_RDF, "Description") {
            strip_description(el, &inner);
        } else {
            strip(&mut el.children, &inner, rdf_name);
        }
    }
    remove_elements(nodes, |el| is_leftover_description(el, scope));
}

fn strip_description(el: &mut Element, scope: &Scope) {
    let has_managed_attr = el.start.attributes().with_checks(false).flatten().any(|a| is_managed(&scope.attribute(a.key.0)));
    if has_managed_attr {
        let mut start = BytesStart::new(el.start.name().0.to_string());
        for attr in el.start.attributes().with_checks(false).flatten() {
            if !is_managed(&scope.attribute(attr.key.0)) {
                start.push_attribute(attr);
            }
        }
        el.start = start.into_owned();
    }
    remove_elements(&mut el.children, |c| is_managed(&scope.enter(&c.start).element(c.start.name().0)));
}

/// Removes the elements `remove` picks, with the indentation in front of each, so repeated
/// updates don't leave blank lines behind.
fn remove_elements(nodes: &mut Vec<Node>, remove: impl Fn(&Element) -> bool) {
    let mut kept: Vec<Node> = Vec::with_capacity(nodes.len());
    for node in nodes.drain(..) {
        if let Node::Element(el) = &node
            && remove(el)
        {
            if matches!(kept.last(), Some(Node::Other(Event::Text(t))) if t.trim().is_empty()) {
                kept.pop();
            }
            continue;
        }
        kept.push(node);
    }
    *nodes = kept;
}

/// A description with only whitespace inside and no attributes besides `rdf:about` and
/// namespace declarations.
fn is_leftover_description(el: &Element, scope: &Scope) -> bool {
    let inner = scope.enter(&el.start);
    if !is(&inner.element(el.start.name().0), NS_RDF, "Description") {
        return false;
    }
    let only_about = el.start.attributes().with_checks(false).flatten().all(|a| {
        let k = a.key.0;
        k == "xmlns" || k.starts_with("xmlns:") || is(&inner.attribute(k), NS_RDF, "about")
    });
    let only_space = el.children.iter().all(|c| matches!(c, Node::Other(Event::Text(t)) if t.trim().is_empty()));
    only_about && only_space
}

fn find_rdf<'a>(nodes: &'a mut [Node], scope: &Scope) -> Option<&'a mut Element> {
    for node in nodes.iter_mut() {
        let Node::Element(el) = node else { continue };
        let inner = scope.enter(&el.start);
        if is(&inner.element(el.start.name().0), NS_RDF, "RDF") {
            return Some(el);
        }
        if let Some(found) = find_rdf(&mut el.children, &inner) {
            return Some(found);
        }
    }
    None
}

fn text(s: &str) -> Node {
    Node::Other(Event::Text(BytesText::new(s).into_owned()))
}

fn leaf(name: &str, value: &str) -> Node {
    Node::Element(Element { start: BytesStart::new(name.to_string()), children: vec![text(value)], empty: false })
}

/// `<prefix:prop><rdf:Bag><rdf:li>…</rdf:li>…</rdf:Bag></prefix:prop>`, indented under `indent`.
fn bag(prop: &str, rdf: &str, items: &[String], indent: &str) -> Node {
    let mut lis = Vec::new();
    for item in items {
        lis.push(text(&format!("\n{indent}    ")));
        lis.push(leaf(&format!("{rdf}:li"), item));
    }
    lis.push(text(&format!("\n{indent}  ")));
    let bag = Element { start: BytesStart::new(format!("{rdf}:Bag")), children: lis, empty: false };
    Node::Element(Element {
        start: BytesStart::new(prop.to_string()),
        children: vec![text(&format!("\n{indent}  ")), Node::Element(bag), text(&format!("\n{indent}"))],
        empty: false,
    })
}

/// Our `rdf:Description` holding `data`. `rdf` is the prefix the document uses for RDF.
fn description(rdf: &str, data: &SidecarData, foreign: &Keywords, is_namespace: &dyn Fn(&str) -> bool, indent: &str) -> Node {
    let mut start = BytesStart::new(format!("{rdf}:Description"));
    start.push_attribute((format!("{rdf}:about").as_str(), ""));
    start.push_attribute(("xmlns:dc", NS_DC));
    start.push_attribute(("xmlns:lr", NS_LR));
    start.push_attribute(("xmlns:boorucraft", NS_BOORUCRAFT));
    let child_indent = format!("{indent}  ");
    let mut children = Vec::new();
    let push = |children: &mut Vec<Node>, node: Node| {
        children.push(text(&format!("\n{child_indent}")));
        children.push(node);
    };
    if let Some(r) = data.rating {
        push(&mut children, leaf("boorucraft:Rating", r.code()));
    }
    if let Some(s) = &data.source {
        push(&mut children, leaf("boorucraft:Source", s));
    }
    if let Some(h) = &data.hash {
        push(&mut children, leaf("boorucraft:Hash", h));
    }
    let mut subjects: Vec<String> = data.tags.iter().cloned().collect();
    subjects.extend(foreign.foreign_subjects.iter().filter(|s| !data.tags.contains(*s)).cloned());
    if !subjects.is_empty() {
        push(&mut children, bag("dc:subject", rdf, &subjects, &child_indent));
    }
    let mut hier: Vec<String> = data
        .tags
        .iter()
        .filter_map(|t| match boorucraft_core::split_namespace(t, is_namespace) {
            (Some(ns), name) => Some(format!("{ns}|{name}")),
            (None, _) => None,
        })
        .collect();
    hier.extend(foreign.foreign_hierarchy.iter().cloned());
    if !hier.is_empty() {
        push(&mut children, bag("lr:hierarchicalSubject", rdf, &hier, &child_indent));
    }
    children.push(text(&format!("\n{indent}")));
    Node::Element(Element { start: start.into_owned(), children, empty: false })
}

/// Writes `data` into a sidecar.
///
/// With `existing`, every field BooruCraft doesn't manage is kept exactly as it was, and so are
/// other apps' keywords in the shared keyword fields (see [`Keywords`]). If the existing file
/// can't be parsed, the error is returned and the caller must leave it alone. Without
/// `existing`, a new minimal document is made.
pub fn update(existing: Option<&[u8]>, data: &SidecarData, is_namespace: &dyn Fn(&str) -> bool) -> Result<Vec<u8>, XmpError> {
    let mut nodes = match existing {
        Some(bytes) => parse(bytes)?,
        None => parse(EMPTY_DOCUMENT.as_bytes())?,
    };
    let mut found = Found::default();
    collect(&nodes, &Scope::default(), false, &mut found);
    let foreign = classify(&found, is_namespace);
    let mut rdf_name = None;
    strip(&mut nodes, &Scope::default(), &mut rdf_name);
    let rdf_name = rdf_name.ok_or(XmpError::NoRdf)?;
    let rdf_prefix = rdf_name.split_once(':').map_or("", |(p, _)| p).to_string();
    let rdf = find_rdf(&mut nodes, &Scope::default()).ok_or(XmpError::NoRdf)?;

    let indent = indent_of(&rdf.children);
    if !data.is_empty() || !foreign.foreign_subjects.is_empty() || !foreign.foreign_hierarchy.is_empty() {
        // An unprefixed rdf:RDF (default namespace) can't be referred to from our element with
        // prefixed names, so our element then binds `rdf` itself.
        let prefix = if rdf_prefix.is_empty() || ["dc", "lr", "boorucraft"].contains(&rdf_prefix.as_str()) { "rdf" } else { rdf_prefix.as_str() };
        let mut ours = description(prefix, data, &foreign, is_namespace, &indent);
        if prefix != rdf_prefix
            && let Node::Element(el) = &mut ours
        {
            el.start.push_attribute(("xmlns:rdf", NS_RDF));
        }
        // Insert before the whitespace that closes rdf:RDF so the layout stays tidy.
        let closing = matches!(rdf.children.last(), Some(Node::Other(Event::Text(t))) if t.trim().is_empty());
        let at = if closing { rdf.children.len().saturating_sub(1) } else { rdf.children.len() };
        rdf.children.splice(at..at, [text(&format!("\n{indent}")), ours]);
        if !closing {
            rdf.children.push(text("\n"));
        }
    }
    let mut writer = Writer::new(Vec::new());
    write_nodes(&nodes, &mut writer)?;
    Ok(writer.into_inner())
}

const EMPTY_DOCUMENT: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
<x:xmpmeta xmlns:x=\"adobe:ns:meta/\">\n \
<rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\">\n \
</rdf:RDF>\n\
</x:xmpmeta>\n";

/// The indentation of the first element inside `children`, or two spaces.
fn indent_of(children: &[Node]) -> String {
    for pair in children.windows(2) {
        if let [Node::Other(Event::Text(t)), Node::Element(_)] = pair
            && let Some((_, after)) = t.rsplit_once('\n')
            && after.trim().is_empty()
        {
            return after.to_string();
        }
    }
    "  ".to_string()
}

#[cfg(test)]
mod tests;
