// SPDX-License-Identifier: LicenseRef-Proprietary
// Copyright (c) 2026 Buntato
//! A `style` attribute on `body` takes part in layout.

use anvil_dom::parse_html;
use anvil_layout::{LayoutBox, Rect, layout_document};
use anvil_style::{compute_styles_for_viewport, parse_css};

fn rect_of(html: &str, id: &str) -> Rect {
    let doc = parse_html(html);
    let styles = compute_styles_for_viewport(&doc, &parse_css(&doc.embedded_css()), 800.0, 600.0);
    let tree = layout_document(&doc, &styles, 800.0);
    let node = doc.get_element_by_id(id).expect("element");
    let mut stack: Vec<&LayoutBox> = vec![&tree];
    while let Some(b) = stack.pop() {
        if b.node == node {
            return b.rect;
        }
        stack.extend(b.children.iter());
    }
    panic!("no box for #{id}");
}

#[test]
fn body_style_margin_is_used() {
    let r = rect_of(
        "<!doctype html><html><body style=\"margin:0\"><div id=\"a\" style=\"height:10px\"></div></body></html>",
        "a",
    );
    assert_eq!((r.x, r.y, r.width, r.height), (0.0, 0.0, 800.0, 10.0));
    let r = rect_of(
        "<body style=\"margin: 20px\"><div id=\"a\" style=\"height:10px\"></div>",
        "a",
    );
    assert_eq!((r.x, r.y, r.width), (20.0, 20.0, 760.0));
}

#[test]
fn body_class_selects_rules() {
    let r = rect_of(
        "<style>body.flush { margin: 0 }</style><body class=flush><div id=a style=\"height:1px\"></div>",
        "a",
    );
    assert_eq!((r.x, r.y), (0.0, 0.0));
}
