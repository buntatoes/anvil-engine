// SPDX-License-Identifier: LicenseRef-Proprietary
// Copyright (c) 2026 Buntato
//! One invalid selector in a list drops the whole rule.
//!
//! A selector that is valid but not matched by this crate yet (a child
//! combinator, a pseudo-class) does not drop the rule: it just never matches.

use anvil_dom::parse_html;
use anvil_style::{Color, compute_styles, parse_css};

/// Colour of the element with `id` after cascading `css` onto `html`.
fn color_of(html: &str, css: &str, id: &str) -> Color {
    let doc = parse_html(html);
    let styles = compute_styles(&doc, &parse_css(css));
    let node = doc.get_element_by_id(id).expect("element with that id");
    styles.get(node).color
}

const PAGE: &str = "<p id=t>x</p>";
const GREEN: &str = "#00ff00";
const RED: &str = "#ff0000";

fn green() -> Color {
    Color::parse(GREEN).unwrap()
}

fn assert_rule_dropped(selector_list: &str) {
    let css = format!("p {{ color: {GREEN} }} {selector_list} {{ color: {RED} }}");
    assert_eq!(
        color_of(PAGE, &css, "t"),
        green(),
        "rule with `{selector_list}` should be dropped"
    );
}

#[test]
fn unknown_prefixed_pseudo_element_drops_the_whole_rule() {
    assert_rule_dropped("p, p::-x-made-up-thing");
    assert_rule_dropped("p::-x-made-up-thing, p");
}

#[test]
fn unknown_pseudo_class_drops_the_whole_rule() {
    assert_rule_dropped("p, p:no-such-state");
    assert_rule_dropped("#t, :no-such-state(1)");
}

#[test]
fn empty_selector_in_the_list_drops_the_whole_rule() {
    assert_rule_dropped("p, , div");
    assert_rule_dropped("p,");
    assert_rule_dropped(", p");
}

#[test]
fn broken_syntax_drops_the_whole_rule() {
    assert_rule_dropped("p, p$");
    assert_rule_dropped("p, > div");
    assert_rule_dropped("p, div >");
    assert_rule_dropped("p, div > > span");
    assert_rule_dropped("p, [unclosed");
    assert_rule_dropped("p, #");
    assert_rule_dropped("p, .1x");
    assert_rule_dropped("p, ::hover");
}

#[test]
fn a_valid_rule_after_a_dropped_one_still_applies() {
    let css = format!("p, p:no-such-state {{ color: {RED} }} p {{ color: {GREEN} }}");
    assert_eq!(color_of(PAGE, &css, "t"), green());
}

#[test]
fn valid_but_unmatched_selectors_keep_the_rule() {
    for list in [
        "p, p:hover",
        "p, div > p",
        "p, a[href^='x'] + b ~ i",
        "p, :is(h1, h2)",
        "p, :not(.a, .b)",
        "p, :has(> img)",
        "p, li:nth-child(2n + 1 of .x)",
        "p, p::before",
        "p, p:first-line",
        "p, .md\\:flex",
    ] {
        let css = format!("{list} {{ color: {GREEN} }}");
        assert_eq!(color_of(PAGE, &css, "t"), green(), "`{list}` should apply");
    }
}

#[test]
fn not_with_a_bad_argument_drops_the_rule() {
    assert_rule_dropped("p, :not(p, )");
    assert_rule_dropped("p, :not(:no-such-state)");
}
