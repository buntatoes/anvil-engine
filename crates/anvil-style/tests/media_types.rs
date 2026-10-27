// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0-OR-Commercial
// Copyright (c) 2026 Buntato
//! Media types, `not`, `only`, `and`, comma lists, and nested `@media`.
//!
//! The page is always a screen, so print-only rules must never apply.

use anvil_dom::parse_html;
use anvil_style::{Color, MediaQuery, compute_styles_with_viewport, parse_css};

fn green() -> Color {
    Color::parse("#00ff00").unwrap()
}

/// Colour of `#t` at `width` after `p { color: green }` and then `css`.
fn color_at(css: &str, width: f32) -> Color {
    let doc = parse_html("<p id=t>x</p>");
    let sheet = parse_css(&format!("p {{ color: #00ff00 }} {css}"));
    let styles = compute_styles_with_viewport(&doc, &sheet, width);
    styles.get(doc.get_element_by_id("t").unwrap()).color
}

fn applies(media: &str, width: f32) -> bool {
    color_at(
        &format!("@media {media} {{ p {{ color: #ff0000 }} }}"),
        width,
    ) != green()
}

#[test]
fn print_only_rules_do_not_leak_onto_the_screen() {
    assert!(!applies("print", 960.0));
    assert!(!applies("PRINT", 960.0));
    assert!(!applies("print and (min-width: 1px)", 960.0));
    assert!(!applies("only print", 960.0));
}

#[test]
fn screen_and_all_match() {
    assert!(applies("screen", 960.0));
    assert!(applies("all", 960.0));
    assert!(applies("only screen", 960.0));
    assert!(applies("screen and (min-width: 600px)", 960.0));
    assert!(!applies("screen and (min-width: 600px)", 480.0));
}

#[test]
fn not_negates_the_whole_query() {
    assert!(applies("not print", 960.0));
    assert!(!applies("not screen", 960.0));
    assert!(!applies("not all", 960.0));
    assert!(applies("not screen and (max-width: 600px)", 960.0));
    assert!(!applies("not screen and (max-width: 600px)", 480.0));
    assert!(applies("not (max-width: 600px)", 960.0));
    assert!(!applies("not (max-width: 600px)", 480.0));
}

#[test]
fn comma_lists_match_when_any_query_does() {
    assert!(applies("print, screen", 960.0));
    assert!(!applies("print, (max-width: 600px)", 960.0));
    assert!(applies("print, (max-width: 600px)", 480.0));
    // A broken query only spoils itself.
    assert!(applies("screen and, screen", 960.0));
}

#[test]
fn and_conditions_all_have_to_match() {
    assert!(applies("(min-width: 400px) and (max-width: 600px)", 500.0));
    assert!(!applies("(min-width: 400px) and (max-width: 600px)", 700.0));
    assert!(!applies("(min-width: 400px) and (max-width: 600px)", 300.0));
    assert!(applies("(max-width: 300px) or (min-width: 900px)", 960.0));
    assert!(!applies("(max-width: 300px) or (min-width: 900px)", 500.0));
}

#[test]
fn unknown_media_types_and_bad_syntax_do_not_match() {
    assert!(!applies("tv", 960.0));
    assert!(!applies("speech", 960.0));
    assert!(!applies("made-up-type", 960.0));
    assert!(!applies("screen (min-width: 1px)", 960.0));
    assert!(!applies("screen or (min-width: 1px)", 960.0));
    assert!(!applies(
        "(min-width: 1px) and (max-width: 2000px) or (min-width: 1px)",
        960.0
    ));
    assert!(!applies("and", 960.0));
    assert!(!applies("screen and", 960.0));
    assert!(!applies("(min-width: 1px", 960.0));
}

#[test]
fn nested_media_blocks_combine_with_and() {
    let css = "@media screen { @media print { p { color: #ff0000 } } }";
    assert_eq!(color_at(css, 960.0), green());
    let css = "@media (min-width: 400px) { @media (max-width: 600px) { p { color: #ff0000 } } }";
    assert_eq!(color_at(css, 700.0), green());
    assert_ne!(color_at(css, 500.0), green());
    let css = "@media print { @media screen { p { color: #ff0000 } } }";
    assert_eq!(color_at(css, 960.0), green());
}

#[test]
fn width_features_keep_their_behaviour() {
    let mq = MediaQuery::parse("(min-width: 600px) and (max-width: 800.4px)");
    assert!(!mq.matches(599.4));
    assert!(mq.matches(599.5));
    assert!(mq.matches(800.0));
    assert!(!mq.matches(801.0));
    // Unknown features and values that do not parse are ignored, as before.
    assert!(MediaQuery::parse("(orientation: portrait)").matches(960.0));
    assert!(MediaQuery::parse("(min-width: 40em)").matches(10.0));
    assert!(MediaQuery::parse("").matches(10.0));
    assert!(MediaQuery::parse("(prefers-color-scheme: dark)").matches(10.0));
    assert!(!MediaQuery::parse("(prefers-color-scheme: light)").matches(10.0));
}

#[test]
fn media_queries_combine_with_and() {
    let outer = MediaQuery::parse("screen, print");
    let inner = MediaQuery::parse("(max-width: 600px)");
    let both = outer.and(&inner);
    assert!(both.matches(500.0));
    assert!(!both.matches(700.0));
    assert!(!MediaQuery::parse("print").and(&inner).matches(500.0));
}

#[test]
fn deeply_nested_conditions_are_rejected_without_overflowing() {
    let deep = format!(
        "{}(min-width: 1px){}",
        "(".repeat(20_000),
        ")".repeat(20_000)
    );
    assert!(!MediaQuery::parse(&deep).matches(960.0));
    let shallow = "(((min-width: 1px)))";
    assert!(MediaQuery::parse(shallow).matches(960.0));
    assert!(!MediaQuery::parse(shallow).matches(0.0));
}
