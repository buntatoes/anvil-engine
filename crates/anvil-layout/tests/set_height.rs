// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0-OR-Commercial
// Copyright (c) 2026 Buntato
//! A definite `height` is the height, not a minimum.

use anvil_dom::parse_html;
use anvil_layout::{LayoutBox, Rect, layout_document};
use anvil_style::{compute_styles_for_viewport, parse_css};

fn rects(html: &str, ids: &[&str]) -> Vec<Rect> {
    let page = format!("<style>body {{ margin: 0 }}</style>{html}");
    let doc = parse_html(&page);
    let styles = compute_styles_for_viewport(&doc, &parse_css(&doc.embedded_css()), 800.0, 600.0);
    let tree = layout_document(&doc, &styles, 800.0);
    ids.iter()
        .map(|id| {
            let node = doc.get_element_by_id(id).expect("element");
            let mut stack: Vec<&LayoutBox> = vec![&tree];
            while let Some(b) = stack.pop() {
                if b.node == node {
                    return b.rect;
                }
                stack.extend(b.children.iter());
            }
            panic!("no box for #{id}");
        })
        .collect()
}

#[test]
fn block_keeps_its_height_when_content_is_taller() {
    let r = rects(
        "<style>#a { width: 50px; height: 50px } #k { height: 100px } #n { height: 5px }</style>\
         <div id=a><div id=k></div></div><div id=n></div>",
        &["a", "k", "n"],
    );
    assert_eq!((r[0].width, r[0].height), (50.0, 50.0));
    assert_eq!(r[1].height, 100.0);
    // The next box follows the set height, not the content.
    assert_eq!(r[2].y, 50.0);
}

#[test]
fn padding_and_border_still_add_to_a_set_height() {
    let r = rects(
        "<style>#a { height: 20px; padding: 5px; border: 2px solid red } #k { height: 90px }</style>\
         <div id=a><div id=k></div></div>",
        &["a"],
    );
    assert_eq!(r[0].height, 34.0);
}

#[test]
fn auto_height_still_fits_the_content() {
    let r = rects(
        "<style>#k { height: 70px }</style><div id=a><div id=k></div></div>",
        &["a"],
    );
    assert_eq!(r[0].height, 70.0);
}

#[test]
fn flex_and_grid_keep_a_set_height() {
    let r = rects(
        "<style>#f { display: flex; height: 30px } #g { display: grid; height: 30px }\
         #c { display: flex; flex-direction: column; height: 30px } .i { height: 80px; width: 10px }\
         </style><div id=f><div class=i></div></div><div id=g><div class=i></div></div>\
         <div id=c><div class=i></div><div class=i></div></div>",
        &["f", "g", "c"],
    );
    assert_eq!(r[0].height, 30.0);
    assert_eq!(r[1].height, 30.0);
    assert_eq!(r[1].y, 30.0);
    assert_eq!(r[2].height, 30.0);
    assert_eq!(r[2].y, 60.0);
}

#[test]
fn hidden_overflow_box_is_its_set_size() {
    let r = rects(
        "<style>#a { width: 50px; height: 50px; overflow: hidden } #k { height: 100px }</style>\
         <div id=a><div id=k></div></div>",
        &["a"],
    );
    assert_eq!(
        (r[0].x, r[0].y, r[0].width, r[0].height),
        (0.0, 0.0, 50.0, 50.0)
    );
}
