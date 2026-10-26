// SPDX-License-Identifier: LicenseRef-Proprietary
// Copyright (c) 2026 Buntato
//! HTML parser for [`crate::Document`].
//!
//! This is the front of the engine pipeline. Bytes become a node list that
//! [`crate::Document`] owns. anvil-style reads that tree next; this module
//! does not compute style, layout, or pixels. anvil-script may mutate the
//! tree later, and anvil-browser keeps the document.
//!
//! Image and video elements are stored as ordinary elements. Their `src`
//! values are collected by the document so a later stage can decode images
//! with anvil-image and hand `<video>` to anvil-video. This parser does not
//! decode either, and this crate does not depend on those libraries.
//!
//! Caps, checked while scanning:
//! - [`MAX_DOM_DEPTH`] (256) limits how deep the open-element stack grows.
//! - [`MAX_DOM_NODES`] (50_000) stops allocation. Leftover input is ignored.
//! - [`MAX_ATTRS_PER_ELEMENT`] (128) drops further attributes on one start tag.

use crate::{Document, NodeData, NodeId};

/// Open-element stack limit. A start tag past this depth is still created,
/// but it is not pushed, so following tokens stay under the current parent.
pub const MAX_DOM_DEPTH: usize = 256;

/// Node-list limit for one document, including text nodes created here.
/// [`crate::Document::set_inner_html`] uses the same cap when cloning.
pub const MAX_DOM_NODES: usize = 50_000;

/// Attributes kept from one start tag. Later attributes on that tag are dropped.
pub const MAX_ATTRS_PER_ELEMENT: usize = 128;

const VOID_TAGS: &[&str] = &[
    "area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "param", "source",
    "track", "wbr",
];

const HEAD_TAGS: &[&str] = &["base", "link", "meta", "style", "title"];

/// Parse a subset of HTML into a document.
///
/// `input` is borrowed for the scan. The returned [`Document`] owns every
/// tag, attribute, and text string. `script` and `style` keep their raw text,
/// including a `<` that would otherwise look like a tag. Void tags do not
/// take children. Attributes on `html`, `head`, and `body` tags go on the
/// elements the document starts with; one already there is kept. Comments
/// and `<!` / `<?` markup are skipped. Tag names are
/// stored in ASCII lowercase. An unknown entity is left in the text. Decimal
/// (`&#39;`) and hex (`&#x27;`) references are decoded in text and attributes.
///
/// The scan stops once [`MAX_DOM_NODES`] nodes exist. Nesting past
/// [`MAX_DOM_DEPTH`] does not open a new stack frame.
pub fn parse_html(input: &str) -> Document {
    let mut doc = Document::new();
    let mut stack = vec![doc.body()];
    let bytes = input.as_bytes();
    let mut index = 0;

    while index < bytes.len() {
        // The node list is not compacted. Hitting the cap drops the rest of the source.
        if doc.node_count() >= MAX_DOM_NODES {
            break;
        }
        if bytes[index] == b'<' && is_markup(bytes, index) {
            if input[index..].starts_with("<!--") {
                index = skip_until(input, index + 4, "-->").unwrap_or(input.len());
                continue;
            }
            if input[index..].starts_with("<!") || input[index..].starts_with("<?") {
                index = skip_char(input, index + 2, '>').unwrap_or(input.len());
                continue;
            }
            if bytes.get(index + 1) == Some(&b'/') {
                let (name, next) = read_end_tag(input, index);
                index = next;
                close_tag(&doc, &mut stack, &name);
                continue;
            }

            let (name, attrs, next) = read_start_tag(input, index);
            index = next;
            if name.is_empty() {
                continue;
            }
            if name == "html" {
                let html = doc.html();
                merge_attributes(&mut doc, html, attrs);
                continue;
            }
            if name == "head" {
                let head = doc.head();
                merge_attributes(&mut doc, head, attrs);
                stack.clear();
                stack.push(head);
                continue;
            }
            if name == "body" {
                let body = doc.body();
                merge_attributes(&mut doc, body, attrs);
                stack.clear();
                stack.push(body);
                continue;
            }

            auto_close_for_start_tag(&doc, &mut stack, &name);

            let parent = placement_parent(&doc, &stack, &name);
            let id = doc.create_element(&name, attrs);
            doc.append(parent, id).expect("new node has no children");

            if name == "script" || name == "style" {
                // Newlines become spaces before the script interpreter sees
                // the text. It treats a raw newline as the end of a statement,
                // and these pages were written for the collapsed form.
                if let Some((text, next)) = read_raw_text(input, index, &name) {
                    let decoded = decode_entities(&text);
                    let prepared = if name == "script" {
                        strip_js_comments_before_collapse(&decoded)
                    } else {
                        decoded
                    };
                    if let Some(text) = normalize_text(&prepared) {
                        if doc.node_count() < MAX_DOM_NODES {
                            let text_id = doc.create_text(&text);
                            doc.append(id, text_id).expect("new text node");
                        }
                    }
                    index = next;
                }
                continue;
            }

            // Depth is the open stack, which starts at `body`. A node past the
            // cap is parented to the current top and does not become a frame.
            if !is_void(&name) && stack.len() < MAX_DOM_DEPTH {
                stack.push(id);
            }
            continue;
        }

        // Text runs to the next `<` that starts markup. A `<` that does not
        // (as in `1 < 2`) is ordinary text, so step past the first byte before
        // looking; otherwise the outer loop would sit on that `<` forever.
        let start = index;
        index += 1;
        while index < bytes.len() && !(bytes[index] == b'<' && is_markup(bytes, index)) {
            index += 1;
        }
        let decoded = decode_entities(&input[start..index]);
        let parent = *stack.last().unwrap_or(&doc.body());
        if let Some(text) = text_for_parent(&doc, parent, &decoded) {
            if doc.node_count() < MAX_DOM_NODES {
                let text_id = doc.create_text(&text);
                doc.append(parent, text_id).expect("new text node");
            }
        }
    }

    doc
}

/// True when `<` begins a tag, comment, or declaration rather than text.
fn is_markup(bytes: &[u8], index: usize) -> bool {
    matches!(
        bytes.get(index + 1).copied(),
        Some(b'/' | b'!' | b'?' | b'a'..=b'z' | b'A'..=b'Z')
    )
}

/// Void tags never take children and are not pushed on the open stack.
fn is_void(tag: &str) -> bool {
    VOID_TAGS.contains(&tag)
}

/// Pop implied end tags before a table cell, row, section, or `option`.
///
/// Only the open stack is changed. Already-built nodes stay where they are.
/// Tags other than `td`, `th`, `tr`, `thead`, `tbody`, `tfoot`, and `option`
/// do not auto-close anything.
fn auto_close_for_start_tag(doc: &Document, stack: &mut Vec<NodeId>, new_tag: &str) {
    match new_tag {
        "td" | "th" => {
            if let Some(&top) = stack.last() {
                if matches!(doc.get(top).data().tag(), Some("td" | "th")) {
                    stack.pop();
                }
            }
        }
        "tr" => {
            while stack.len() > 1 {
                let top = *stack.last().unwrap();
                match doc.get(top).data().tag() {
                    Some("td" | "th" | "tr") => {
                        stack.pop();
                    }
                    _ => break,
                }
            }
        }
        "thead" | "tbody" | "tfoot" => {
            while stack.len() > 1 {
                let top = *stack.last().unwrap();
                match doc.get(top).data().tag() {
                    Some("td" | "th" | "tr" | "thead" | "tbody" | "tfoot") => {
                        stack.pop();
                    }
                    _ => break,
                }
            }
        }
        "option" => {
            if let Some(&top) = stack.last() {
                if doc.get(top).data().tag() == Some("option") {
                    stack.pop();
                }
            }
        }
        _ => {}
    }
}

/// Parent for a new element. Head-only tags under `body` or `html` go to `head`.
///
/// An empty stack falls back to `body`. The returned id is copied from `stack`
/// or from the document skeleton. It is not a newly allocated node.
fn placement_parent(doc: &Document, stack: &[NodeId], tag: &str) -> NodeId {
    if HEAD_TAGS.contains(&tag) {
        if let Some(top) = stack.last() {
            if *top == doc.body() || doc.get(*top).data().tag() == Some("html") {
                return doc.head();
            }
        }
    }
    stack.last().copied().unwrap_or_else(|| doc.body())
}

/// Copy attributes from an `html`, `head`, or `body` start tag onto the
/// element [`Document::new`] already made. An attribute the element has
/// already got keeps its value, so a second `<body>` only adds new ones.
/// The element never ends up with more than [`MAX_ATTRS_PER_ELEMENT`].
fn merge_attributes(doc: &mut Document, id: NodeId, attrs: Vec<(String, String)>) {
    for (name, value) in attrs {
        let data = doc.get(id).data();
        let count = match data {
            NodeData::Element { attrs, .. } => attrs.len(),
            _ => return,
        };
        if data.attr(&name).is_none() && count < MAX_ATTRS_PER_ELEMENT {
            doc.set_attribute(id, &name, &value);
        }
    }
}

/// Pop the open stack through the matching start tag.
///
/// `head`, `body`, and `html` reset the stack to `body`. An unknown name pops
/// until the stack would be empty, then restores `body`. Nodes are not removed.
fn close_tag(doc: &Document, stack: &mut Vec<NodeId>, name: &str) {
    if name.is_empty() {
        return;
    }
    if name == "head" || name == "body" || name == "html" {
        stack.clear();
        stack.push(doc.body());
        return;
    }
    while stack.len() > 1 {
        let top = stack.pop().expect("stack length was checked");
        if doc.get(top).data().tag() == Some(name) {
            break;
        }
    }
    if stack.is_empty() {
        stack.push(doc.body());
    }
}

/// Read a start tag at `start` (the `<` byte).
///
/// Returns the lowercase name, owned attribute pairs, and the index after `>`.
/// A `/` before `>` is ignored, so a self-closing slash does not by itself make
/// the tag void. Attribute values are entity-decoded. Past
/// [`MAX_ATTRS_PER_ELEMENT`], further attributes are skipped.
fn read_start_tag(input: &str, start: usize) -> (String, Vec<(String, String)>, usize) {
    let bytes = input.as_bytes();
    let mut index = start + 1;
    let name = read_name(input, &mut index);
    let mut attrs = Vec::new();
    loop {
        index = skip_ascii_space(bytes, index);
        if index >= bytes.len() || bytes[index] == b'>' {
            break;
        }
        if bytes[index] == b'/' {
            index += 1;
            continue;
        }
        let attr_name = read_name(input, &mut index);
        if attr_name.is_empty() {
            // Skip one whole character so the next slice starts on a boundary.
            index += input[index..].chars().next().map_or(1, char::len_utf8);
            continue;
        }
        index = skip_ascii_space(bytes, index);
        let value = if index < bytes.len() && bytes[index] == b'=' {
            index += 1;
            index = skip_ascii_space(bytes, index);
            read_attr_value(input, &mut index)
        } else {
            String::new()
        };
        // Overflow attributes are discarded. The tag itself is still created.
        if attrs.len() < MAX_ATTRS_PER_ELEMENT {
            attrs.push((attr_name, value));
        }
    }
    if index < bytes.len() && bytes[index] == b'>' {
        index += 1;
    }
    (name, attrs, index)
}

/// Read an end tag at `start` (the `<` of `</`). Returns the lowercase name
/// and the index after `>`, or the end of input when `>` is missing.
fn read_end_tag(input: &str, start: usize) -> (String, usize) {
    let bytes = input.as_bytes();
    let mut index = start + 2;
    let name = read_name(input, &mut index);
    while index < bytes.len() && bytes[index] != b'>' {
        index += 1;
    }
    if index < bytes.len() {
        index += 1;
    }
    (name, index)
}

/// Read a tag or attribute name and advance `index` past it.
///
/// The returned string is owned and ASCII-lowercased. Characters outside
/// [`is_name_byte`] end the name.
fn read_name(input: &str, index: &mut usize) -> String {
    let bytes = input.as_bytes();
    let start = *index;
    while *index < bytes.len() && is_name_byte(bytes[*index]) {
        *index += 1;
    }
    input[start..*index].to_ascii_lowercase()
}

/// ASCII alphanumeric, `-`, or `:`. Other bytes end a name.
fn is_name_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'-' || byte == b':'
}

/// Read an attribute value and advance `index` past it.
///
/// Quoted values run to the matching quote. Unquoted values run to whitespace,
/// `>`, or `/`. The result is owned and entity-decoded. A missing value is empty.
fn read_attr_value(input: &str, index: &mut usize) -> String {
    let bytes = input.as_bytes();
    if *index >= bytes.len() {
        return String::new();
    }
    let quote = bytes[*index];
    if quote == b'"' || quote == b'\'' {
        *index += 1;
        let start = *index;
        while *index < bytes.len() && bytes[*index] != quote {
            *index += 1;
        }
        let value = decode_entities(&input[start..*index]);
        if *index < bytes.len() {
            *index += 1;
        }
        return value;
    }
    let start = *index;
    while *index < bytes.len()
        && !matches!(bytes[*index], b' ' | b'\t' | b'\n' | b'\r' | b'>' | b'/')
    {
        *index += 1;
    }
    decode_entities(&input[start..*index])
}

/// Take raw text until the case-insensitive end tag `</tag`.
///
/// Returns `None` when that end tag is absent; the caller then leaves the
/// following source to the normal scanner. The text is owned and not parsed
/// as markup. The end index is just after the end tag's `>`.
fn read_raw_text(input: &str, start: usize, tag: &str) -> Option<(String, usize)> {
    let lower = input[start..].to_ascii_lowercase();
    let marker = format!("</{tag}");
    let found = lower.find(&marker)?;
    let text = input[start..start + found].to_string();
    let mut end = start + found + marker.len();
    let bytes = input.as_bytes();
    while end < bytes.len() && bytes[end] != b'>' {
        end += 1;
    }
    if end < bytes.len() {
        end += 1;
    }
    Some((text, end))
}

/// Index just after `marker`, or `None` if `marker` does not occur.
fn skip_until(input: &str, start: usize, marker: &str) -> Option<usize> {
    input[start..]
        .find(marker)
        .map(|offset| start + offset + marker.len())
}

/// Index just after the next `marker` character, or `None` if it does not occur.
fn skip_char(input: &str, start: usize, marker: char) -> Option<usize> {
    input[start..]
        .find(marker)
        .map(|offset| start + offset + marker.len_utf8())
}

/// Advance `index` past ASCII whitespace. The returned index may be `bytes.len()`.
fn skip_ascii_space(bytes: &[u8], mut index: usize) -> usize {
    while index < bytes.len() && bytes[index].is_ascii_whitespace() {
        index += 1;
    }
    index
}

/// Decode named and numeric character references into an owned string.
///
/// Named names: `amp`, `lt`, `gt`, `quot`, `apos`, `nbsp`. A decimal
/// reference is `&#39;`. A hex reference is `&#x27;` or `&#X27;`. A `&` with
/// no `;`, a name this list does not know, or a number that is not a scalar
/// is copied through. The input is borrowed; the output is owned.
fn decode_entities(input: &str) -> String {
    let mut out = String::new();
    let mut rest = input;
    while let Some(start) = rest.find('&') {
        out.push_str(&rest[..start]);
        rest = &rest[start..];
        let Some(end) = rest.find(';') else {
            out.push_str(rest);
            return out;
        };
        if end == 0 || end > 32 {
            out.push('&');
            rest = &rest[1..];
            continue;
        }
        let entity = &rest[1..end];
        if let Some(ch) = decode_one_entity(entity) {
            out.push(ch);
            rest = &rest[end + 1..];
            continue;
        }
        out.push('&');
        rest = &rest[1..];
    }
    out.push_str(rest);
    out
}

/// One character for an entity name or numeric body, without the leading `&`
/// or the trailing `;`. `None` when it is not a reference this parser knows.
fn decode_one_entity(entity: &str) -> Option<char> {
    let named = match entity {
        "amp" => Some('&'),
        "lt" => Some('<'),
        "gt" => Some('>'),
        "quot" => Some('"'),
        "apos" => Some('\''),
        "nbsp" => Some('\u{00a0}'),
        _ => None,
    };
    if named.is_some() {
        return named;
    }
    let hex = entity
        .strip_prefix("#x")
        .or_else(|| entity.strip_prefix("#X"));
    let code = if let Some(digits) = hex {
        if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return None;
        }
        u32::from_str_radix(digits, 16).ok()?
    } else if let Some(digits) = entity.strip_prefix('#') {
        if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        digits.parse::<u32>().ok()?
    } else {
        return None;
    };
    char::from_u32(code).filter(|ch| *ch != '\0')
}

/// Collapse whitespace, but keep one leading or trailing space when the
/// source had it. A node that is only whitespace is dropped (`None`).
///
/// The returned string is owned. Interior whitespace runs become one space.
/// Inside `pre` or `textarea`, newlines are kept instead.
fn text_for_parent(doc: &Document, parent: NodeId, raw: &str) -> Option<String> {
    if preserves_whitespace(doc, parent) {
        preserve_text(raw)
    } else {
        normalize_text(raw)
    }
}

fn preserves_whitespace(doc: &Document, mut id: NodeId) -> bool {
    loop {
        if matches!(doc.get(id).data().tag(), Some("pre" | "textarea")) {
            return true;
        }
        match doc.parent_element(id) {
            Some(parent) => id = parent,
            None => return false,
        }
    }
}

fn preserve_text(raw: &str) -> Option<String> {
    let mut out = String::new();
    let mut chars = raw.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\r' {
            if chars.peek() == Some(&'\n') {
                chars.next();
            }
            out.push('\n');
        } else {
            out.push(ch);
        }
    }
    if out.is_empty() { None } else { Some(out) }
}

fn strip_js_comments_before_collapse(source: &str) -> String {
    let mut out = String::with_capacity(source.len());
    let mut chars = source.chars().peekable();
    let mut quote: Option<char> = None;
    while let Some(ch) = chars.next() {
        if let Some(q) = quote {
            out.push(ch);
            if ch == '\\' {
                if let Some(esc) = chars.next() {
                    out.push(esc);
                }
            } else if ch == q {
                quote = None;
            }
            continue;
        }
        if ch == '"' || ch == '\'' || ch == '`' {
            quote = Some(ch);
            out.push(ch);
            continue;
        }
        if ch == '/' && chars.peek() == Some(&'/') {
            let _ = chars.next();
            for next_ch in chars.by_ref() {
                if next_ch == '\n' {
                    out.push('\n');
                    break;
                }
            }
            continue;
        }
        if ch == '/' && chars.peek() == Some(&'*') {
            let _ = chars.next();
            while let Some(next_ch) = chars.next() {
                if next_ch == '*' && chars.peek() == Some(&'/') {
                    let _ = chars.next();
                    out.push(' ');
                    break;
                }
            }
            continue;
        }
        out.push(ch);
    }
    out
}

fn normalize_text(raw: &str) -> Option<String> {
    if !raw.chars().any(|ch| !ch.is_whitespace()) {
        return None;
    }
    let lead = raw.chars().next().is_some_and(|ch| ch.is_whitespace());
    let trail = raw.chars().next_back().is_some_and(|ch| ch.is_whitespace());
    let mut collapsed = String::new();
    let mut pending_space = false;
    for ch in raw.chars() {
        if ch.is_whitespace() {
            pending_space = true;
            continue;
        }
        if pending_space && !collapsed.is_empty() {
            collapsed.push(' ');
        }
        pending_space = false;
        collapsed.push(ch);
    }
    let mut out = String::new();
    if lead {
        out.push(' ');
    }
    out.push_str(&collapsed);
    if trail {
        out.push(' ');
    }
    Some(out)
}
