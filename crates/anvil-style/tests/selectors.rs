// SPDX-License-Identifier: LicenseRef-Proprietary
// Copyright (c) 2026 Buntato
//! Selector matching through the cascade: combinators, attribute selectors,
//! pseudo-classes, specificity, the limits, and large documents.

use anvil_dom::{Document, NodeId, parse_html};
use anvil_style::{
    Color, MAX_SELECTOR_COMPOUNDS, MAX_SELECTOR_NESTING, compute_styles,
    compute_styles_for_viewport, compute_styles_parallel, parse_css,
};
use std::time::{Duration, Instant};

const GREEN: &str = "#00ff00";
const RED: &str = "#ff0000";

fn green() -> Color {
    Color::parse(GREEN).unwrap()
}

fn red() -> Color {
    Color::parse(RED).unwrap()
}

/// Colour of each element in `ids` after cascading `css` onto `html`.
fn colors(html: &str, css: &str, ids: &[&str]) -> Vec<Color> {
    let doc = parse_html(html);
    let styles = compute_styles(&doc, &parse_css(css));
    ids.iter()
        .map(|id| {
            let node = doc
                .get_element_by_id(id)
                .unwrap_or_else(|| panic!("no element #{id}"));
            styles.get(node).color
        })
        .collect()
}

/// Every element whose id starts with `g` must be green and every one whose
/// id starts with `r` red, after `* { color: red }` and `selector { color:
/// green }`.
fn check(html: &str, selector: &str) {
    let css = format!("* {{ color: {RED} }} {selector} {{ color: {GREEN} }}");
    let doc = parse_html(html);
    let styles = compute_styles(&doc, &parse_css(&css));
    let mut seen = 0;
    let mut stack = vec![doc.root()];
    while let Some(id) = stack.pop() {
        stack.extend(doc.children(id).iter().copied());
        let Some(name) = doc.get(id).data().id() else {
            continue;
        };
        let want = match name.as_bytes().first() {
            Some(b'g') => green(),
            Some(b'r') => red(),
            _ => continue,
        };
        seen += 1;
        assert_eq!(
            styles.get(id).color,
            want,
            "`{selector}` on #{name} in {html}"
        );
    }
    assert!(seen > 0, "no marked elements in {html}");
}

/// The rule with `selector` must be dropped: the earlier green rule wins.
fn assert_dropped(selector: &str) {
    let css = format!("p {{ color: {GREEN} }} p, {selector} {{ color: {RED} }}");
    assert_eq!(
        colors("<p id=t>x</p>", &css, &["t"]),
        vec![green()],
        "`{selector}` should drop the rule"
    );
}

/// The rule with `selector` must be kept: its `p` entry applies.
fn assert_kept(selector: &str) {
    let css = format!("p {{ color: {RED} }} p, {selector} {{ color: {GREEN} }}");
    assert_eq!(
        colors("<p id=t>x</p>", &css, &["t"]),
        vec![green()],
        "`{selector}` should keep the rule"
    );
}

fn specificity(selector: &str) -> (u16, u16, u16) {
    let sheet = parse_css(&format!("{selector} {{ color: red }}"));
    let rule = sheet
        .rules
        .first()
        .unwrap_or_else(|| panic!("`{selector}` did not parse"));
    rule.selectors[0].specificity()
}

#[test]
fn child_combinator_matches_only_direct_children() {
    check(
        "<div class=a><p id=g1>x</p><section><p id=r1>y</p></section></div><p id=r2>z</p>",
        ".a > p",
    );
    check(
        "<ul><li><ol><li id=g1><b id=r1>x</b></li></ol></li></ul>",
        "ul > li > ol > li",
    );
}

#[test]
fn sibling_combinators() {
    let page = "<div><h2 id=r1>a</h2><p id=g1>b</p><p id=r2>c</p><span id=r3>d</span></div>";
    check(page, "h2 + p");
    check(
        "<div><p id=r1>a</p><h2 id=r2>b</h2><span id=r3>c</span><p id=g1>d</p><p id=g2>e</p></div>",
        "h2 ~ p",
    );
    check(
        "<div><i id=r1></i><b id=r2></b><i id=r3></i><b id=r4></b><u id=g1></u></div>",
        "i + b ~ i + b + u",
    );
}

#[test]
fn mixed_combinators_backtrack() {
    // The first `.a` ancestor has no `.b` parent; the second one does.
    check(
        "<div class=b><section class=a><div class=a><p id=g1>x</p></div></section></div>\
         <div class=c><div class=a><p id=r1>y</p></div></div>",
        ".b > .a p",
    );
    check(
        "<main><h1></h1><div><section><div><p id=g1>x</p></div></section></div></main>\
         <main><div><div><p id=r1>y</p></div></div></main>",
        "h1 ~ div div > p",
    );
}

#[test]
fn attribute_operators() {
    check("<a id=g1 title>x</a><a id=r1>y</a>", "[title]");
    check("<a id=g1 lang=en>x</a><a id=r1 lang=eng>y</a>", "[lang=en]");
    check(
        "<a id=g1 rel='nofollow  noopener'>x</a><a id=r1 rel='nofollowx'>y</a>\
         <a id=r2 rel=''>z</a>",
        "[rel~=noopener]",
    );
    check(
        "<a id=g1 hreflang=en>x</a><a id=g2 hreflang=en-GB>y</a><a id=r1 hreflang=eng>z</a>",
        "[hreflang|=en]",
    );
    check(
        "<a id=g1 href='https://x'>x</a><a id=r1 href='http://x'>y</a>",
        "[href^='https:']",
    );
    check(
        "<a id=g1 href='a.pdf'>x</a><a id=r1 href='a.pdf.txt'>y</a>",
        "[href$=\".pdf\"]",
    );
    check(
        "<a id=g1 data-x='abcdef'>x</a><a id=r1 data-x='ab'>y</a>",
        "[data-x*=bcd]",
    );
    // Empty values never match the substring operators or `~=`.
    check(
        "<a id=r1 data-x='abc'>x</a><a id=g1 data-y>y</a>",
        "[data-x^=''], [data-y]",
    );
    check(
        "<a id=r1 data-x='abc'>x</a><a id=g1 data-y>y</a>",
        "[data-x*=''], [data-y]",
    );
    check(
        "<a id=r1 data-x='a b'>x</a><a id=g1 data-y>y</a>",
        "[data-x~='a b'], [data-y]",
    );
}

#[test]
fn attribute_case() {
    check(
        "<a id=g1 data-k=abc>x</a><a id=r1 data-k=ABC>y</a>",
        "[data-k=abc]",
    );
    check(
        "<a id=g1 data-k=abc>x</a><a id=g2 data-k=ABC>y</a>",
        "[data-k=abc i]",
    );
    // `type` values compare without case in HTML, unless `s` says so.
    check(
        "<input id=g1 type=CHECKBOX><input id=g2 type=checkbox>",
        "[type=checkbox]",
    );
    check(
        "<input id=r1 type=CHECKBOX><input id=g1 type=checkbox>",
        "[type=checkbox s]",
    );
    // Attribute names never care about case.
    check("<a id=g1 DATA-K=v>x</a>", "[data-K=v]");
}

#[test]
fn ids_and_classes_keep_case_and_escapes() {
    check("<p id=g1 class=Foo>x</p><p id=r1 class=foo>y</p>", ".Foo");
    check(
        "<p id=g1 class='md:flex'>x</p><p id=g2 class='w-1/2'>y</p><p id=g3 class=10>z</p>\
         <p id=r1 class=md>w</p>",
        ".md\\:flex, .w-1\\/2, .\\31 0",
    );
    check("<p id=123>x</p><p id=g1 class=k>y</p>", ".\\6b");
    let got = colors(
        "<p id=123>x</p>",
        &format!("#\\31 23 {{ color: {GREEN} }}"),
        &["123"],
    );
    assert_eq!(got, vec![green()]);
}

#[test]
fn type_selectors_ignore_case_and_namespaces_resolve_only_with_star() {
    check("<p id=g1>x</p><div id=r1>y</div>", "P");
    check("<p id=g1>x</p><div id=r1>y</div>", "*|p");
    // No `@namespace` is read, so a prefix or `|` alone never matches, and
    // does not drop the rest of the list.
    check("<p id=r1>x</p><b id=g1>y</b>", "svg|p, |p, b");
}

#[test]
fn child_index_pseudo_classes() {
    let page = "<ul><li id=g1>a</li><li id=r1>b</li><li id=r2>c</li></ul>";
    check(page, "li:first-child");
    check(
        "<ul><li id=r1>a</li><li id=r2>b</li><li id=g1>c</li></ul>",
        "li:last-child",
    );
    check(
        "<div><p id=g1>a</p></div><div><p id=r1>b</p><p id=r2>c</p></div>",
        "p:only-child",
    );
    check(
        "<div><b id=r1></b><p id=g1></p><b id=r2></b><p id=r3></p><p id=g2></p></div>",
        "p:first-of-type, p:last-of-type",
    );
    check(
        "<div><b id=r1></b><p id=g1></p><b id=r2></b></div>",
        "p:only-of-type",
    );
    // Text and comments are not counted.
    check(
        "<div> text <p id=g1>a</p> more </div>",
        "p:first-child:last-child",
    );
}

#[test]
fn nth_formulas() {
    let page = |marks: &str| {
        let mut html = String::from("<ol>");
        for (i, m) in marks.chars().enumerate() {
            html.push_str(&format!("<li id={m}{i}>x</li>"));
        }
        html.push_str("</ol>");
        html
    };
    check(&page("rgrgrgr"), "li:nth-child(2n)");
    check(&page("rgrgrgr"), "li:nth-child(even)");
    check(&page("grgrgrg"), "li:nth-child(odd)");
    check(&page("grgrgrg"), "li:nth-child(2n+1)");
    check(&page("rrgrrrr"), "li:nth-child(3)");
    check(&page("gggrrrr"), "li:nth-child(-n + 3)");
    check(&page("rrggggg"), "li:nth-child(n+3)");
    check(&page("rrgrrgr"), "li:nth-child( 3n - 0 )");
    check(&page("rgrrgrr"), "li:nth-child(3n- 1)");
    check(&page("rrrrrgg"), "li:nth-last-child(-n+2)");
    check(&page("rrrrgrr"), "li:nth-last-child(3)");
    check(&page("rrrrrrr"), "li:nth-child(0n+0)");
    check(&page("rgrrrrr"), "li:nth-child(+0n+2)");
    check(&page("rrrrrrr"), "li:nth-child(-2n-1)");
}

#[test]
fn nth_of_type_and_of_selector() {
    check(
        "<div><p id=g1></p><b id=r1></b><p id=r2></p><b id=r3></b><p id=g2></p></div>",
        "p:nth-of-type(odd)",
    );
    check(
        "<div><p id=r1></p><b id=r2></b><p id=g1></p><b id=r3></b><p id=r4></p></div>",
        "p:nth-last-of-type(2)",
    );
    check(
        "<ul><li id=r1>a</li><li id=r2 class=x>b</li><li id=r3>c</li><li id=g1 class=x>d</li>\
         <li id=r4 class=x>e</li></ul>",
        "li:nth-child(2 of .x)",
    );
    check(
        "<ul><li id=r1 class=x>a</li><li id=g1 class=x>b</li><li id=r2>c</li></ul>",
        "li:nth-last-child(1 of .x)",
    );
}

#[test]
fn bad_nth_arguments_drop_the_rule() {
    for bad in [
        "li:nth-child(2n+)",
        "li:nth-child(+ 2n)",
        "li:nth-child(2 n)",
        "li:nth-child(n 2)",
        "li:nth-child(2n + + 1)",
        "li:nth-child(2n+-1)",
        "li:nth-child()",
        "li:nth-child(foo)",
        "li:nth-child(1.5)",
        "li:nth-of-type(2 of .x)",
        "li:nth-child(2 of)",
        "li:nth-child(2 of :no-such-state)",
    ] {
        assert_dropped(bad);
    }
    for good in [
        "li:nth-child(-n+3)",
        "li:nth-child(N)",
        "li:nth-child( +5 )",
    ] {
        assert_kept(good);
    }
}

#[test]
fn root_empty_and_scope() {
    check("<p id=g1></p><p id=r1>x</p><p id=r2><b></b></p>", "p:empty");
    check("<p id=g1><!-- note --></p><p id=r1>.</p>", "p:empty");
    let got = colors(
        "<p id=t>x</p>",
        &format!(":root {{ color: {GREEN} }} :scope p {{ background: red }}"),
        &["t"],
    );
    assert_eq!(got, vec![green()]);
    check(
        "<div id=r1><p id=r2>x</p></div>",
        ":root > div > p > *, div:root",
    );
}

#[test]
fn is_where_not() {
    check(
        "<h1 id=g1>a</h1><h2 id=g2>b</h2><h3 id=r1>c</h3>",
        ":is(h1, h2)",
    );
    // Forgiving: the bad entry is skipped and the rest still match.
    check(
        "<h1 id=g1>a</h1><h3 id=r1>c</h3>",
        ":is(h1, :no-such-state, h1$)",
    );
    check("<h1 id=g1>a</h1><h3 id=r1>c</h3>", ":where(h1, ::before)");
    check(
        "<p id=g1>a</p><p id=r1 class=a>b</p><p id=r2 class=b>c</p>",
        "p:not(.a, .b)",
    );
    check(
        "<div class=x><p id=r1>a</p></div><div><p id=g1>b</p></div>",
        "div:not(.x) > p",
    );
    // A complex selector inside :not().
    check(
        "<div class=x><p id=r1>a</p></div><div><p id=g1>b</p></div>",
        "p:not(.x p)",
    );
    // :is() with a complex selector looks at the whole tree.
    check(
        "<section><div><p id=g1>a</p></div></section><div><p id=r1>b</p></div>",
        "p:is(section *)",
    );
}

#[test]
fn not_and_has_are_unforgiving() {
    assert_dropped("p:not(p, )");
    assert_dropped("p:not(:no-such-state)");
    assert_dropped("p:not(::before)");
    assert_dropped("p:has(> img, $)");
    assert_dropped("p:has(:has(img))");
    assert_dropped("p:has(::before)");
    assert_dropped("p:has()");
    assert_kept("p:is()");
    assert_kept("p:where(:no-such-state)");
}

#[test]
fn has_relative_selectors() {
    check(
        "<section id=g1><div><i class=k>k</i></div></section><section id=r1>b</section>",
        "section:has(.k)",
    );
    check(
        "<div id=g1><img></div><div id=r1><p><img></p></div>",
        "div:has(> img)",
    );
    check(
        "<div><h2 id=g1>c</h2><p>p</p><h2 id=r1>d</h2></div>",
        "h2:has(+ p)",
    );
    check(
        "<div><h2 id=g1>c</h2><b></b><p>p</p><h2 id=r1>d</h2></div>",
        "h2:has(~ p)",
    );
    // Several compounds, and a subject below a later sibling.
    check(
        "<div id=g1><ul><li><a>x</a></li></ul></div><div id=r1><ul><li>x</li></ul><a>y</a></div>",
        "div:has(> ul li > a)",
    );
    check(
        "<div><h2 id=g1>t</h2><section><p class=k>x</p></section><h2 id=r1>u</h2><p>y</p></div>",
        "h2:has(+ section .k)",
    );
    check(
        "<div><h2 id=g1>t</h2><b></b><i></i><u></u></div><div><h2 id=r1>t</h2><i></i><b></b></div>",
        "h2:has(~ b + i ~ u)",
    );
    // The anchor itself is never its own descendant.
    check("<div id=r1 class=k>x</div>", ".k:has(.k)");
    // Ancestors above the anchor do not count for the relative part.
    check(
        "<div class=out><div id=r1><p>x</p></div></div>",
        "div:has(.out p)",
    );
    check(
        "<ul id=g1><li>x</li></ul><ul id=r1></ul>",
        "ul:has(li):not(:has(b))",
    );
}

#[test]
fn form_states() {
    check(
        "<input id=g1 type=checkbox checked><input id=r1 type=checkbox>\
         <input id=g2 type=radio checked><input id=r2 type=text checked>",
        ":checked",
    );
    check(
        "<select><option id=r1>a</option><option id=g1 selected>b</option></select>\
         <select><option id=g2>a</option><option id=r2>b</option></select>\
         <select multiple><option id=r3>a</option></select>",
        "option:checked",
    );
    check(
        "<input id=g1 disabled><button id=g2 disabled>b</button>\
         <fieldset id=g3 disabled><input id=g4><legend><input id=r1></legend>\
         <legend><input id=g5></legend></fieldset>\
         <select><optgroup disabled><option id=g6>x</option></optgroup><option id=r2>y</option></select>\
         <p id=r3 disabled>z</p>",
        ":disabled",
    );
    check(
        "<input id=g1><input id=r1 disabled><p id=r2>x</p><select id=g2></select>",
        ":enabled",
    );
    check(
        "<input id=g1 required><input id=r1><select id=g2 required></select>\
         <input id=r2 type=submit required><p id=r3 required>x</p>",
        ":required",
    );
    check(
        "<input id=r1 required><input id=g1><textarea id=g2></textarea><p id=r2>x</p>",
        ":optional",
    );
    check(
        "<input id=g1><input id=r1 readonly><input id=r2 disabled><input id=r3 type=checkbox>\
         <textarea id=g2></textarea><div id=g3 contenteditable><p id=g4>x</p></div>\
         <div contenteditable><p id=r4 contenteditable=false>y</p></div><p id=r5>z</p>",
        ":read-write",
    );
    check(
        "<input id=r1><input id=g1 readonly><p id=g2>z</p>",
        ":read-only",
    );
    check(
        "<input id=g1 placeholder=p><input id=r1 placeholder=p value=v><input id=r2>\
         <textarea id=g2 placeholder=p></textarea><textarea id=r3 placeholder=p>t</textarea>\
         <input id=r4 type=checkbox placeholder=p>",
        ":placeholder-shown",
    );
    check(
        "<form><input id=r1 type=text><button id=g1>a</button><button id=r2>b</button>\
         <input id=g2 type=checkbox checked><option id=g3 selected>o</option></form>\
         <button id=r3>c</button>",
        ":default",
    );
}

#[test]
fn link_lang_dir_and_other_states() {
    check(
        "<a id=g1 href=x>a</a><a id=r1>b</a><area id=g2 href=x><link id=r2 href=x>",
        ":link",
    );
    check("<a id=g1 href=x>a</a><a id=r1>b</a>", ":any-link");
    check(
        "<div lang=en><p id=g1>a</p><p id=g2 lang=en-US>b</p><p id=r1 lang=eno>c</p>\
         <p id=r2 lang=fr>d</p></div>",
        "p:lang(en)",
    );
    check(
        "<p id=g1 lang=de-Latn-DE>a</p><p id=g2 lang=de-DE>b</p><p id=r1 lang=de-x-DE>c</p>",
        "p:lang('de-DE')",
    );
    check("<p id=g1 lang=fr>a</p><p id=r1>b</p>", "p:lang(\"*\")");
    check(
        "<div dir=rtl><p id=g1>a</p><p id=r1 dir=ltr>b</p></div><p id=r2>c</p>",
        "p:dir(rtl)",
    );
    check(
        "<details id=g1 open></details><details id=r1></details><p id=r2 open>x</p>",
        ":open",
    );
    check(
        "<progress id=g1></progress><progress id=r1 value=1></progress>",
        ":indeterminate",
    );
    check("<p id=g1>a</p><my-el id=r1>b</my-el>", ":defined");
}

#[test]
fn interaction_states_never_match() {
    for state in [
        ":hover",
        ":active",
        ":focus",
        ":focus-visible",
        ":focus-within",
        ":visited",
        ":target",
        ":valid",
        ":invalid",
        ":playing",
        ":modal",
        ":host",
        ":current(p)",
        ":state(x)",
    ] {
        check(
            "<a id=r1 href=x>a</a><input id=r2>",
            &format!("a{state}, input{state}"),
        );
        check("<a id=g1 href=x>a</a>", &format!("a:not({state})"));
    }
}

#[test]
fn pseudo_elements_do_not_style_the_element() {
    check(
        "<p id=r1>a</p>",
        "p::before, p::after, p:first-line, p::marker",
    );
    check("<p id=g1>a</p>", "p::before, p");
}

#[test]
fn specificity_values() {
    assert_eq!(specificity("*"), (0, 0, 0));
    assert_eq!(specificity("li"), (0, 0, 1));
    assert_eq!(specificity("ul li"), (0, 0, 2));
    assert_eq!(specificity("ul ol+li"), (0, 0, 3));
    assert_eq!(specificity("h1 + *[rel=up]"), (0, 1, 1));
    assert_eq!(specificity("ul ol li.red"), (0, 1, 3));
    assert_eq!(specificity("li.red.level"), (0, 2, 1));
    assert_eq!(specificity("#x34y"), (1, 0, 0));
    assert_eq!(specificity("#s12:not(FOO)"), (1, 0, 1));
    assert_eq!(specificity(".foo :is(.bar, #baz)"), (1, 1, 0));
    assert_eq!(specificity(":where(#a, .b) p"), (0, 0, 1));
    assert_eq!(specificity("p:has(> #a, .b)"), (1, 0, 1));
    assert_eq!(specificity("li:nth-child(2n of #a, .b)"), (1, 1, 1));
    assert_eq!(specificity("li:nth-child(2n)"), (0, 1, 1));
    assert_eq!(specificity("p::before"), (0, 0, 2));
    assert_eq!(specificity("p:before"), (0, 0, 2));
    assert_eq!(specificity("*|p"), (0, 0, 1));
    assert_eq!(specificity(":is(:no-such-state, p)"), (0, 0, 1));
}

#[test]
fn higher_specificity_wins_regardless_of_order() {
    let css = format!(
        ":is(#t, p) {{ color: {GREEN} }} body > p:first-child {{ color: {RED} }}\
         :where(#u) {{ color: {RED} }} p {{ color: {GREEN} }}"
    );
    assert_eq!(
        colors("<p id=t>x</p><p id=u>y</p>", &css, &["t", "u"]),
        vec![green(), green()]
    );
}

#[test]
fn broken_selectors_drop_the_rule() {
    for bad in [
        "p >",
        "> p",
        "p + + b",
        "p ~",
        "p | b",
        "p || b",
        "[x=]",
        "[x y]",
        "[x=y z]",
        "[x=1]",
        "[x='y]",
        "[|]",
        "p::before.x",
        "p::before#x",
        "p::before[x]",
        "p::no-such-thing",
        ":lang()",
        ":lang(en,)",
        ":dir()",
        ":dir(ltr rtl)",
        "p&",
        "#1x",
        "p:is(",
    ] {
        assert_dropped(bad);
    }
    for good in [
        "[x=\"\"]",
        "[ x = y i ]",
        "[x='y' s]",
        "*|*",
        "|p",
        "p:lang(en, 'fr')",
        "p:dir(auto)",
        "p::before:hover",
        "p::part(x)",
    ] {
        assert_kept(good);
    }
}

#[test]
fn nesting_and_length_limits() {
    let nested = |depth: usize| {
        let mut s = String::from("p");
        for _ in 0..depth {
            s = format!(":is({s})");
        }
        s
    };
    assert_kept(&nested(MAX_SELECTOR_NESTING));
    assert_dropped(&nested(MAX_SELECTOR_NESTING + 1));
    // A forgiving list still fails on a limit.
    assert_dropped(&format!(":is(h1, {})", nested(MAX_SELECTOR_NESTING)));
    let chain = |n: usize| vec!["div"; n].join(" ");
    assert_kept(&chain(MAX_SELECTOR_COMPOUNDS));
    assert_dropped(&chain(MAX_SELECTOR_COMPOUNDS + 1));
    // A hostile selector far past the limit parses quickly and is dropped.
    let started = Instant::now();
    assert_dropped(&format!(
        "{}p{}",
        ":not(".repeat(50_000),
        ")".repeat(50_000)
    ));
    assert_dropped(&"a > ".repeat(50_000));
    assert!(started.elapsed() < Duration::from_secs(10));
}

/// A wide, deep document for the speed and consistency tests: `blocks`
/// sections, each `depth` divs deep with a run of `width` siblings at
/// the bottom.
fn big_page(blocks: usize, depth: usize, width: usize) -> String {
    let mut html = String::new();
    for b in 0..blocks {
        html.push_str(&format!("<section class=s{}>", b % 3));
        for d in 0..depth {
            html.push_str(&format!("<div class='d d{}'>", d % 4));
        }
        for w in 0..width {
            let kind = if w % 5 == 0 { "b" } else { "i" };
            html.push_str(&format!("<{kind} class=k{}>x</{kind}>", w % 3));
        }
        for _ in 0..depth {
            html.push_str("</div>");
        }
        html.push_str("</section>");
    }
    html
}

const HEAVY_CSS: &str = "
    div div div div div div div div div div div div i { color: #010101 }
    .d0 .d1 .d2 .d3 .d0 .d1 .d2 .d3 .d0 .d1 .d2 .d3 .d0 .d1 .nope i { color: #020202 }
    i ~ i ~ i ~ i ~ i ~ i ~ i ~ i ~ b { color: #030303 }
    i ~ i ~ i ~ i ~ i ~ i ~ i ~ i ~ .nope { color: #040404 }
    section:has(.k2 ~ b) { color: #050505 }
    div:has(i.k1) { background: #060606 }
    div:has(> b + i ~ .k2) { background: #070707 }
    i:nth-child(3n + 1 of .k1, .k2) { color: #080808 }
    i:nth-last-of-type(2) { color: #090909 }
    :is(section, div) :where(div, i):not(:has(*)) { border-color: #0a0a0a }
    .s0 > div:only-child div:first-child i:last-child { color: #0b0b0b }
";

#[test]
fn large_documents_stay_fast() {
    // About 20 thousand elements, 60 deep and 100 wide at the bottom.
    let html = big_page(3, 60, 6_000);
    let doc = parse_html(&html);
    let sheet = parse_css(HEAVY_CSS);
    assert_eq!(sheet.rules.len(), 11);
    let started = Instant::now();
    let styles = compute_styles(&doc, &sheet);
    let spent = started.elapsed();
    assert!(
        spent < Duration::from_secs(30),
        "cascade took {spent:?} for {} nodes",
        doc.node_count()
    );
    let last = doc.get_element_by_id("none");
    assert!(last.is_none());
    let section = doc.children(doc.body())[0];
    assert_eq!(styles.get(section).color, Color::parse("#050505").unwrap());
}

#[test]
fn deep_documents_do_not_overflow() {
    // Built by hand, far deeper than the HTML parser allows.
    let mut doc = Document::new();
    let mut parent = doc.body();
    let mut deepest = parent;
    for i in 0..20_000 {
        let class = if i % 2 == 0 { "a" } else { "b" };
        let node = doc.create_element("div", vec![("class".into(), class.into())]);
        doc.append(parent, node).unwrap();
        parent = node;
        deepest = node;
    }
    let sheet = parse_css(
        "body:has(.b .a .zz) {} .a .b .a .b .zz {} body :is(.a .b .a .q) {} \
         :has(> .a > .b > .a > .q) {} .a ~ .b {} .a:nth-child(1 of .a .b) {}",
    );
    let top = doc.body();
    for rule in &sheet.rules {
        let sel = &rule.selectors[0];
        assert!(!sel.matches(&doc, top));
        assert!(!sel.matches(&doc, deepest));
    }
}

#[test]
fn single_element_matching_agrees_with_the_cascade() {
    let html = big_page(2, 8, 30);
    let doc = parse_html(&html);
    let css = "
        .s0 i { color: #010101 }
        div > b + i { color: #020202 }
        i ~ b { color: #030303 }
        section:has(b.k2) { color: #040404 }
        i:nth-child(2n of .k1) { color: #050505 }
        b:nth-last-of-type(2) { color: #060606 }
        div:not(.d1):has(> div) { color: #070707 }
        :is(.d0, .d2) > :where(.d1, .d3) { color: #080808 }
        i:first-child, i:last-child, b:only-of-type { color: #090909 }
    ";
    let sheet = parse_css(css);
    let styles = compute_styles(&doc, &sheet);
    let mut stack = vec![doc.root()];
    let mut checked = 0;
    while let Some(id) = stack.pop() {
        stack.extend(doc.children(id).iter().copied());
        if doc.get(id).data().tag().is_none() {
            continue;
        }
        // The last matching rule wins: every rule here has its own colour.
        let expected = sheet
            .rules
            .iter()
            .enumerate()
            .filter(|(_, rule)| rule.selectors.iter().any(|s| s.matches(&doc, id)))
            .max_by_key(|(order, rule)| {
                let spec = rule
                    .selectors
                    .iter()
                    .filter(|s| s.matches(&doc, id))
                    .map(|s| s.specificity())
                    .max();
                (spec, *order)
            })
            .map(|(_, rule)| rule.declarations[0].value.clone());
        if let Some(value) = expected {
            assert_eq!(styles.get(id).color, Color::parse(&value).unwrap());
            checked += 1;
        }
    }
    assert!(checked > 40, "only {checked} elements matched");
}

#[test]
fn parallel_cascade_matches_the_single_thread_one() {
    let html = big_page(8, 6, 40);
    let doc = parse_html(&html);
    let sheet = parse_css(HEAVY_CSS);
    let one = compute_styles_for_viewport(&doc, &sheet, 800.0, 600.0);
    let many = compute_styles_parallel(&doc, &sheet, 800.0, 600.0);
    assert_eq!(one, many);
}

#[test]
fn out_of_range_node_does_not_match() {
    let doc = parse_html("<p>x</p>");
    let sheet = parse_css("p {}");
    let far = NodeId::from_raw(1_000_000);
    assert!(!sheet.rules[0].selectors[0].matches(&doc, far));
}

/// Seeded random selector text: must parse or be dropped, never panic, and
/// the cascade over a small page must not panic either.
#[test]
fn random_selectors_do_not_panic() {
    const PIECES: &[&str] = &[
        "p",
        "div",
        "*",
        "|",
        "*|",
        "#a",
        ".b",
        ".\\31",
        "[x]",
        "[x=y",
        "[x~='a b' i]",
        "]",
        ":is(",
        ":where(",
        ":not(",
        ":has(",
        ")",
        ",",
        " ",
        ">",
        "+",
        "~",
        ":hover",
        ":root",
        ":nth-child(",
        "2n+1",
        " of ",
        ":nth-last-of-type(",
        "odd",
        ":lang(",
        "en",
        ":dir(",
        "rtl",
        "::before",
        ":before",
        "::part(",
        "\\",
        "'",
        "\"",
        "(",
        "[",
        "-",
        "--",
        "1",
        ":empty",
        ":first-child",
        ":checked",
        ":disabled",
        ":placeholder-shown",
        "&",
        "|=",
    ];
    let page = "<div id=a class=b><p lang=en x=y>t</p><p dir=rtl><input checked></p></div>\
                <ul><li>1</li><li class=b>2</li></ul>";
    let doc = parse_html(page);
    let mut seed = 0x2545_f491_4f6c_dd1du64;
    for _ in 0..3_000 {
        let mut text = String::new();
        let len = 1 + (seed % 12) as usize;
        for _ in 0..len {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            text.push_str(PIECES[(seed % PIECES.len() as u64) as usize]);
        }
        let sheet = parse_css(&format!("{text} {{ color: red }}"));
        let _ = compute_styles(&doc, &sheet);
    }
}
