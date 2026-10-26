// SPDX-License-Identifier: LicenseRef-Proprietary
// Copyright (c) 2026 Buntato
//! `!important` in author rules and inline `style` attributes.
//!
//! The order checked here: normal author declarations, then normal inline
//! ones, then important author ones, then important inline ones. Inside one
//! group, specificity decides and then source order.

use anvil_dom::parse_html;
use anvil_style::{Color, ComputedStyle, compute_styles, parse_css, parse_declarations};

/// Style of the element with `id` after cascading `css` onto `html`.
fn style_of(html: &str, css: &str, id: &str) -> ComputedStyle {
    let doc = parse_html(html);
    let styles = compute_styles(&doc, &parse_css(css));
    let node = doc.get_element_by_id(id).expect("element with that id");
    *styles.get(node)
}

fn color(hex: &str) -> Color {
    Color::parse(hex).expect("valid colour")
}

#[test]
fn important_is_taken_off_the_value() {
    let parsed = parse_declarations(
        "color: red !important; margin: 1px ! IMPORTANT; padding: 2px!Important; width: 3px",
    );
    let pairs: Vec<(&str, &str, bool)> = parsed
        .iter()
        .map(|d| (d.name.as_str(), d.value.as_str(), d.important))
        .collect();
    assert_eq!(
        pairs,
        [
            ("color", "red", true),
            ("margin", "1px", true),
            ("padding", "2px", true),
            ("width", "3px", false),
        ]
    );
}

#[test]
fn important_declaration_is_applied_instead_of_dropped() {
    let style = style_of("<p id=t>x</p>", "p { color: #00ff00 !important }", "t");
    assert_eq!(style.color, color("#00ff00"));
}

#[test]
fn important_beats_higher_specificity() {
    let style = style_of(
        "<p id=t class=c>x</p>",
        "p { color: #00ff00 !important } #t.c { color: #ff0000 }",
        "t",
    );
    assert_eq!(style.color, color("#00ff00"));
}

#[test]
fn important_beats_later_source_order() {
    let style = style_of(
        "<p id=t>x</p>",
        "p { color: #00ff00 !important } p { color: #ff0000 }",
        "t",
    );
    assert_eq!(style.color, color("#00ff00"));
}

#[test]
fn among_important_ones_specificity_then_order_decide() {
    let by_specificity = style_of(
        "<p id=t>x</p>",
        "#t { color: #00ff00 !important } p { color: #ff0000 !important }",
        "t",
    );
    assert_eq!(by_specificity.color, color("#00ff00"));
    let by_order = style_of(
        "<p id=t>x</p>",
        "p { color: #ff0000 !important } p { color: #00ff00 !important }",
        "t",
    );
    assert_eq!(by_order.color, color("#00ff00"));
}

#[test]
fn inline_normal_beats_author_normal_but_not_author_important() {
    let normal = style_of(
        "<p id=t style='color: #00ff00'>x</p>",
        "#t { color: #ff0000 }",
        "t",
    );
    assert_eq!(normal.color, color("#00ff00"));
    let important = style_of(
        "<p id=t style='color: #ff0000'>x</p>",
        "p { color: #00ff00 !important }",
        "t",
    );
    assert_eq!(important.color, color("#00ff00"));
}

#[test]
fn inline_important_beats_author_important() {
    let style = style_of(
        "<p id=t style='color: #00ff00 !important'>x</p>",
        "#t { color: #ff0000 !important }",
        "t",
    );
    assert_eq!(style.color, color("#00ff00"));
}

#[test]
fn shorthand_keeps_its_importance() {
    let style = style_of(
        "<div id=t class=c></div>",
        "div { margin: 5px !important } #t.c { margin-left: 1px; margin-top: 2px }",
        "t",
    );
    assert_eq!(style.margin.left, 5.0);
    assert_eq!(style.margin.top, 5.0);
    let longhand_wins = style_of(
        "<div id=t></div>",
        "div { margin: 5px } div { margin-left: 1px !important } div { margin: 7px }",
        "t",
    );
    assert_eq!(longhand_wins.margin.left, 1.0);
    assert_eq!(longhand_wins.margin.top, 7.0);
}

#[test]
fn important_custom_property_wins() {
    let style = style_of(
        "<p id=t>x</p>",
        "p { --c: #00ff00 !important } #t { --c: #ff0000 } p { color: var(--c) }",
        "t",
    );
    assert_eq!(style.color, color("#00ff00"));
}

#[test]
fn bang_inside_a_string_or_url_is_not_the_flag() {
    let parsed = parse_declarations(
        "content: \"a !important\"; background: url(x!important); quotes: 'b!important' !important",
    );
    let pairs: Vec<(&str, bool)> = parsed
        .iter()
        .map(|d| (d.value.as_str(), d.important))
        .collect();
    assert_eq!(
        pairs,
        [
            ("\"a !important\"", false),
            ("url(x!important)", false),
            ("'b!important'", true),
        ]
    );
}

#[test]
fn a_flag_that_is_not_last_or_has_no_value_is_not_honoured() {
    let parsed = parse_declarations("color: red !important blue; margin: !important; width: 1px !");
    let pairs: Vec<(&str, &str, bool)> = parsed
        .iter()
        .map(|d| (d.name.as_str(), d.value.as_str(), d.important))
        .collect();
    // The first and last keep their text and later fail as values. The
    // middle one has no value left and is dropped.
    assert_eq!(
        pairs,
        [
            ("color", "red !important blue", false),
            ("width", "1px !", false)
        ]
    );
    let style = style_of(
        "<p id=t>x</p>",
        "p { color: #00ff00 } p { color: #ff0000 !important blue }",
        "t",
    );
    assert_eq!(style.color, color("#00ff00"));
}

#[test]
fn important_inside_a_keyframe_is_ignored() {
    let sheet = parse_css(
        "@keyframes k { from { margin-left: 1px !important; margin-top: 2px } to { margin-top: 3px } }",
    );
    let names: Vec<&str> = sheet.keyframes[0].frames[0]
        .declarations
        .iter()
        .map(|d| d.name.as_str())
        .collect();
    assert_eq!(names, ["margin-top"]);
}
