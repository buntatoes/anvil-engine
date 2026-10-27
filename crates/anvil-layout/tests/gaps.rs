// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0-OR-Commercial
// Copyright (c) 2026 Buntato
//! `row-gap`, `column-gap`, and `gap` in flex and grid layout.

use anvil_dom::parse_html;
use anvil_layout::{LayoutBox, NodeId, Rect, layout_document};
use anvil_style::{compute_styles_for_viewport, parse_css};

/// Lay `html` out at 400 by 300 and return the border box of each id.
fn boxes(html: &str, ids: &[&str]) -> Vec<(f32, f32, f32, f32)> {
    let page =
        format!("<style>html, body {{ margin: 0; padding: 0; font-size: 16px; }}</style>{html}");
    let doc = parse_html(&page);
    let styles = compute_styles_for_viewport(&doc, &parse_css(&doc.embedded_css()), 400.0, 300.0);
    let tree = layout_document(&doc, &styles, 400.0);
    ids.iter()
        .map(|id| {
            let node = doc
                .get_element_by_id(id)
                .unwrap_or_else(|| panic!("no #{id}"));
            let rect = find(&tree, node).unwrap_or_else(|| panic!("no box for #{id}"));
            (rect.x, rect.y, rect.width, rect.height)
        })
        .collect()
}

fn find(tree: &LayoutBox, node: NodeId) -> Option<Rect> {
    let mut stack = vec![tree];
    while let Some(b) = stack.pop() {
        if b.node == node {
            return Some(b.rect);
        }
        stack.extend(b.children.iter());
    }
    None
}

const ITEMS: &str = "<div id=a class=i></div><div id=b class=i></div><div id=c class=i></div>";

fn flex(css: &str) -> Vec<(f32, f32, f32, f32)> {
    boxes(
        &format!(
            "<style>#f {{ display: flex; {css} }} .i {{ width: 100px; height: 20px; }}</style>\
             <div id=f>{ITEMS}</div>"
        ),
        &["a", "b", "c", "f"],
    )
}

fn grid(css: &str) -> Vec<(f32, f32, f32, f32)> {
    boxes(
        &format!(
            "<style>#g {{ display: grid; grid-template-columns: 100px 100px; {css} }}\
             .i {{ height: 20px; }}</style><div id=g>{ITEMS}</div>"
        ),
        &["a", "b", "c", "g"],
    )
}

#[test]
fn flex_row_uses_column_gap_between_items() {
    let got = flex("column-gap: 10px; row-gap: 99px;");
    assert_eq!(got[0].0, 0.0);
    assert_eq!(got[1].0, 110.0);
    assert_eq!(got[2].0, 220.0);
    assert_eq!(got[3].3, 20.0);
}

#[test]
fn flex_wrap_uses_row_gap_between_lines() {
    let got = flex("flex-wrap: wrap; width: 230px; column-gap: 15px; row-gap: 7px;");
    assert_eq!((got[0].0, got[0].1), (0.0, 0.0));
    assert_eq!((got[1].0, got[1].1), (115.0, 0.0));
    assert_eq!((got[2].0, got[2].1), (0.0, 27.0));
    assert_eq!(got[3].3, 47.0);
}

#[test]
fn flex_column_uses_row_gap() {
    let got = flex("flex-direction: column; row-gap: 5px; column-gap: 99px;");
    assert_eq!(got[0].1, 0.0);
    assert_eq!(got[1].1, 25.0);
    assert_eq!(got[2].1, 50.0);
    assert_eq!(got[3].3, 70.0);
}

#[test]
fn grid_keeps_rows_and_columns_apart() {
    let got = grid("column-gap: 10px; row-gap: 30px;");
    assert_eq!((got[0].0, got[0].1), (0.0, 0.0));
    assert_eq!((got[1].0, got[1].1), (110.0, 0.0));
    assert_eq!((got[2].0, got[2].1), (0.0, 50.0));
    assert_eq!(got[3].3, 70.0);
}

#[test]
fn gap_shorthand_is_row_then_column() {
    let got = grid("gap: 30px 10px;");
    assert_eq!(got[1].0, 110.0);
    assert_eq!(got[2].1, 50.0);
    let got = grid("gap: 12px;");
    assert_eq!(got[1].0, 112.0);
    assert_eq!(got[2].1, 32.0);
    // Legacy names.
    let got = grid("grid-gap: 4px 6px;");
    assert_eq!(got[1].0, 106.0);
    assert_eq!(got[2].1, 24.0);
    let got = grid("grid-row-gap: 3px; grid-column-gap: 9px;");
    assert_eq!(got[1].0, 109.0);
    assert_eq!(got[2].1, 23.0);
}

#[test]
fn percentages_use_the_content_box() {
    // 10% of a 200px wide grid.
    let got = boxes(
        "<style>#g { display: grid; width: 200px; grid-template-columns: 1fr 1fr; \
         column-gap: 10%; } #g > div { height: 10px; }</style>\
         <div id=g><div id=a></div><div id=b></div></div>",
        &["a", "b"],
    );
    assert_eq!(got[0], (0.0, 0.0, 90.0, 10.0));
    assert_eq!(got[1], (110.0, 0.0, 90.0, 10.0));
    // A row-gap percentage needs a definite height.
    assert_eq!(grid("row-gap: 50%;")[2].1, 20.0);
    assert_eq!(grid("height: 100px; row-gap: 10%;")[2].1, 30.0);
    // Flex: column-gap against the width.
    assert_eq!(flex("width: 300px; column-gap: 10%;")[1].0, 130.0);
}

#[test]
fn font_relative_and_calc_gaps() {
    assert_eq!(flex("font-size: 20px; column-gap: 0.5em;")[1].0, 110.0);
    assert_eq!(flex("column-gap: 1rem;")[1].0, 116.0);
    assert_eq!(
        flex("width: 200px; column-gap: calc(10% + 2px);")[1].0,
        122.0
    );
    assert_eq!(grid("column-gap: calc(4px + 4px);")[1].0, 108.0);
    // A calc() that comes out negative is 0.
    assert_eq!(grid("column-gap: calc(2px - 10px);")[1].0, 100.0);
}

#[test]
fn normal_and_bad_values() {
    assert_eq!(grid("column-gap: 10px; column-gap: normal;")[1].0, 100.0);
    for bad in ["-5px", "10", "auto", "10px 20px", "red", "-1%"] {
        assert_eq!(
            grid(&format!("column-gap: 10px; column-gap: {bad};"))[1].0,
            110.0,
            "column-gap: {bad}"
        );
    }
    for bad in ["1px 2px 3px", "-1px 2px", "2px -1px", "auto"] {
        let got = grid(&format!("gap: 10px 20px; gap: {bad};"));
        assert_eq!((got[1].0, got[2].1), (120.0, 30.0), "gap: {bad}");
    }
}

#[test]
fn block_containers_ignore_gaps() {
    let got = boxes(
        "<style>#d { gap: 50px; } .i { height: 20px; }</style>\
         <div id=d><div id=a class=i></div><div id=b class=i></div></div>",
        &["a", "b"],
    );
    assert_eq!(got[1].1, 20.0);
}
