// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0-OR-Commercial
// Copyright (c) 2026 Buntato
//! Sheets that carry a `media` attribute, joined in document order.

use anvil_dom::{StyleSheetSource, parse_html};
use anvil_style::{
    Color, Stylesheet, compute_styles_with_viewport, parse_css, parse_css_with_media,
};

fn color(hex: &str) -> Color {
    Color::parse(hex).unwrap()
}

/// Build one sheet from the `<style>` elements of `html`, in document order,
/// and return the colour of `#t` at `width`.
fn color_from_styles(html: &str, width: f32) -> Color {
    let doc = parse_html(html);
    let mut sheet = Stylesheet::default();
    for source in doc.style_sheets() {
        if let StyleSheetSource::Style { css, media } = source {
            sheet.append(parse_css_with_media(&css, media.as_deref()));
        }
    }
    let styles = compute_styles_with_viewport(&doc, &sheet, width);
    styles.get(doc.get_element_by_id("t").unwrap()).color
}

#[test]
fn media_attribute_limits_every_rule_of_the_sheet() {
    let css = "p { color: #ff0000 } @media (min-width: 1px) { p { color: #ff0000 } }";
    let base = parse_css("p { color: #00ff00 }");
    for (media, applies) in [
        ("print", false),
        ("screen", true),
        ("all", true),
        ("not print", true),
        ("print, screen", true),
        ("", true),
        ("(max-width: 100px)", false),
    ] {
        let mut sheet = base.clone();
        sheet.append(parse_css_with_media(css, Some(media)));
        let doc = parse_html("<p id=t>x</p>");
        let styles = compute_styles_with_viewport(&doc, &sheet, 960.0);
        let got = styles.get(doc.get_element_by_id("t").unwrap()).color;
        assert_eq!(got == color("#ff0000"), applies, "media `{media}`");
    }
}

#[test]
fn no_media_is_the_same_as_parse_css() {
    let css = "p { color: red } @media print { p { color: blue } }";
    assert_eq!(parse_css_with_media(css, None), parse_css(css));
}

#[test]
fn append_keeps_source_order() {
    let mut sheet = parse_css("p { color: #ff0000 }");
    sheet.append(parse_css("p { color: #00ff00 }"));
    let doc = parse_html("<p id=t>x</p>");
    let styles = compute_styles_with_viewport(&doc, &sheet, 960.0);
    assert_eq!(
        styles.get(doc.get_element_by_id("t").unwrap()).color,
        color("#00ff00")
    );
}

#[test]
fn style_elements_with_media_apply_in_document_order() {
    let html = "<style>p { color: #00ff00 }</style>\
                <style media=print>p { color: #ff0000 }</style>\
                <p id=t>x</p>";
    assert_eq!(color_from_styles(html, 960.0), color("#00ff00"));
    let html = "<style>p { color: #ff0000 }</style>\
                <style media='screen and (min-width: 500px)'>p { color: #00ff00 }</style>\
                <p id=t>x</p>";
    assert_eq!(color_from_styles(html, 960.0), color("#00ff00"));
    assert_eq!(color_from_styles(html, 400.0), color("#ff0000"));
}
