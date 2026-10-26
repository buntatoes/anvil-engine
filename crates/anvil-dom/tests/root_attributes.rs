// SPDX-License-Identifier: LicenseRef-Proprietary
// Copyright (c) 2026 Buntato
//! Attributes written on `html`, `head`, and `body` start tags.

use anvil_dom::{MAX_ATTRS_PER_ELEMENT, NodeData, parse_html};

#[test]
fn body_attributes_are_kept() {
    let doc = parse_html(
        "<!doctype html><html><body style=\"margin:0\" class=\"page dark\"><p>x</p></body></html>",
    );
    let body = doc.get(doc.body()).data();
    assert_eq!(body.attr("style"), Some("margin:0"));
    assert!(body.has_class("dark"));
}

#[test]
fn html_and_head_attributes_are_kept() {
    let doc = parse_html("<html lang=fr dir=rtl><head data-x=1><title>t</title></head></html>");
    assert_eq!(doc.get(doc.html()).data().attr("lang"), Some("fr"));
    assert_eq!(doc.get(doc.html()).data().attr("dir"), Some("rtl"));
    assert_eq!(doc.get(doc.head()).data().attr("data-x"), Some("1"));
}

#[test]
fn a_later_tag_only_adds_new_attributes() {
    let doc = parse_html("<body id=a class=one><p>x</p><body id=b title=t>");
    let body = doc.get(doc.body()).data();
    assert_eq!(body.attr("id"), Some("a"));
    assert_eq!(body.attr("class"), Some("one"));
    assert_eq!(body.attr("title"), Some("t"));
    // The body is still the one element; the second tag made no node.
    assert_eq!(doc.children(doc.html()).len(), 2);
}

#[test]
fn names_are_matched_without_case() {
    let doc = parse_html("<BODY Class=x><body CLASS=y>");
    assert_eq!(doc.get(doc.body()).data().attr("class"), Some("x"));
}

#[test]
fn repeated_tags_stay_under_the_attribute_cap() {
    let mut html = String::new();
    for tag in 0..4 {
        html.push_str("<body");
        for i in 0..MAX_ATTRS_PER_ELEMENT {
            html.push_str(&format!(" a{tag}x{i}=v"));
        }
        html.push('>');
    }
    let doc = parse_html(&html);
    let NodeData::Element { attrs, .. } = doc.get(doc.body()).data() else {
        panic!("body is an element");
    };
    assert_eq!(attrs.len(), MAX_ATTRS_PER_ELEMENT);
}
