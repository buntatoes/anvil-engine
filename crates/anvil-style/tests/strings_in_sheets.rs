// SPDX-License-Identifier: LicenseRef-Proprietary
// Copyright (c) 2026 Buntato
//! Braces, semicolons, and comment markers inside strings, `url()`, and
//! comments must not end a rule or a declaration.
//!
//! Unterminated strings follow CSS syntax: a string ends at its closing
//! quote, at an unescaped newline, or at the end of the sheet. A string that
//! runs to the end of the sheet keeps the rest of the sheet inside the rule
//! it started in, and nothing panics.

use anvil_dom::parse_html;
use anvil_style::{Color, Stylesheet, compute_styles, parse_css, parse_declarations};

const PAGE: &str = "<p id=p>x</p><div id=d>y</div>";

fn color_of(html: &str, css: &str, id: &str) -> Color {
    let doc = parse_html(html);
    let styles = compute_styles(&doc, &parse_css(css));
    let node = doc.get_element_by_id(id).expect("element with that id");
    styles.get(node).color
}

fn color(hex: &str) -> Color {
    Color::parse(hex).unwrap()
}

/// `(name, value)` for every declaration of rule `index`.
fn declarations(sheet: &Stylesheet, index: usize) -> Vec<(String, String)> {
    sheet.rules[index]
        .declarations
        .iter()
        .map(|d| (d.name.clone(), d.value.clone()))
        .collect()
}

fn pair(name: &str, value: &str) -> (String, String) {
    (name.to_string(), value.to_string())
}

#[test]
fn closing_brace_in_a_string_does_not_end_the_rule() {
    let css = "p { content: \"}\"; color: #00ff00 } div { color: #0000ff }";
    let sheet = parse_css(css);
    assert_eq!(sheet.rules.len(), 2);
    assert_eq!(
        declarations(&sheet, 0),
        [pair("content", "\"}\""), pair("color", "#00ff00")]
    );
    assert_eq!(color_of(PAGE, css, "p"), color("#00ff00"));
    assert_eq!(color_of(PAGE, css, "d"), color("#0000ff"));
}

#[test]
fn single_quoted_brace_and_semicolon_stay_in_the_value() {
    let sheet = parse_css("p { content: '}{;'; color: #00ff00 }");
    assert_eq!(
        declarations(&sheet, 0),
        [pair("content", "'}{;'"), pair("color", "#00ff00")]
    );
}

#[test]
fn semicolon_in_a_string_does_not_split_the_declaration() {
    let parsed = parse_declarations("content: \";\"; color: red");
    assert_eq!(parsed.len(), 2);
    assert_eq!(parsed[0].value, "\";\"");
    assert_eq!(parsed[1].value, "red");
}

#[test]
fn escaped_quotes_do_not_end_the_string() {
    let css = "p { content: \"a\\\"}b\"; color: #00ff00 } div { content: 'c\\'}'; color: #0000ff }";
    let sheet = parse_css(css);
    assert_eq!(sheet.rules.len(), 2);
    assert_eq!(declarations(&sheet, 0)[0], pair("content", "\"a\\\"}b\""));
    assert_eq!(declarations(&sheet, 1)[0], pair("content", "'c\\'}'"));
    assert_eq!(color_of(PAGE, css, "d"), color("#0000ff"));
}

#[test]
fn data_url_with_semicolons_stays_one_declaration() {
    let css = "p { background: url(data:image/png;base64,iVBORw0KGgo=); color: #00ff00 }";
    let sheet = parse_css(css);
    assert_eq!(
        declarations(&sheet, 0),
        [
            pair("background", "url(data:image/png;base64,iVBORw0KGgo=)"),
            pair("color", "#00ff00"),
        ]
    );
    let quoted = parse_declarations("background: url(\"data:text/plain;x=)\"); color: red");
    assert_eq!(quoted.len(), 2);
    assert_eq!(quoted[0].value, "url(\"data:text/plain;x=)\")");
}

#[test]
fn braces_inside_comments_are_ignored() {
    let css = "p { /* } */ color: #00ff00 } /* div { color: #ff0000 } { */ div { color: #0000ff }";
    assert_eq!(parse_css(css).rules.len(), 2);
    assert_eq!(color_of(PAGE, css, "p"), color("#00ff00"));
    assert_eq!(color_of(PAGE, css, "d"), color("#0000ff"));
}

#[test]
fn comment_markers_inside_a_string_are_text() {
    let sheet = parse_css("p { content: \"/* not a comment */\"; color: #00ff00 }");
    assert_eq!(
        declarations(&sheet, 0),
        [
            pair("content", "\"/* not a comment */\""),
            pair("color", "#00ff00")
        ]
    );
}

#[test]
fn brace_in_an_attribute_string_does_not_open_the_block() {
    let css = "[title=\"{\"], p { color: #00ff00 } div { color: #0000ff }";
    assert_eq!(color_of(PAGE, css, "p"), color("#00ff00"));
    assert_eq!(color_of(PAGE, css, "d"), color("#0000ff"));
}

#[test]
fn escaped_brace_in_a_selector_does_not_open_the_block() {
    let css = ".a\\{b, p { color: #00ff00 } div { color: #0000ff }";
    assert_eq!(color_of(PAGE, css, "p"), color("#00ff00"));
    assert_eq!(color_of(PAGE, css, "d"), color("#0000ff"));
}

#[test]
fn at_rule_statements_end_at_their_semicolon() {
    let css = "@charset \"utf-8\"; @import url(\"a;b.css\"); p { color: #00ff00 }";
    assert_eq!(color_of(PAGE, css, "p"), color("#00ff00"));
}

#[test]
fn inline_style_ignores_comments_and_keeps_data_urls() {
    let html =
        "<p id=p style='background: url(data:a;b); /* color: #ff0000; */ color: #00ff00'>x</p>";
    assert_eq!(color_of(html, "", "p"), color("#00ff00"));
}

#[test]
fn unterminated_string_at_the_end_keeps_the_rest_in_that_rule() {
    // The string runs to the end, so the `div` block never closes. Its
    // earlier declarations still apply and the `p` rule before it is fine.
    let css = "p { color: #00ff00 } div { color: #0000ff; content: \"abc } p { color: #ff0000 }";
    let sheet = parse_css(css);
    assert_eq!(sheet.rules.len(), 2);
    assert_eq!(
        declarations(&sheet, 1),
        [
            pair("color", "#0000ff"),
            pair("content", "\"abc } p { color: #ff0000 }")
        ]
    );
    assert_eq!(color_of(PAGE, css, "p"), color("#00ff00"));
    assert_eq!(color_of(PAGE, css, "d"), color("#0000ff"));
}

#[test]
fn unterminated_string_stops_at_a_newline() {
    let css = "p { content: \"abc\n; color: #00ff00 } div { color: #0000ff }";
    let sheet = parse_css(css);
    assert_eq!(sheet.rules.len(), 2);
    assert_eq!(color_of(PAGE, css, "p"), color("#00ff00"));
    assert_eq!(color_of(PAGE, css, "d"), color("#0000ff"));
}

#[test]
fn odd_endings_do_not_panic() {
    for css in [
        "p { content: \"",
        "p { content: '\\",
        "p { content: \"\\",
        "\\",
        "p { color: red; /* unclosed",
        "\"{",
        "@import \"x",
        "p { background: url(data:;",
        "p { content: \"é\\é\" }",
    ] {
        let sheet = parse_css(css);
        let _ = compute_styles(&parse_html(PAGE), &sheet);
        let _ = parse_declarations(css);
    }
}
