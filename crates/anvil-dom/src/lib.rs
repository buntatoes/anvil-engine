// SPDX-License-Identifier: LicenseRef-Proprietary
// Copyright (c) 2026 Buntato
//! Document tree. Nodes live in one list and point at each other by id.
//!
//! First stage of the pipeline. [`parse_html`] turns source into a [`Document`]
//! this crate owns. anvil-style reads the tree next and does not mutate it.
//! anvil-layout and anvil-paint never see the HTML source. anvil-script mutates
//! the same document. anvil-browser is the owner of the live page.
//!
//! `<img src>` and `<video src>` (or the first `<source src>`) are recorded
//! as element attributes. Pixel decode is anvil-image's job. Timed video is
//! anvil-video's job. This crate does not decode either.
//!
//! Hard caps, enforced by the parser and by [`Document::set_inner_html`]:
//! [`MAX_DOM_DEPTH`] is 256 open elements, [`MAX_DOM_NODES`] is 50_000 nodes,
//! and [`MAX_ATTRS_PER_ELEMENT`] is 128 attributes on one start tag.
//! Nodes detached by edits stay in the list until the document is dropped.
//!
//! Selectors here are a single compound: a tag, `#id`, `.class`, `tag#id`,
//! or `tag.class`. Descendant combinators and the cascade live in anvil-style.

mod html;

pub use html::{MAX_ATTRS_PER_ELEMENT, MAX_DOM_DEPTH, MAX_DOM_NODES, parse_html};

/// One style sheet the document names, from [`Document::style_sheets`].
///
/// `media` is the trimmed `media` attribute, or `None` when there is none.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StyleSheetSource {
    /// `<link rel=stylesheet href>`. The browser fetches `href` itself.
    Link {
        href: String,
        integrity: Option<String>,
        media: Option<String>,
    },
    /// A `<style>` element and its text.
    Style { css: String, media: Option<String> },
}

/// Index of a node inside one [`Document`]'s node list.
///
/// The id is a copy of the slot index. It does not own the node. An id from
/// another document, or past [`Document::node_count`], is not valid here.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct NodeId(u32);

impl NodeId {
    /// Build an id from a raw slot index. Does not check that a node exists.
    pub fn from_raw(index: u32) -> Self {
        Self(index)
    }

    /// Slot index. Used to index the document's node vector.
    pub fn index(self) -> usize {
        self.0 as usize
    }
}

/// Why [`Document::append`] refused an edit. The tree is left unchanged.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TreeError {
    /// `parent` or `child` is not a slot in this document.
    Missing,
    /// `child` is `parent`, or `parent` is already inside `child`.
    Cycle,
}

/// Payload owned by one node. The tree shape lives on [`Node`], not here.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NodeData {
    /// The document slot at index 0. It has no tag and no text.
    Document,
    /// An element. `tag` is ASCII-lowercase and owned. `attrs` owns each
    /// name and value. Names are stored lowercase by the writers in this crate.
    Element {
        tag: String,
        attrs: Vec<(String, String)>,
    },
    /// Owned character data. Not parsed as markup.
    Text(String),
}

impl NodeData {
    /// An element with an owned lowercase tag and no attributes.
    /// `tag` is borrowed only for the copy.
    pub fn element(tag: &str) -> Self {
        Self::Element {
            tag: tag.to_ascii_lowercase(),
            attrs: Vec::new(),
        }
    }

    /// The element tag, borrowed from this node. `None` for the document and for text.
    pub fn tag(&self) -> Option<&str> {
        match self {
            Self::Element { tag, .. } => Some(tag),
            _ => None,
        }
    }

    /// The text, borrowed from this node. `None` unless this is [`NodeData::Text`].
    pub fn text(&self) -> Option<&str> {
        match self {
            Self::Text(text) => Some(text),
            _ => None,
        }
    }

    /// Attribute value borrowed from this element. `None` if this is not an
    /// element or the name is absent. Lookup ignores ASCII case.
    pub fn attr(&self, name: &str) -> Option<&str> {
        let Self::Element { attrs, .. } = self else {
            return None;
        };
        attrs
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    /// The `id` attribute, borrowed. `None` when it is missing.
    pub fn id(&self) -> Option<&str> {
        self.attr("id")
    }

    /// True when `class` contains `class_name` as a whole whitespace-separated token.
    /// A missing class attribute is false. The name is not trimmed here.
    pub fn has_class(&self, class_name: &str) -> bool {
        self.attr("class")
            .is_some_and(|classes| classes.split_ascii_whitespace().any(|c| c == class_name))
    }

    /// Set an attribute, storing the name in lowercase. No-op for non-elements.
    /// Replaces an existing name. `value` is copied.
    pub fn set_attr(&mut self, name: &str, value: &str) {
        let Self::Element { attrs, .. } = self else {
            return;
        };
        let key = name.to_ascii_lowercase();
        if let Some((_, slot)) = attrs.iter_mut().find(|(k, _)| *k == key) {
            *slot = value.to_string();
        } else {
            attrs.push((key, value.to_string()));
        }
    }

    /// Remove every attribute whose name matches, ignoring ASCII case.
    /// Returns false for a non-element or when nothing was removed.
    pub fn remove_attr(&mut self, name: &str) -> bool {
        let Self::Element { attrs, .. } = self else {
            return false;
        };
        let before = attrs.len();
        attrs.retain(|(k, _)| !k.eq_ignore_ascii_case(name));
        attrs.len() != before
    }

    /// Append a class token if it is non-empty and not already present.
    /// No-op for non-elements. The class string is owned by the attribute.
    pub fn add_class(&mut self, class_name: &str) {
        let class_name = class_name.trim();
        if class_name.is_empty() || self.has_class(class_name) {
            return;
        }
        let next = match self.attr("class") {
            Some(existing) if !existing.trim().is_empty() => {
                format!("{} {class_name}", existing.trim())
            }
            _ => class_name.to_string(),
        };
        self.set_attr("class", &next);
    }

    /// Drop one class token. A missing class attribute is a no-op.
    /// The remaining tokens stay in one owned `class` value, which may be empty.
    pub fn remove_class(&mut self, class_name: &str) {
        let class_name = class_name.trim();
        let Some(existing) = self.attr("class") else {
            return;
        };
        let kept: Vec<&str> = existing
            .split_ascii_whitespace()
            .filter(|c| *c != class_name)
            .collect();
        let joined = kept.join(" ");
        self.set_attr("class", &joined);
    }

    /// Add the class and return true, or remove it and return false.
    /// The bool is the class's presence after the call.
    pub fn toggle_class(&mut self, class_name: &str) -> bool {
        if self.has_class(class_name) {
            self.remove_class(class_name);
            false
        } else {
            self.add_class(class_name);
            true
        }
    }
}

/// One slot in a [`Document`]. The document owns the node. Children are ids,
/// not nested `Node` values. `parent` is `None` for the root and for a node
/// that has been detached.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Node {
    id: NodeId,
    parent: Option<NodeId>,
    children: Vec<NodeId>,
    data: NodeData,
}

impl Node {
    /// This slot's id. It is a copy, not a borrow of the document.
    pub fn id(&self) -> NodeId {
        self.id
    }

    /// Parent id. `None` if this is the root or the node was detached.
    pub fn parent(&self) -> Option<NodeId> {
        self.parent
    }

    /// Borrowed payload. The node keeps ownership.
    pub fn data(&self) -> &NodeData {
        &self.data
    }

    /// Mutable payload. Tree links are not exposed here.
    pub fn data_mut(&mut self) -> &mut NodeData {
        &mut self.data
    }
}

/// A document and every node it owns.
///
/// Slots are append-only. [`Self::remove_node`], [`Self::set_text_content`],
/// and [`Self::set_inner_html`] unlink children but leave the old slots in
/// `nodes`, so [`Self::node_count`] does not shrink. The skeleton is the
/// document node, then `html`, `head`, and `body`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Document {
    nodes: Vec<Node>,
    html: NodeId,
    head: NodeId,
    body: NodeId,
}

impl Default for Document {
    /// Same document as [`Document::new`]: document, `html`, `head`, and `body`.
    fn default() -> Self {
        Self::new()
    }
}

/// True when a `script` `type` is JavaScript, or when `type` is missing.
///
/// `text/javascript`, `application/javascript`, and `module` count. A MIME
/// parameter such as `;charset=utf-8` is ignored. `application/json` does not
/// count, so a data block is not handed to the interpreter.
fn is_javascript_type(raw: Option<&str>) -> bool {
    let Some(raw) = raw else {
        return true;
    };
    let mime = raw.split(';').next().unwrap_or(raw).trim();
    if mime.is_empty() {
        return true;
    }
    matches!(
        mime.to_ascii_lowercase().as_str(),
        "text/javascript"
            | "application/javascript"
            | "text/ecmascript"
            | "application/ecmascript"
            | "text/jscript"
            | "module"
    )
}

impl Document {
    /// An empty document with `html`, `head`, and `body` already in place.
    ///
    /// The returned value owns four nodes: document, `html`, `head`, `body`.
    pub fn new() -> Self {
        let mut doc = Self {
            nodes: Vec::new(),
            html: NodeId(0),
            head: NodeId(0),
            body: NodeId(0),
        };
        let root = doc.alloc(NodeData::Document);
        let html = doc.alloc(NodeData::element("html"));
        let head = doc.alloc(NodeData::element("head"));
        let body = doc.alloc(NodeData::element("body"));
        doc.append(root, html).expect("document skeleton");
        doc.append(html, head).expect("document skeleton");
        doc.append(html, body).expect("document skeleton");
        doc.html = html;
        doc.head = head;
        doc.body = body;
        doc
    }

    /// The document node. Always slot 0. Not the `html` element.
    pub fn root(&self) -> NodeId {
        NodeId(0)
    }

    /// The `html` element created by [`Self::new`].
    pub fn html(&self) -> NodeId {
        self.html
    }

    /// The `head` element created by [`Self::new`].
    pub fn head(&self) -> NodeId {
        self.head
    }

    /// The `body` element created by [`Self::new`].
    pub fn body(&self) -> NodeId {
        self.body
    }

    /// Slots allocated, including detached nodes that are no longer in the tree.
    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    /// Borrow the node at `id`. Panics if `id` is outside this document.
    pub fn get(&self, id: NodeId) -> &Node {
        &self.nodes[id.index()]
    }

    /// Mutably borrow the node. `None` if `id` is outside this document.
    pub fn get_mut(&mut self, id: NodeId) -> Option<&mut Node> {
        self.nodes.get_mut(id.index())
    }

    /// Child ids in tree order, borrowed from the parent. Panics if `id` is outside.
    pub fn children(&self, id: NodeId) -> &[NodeId] {
        &self.nodes[id.index()].children
    }

    /// Allocate an element that is not yet in the tree (`parent` is `None`).
    ///
    /// `tag` is copied and lowercased. `attrs` is consumed; each name is lowercased.
    /// The returned id is owned by this document.
    pub fn create_element(&mut self, tag: &str, attrs: Vec<(String, String)>) -> NodeId {
        let mut data = NodeData::element(tag);
        if let NodeData::Element { attrs: slot, .. } = &mut data {
            *slot = attrs
                .into_iter()
                .map(|(name, value)| (name.to_ascii_lowercase(), value))
                .collect();
        }
        self.alloc(data)
    }

    /// Allocate a text node that is not yet in the tree. `text` is copied.
    pub fn create_text(&mut self, text: &str) -> NodeId {
        self.alloc(NodeData::Text(text.to_string()))
    }

    /// Set one attribute. No-op when `id` is outside the document or not an element.
    pub fn set_attribute(&mut self, id: NodeId, name: &str, value: &str) {
        if let Some(node) = self.nodes.get_mut(id.index()) {
            node.data.set_attr(name, value);
        }
    }

    /// Remove one attribute. False when `id` is missing or the attribute was absent.
    pub fn remove_attribute(&mut self, id: NodeId, name: &str) -> bool {
        self.nodes
            .get_mut(id.index())
            .is_some_and(|node| node.data.remove_attr(name))
    }

    /// Add a class token on `id`. No-op when `id` is missing or not an element.
    pub fn add_class(&mut self, id: NodeId, class_name: &str) {
        if let Some(node) = self.nodes.get_mut(id.index()) {
            node.data.add_class(class_name);
        }
    }

    /// Remove a class token on `id`. No-op when `id` is missing.
    pub fn remove_class(&mut self, id: NodeId, class_name: &str) {
        if let Some(node) = self.nodes.get_mut(id.index()) {
            node.data.remove_class(class_name);
        }
    }

    /// Toggle a class on `id`. False when `id` is missing, or when the class
    /// was removed. True when the class is present after the call.
    pub fn toggle_class(&mut self, id: NodeId, class_name: &str) -> bool {
        self.nodes
            .get_mut(id.index())
            .is_some_and(|node| node.data.toggle_class(class_name))
    }

    /// Owned current value of a form control.
    ///
    /// A missing node yields an empty string. A `select` uses [`Self::select_value`].
    /// Any element with a `value` attribute returns that attribute, including a
    /// `textarea`. A `textarea` without `value` returns its text content.
    /// Other nodes without `value` yield an empty string.
    pub fn input_value(&self, id: NodeId) -> String {
        let Some(node) = self.nodes.get(id.index()) else {
            return String::new();
        };
        let data = node.data();
        if data.tag() == Some("select") {
            return self.select_value(id);
        }
        if let Some(val) = data.attr("value") {
            return val.to_string();
        }
        if data.tag() == Some("textarea") {
            return self.text_content(id);
        }
        String::new()
    }

    /// Store `value` on the `value` attribute. For a `textarea`, also replace
    /// its text content with the same string. No-op when `id` is missing.
    /// `value` is copied.
    pub fn set_input_value(&mut self, id: NodeId, value: &str) {
        let tag = self
            .nodes
            .get(id.index())
            .and_then(|n| n.data().tag().map(str::to_string));
        self.set_attribute(id, "value", value);
        if tag.as_deref() == Some("textarea") {
            self.set_text_content(id, value);
        }
    }

    /// True when a `checked` attribute is present and is not the word `false`.
    /// A missing node is false.
    pub fn is_checked(&self, id: NodeId) -> bool {
        let Some(node) = self.nodes.get(id.index()) else {
            return false;
        };
        node.data()
            .attr("checked")
            .is_some_and(|val| !val.eq_ignore_ascii_case("false"))
    }

    /// Set `checked="checked"`, or remove `checked` when `checked` is false.
    pub fn set_checked(&mut self, id: NodeId, checked: bool) {
        if checked {
            self.set_attribute(id, "checked", "checked");
        } else {
            self.remove_attribute(id, "checked");
        }
    }

    /// Flip [`Self::is_checked`] and return the new state.
    pub fn toggle_checked(&mut self, id: NodeId) -> bool {
        let next = !self.is_checked(id);
        self.set_checked(id, next);
        next
    }

    /// True for `textarea`, or for `input` whose type is empty, `text`,
    /// `search`, `password`, `email`, `url`, `tel`, or `number`.
    /// A missing node is false. A missing `type` counts as text.
    pub fn is_text_input(&self, id: NodeId) -> bool {
        let Some(node) = self.nodes.get(id.index()) else {
            return false;
        };
        let data = node.data();
        match data.tag() {
            Some("textarea") => true,
            Some("input") => {
                let kind = data.attr("type").unwrap_or("text").trim();
                matches!(
                    kind.to_ascii_lowercase().as_str(),
                    "" | "text" | "search" | "password" | "email" | "url" | "tel" | "number"
                )
            }
            _ => false,
        }
    }

    /// True only for `<input type="checkbox">`. A missing node is false.
    pub fn is_checkbox_input(&self, id: NodeId) -> bool {
        let Some(node) = self.nodes.get(id.index()) else {
            return false;
        };
        let data = node.data();
        data.tag() == Some("input")
            && data
                .attr("type")
                .is_some_and(|t| t.trim().eq_ignore_ascii_case("checkbox"))
    }

    /// True when `id` is a `select` element. A missing node is false.
    pub fn is_select_control(&self, id: NodeId) -> bool {
        self.nodes
            .get(id.index())
            .is_some_and(|n| n.data().tag() == Some("select"))
    }

    /// Direct `option` children, in tree order. An owned list of ids.
    /// A missing node yields an empty vec. Nested options are not walked.
    pub fn select_options(&self, select_id: NodeId) -> Vec<NodeId> {
        let Some(node) = self.nodes.get(select_id.index()) else {
            return Vec::new();
        };
        node.children
            .iter()
            .copied()
            .filter(|cid| self.get(*cid).data().tag() == Some("option"))
            .collect()
    }

    /// The first option with `selected` set and not equal to `false`.
    /// If none is marked, the first option. `None` when there are no options.
    pub fn selected_option(&self, select_id: NodeId) -> Option<NodeId> {
        let opts = self.select_options(select_id);
        opts.iter()
            .copied()
            .find(|oid| {
                self.get(*oid)
                    .data()
                    .attr("selected")
                    .is_some_and(|v| !v.eq_ignore_ascii_case("false"))
            })
            .or_else(|| opts.first().copied())
    }

    /// Owned value of the selected option: its `value` attribute, or its
    /// trimmed text when `value` is absent. Empty when nothing is selected.
    pub fn select_value(&self, select_id: NodeId) -> String {
        let Some(opt_id) = self.selected_option(select_id) else {
            return String::new();
        };
        if let Some(val) = self.get(opt_id).data().attr("value") {
            return val.to_string();
        }
        self.text_content(opt_id).trim().to_string()
    }

    /// Owned visible label of the selected option. Falls back to
    /// [`Self::select_value`] when the option's text is empty. Empty when
    /// nothing is selected.
    pub fn select_display_label(&self, select_id: NodeId) -> String {
        let Some(opt_id) = self.selected_option(select_id) else {
            return String::new();
        };
        let text = self.text_content(opt_id).trim().to_string();
        if !text.is_empty() {
            text
        } else {
            self.select_value(select_id)
        }
    }

    /// Mark the next direct option as `selected` and clear the others.
    ///
    /// Wraps to the first option. Also writes the new value onto the `select`.
    /// Returns that owned value. `None` when `select_id` has no option children.
    pub fn cycle_select_option(&mut self, select_id: NodeId) -> Option<String> {
        let opts = self.select_options(select_id);
        if opts.is_empty() {
            return None;
        }
        let current = self.selected_option(select_id);
        let next_idx = match current.and_then(|c| opts.iter().position(|&id| id == c)) {
            Some(idx) => (idx + 1) % opts.len(),
            None => 0,
        };
        for (i, &oid) in opts.iter().enumerate() {
            if i == next_idx {
                self.set_attribute(oid, "selected", "selected");
            } else {
                self.remove_attribute(oid, "selected");
            }
        }
        let new_val = self.select_value(select_id);
        self.set_attribute(select_id, "value", &new_val);
        Some(new_val)
    }

    /// True for `<input type="submit">`, or a `button` whose type is `submit`
    /// or omitted. A missing node is false.
    pub fn is_submit_control(&self, id: NodeId) -> bool {
        let Some(node) = self.nodes.get(id.index()) else {
            return false;
        };
        let data = node.data();
        match data.tag() {
            Some("input") => data
                .attr("type")
                .is_some_and(|t| t.trim().eq_ignore_ascii_case("submit")),
            Some("button") => {
                let kind = data.attr("type").unwrap_or("submit").trim();
                kind.eq_ignore_ascii_case("submit")
            }
            _ => false,
        }
    }

    /// The nearest `form` ancestor, including `id` itself.
    /// `None` if `id` is missing or no ancestor is a form.
    pub fn enclosing_form(&self, id: NodeId) -> Option<NodeId> {
        let mut cursor = Some(id);
        while let Some(curr) = cursor {
            let node = self.nodes.get(curr.index())?;
            if node.data().tag() == Some("form") {
                return Some(curr);
            }
            cursor = node.parent();
        }
        None
    }

    /// Owned `(action, method, urlencoded body)` for a `form` node.
    ///
    /// A missing node yields `("", "GET", "")`. `method` is `POST` only when
    /// the attribute is `POST` ignoring case; every other value, including a
    /// missing one, is `GET`. `action` is the trimmed attribute or empty.
    /// Named `input`, `textarea`, and `select` descendants are included.
    /// Checkboxes and radios are included only when checked, defaulting the
    /// value to `on`. Submit, button, and reset inputs are skipped.
    /// Successful controls are joined with `&` using [`urlencode_component`].
    pub fn serialize_form(&self, form_id: NodeId) -> (String, String, String) {
        let Some(form_node) = self.nodes.get(form_id.index()) else {
            return (String::new(), "GET".to_string(), String::new());
        };
        let action = form_node
            .data()
            .attr("action")
            .unwrap_or("")
            .trim()
            .to_string();
        let method = form_node
            .data()
            .attr("method")
            .unwrap_or("GET")
            .trim()
            .to_ascii_uppercase();
        let method = if method == "POST" {
            "POST".to_string()
        } else {
            "GET".to_string()
        };
        let mut pairs = Vec::new();
        self.collect_form_pairs(form_id, &mut pairs);
        let encoded = pairs
            .into_iter()
            .map(|(k, v)| format!("{}={}", urlencode_component(&k), urlencode_component(&v)))
            .collect::<Vec<_>>()
            .join("&");
        (action, method, encoded)
    }

    /// Replace the children of `id` with one text node, or with nothing when
    /// `text` is empty.
    ///
    /// Previous children stay allocated with `parent` cleared. A missing `id`
    /// is a no-op. `text` is copied.
    pub fn set_text_content(&mut self, id: NodeId, text: &str) {
        if let Some(node) = self.nodes.get_mut(id.index()) {
            if let NodeData::Text(slot) = &mut node.data {
                *slot = text.to_string();
                return;
            }
        }
        if self.nodes.get(id.index()).is_none() {
            return;
        }
        let old_children = std::mem::take(&mut self.nodes[id.index()].children);
        for child in old_children {
            if let Some(node) = self.nodes.get_mut(child.index()) {
                node.parent = None;
            }
        }
        if !text.is_empty() {
            let text_id = self.create_text(text);
            let _ = self.append(id, text_id);
        }
    }

    /// Make `child` the last child of `parent`.
    ///
    /// Both ids must already be in this document. On success the child is
    /// detached from its previous parent. [`TreeError::Missing`] means an id
    /// is out of range. [`TreeError::Cycle`] means the child is the parent or
    /// an ancestor of the parent. The error leaves the tree unchanged.
    pub fn append(&mut self, parent: NodeId, child: NodeId) -> Result<(), TreeError> {
        if self.nodes.get(parent.index()).is_none() || self.nodes.get(child.index()).is_none() {
            return Err(TreeError::Missing);
        }
        if parent == child {
            return Err(TreeError::Cycle);
        }
        let mut cursor = Some(parent);
        while let Some(id) = cursor {
            if id == child {
                return Err(TreeError::Cycle);
            }
            cursor = self.nodes[id.index()].parent;
        }
        self.detach(child);
        self.nodes[child.index()].parent = Some(parent);
        self.nodes[parent.index()].children.push(child);
        Ok(())
    }

    /// Owned trimmed text of the first `title` element. Empty if there is none.
    pub fn title(&self) -> String {
        let Some(id) = self.find_element(self.root(), "title") else {
            return String::new();
        };
        self.text_content(id).trim().to_string()
    }

    /// Replace the first `title` element's text, or create one under `head`.
    /// `title` is copied.
    pub fn set_title(&mut self, title: &str) {
        let title_id = match self.find_element(self.root(), "title") {
            Some(id) => id,
            None => {
                let id = self.create_element("title", Vec::new());
                let _ = self.append(self.head, id);
                id
            }
        };
        self.set_text_content(title_id, title);
    }

    /// Serialize the live tree as HTML, starting at the `html` element.
    ///
    /// Detached nodes are left out. Text and attribute values are escaped.
    /// The parser decodes those escapes, so a later `parse_html` of this
    /// string restores the text. Comments are not stored on the tree, so they
    /// are not written back.
    pub fn to_html(&self) -> String {
        let mut out = String::from("<!DOCTYPE html>");
        self.write_html(self.html, &mut out);
        out
    }

    /// Write `id` and its live children. The tag string is copied before the
    /// child walk so the recursive borrow can use `self` again.
    fn write_html(&self, id: NodeId, out: &mut String) {
        match self.get(id).data() {
            NodeData::Text(text) => push_escaped(out, text, false),
            NodeData::Document => {
                let kids: Vec<NodeId> = self.children(id).to_vec();
                for child in kids {
                    self.write_html(child, out);
                }
            }
            NodeData::Element { tag, attrs } => {
                let tag = tag.clone();
                let attrs = attrs.clone();
                out.push('<');
                out.push_str(&tag);
                for (name, value) in &attrs {
                    out.push(' ');
                    out.push_str(name);
                    out.push_str("=\"");
                    push_escaped(out, value, true);
                    out.push('"');
                }
                let kids: Vec<NodeId> = self.children(id).to_vec();
                if kids.is_empty() && is_void_tag(&tag) {
                    out.push('>');
                    return;
                }
                out.push('>');
                for child in kids {
                    self.write_html(child, out);
                }
                out.push_str("</");
                out.push_str(&tag);
                out.push('>');
            }
        }
    }

    /// First element in tree order whose `id` attribute equals `target_id`.
    ///
    /// An empty `target_id` is `None`. The match is case-sensitive. The id is
    /// copied; the document is borrowed.
    pub fn get_element_by_id(&self, target_id: &str) -> Option<NodeId> {
        if target_id.is_empty() {
            return None;
        }
        self.find_by_id(self.root(), target_id)
    }

    /// First element in tree order that matches a simple selector.
    ///
    /// Accepted forms: `tag`, `#id`, `.class`, `tag#id`, `tag.class`.
    /// Whitespace is trimmed. An empty selector is `None`. Combinators are
    /// not supported; a descendant selector does not match. `None` if nothing matches.
    pub fn query_selector(&self, selector: &str) -> Option<NodeId> {
        let selector = selector.trim();
        if selector.is_empty() {
            return None;
        }
        self.find_by_simple_selector(self.root(), selector)
    }

    /// Every element matching [`Self::query_selector`]'s selector grammar,
    /// in tree order. An empty selector yields an empty vec. The ids are owned.
    pub fn query_selector_all(&self, selector: &str) -> Vec<NodeId> {
        let selector = selector.trim();
        if selector.is_empty() {
            return Vec::new();
        }
        let mut matches = Vec::new();
        self.collect_by_simple_selector(self.root(), selector, &mut matches);
        matches
    }

    /// First descendant of `scope` in tree order that matches `selector`.
    pub fn query_selector_within(&self, scope: NodeId, selector: &str) -> Option<NodeId> {
        let selector = selector.trim();
        if selector.is_empty() || self.nodes.get(scope.index()).is_none() {
            return None;
        }
        for child in self.children(scope) {
            if let Some(found) = self.find_by_simple_selector(*child, selector) {
                return Some(found);
            }
        }
        None
    }

    /// Every descendant of `scope` in tree order that matches `selector`.
    pub fn query_selector_all_within(&self, scope: NodeId, selector: &str) -> Vec<NodeId> {
        let selector = selector.trim();
        if selector.is_empty() || self.nodes.get(scope.index()).is_none() {
            return Vec::new();
        }
        let mut matches = Vec::new();
        for child in self.children(scope) {
            self.collect_by_simple_selector(*child, selector, &mut matches);
        }
        matches
    }

    /// True when `id` matches `selector` (compound, attribute, combinator, or comma list).
    pub fn matches_selector(&self, id: NodeId, selector: &str) -> bool {
        let selector = selector.trim();
        if selector.is_empty() || self.nodes.get(id.index()).is_none() {
            return false;
        }
        self.node_matches_selector(id, selector)
    }

    /// Nearest element ancestor of `id` (including `id` itself) matching `selector`.
    pub fn closest(&self, id: NodeId, selector: &str) -> Option<NodeId> {
        let selector = selector.trim();
        if selector.is_empty() {
            return None;
        }
        let mut cursor = Some(id);
        while let Some(curr) = cursor {
            let node = self.nodes.get(curr.index())?;
            if node.data().tag().is_some() && self.node_matches_selector(curr, selector) {
                return Some(curr);
            }
            cursor = node.parent();
        }
        None
    }

    /// Insert `new_child` under `parent` immediately before `reference_child`
    /// (or at the end when `reference_child` is `None`).
    pub fn insert_before(
        &mut self,
        parent: NodeId,
        new_child: NodeId,
        reference_child: Option<NodeId>,
    ) -> Result<NodeId, TreeError> {
        let Some(ref_id) = reference_child else {
            self.append(parent, new_child)?;
            return Ok(new_child);
        };
        if self.nodes.get(parent.index()).is_none()
            || self.nodes.get(new_child.index()).is_none()
            || self.nodes.get(ref_id.index()).is_none()
        {
            return Err(TreeError::Missing);
        }
        if self.nodes[ref_id.index()].parent != Some(parent) {
            return Err(TreeError::Missing);
        }
        if parent == new_child {
            return Err(TreeError::Cycle);
        }
        let mut cursor = Some(parent);
        while let Some(id) = cursor {
            if id == new_child {
                return Err(TreeError::Cycle);
            }
            cursor = self.nodes[id.index()].parent;
        }
        if new_child == ref_id {
            return Ok(new_child);
        }
        self.detach(new_child);
        self.nodes[new_child.index()].parent = Some(parent);
        let children = &mut self.nodes[parent.index()].children;
        if let Some(pos) = children.iter().position(|&c| c == ref_id) {
            children.insert(pos, new_child);
        } else {
            children.push(new_child);
        }
        Ok(new_child)
    }

    /// Detach `child` from `parent` if `child`'s parent is `parent`.
    pub fn remove_child(&mut self, parent: NodeId, child: NodeId) -> Result<NodeId, TreeError> {
        if self.nodes.get(parent.index()).is_none() || self.nodes.get(child.index()).is_none() {
            return Err(TreeError::Missing);
        }
        if self.nodes[child.index()].parent != Some(parent) {
            return Err(TreeError::Missing);
        }
        self.remove_node(child);
        Ok(child)
    }

    /// Replace `old_child` under `parent` with `new_child` and return `old_child`.
    pub fn replace_child(
        &mut self,
        parent: NodeId,
        new_child: NodeId,
        old_child: NodeId,
    ) -> Result<NodeId, TreeError> {
        self.insert_before(parent, new_child, Some(old_child))?;
        self.remove_child(parent, old_child)
    }

    /// Clone `id` into a new detached node in the same document. When `deep` is
    /// true, also clones its descendant subtree up to [`MAX_DOM_NODES`].
    pub fn clone_node(&mut self, id: NodeId, deep: bool) -> Option<NodeId> {
        let data = self.nodes.get(id.index())?.data().clone();
        if matches!(data, NodeData::Document) || self.nodes.len() >= MAX_DOM_NODES {
            return None;
        }
        let new_id = self.alloc(data);
        if deep {
            let children: Vec<NodeId> = self.children(id).to_vec();
            for child in children {
                self.clone_same_doc_subtree(child, new_id);
            }
        }
        Some(new_id)
    }

    fn clone_same_doc_subtree(&mut self, src_id: NodeId, dst_parent: NodeId) {
        if self.nodes.len() >= MAX_DOM_NODES {
            return;
        }
        let Some(data) = self.nodes.get(src_id.index()).map(|n| n.data().clone()) else {
            return;
        };
        if matches!(data, NodeData::Document) {
            return;
        }
        let new_node = self.alloc(data);
        if self.append(dst_parent, new_node).is_ok() {
            let children: Vec<NodeId> = self.children(src_id).to_vec();
            for child in children {
                self.clone_same_doc_subtree(child, new_node);
            }
        }
    }

    /// Direct element children of `id` in tree order.
    pub fn element_children(&self, id: NodeId) -> Vec<NodeId> {
        let Some(node) = self.nodes.get(id.index()) else {
            return Vec::new();
        };
        node.children
            .iter()
            .copied()
            .filter(|cid| self.get(*cid).data().tag().is_some())
            .collect()
    }

    /// First direct element child of `id`.
    pub fn first_element_child(&self, id: NodeId) -> Option<NodeId> {
        self.element_children(id).into_iter().next()
    }

    /// Last direct element child of `id`.
    pub fn last_element_child(&self, id: NodeId) -> Option<NodeId> {
        self.element_children(id).into_iter().last()
    }

    /// Next sibling of `id` that is an element.
    pub fn next_element_sibling(&self, id: NodeId) -> Option<NodeId> {
        let parent = self.nodes.get(id.index())?.parent?;
        let siblings = &self.nodes.get(parent.index())?.children;
        let pos = siblings.iter().position(|&cid| cid == id)?;
        siblings[pos + 1..]
            .iter()
            .copied()
            .find(|&cid| self.get(cid).data().tag().is_some())
    }

    /// Previous sibling of `id` that is an element.
    pub fn previous_element_sibling(&self, id: NodeId) -> Option<NodeId> {
        let parent = self.nodes.get(id.index())?.parent?;
        let siblings = &self.nodes.get(parent.index())?.children;
        let pos = siblings.iter().position(|&cid| cid == id)?;
        siblings[..pos]
            .iter()
            .rev()
            .copied()
            .find(|&cid| self.get(cid).data().tag().is_some())
    }

    /// Parent of `id` if that parent is an element node.
    pub fn parent_element(&self, id: NodeId) -> Option<NodeId> {
        let parent = self.nodes.get(id.index())?.parent?;
        self.get(parent).data().tag().map(|_| parent)
    }

    /// True when `descendant` is `ancestor` or a descendant of `ancestor`.
    pub fn contains_node(&self, ancestor: NodeId, descendant: NodeId) -> bool {
        if self.nodes.get(ancestor.index()).is_none()
            || self.nodes.get(descendant.index()).is_none()
        {
            return false;
        }
        let mut cursor = Some(descendant);
        while let Some(curr) = cursor {
            if curr == ancestor {
                return true;
            }
            cursor = self.nodes[curr.index()].parent;
        }
        false
    }

    /// Serialize the children of `id` as an HTML fragment.
    pub fn inner_html(&self, id: NodeId) -> String {
        let Some(node) = self.nodes.get(id.index()) else {
            return String::new();
        };
        let kids: Vec<NodeId> = node.children.clone();
        let mut out = String::new();
        for child in kids {
            self.write_html(child, &mut out);
        }
        out
    }

    /// Serialize `id` and its subtree as an HTML fragment.
    pub fn outer_html(&self, id: NodeId) -> String {
        if self.nodes.get(id.index()).is_none() {
            let _ = self;
            return String::new();
        }
        let mut out = String::new();
        self.write_html(id, &mut out);
        out
    }

    fn node_matches_selector(&self, id: NodeId, selector: &str) -> bool {
        for branch in split_selector_commas(selector) {
            if self.node_matches_combinator_chain(id, branch) {
                return true;
            }
        }
        false
    }

    fn node_matches_combinator_chain(&self, id: NodeId, chain: &str) -> bool {
        let steps = parse_combinator_chain(chain);
        let Some((last_comb, last_compound)) = steps.last() else {
            return false;
        };
        let _ = last_comb;
        if !matches_simple_selector(self.get(id).data(), last_compound) {
            return false;
        }
        if steps.len() == 1 {
            return true;
        }
        self.match_ancestor_steps(id, &steps[..steps.len() - 1], steps[steps.len() - 1].0)
    }

    fn match_ancestor_steps(
        &self,
        current_id: NodeId,
        remaining: &[(SelectorCombinator, String)],
        combinator_to_current: SelectorCombinator,
    ) -> bool {
        let Some((prev_comb, target_compound)) = remaining.last() else {
            return true;
        };
        let rest = &remaining[..remaining.len() - 1];
        match combinator_to_current {
            SelectorCombinator::Child => {
                let Some(parent) = self.get(current_id).parent() else {
                    return false;
                };
                if !matches_simple_selector(self.get(parent).data(), target_compound) {
                    return false;
                }
                if rest.is_empty() {
                    true
                } else {
                    self.match_ancestor_steps(parent, rest, *prev_comb)
                }
            }
            SelectorCombinator::Descendant => {
                let mut cursor = self.get(current_id).parent();
                while let Some(anc) = cursor {
                    if matches_simple_selector(self.get(anc).data(), target_compound)
                        && (rest.is_empty() || self.match_ancestor_steps(anc, rest, *prev_comb))
                    {
                        return true;
                    }
                    cursor = self.get(anc).parent();
                }
                false
            }
        }
    }
    #[allow(dead_code)]
    fn _noop_dom_marker(&self) {}

    /// Unlink `id` from its parent. The document node, `html`, `head`, and
    /// `body` are refused. The slot and its descendants stay allocated.
    /// Descendants keep their parent pointers.
    pub fn remove_node(&mut self, id: NodeId) {
        if id == self.root() || id == self.html || id == self.head || id == self.body {
            return;
        }
        self.detach(id);
    }

    /// Replace the children of `id` with a parse of `html_fragment`.
    ///
    /// The fragment is parsed as a document and the new `body` children are
    /// cloned under `id`. Cloning stops at [`MAX_DOM_NODES`]. A missing `id`
    /// is a no-op. Blank markup only detaches the old children. Old slots
    /// stay allocated. `html_fragment` is borrowed.
    pub fn set_inner_html(&mut self, id: NodeId, html_fragment: &str) {
        if self.nodes.get(id.index()).is_none() {
            return;
        }
        let old_children = std::mem::take(&mut self.nodes[id.index()].children);
        for child in old_children {
            if let Some(node) = self.nodes.get_mut(child.index()) {
                node.parent = None;
            }
        }
        if html_fragment.trim().is_empty() {
            return;
        }
        let parsed = parse_html(html_fragment);
        let body_children: Vec<NodeId> = parsed.children(parsed.body()).to_vec();
        for src_child in body_children {
            self.clone_subtree_into(&parsed, src_child, id);
        }
    }

    /// Copy one node from a parsed fragment onto `dst_parent`, then its children.
    ///
    /// `Document` nodes are skipped. Past [`MAX_DOM_NODES`] the copy stops and
    /// the rest of that subtree is dropped. A failed [`Self::append`] also
    /// drops the new node's children. `set_inner_html` uses this to move the
    /// fragment's body children into the live tree.
    fn clone_subtree_into(&mut self, src_doc: &Document, src_id: NodeId, dst_parent: NodeId) {
        // Cloning stops at the same node cap as the parser. The rest of the subtree is dropped.
        if self.nodes.len() >= MAX_DOM_NODES {
            return;
        }
        let new_node = match src_doc.get(src_id).data() {
            NodeData::Text(t) => self.create_text(t),
            NodeData::Element { tag, attrs } => self.create_element(tag, attrs.clone()),
            NodeData::Document => return,
        };
        if self.append(dst_parent, new_node).is_ok() {
            for child in src_doc.children(src_id) {
                self.clone_subtree_into(src_doc, *child, new_node);
            }
        }
    }

    /// Owned text of every `style` element, joined by newlines, in tree order.
    /// A `style` element's descendants are not scanned separately.
    pub fn embedded_css(&self) -> String {
        let mut blocks = Vec::new();
        self.collect_style(self.root(), &mut blocks);
        blocks.join("\n")
    }

    /// Owned text of every JavaScript `script` element whose text is not only
    /// whitespace. `type="application/json"` and other non-script types are
    /// skipped. External `src` scripts are not included. See
    /// [`Self::external_scripts_with_integrity`].
    pub fn scripts(&self) -> Vec<String> {
        let mut blocks = Vec::new();
        self.collect_scripts(self.root(), &mut blocks);
        blocks
    }

    /// Every stylesheet `<link>` and `<style>` element, in tree order.
    ///
    /// Links follow the same rules as [`Self::stylesheet_links_with_integrity`]
    /// and styles the same rules as [`Self::embedded_css`]: an empty `style`
    /// element is skipped. Each entry keeps its `media` attribute, so the
    /// cascade can see document order and media together.
    pub fn style_sheets(&self) -> Vec<StyleSheetSource> {
        let mut sheets = Vec::new();
        self.collect_style_sheets(self.root(), &mut sheets);
        sheets
    }

    /// Owned `href` of every stylesheet `<link>`, in tree order.
    /// Integrity metadata is dropped. See [`Self::stylesheet_links_with_integrity`].
    pub fn stylesheet_links(&self) -> Vec<String> {
        self.stylesheet_links_with_integrity()
            .into_iter()
            .map(|(href, _)| href)
            .collect()
    }

    /// Stylesheet links as owned `(href, integrity)`.
    ///
    /// A link counts when `rel` contains the token `stylesheet`. `integrity`
    /// is `None` when the attribute is absent. Empty `href` values are skipped.
    pub fn stylesheet_links_with_integrity(&self) -> Vec<(String, Option<String>)> {
        let mut links = Vec::new();
        self.collect_stylesheet_links(self.root(), &mut links);
        links
    }

    /// External scripts as owned `(src, integrity)`, in tree order.
    /// A script with an empty or missing `src` is skipped. Inline script text
    /// is not included.
    pub fn external_scripts_with_integrity(&self) -> Vec<(String, Option<String>)> {
        let mut scripts = Vec::new();
        self.collect_external_scripts(self.root(), &mut scripts);
        scripts
    }

    /// `<img>` nodes that have a non-empty `src`, as owned `(id, src)` pairs.
    /// The image bytes are not decoded here.
    pub fn images(&self) -> Vec<(NodeId, String)> {
        let mut imgs = Vec::new();
        self.collect_images(self.root(), &mut imgs);
        imgs
    }

    /// `<video>` nodes as owned `(id, src)` pairs.
    ///
    /// Uses the element's `src` when it is non-empty. Otherwise uses the first
    /// child `<source src>`. Videos with neither are omitted. Bytes are not decoded.
    pub fn videos(&self) -> Vec<(NodeId, String)> {
        let mut vids = Vec::new();
        self.collect_videos(self.root(), &mut vids);
        vids
    }

    /// Join every `<meta http-equiv="Content-Security-Policy">` `content`
    /// value with `"; "`. `None` when no such meta element has content.
    /// The string is owned.
    pub fn meta_csp(&self) -> Option<String> {
        let mut out = Vec::new();
        self.collect_meta_csp(self.root(), &mut out);
        if out.is_empty() {
            None
        } else {
            Some(out.join("; "))
        }
    }

    /// Owned concatenation of this node's text and its descendants' text.
    /// A missing id panics through [`Self::get`]. Element tags are not included.
    pub fn text_content(&self, id: NodeId) -> String {
        let mut out = String::new();
        self.collect_text(id, &mut out);
        out
    }

    /// Append a node with no parent. The id is the index in `nodes`.
    /// Callers must check [`MAX_DOM_NODES`] before this. The slot is not reused
    /// after [`Self::detach`].
    fn alloc(&mut self, data: NodeData) -> NodeId {
        let id = NodeId(self.nodes.len() as u32);
        self.nodes.push(Node {
            id,
            parent: None,
            children: Vec::new(),
            data,
        });
        id
    }

    /// Unlink `child` from its parent. The node stays in `nodes`.
    /// A node that already has no parent is left as it is.
    fn detach(&mut self, child: NodeId) {
        let Some(parent) = self.nodes[child.index()].parent else {
            return;
        };
        self.nodes[parent.index()]
            .children
            .retain(|id| *id != child);
        self.nodes[child.index()].parent = None;
    }

    /// Depth-first search for the first element whose tag equals `tag`.
    /// `tag` must already be lowercase. Returns `id` itself when it matches.
    fn find_element(&self, id: NodeId, tag: &str) -> Option<NodeId> {
        if self.get(id).data().tag() == Some(tag) {
            return Some(id);
        }
        for child in self.children(id) {
            if let Some(found) = self.find_element(*child, tag) {
                return Some(found);
            }
        }
        None
    }

    /// Depth-first search for the first node whose `id` attribute equals `target_id`.
    /// The compare is case-sensitive. The first match in tree order wins.
    fn find_by_id(&self, id: NodeId, target_id: &str) -> Option<NodeId> {
        if self.get(id).data().id() == Some(target_id) {
            return Some(id);
        }
        for child in self.children(id) {
            if let Some(found) = self.find_by_id(*child, target_id) {
                return Some(found);
            }
        }
        None
    }

    /// Depth-first search using [`matches_simple_selector`]. First match wins.
    fn find_by_simple_selector(&self, id: NodeId, selector: &str) -> Option<NodeId> {
        if self.node_matches_selector(id, selector) {
            return Some(id);
        }
        for child in self.children(id) {
            if let Some(found) = self.find_by_simple_selector(*child, selector) {
                return Some(found);
            }
        }
        None
    }

    /// Push every match of `selector` under `id`, including `id`, in tree order.
    fn collect_by_simple_selector(&self, id: NodeId, selector: &str, out: &mut Vec<NodeId>) {
        if self.node_matches_selector(id, selector) {
            out.push(id);
        }
        for child in self.children(id) {
            self.collect_by_simple_selector(*child, selector, out);
        }
    }

    /// Append this node's text, then each child's text, with no separator.
    fn collect_text(&self, id: NodeId, out: &mut String) {
        if let Some(text) = self.get(id).data().text() {
            out.push_str(text);
        }
        for child in self.children(id) {
            self.collect_text(*child, out);
        }
    }

    /// Collect non-empty `style` element text. A `style` element's children are
    /// not walked again, so a nested element inside `style` is not a second block.
    fn collect_style(&self, id: NodeId, out: &mut Vec<String>) {
        if self.get(id).data().tag() == Some("style") {
            let css = self.text_content(id);
            if !css.is_empty() {
                out.push(css);
            }
            return;
        }
        for child in self.children(id) {
            self.collect_style(*child, out);
        }
    }

    /// Record `<link rel=stylesheet>` and `<style>` in tree order. A `style`
    /// element's children are not walked again.
    fn collect_style_sheets(&self, id: NodeId, out: &mut Vec<StyleSheetSource>) {
        let data = self.get(id).data();
        let media = || data.attr("media").map(|m| m.trim().to_string());
        match data.tag() {
            Some("style") => {
                let css = self.text_content(id);
                if !css.is_empty() {
                    out.push(StyleSheetSource::Style {
                        css,
                        media: media(),
                    });
                }
                return;
            }
            Some("link") => {
                let mut links = Vec::new();
                self.collect_stylesheet_links(id, &mut links);
                if let Some((href, integrity)) = links.into_iter().next() {
                    out.push(StyleSheetSource::Link {
                        href,
                        integrity,
                        media: media(),
                    });
                }
                return;
            }
            _ => {}
        }
        for child in self.children(id) {
            self.collect_style_sheets(*child, out);
        }
    }

    /// Collect inline JavaScript that is not only whitespace.
    /// A script element's children are not walked again. `src` scripts are
    /// still visited here for their text; [`Self::collect_external_scripts`]
    /// records the URL separately. A non-JavaScript `type` is skipped.
    /// `anvil-script` runs the strings this returns.
    fn collect_scripts(&self, id: NodeId, out: &mut Vec<String>) {
        let data = self.get(id).data();
        if data.tag() == Some("script") {
            if is_javascript_type(data.attr("type")) {
                let code = self.text_content(id);
                if !code.trim().is_empty() {
                    out.push(code);
                }
            }
            return;
        }
        for child in self.children(id) {
            self.collect_scripts(*child, out);
        }
    }

    /// Record `link rel=stylesheet` as `(href, integrity)` and keep walking.
    /// `rel` matches the token `stylesheet`, ignoring case. Empty `href` is skipped.
    /// `anvil-browser` fetches these URLs and appends the CSS before `anvil-style` runs.
    fn collect_stylesheet_links(&self, id: NodeId, out: &mut Vec<(String, Option<String>)>) {
        let data = self.get(id).data();
        if data.tag() == Some("link") {
            let is_stylesheet = data.attr("rel").is_some_and(|rel| {
                rel.split_ascii_whitespace()
                    .any(|token| token.eq_ignore_ascii_case("stylesheet"))
            });
            if is_stylesheet {
                if let Some(href) = data.attr("href") {
                    let href = href.trim();
                    if !href.is_empty() {
                        let integrity = data.attr("integrity").map(|s| s.trim().to_string());
                        out.push((href.to_string(), integrity));
                    }
                }
            }
        }
        for child in self.children(id) {
            self.collect_stylesheet_links(*child, out);
        }
    }

    /// Record `script src` as `(src, integrity)` and keep walking into children.
    /// Empty `src` is skipped. `anvil-browser` fetches the URL, checks SRI when
    /// the integrity string is present, then passes the body to `anvil-script`.
    fn collect_external_scripts(&self, id: NodeId, out: &mut Vec<(String, Option<String>)>) {
        let data = self.get(id).data();
        if data.tag() == Some("script") && is_javascript_type(data.attr("type")) {
            if let Some(src) = data.attr("src") {
                let src = src.trim();
                if !src.is_empty() {
                    let integrity = data.attr("integrity").map(|s| s.trim().to_string());
                    out.push((src.to_string(), integrity));
                }
            }
        }
        for child in self.children(id) {
            self.collect_external_scripts(*child, out);
        }
    }

    /// Record `img` with a usable URL. Does not decode bytes.
    ///
    /// `src` wins when it is not a `data:` image. Otherwise the first `srcset`
    /// candidate is used, then `data-src`, `data-lazy-src`, or `data-original`
    /// when that value is an http(s) or root-relative URL. A `picture` may
    /// supply the URL from an earlier `source`.
    fn collect_images(&self, id: NodeId, out: &mut Vec<(NodeId, String)>) {
        let data = self.get(id).data();
        if data.tag() == Some("img") {
            let url = image_url_from(data).or_else(|| self.picture_source_url(id));
            if let Some(url) = url {
                out.push((id, url));
            }
        }
        for child in self.children(id) {
            self.collect_images(*child, out);
        }
    }

    fn picture_source_url(&self, img: NodeId) -> Option<String> {
        let parent = self.parent_element(img)?;
        if self.get(parent).data().tag() != Some("picture") {
            return None;
        }
        for child in self.children(parent) {
            if *child == img {
                break;
            }
            let data = self.get(*child).data();
            if data.tag() == Some("source") {
                if let Some(url) = source_url_from(data) {
                    return Some(url);
                }
            }
        }
        None
    }

    /// Record one URL per `video`: its `src`, or else the first child `source src`.
    /// Nodes with neither are omitted. Bytes stay undecoded. `anvil-browser`
    /// passes the fetched body to `anvil-video`.
    fn collect_videos(&self, id: NodeId, out: &mut Vec<(NodeId, String)>) {
        let data = self.get(id).data();
        if data.tag() == Some("video") {
            if let Some(src) = data.attr("src") {
                let src = src.trim();
                if !src.is_empty() {
                    out.push((id, src.to_string()));
                }
            } else {
                for child in self.children(id) {
                    let cdata = self.get(*child).data();
                    if cdata.tag() == Some("source") {
                        if let Some(src) = cdata.attr("src") {
                            let src = src.trim();
                            if !src.is_empty() {
                                out.push((id, src.to_string()));
                                break;
                            }
                        }
                    }
                }
            }
        }
        for child in self.children(id) {
            self.collect_videos(*child, out);
        }
    }

    /// Record `meta http-equiv=Content-Security-Policy` content values.
    /// The compare on `http-equiv` ignores case. Empty content is skipped.
    /// `anvil-net` parses the joined string as a content security policy.
    fn collect_meta_csp(&self, id: NodeId, out: &mut Vec<String>) {
        let data = self.get(id).data();
        if data.tag() == Some("meta") {
            let is_csp = data
                .attr("http-equiv")
                .is_some_and(|v| v.trim().eq_ignore_ascii_case("content-security-policy"));
            if is_csp {
                if let Some(content) = data.attr("content") {
                    let trimmed = content.trim();
                    if !trimmed.is_empty() {
                        out.push(trimmed.to_string());
                    }
                }
            }
        }
        for child in self.children(id) {
            self.collect_meta_csp(*child, out);
        }
    }

    /// Walk form controls and push `(name, value)` pairs for a submit body.
    ///
    /// Skips controls with an empty `name`. Checkboxes and radios contribute
    /// only when checked (`value`, or `on` when `value` is absent). `submit`,
    /// `button`, and `reset` inputs are skipped. Other inputs use the `value`
    /// attribute. A `textarea` uses [`Self::input_value`]. A `select` uses the
    /// selected option. `anvil-browser` encodes the pairs for `anvil-net` POST.
    fn collect_form_pairs(&self, id: NodeId, out: &mut Vec<(String, String)>) {
        let data = self.get(id).data();
        match data.tag() {
            Some("input") => {
                if let Some(name) = data.attr("name") {
                    let name = name.trim();
                    if !name.is_empty() {
                        let kind = data
                            .attr("type")
                            .unwrap_or("text")
                            .trim()
                            .to_ascii_lowercase();
                        match kind.as_str() {
                            "checkbox" | "radio" => {
                                if self.is_checked(id) {
                                    let val = data.attr("value").unwrap_or("on");
                                    out.push((name.to_string(), val.to_string()));
                                }
                            }
                            "submit" | "button" | "reset" => {}
                            _ => {
                                let val = data.attr("value").unwrap_or("");
                                out.push((name.to_string(), val.to_string()));
                            }
                        }
                    }
                }
            }
            Some("textarea") => {
                if let Some(name) = data.attr("name") {
                    let name = name.trim();
                    if !name.is_empty() {
                        out.push((name.to_string(), self.input_value(id)));
                    }
                }
            }
            Some("select") => {
                if let Some(name) = data.attr("name") {
                    let name = name.trim();
                    if !name.is_empty() {
                        out.push((name.to_string(), self.select_value(id)));
                    }
                }
            }
            _ => {}
        }
        for child in self.children(id) {
            self.collect_form_pairs(*child, out);
        }
    }
}

/// Encode one form field for `application/x-www-form-urlencoded`.
///
/// `input` is borrowed. The result is owned. Unreserved ASCII stays as-is.
/// A space becomes `+`. Every other byte becomes `%XX` using uppercase hex.
pub fn urlencode_component(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for byte in input.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char);
            }
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// Match one simple selector against borrowed node data.
///
/// Forms: `#id`, `.class`, `tag#id`, `tag.class`, or a tag name. A leading
/// empty tag (`#id` is handled first) is allowed in the split forms. Text and
/// the document node return false. Class and id compares are case-sensitive.
/// The tag compare ignores ASCII case.
/// Escape `&`, `<`, and `>` in text. Attribute values also escape `"`.
fn push_escaped(out: &mut String, text: &str, attr: bool) {
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' if attr => out.push_str("&quot;"),
            _ => out.push(ch),
        }
    }
}

/// Tags that have no end tag in the serializer when they have no children.
fn is_void_tag(tag: &str) -> bool {
    matches!(
        tag,
        "area"
            | "base"
            | "br"
            | "col"
            | "embed"
            | "hr"
            | "img"
            | "input"
            | "link"
            | "meta"
            | "param"
            | "source"
            | "track"
            | "wbr"
    )
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SelectorCombinator {
    Descendant,
    Child,
}

fn split_selector_commas(selector: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0usize;
    let mut bracket_depth = 0i32;
    let mut quote: Option<char> = None;
    for (idx, ch) in selector.char_indices() {
        if let Some(q) = quote {
            if ch == q {
                quote = None;
            }
            continue;
        }
        match ch {
            '"' | '\'' => quote = Some(ch),
            '[' => bracket_depth += 1,
            ']' => bracket_depth = (bracket_depth - 1).max(0),
            ',' if bracket_depth == 0 => {
                let slice = selector[start..idx].trim();
                if !slice.is_empty() {
                    out.push(slice);
                }
                start = idx + 1;
            }
            _ => {}
        }
    }
    let tail = selector[start..].trim();
    if !tail.is_empty() {
        out.push(tail);
    }
    out
}

fn parse_combinator_chain(chain: &str) -> Vec<(SelectorCombinator, String)> {
    let mut steps = Vec::new();
    let mut current = String::new();
    let mut pending = SelectorCombinator::Descendant;
    let mut bracket_depth = 0i32;
    let mut quote: Option<char> = None;

    for ch in chain.chars() {
        if let Some(q) = quote {
            current.push(ch);
            if ch == q {
                quote = None;
            }
            continue;
        }
        match ch {
            '"' | '\'' => {
                quote = Some(ch);
                current.push(ch);
            }
            '[' => {
                bracket_depth += 1;
                current.push(ch);
            }
            ']' => {
                bracket_depth = (bracket_depth - 1).max(0);
                current.push(ch);
            }
            '>' if bracket_depth == 0 => {
                let trimmed = current.trim().to_string();
                if !trimmed.is_empty() {
                    steps.push((pending, trimmed));
                    current.clear();
                }
                pending = SelectorCombinator::Child;
            }
            c if c.is_whitespace() && bracket_depth == 0 => {
                let trimmed = current.trim().to_string();
                if !trimmed.is_empty() {
                    steps.push((pending, trimmed));
                    current.clear();
                    pending = SelectorCombinator::Descendant;
                }
            }
            _ => current.push(ch),
        }
    }
    let tail = current.trim().to_string();
    if !tail.is_empty() {
        steps.push((pending, tail));
    }
    steps
}

fn matches_simple_selector(data: &NodeData, selector: &str) -> bool {
    let selector = selector.trim();
    if selector.is_empty() {
        return false;
    }
    let Some(actual_tag) = data.tag() else {
        return false;
    };
    if selector == "*" {
        return true;
    }
    // Extract attribute selectors `[name]` or `[name="value"]`
    let mut base = String::new();
    let mut attrs_to_check: Vec<String> = Vec::new();
    let mut rest = selector;
    while let Some(open) = rest.find('[') {
        base.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        let Some(close) = after.find(']') else {
            return false;
        };
        attrs_to_check.push(after[..close].trim().to_string());
        rest = &after[close + 1..];
    }
    base.push_str(rest);

    for attr_filter in &attrs_to_check {
        if let Some((attr_name, raw_val)) = attr_filter.split_once('=') {
            let name = attr_name.trim().trim_end_matches(['~', '|', '^', '$', '*']);
            let expected = raw_val.trim().trim_matches('"').trim_matches('\'');
            let Some(actual) = data.attr(name) else {
                return false;
            };
            if attr_filter.contains("*=") {
                if !actual.contains(expected) {
                    return false;
                }
            } else if attr_filter.contains("^=") {
                if !actual.starts_with(expected) {
                    return false;
                }
            } else if attr_filter.contains("$=") {
                if !actual.ends_with(expected) {
                    return false;
                }
            } else if actual != expected {
                return false;
            }
        } else if data.attr(attr_filter.trim()).is_none() {
            return false;
        }
    }

    let base = base.trim();
    if base.is_empty() || base == "*" {
        return true;
    }

    // Parse compound `tag#id.class1.class2`
    let mut tag_req: Option<&str> = None;
    let mut id_req: Option<&str> = None;
    let mut classes_req: Vec<&str> = Vec::new();

    let first_sep = base.find(['#', '.']).unwrap_or(base.len());
    if first_sep > 0 {
        let t = &base[..first_sep];
        if t != "*" {
            tag_req = Some(t);
        }
    }
    let mut rem = &base[first_sep..];
    while !rem.is_empty() {
        let kind = rem.as_bytes()[0];
        let after = &rem[1..];
        let next_sep = after.find(['#', '.']).unwrap_or(after.len());
        let token = &after[..next_sep];
        if token.is_empty() {
            return false;
        }
        if kind == b'#' {
            id_req = Some(token);
        } else if kind == b'.' {
            classes_req.push(token);
        }
        rem = &after[next_sep..];
    }

    if let Some(t) = tag_req {
        if !actual_tag.eq_ignore_ascii_case(t) {
            return false;
        }
    }
    if let Some(req_id) = id_req {
        if data.id() != Some(req_id) {
            return false;
        }
    }
    for cls in classes_req {
        if !data.has_class(cls) {
            return false;
        }
    }
    true
}

#[allow(dead_code)]
fn _legacy_matches_simple_selector(data: &NodeData, selector: &str) -> bool {
    let Some(actual_tag) = data.tag() else {
        return false;
    };
    if let Some(id_name) = selector.strip_prefix('#') {
        return data.id() == Some(id_name);
    }
    if let Some(class_name) = selector.strip_prefix('.') {
        return data.has_class(class_name);
    }
    if let Some((tag_part, id_name)) = selector.split_once('#') {
        return (tag_part.is_empty() || actual_tag.eq_ignore_ascii_case(tag_part))
            && data.id() == Some(id_name);
    }
    if let Some((tag_part, class_name)) = selector.split_once('.') {
        return (tag_part.is_empty() || actual_tag.eq_ignore_ascii_case(tag_part))
            && data.has_class(class_name);
    }
    actual_tag.eq_ignore_ascii_case(selector)
}

fn image_url_from(data: &NodeData) -> Option<String> {
    let src = data.attr("src").unwrap_or("").trim();
    if let Some(url) = usable_image_url(src) {
        if !url.starts_with("data:") {
            return Some(url);
        }
    }
    if let Some(set) = data.attr("srcset") {
        if let Some(url) = first_srcset_url(set) {
            return Some(url);
        }
    }
    for name in ["data-src", "data-lazy-src", "data-original"] {
        if let Some(lazy) = data.attr(name) {
            let lazy = lazy.trim();
            if lazy.starts_with("https://") || lazy.starts_with("http://") || lazy.starts_with('/')
            {
                return Some(lazy.to_string());
            }
        }
    }
    usable_image_url(src)
}

fn source_url_from(data: &NodeData) -> Option<String> {
    if let Some(set) = data.attr("srcset") {
        if let Some(url) = first_srcset_url(set) {
            return Some(url);
        }
    }
    data.attr("src").and_then(usable_image_url)
}

fn first_srcset_url(srcset: &str) -> Option<String> {
    for part in srcset.split(',') {
        let url = part.split_whitespace().next().unwrap_or("");
        if let Some(url) = usable_image_url(url) {
            if !url.starts_with("data:") {
                return Some(url);
            }
        }
    }
    None
}

fn usable_image_url(raw: &str) -> Option<String> {
    let url = raw.trim();
    if url.is_empty() || url.to_ascii_lowercase().starts_with("javascript:") {
        return None;
    }
    Some(url.to_string())
}

#[cfg(test)]
mod tests {
    //! Parser caps, selectors, forms, and cycle checks against a real document.

    use super::*;

    #[test]
    fn append_refuses_a_cycle() {
        let mut doc = Document::new();
        let div = doc.create_element("div", Vec::new());
        doc.append(doc.body(), div).unwrap();
        assert_eq!(doc.append(div, doc.body()), Err(TreeError::Cycle));
    }

    #[test]
    fn reads_text_attributes_and_title() {
        let doc = parse_html(
            "<title>Hello</title><p class=\"note\">A &amp; B</p><!-- hidden --><br><span>C</span>",
        );
        assert_eq!(doc.title(), "Hello");
        let paragraph = doc
            .children(doc.body())
            .iter()
            .copied()
            .find(|id| doc.get(*id).data().tag() == Some("p"))
            .unwrap();
        assert_eq!(doc.get(paragraph).data().attr("class"), Some("note"));
        assert_eq!(doc.text_content(paragraph), "A & B");
        let again = parse_html(&doc.to_html());
        let again_p = again
            .children(again.body())
            .iter()
            .copied()
            .find(|id| again.get(*id).data().tag() == Some("p"))
            .expect("paragraph");
        assert_eq!(again.text_content(again_p), "A & B");
        assert_eq!(again.get(again_p).data().attr("class"), Some("note"));
        let tags: Vec<_> = doc
            .children(doc.body())
            .iter()
            .filter_map(|id| doc.get(*id).data().tag())
            .collect();
        assert_eq!(tags, ["p", "br", "span"]);
    }

    #[test]
    fn serializes_replaced_text() {
        let mut doc = parse_html("<p id=\"out\">secret-marker</p>");
        let id = doc.get_element_by_id("out").unwrap();
        doc.set_text_content(id, "from-worker");
        let again = parse_html(&doc.to_html());
        let id2 = again.get_element_by_id("out").unwrap();
        assert_eq!(again.text_content(id2), "from-worker");
    }

    #[test]
    fn script_text_keeps_a_less_than() {
        let doc = parse_html("<script>if (a < b) {}</script><p>Hi</p>");
        let script = doc.find_element(doc.root(), "script").unwrap();
        assert_eq!(doc.text_content(script), "if (a < b) {}");
        assert_eq!(doc.scripts(), vec!["if (a < b) {}"]);
        assert!(doc.find_element(doc.body(), "p").is_some());
    }

    #[test]
    fn reads_srcset_lazy_images_and_keeps_pre_lines() {
        let doc = parse_html(
            "<img id=\"a\" srcset=\"https://cdn.example/a.jpg 480w, https://cdn.example/b.jpg 800w\"><img id=\"b\" src=\"data:image/gif;base64,AAAA\" data-src=\"https://cdn.example/c.jpg\"><picture><source srcset=\"https://cdn.example/d.jpg 1x\"><img id=\"c\"></picture><pre>one\r\ntwo</pre>",
        );
        let urls: Vec<_> = doc.images().into_iter().map(|(_, src)| src).collect();
        assert_eq!(
            urls,
            [
                "https://cdn.example/a.jpg".to_string(),
                "https://cdn.example/c.jpg".to_string(),
                "https://cdn.example/d.jpg".to_string(),
            ]
        );
        let pre = doc.find_element(doc.body(), "pre").unwrap();
        assert_eq!(doc.text_content(pre), "one\ntwo");
    }

    #[test]
    fn decodes_numeric_entities_and_skips_json_scripts() {
        let doc = parse_html(
            "<p id=\"t\">Tom&#39;s &#x26; tea</p>
             <div id=\"player&#45;placeholder\"></div>
             <script type=\"application/json\">{\"a\":1}</script>
             <script>var ready = 1;</script>",
        );
        let text = doc.get_element_by_id("t").unwrap();
        assert_eq!(doc.text_content(text).trim(), "Tom's & tea");
        assert!(doc.get_element_by_id("player-placeholder").is_some());
        assert_eq!(doc.scripts(), vec!["var ready = 1;"]);
    }

    #[test]
    fn lists_links_and_styles_in_document_order_with_media() {
        let doc = parse_html(
            "<head><link rel=stylesheet href=a.css media=' print '><style>p{}</style>\
             <style></style><link rel=icon href=i.png></head>\
             <body><style media=screen>b{}</style><link rel='Stylesheet alternate' href=b.css></body>",
        );
        assert_eq!(
            doc.style_sheets(),
            vec![
                StyleSheetSource::Link {
                    href: "a.css".to_string(),
                    integrity: None,
                    media: Some("print".to_string()),
                },
                StyleSheetSource::Style {
                    css: "p{}".to_string(),
                    media: None,
                },
                StyleSheetSource::Style {
                    css: "b{}".to_string(),
                    media: Some("screen".to_string()),
                },
                StyleSheetSource::Link {
                    href: "b.css".to_string(),
                    integrity: None,
                    media: None,
                },
            ]
        );
    }

    #[test]
    fn collects_stylesheet_links_in_order() {
        let doc = parse_html(
            "<head>
                <link rel=\"icon\" href=\"favicon.ico\">
                <link rel=\"stylesheet\" href=\"base.css\">
                <link rel=\"alternate stylesheet\" href=\"theme.css\">
             </head>",
        );
        assert_eq!(doc.stylesheet_links(), vec!["base.css", "theme.css"]);
    }

    #[test]
    fn queries_and_mutates_elements_by_id_and_selector() {
        let mut doc = parse_html("<div id=\"banner\" class=\"card primary\">Old</div>");
        let id = doc.get_element_by_id("banner").unwrap();
        assert_eq!(doc.query_selector(".primary"), Some(id));
        assert_eq!(doc.query_selector("div.card"), Some(id));
        doc.set_text_content(id, "Updated");
        doc.set_attribute(id, "data-state", "active");
        assert_eq!(doc.text_content(id), "Updated");
        assert_eq!(doc.get(id).data().attr("data-state"), Some("active"));
    }

    #[test]
    fn serializes_form_inputs_checkboxes_and_toggles_classes() {
        let mut doc = parse_html(
            "<form id=\"search-form\" action=\"/search\" method=\"POST\">
                <input id=\"q\" type=\"text\" name=\"query\" value=\"anvil engine\">
                <input id=\"exact\" type=\"checkbox\" name=\"exact\" value=\"1\" checked>
                <button id=\"go\" type=\"submit\">Search</button>
             </form>",
        );
        let q = doc.get_element_by_id("q").unwrap();
        let exact = doc.get_element_by_id("exact").unwrap();
        let go = doc.get_element_by_id("go").unwrap();
        let form = doc.enclosing_form(go).unwrap();

        assert!(doc.is_text_input(q));
        assert!(doc.is_checkbox_input(exact));
        assert!(doc.is_submit_control(go));

        let (action, method, body) = doc.serialize_form(form);
        assert_eq!(action, "/search");
        assert_eq!(method, "POST");
        assert_eq!(body, "query=anvil+engine&exact=1");

        doc.set_input_value(q, "a&b");
        assert!(!doc.toggle_checked(exact));
        let (_, _, body2) = doc.serialize_form(form);
        assert_eq!(body2, "query=a%26b");

        assert!(doc.toggle_class(form, "submitted"));
        assert!(doc.get(form).data().has_class("submitted"));
        assert!(!doc.toggle_class(form, "submitted"));
        assert!(!doc.get(form).data().has_class("submitted"));
    }

    #[test]
    fn caps_adversarial_dom_nesting_depth_and_attribute_counts() {
        let mut deep_html = String::new();
        for _ in 0..400 {
            deep_html.push_str("<div>");
        }
        deep_html.push_str("<p id=\"leaf\">Safe Leaf</p>");
        let doc = parse_html(&deep_html);
        let leaf = doc.get_element_by_id("leaf").unwrap();
        let mut depth = 0usize;
        let mut cursor = Some(leaf);
        while let Some(curr) = cursor {
            depth += 1;
            cursor = doc.get(curr).parent();
        }
        assert!(depth <= MAX_DOM_DEPTH + 4);
    }

    #[test]
    fn parses_tables_textareas_and_cycles_select_options() {
        let mut doc = parse_html(
            "<table id=\"tbl\">
               <tr><th>Col A<th>Col B
               <tr><td>1<td>2
             </table>
             <form id=\"f\" action=\"/save\" method=\"POST\">
               <textarea id=\"bio\" name=\"bio\">Line 1\nLine 2</textarea>
               <select id=\"tier\" name=\"tier\">
                 <option value=\"free\">Free Tier
                 <option value=\"pro\" selected>Pro Tier
                 <option value=\"max\">Max Tier
               </select>
             </form>",
        );
        let tbl = doc.get_element_by_id("tbl").unwrap();
        assert_eq!(doc.children(tbl).len(), 2);

        let tier = doc.get_element_by_id("tier").unwrap();
        assert!(doc.is_select_control(tier));
        assert_eq!(doc.select_value(tier), "pro");
        assert_eq!(doc.select_display_label(tier), "Pro Tier");

        assert_eq!(doc.cycle_select_option(tier), Some("max".to_string()));
        assert_eq!(doc.cycle_select_option(tier), Some("free".to_string()));

        let form = doc.get_element_by_id("f").unwrap();
        let (_, _, payload) = doc.serialize_form(form);
        assert_eq!(payload, "bio=Line+1%0ALine+2&tier=free");
    }

    #[test]
    fn supports_combinator_attribute_selectors_and_dom_tree_mutations() {
        let mut doc = parse_html(
            "<div id=\"wrap\" class=\"panel main\">
               <ul id=\"list\">
                 <li class=\"item first\" data-id=\"10\">Alpha</li>
                 <li class=\"item second\" data-id=\"20\">Beta</li>
               </ul>
             </div>",
        );
        let wrap = doc.get_element_by_id("wrap").unwrap();
        let list = doc.get_element_by_id("list").unwrap();
        let items = doc.query_selector_all("#wrap > ul li.item");
        assert_eq!(items.len(), 2);
        assert_eq!(doc.query_selector("li[data-id=\"20\"]"), Some(items[1]));
        assert_eq!(doc.closest(items[0], "div.panel"), Some(wrap));
        assert!(doc.contains_node(wrap, items[1]));
        assert_eq!(doc.first_element_child(list), Some(items[0]));
        assert_eq!(doc.last_element_child(list), Some(items[1]));
        assert_eq!(doc.next_element_sibling(items[0]), Some(items[1]));
        assert_eq!(doc.previous_element_sibling(items[1]), Some(items[0]));

        let inserted = doc.create_element("li", vec![("class".into(), "item mid".into())]);
        doc.set_text_content(inserted, "Middle");
        doc.insert_before(list, inserted, Some(items[1])).unwrap();
        assert_eq!(
            doc.element_children(list),
            vec![items[0], inserted, items[1]]
        );

        let cloned = doc.clone_node(inserted, true).unwrap();
        doc.replace_child(list, cloned, items[1]).unwrap();
        assert_eq!(doc.element_children(list), vec![items[0], inserted, cloned]);
        doc.remove_child(list, inserted).unwrap();
        assert_eq!(doc.element_children(list), vec![items[0], cloned]);
    }

    /// Parse on a worker thread and give up after a few seconds, so a parser
    /// that stops advancing fails the test instead of hanging the run.
    fn parse_with_deadline(input: &str) -> Option<(Vec<String>, String)> {
        let input = input.to_string();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let doc = parse_html(&input);
            let tags = doc
                .children(doc.body())
                .iter()
                .filter_map(|id| doc.get(*id).data().tag().map(str::to_string))
                .collect::<Vec<_>>();
            let text = doc.text_content(doc.body());
            let _ = tx.send((tags, text));
        });
        rx.recv_timeout(std::time::Duration::from_secs(5)).ok()
    }

    #[test]
    fn stray_less_than_in_text_is_kept_and_does_not_stall() {
        let (tags, text) = parse_with_deadline("<p>1 < 2</p><p>after</p>")
            .expect("parser stalled on a '<' that does not start a tag");
        assert_eq!(tags, ["p", "p"]);
        assert!(text.contains("1 < 2"), "text was {text:?}");
        assert!(text.contains("after"));
    }

    #[test]
    fn less_than_at_odd_places_does_not_stall() {
        for input in [
            "<",
            "<<",
            "a <",
            "< b",
            "<p>x <= y && y >= z</p>",
            "<p>a<1</p>",
            "<p>a< </p>",
            "<p>a <\u{e9}</p>",
            "<div>< <<< <3 <-- <=></div>",
            "<p a=1 \u{e9}\u{4e2d}>multi-byte attribute junk</p>",
        ] {
            assert!(
                parse_with_deadline(input).is_some(),
                "parser stalled on {input:?}"
            );
        }
        let (_, text) = parse_with_deadline("<p>a <\u{e9}</p>").unwrap();
        assert!(text.contains("a <\u{e9}"), "text was {text:?}");
    }
}
