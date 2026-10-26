// SPDX-License-Identifier: LicenseRef-Proprietary
// Copyright (c) 2026 Buntato
//! `row-gap`, `column-gap`, and the `gap` shorthand in the cascade.

use anvil_dom::parse_html;
use anvil_style::{
    Animator, ComputedStyle, Size, compute_styles, compute_styles_animated, parse_css,
};

fn style_of(css: &str) -> ComputedStyle {
    let doc = parse_html("<div id=t></div>");
    let styles = compute_styles(&doc, &parse_css(&format!("#t {{ {css} }}")));
    *styles.get(doc.get_element_by_id("t").unwrap())
}

fn gaps(css: &str) -> (Size, Size) {
    let style = style_of(css);
    (style.row_gap, style.column_gap)
}

#[test]
fn initial_gaps_are_zero_and_not_inherited() {
    assert_eq!(gaps(""), (Size::Px(0.0), Size::Px(0.0)));
    let doc = parse_html("<div id=p><div id=c></div></div>");
    let styles = compute_styles(&doc, &parse_css("#p { gap: 5px }"));
    let child = styles.get(doc.get_element_by_id("c").unwrap());
    assert_eq!(child.row_gap, Size::Px(0.0));
}

#[test]
fn longhands_keep_lengths_percentages_and_calc() {
    assert_eq!(gaps("row-gap: 4px").0, Size::Px(4.0));
    assert_eq!(gaps("column-gap: 10%").1, Size::Percent(10.0));
    assert_eq!(gaps("column-gap: 2em").1, Size::Em(2.0));
    assert_eq!(gaps("row-gap: 1.5rem").0, Size::Rem(1.5));
    assert_eq!(gaps("row-gap: 3vw").0, Size::Vw(3.0));
    assert_eq!(gaps("row-gap: 0").0, Size::Px(0.0));
    assert_eq!(
        gaps("column-gap: calc(10% + 2px)").1,
        Size::Calc {
            px: 2.0,
            percent: 10.0,
            em: 0.0,
            rem: 0.0
        }
    );
    assert_eq!(gaps("row-gap: 4px; row-gap: normal").0, Size::Px(0.0));
    assert_eq!(gaps("row-gap: 4px; row-gap: NORMAL").0, Size::Px(0.0));
    assert_eq!(
        gaps("grid-row-gap: 3px; grid-column-gap: 7px"),
        (Size::Px(3.0), Size::Px(7.0))
    );
}

#[test]
fn bad_longhand_values_are_ignored() {
    for bad in ["-1px", "-5%", "auto", "4", "4px 5px", "blue", "calc(", ""] {
        assert_eq!(
            gaps(&format!("column-gap: 9px; column-gap: {bad}")).1,
            Size::Px(9.0),
            "column-gap: {bad}"
        );
    }
}

#[test]
fn shorthand_sets_row_then_column() {
    assert_eq!(gaps("gap: 4px"), (Size::Px(4.0), Size::Px(4.0)));
    assert_eq!(gaps("gap: 4px 10%"), (Size::Px(4.0), Size::Percent(10.0)));
    assert_eq!(gaps("gap: normal 2em"), (Size::Px(0.0), Size::Em(2.0)));
    assert_eq!(gaps("grid-gap: 1px 2px"), (Size::Px(1.0), Size::Px(2.0)));
    // A calc() of plain pixels folds to a length.
    assert_eq!(
        gaps("gap: calc(1px + 1px) 3px"),
        (Size::Px(2.0), Size::Px(3.0))
    );
    // One bad part drops the whole declaration.
    for bad in ["1px 2px 3px", "1px -2px", "auto 1px", "1px x"] {
        assert_eq!(
            gaps(&format!("gap: 5px 6px; gap: {bad}")),
            (Size::Px(5.0), Size::Px(6.0)),
            "gap: {bad}"
        );
    }
    // Longhands after the shorthand win, and the other way round.
    assert_eq!(
        gaps("gap: 5px; column-gap: 1px"),
        (Size::Px(5.0), Size::Px(1.0))
    );
    assert_eq!(
        gaps("column-gap: 1px; gap: 5px"),
        (Size::Px(5.0), Size::Px(5.0))
    );
}

#[test]
fn gap_transitions_move_both_longhands() {
    let mut doc = parse_html("<div id=t></div>");
    let sheet = parse_css("#t { gap: 0px; transition: gap 100ms linear; }");
    let t = doc.get_element_by_id("t").unwrap();
    let mut animator = Animator::new();
    let _ = compute_styles_animated(&doc, &sheet, 800.0, 600.0, &mut animator);
    doc.set_attribute(t, "style", "row-gap: 20px; column-gap: 40px");
    let _ = compute_styles_animated(&doc, &sheet, 800.0, 600.0, &mut animator);
    animator.tick(50.0);
    let styles = compute_styles_animated(&doc, &sheet, 800.0, 600.0, &mut animator);
    let style = styles.get(t);
    assert_eq!(style.row_gap, Size::Px(10.0));
    assert_eq!(style.column_gap, Size::Px(20.0));
}

#[test]
fn row_gap_transition_leaves_column_gap_alone() {
    let mut doc = parse_html("<div id=t></div>");
    let sheet = parse_css("#t { transition: row-gap 100ms linear; }");
    let t = doc.get_element_by_id("t").unwrap();
    let mut animator = Animator::new();
    let _ = compute_styles_animated(&doc, &sheet, 800.0, 600.0, &mut animator);
    doc.set_attribute(t, "style", "gap: 20px");
    let _ = compute_styles_animated(&doc, &sheet, 800.0, 600.0, &mut animator);
    animator.tick(50.0);
    let styles = compute_styles_animated(&doc, &sheet, 800.0, 600.0, &mut animator);
    let style = styles.get(t);
    assert_eq!(style.row_gap, Size::Px(10.0));
    assert_eq!(style.column_gap, Size::Px(20.0));
}
