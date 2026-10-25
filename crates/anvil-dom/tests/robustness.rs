// SPDX-License-Identifier: LicenseRef-Proprietary
// Copyright (c) 2026 Buntato
//! Robustness checks for the HTML parser and the document queries.
//!
//! Inputs come from a seeded generator, so a failure names a case number
//! that reproduces on every run. Each test stops after a short time budget.
//! Any input is a valid document to this parser. It must not panic or hang.
//! A `<` that does not start a tag (for example `a < b`) is ordinary text and
//! gets its own test below.

use anvil_dom::{Document, parse_html};

// ---- shared harness (same text in each repo's robustness test) ----

use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

/// Wall-clock budget for one robustness test. Cases run until it is spent.
/// Set `ANVIL_ROBUSTNESS_MS` for a longer local soak run.
fn test_budget() -> Duration {
    let millis = std::env::var("ANVIL_ROBUSTNESS_MS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(1500);
    Duration::from_millis(millis)
}

/// One input that runs longer than this counts as a hang.
const CASE_LIMIT: Duration = Duration::from_secs(10);

/// Upper bound on cases per test, so a fast machine does not loop forever.
const MAX_CASES: usize = 200_000;

/// xorshift64 generator. A fixed seed gives the same inputs on every run.
struct Rng(u64);

#[allow(dead_code)] // Not every test file uses every helper.
impl Rng {
    fn new(seed: u64) -> Self {
        Rng((seed ^ 0x9E37_79B9_7F4A_7C15) | 1)
    }

    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    /// Value in `0..n`. Returns 0 when `n` is 0.
    fn below(&mut self, n: usize) -> usize {
        if n == 0 { 0 } else { (self.next_u64() % n as u64) as usize }
    }

    fn byte(&mut self) -> u8 {
        self.next_u64() as u8
    }

    fn one_in(&mut self, n: usize) -> bool {
        self.below(n) == 0
    }

    fn pick<T: Copy>(&mut self, items: &[T]) -> T {
        items[self.below(items.len())]
    }

    fn bytes(&mut self, max_len: usize) -> Vec<u8> {
        let len = self.below(max_len + 1);
        (0..len).map(|_| self.byte()).collect()
    }
}

/// Apply 1 to 8 random edits: bit flips, byte overwrites, boundary values,
/// inserts, deletes, duplicated runs, and truncation.
fn mutate(rng: &mut Rng, seed: &[u8]) -> Vec<u8> {
    let mut out = seed.to_vec();
    let edits = 1 + rng.below(8);
    for _ in 0..edits {
        let len = out.len();
        match rng.below(8) {
            0 if len > 0 => {
                let i = rng.below(len);
                out[i] ^= 1 << rng.below(8);
            }
            1 if len > 0 => {
                let i = rng.below(len);
                out[i] = rng.byte();
            }
            2 if len > 0 => {
                let i = rng.below(len);
                out[i] = rng.pick(&[0x00, 0x01, 0x7F, 0x80, 0xFE, 0xFF]);
            }
            3 if len >= 4 => {
                let i = rng.below(len - 3);
                let v = rng.pick(&[0u32, 1, 0x7FFF_FFFF, 0x8000_0000, 0xFFFF_FFFF, 0xFFFF]);
                out[i..i + 4].copy_from_slice(&v.to_be_bytes());
            }
            4 => {
                let i = rng.below(len + 1);
                let extra = rng.bytes(16);
                out.splice(i..i, extra);
            }
            5 if len > 0 => {
                let i = rng.below(len);
                let n = 1 + rng.below((len - i).min(32));
                out.drain(i..i + n);
            }
            6 if len > 0 => {
                let i = rng.below(len);
                let n = 1 + rng.below((len - i).min(64));
                let run = out[i..i + n].to_vec();
                let at = rng.below(len + 1);
                out.splice(at..at, run);
            }
            _ => {
                let keep = rng.below(len + 1);
                out.truncate(keep);
            }
        }
    }
    out
}

fn preview(input: &[u8]) -> String {
    let shown: Vec<String> = input.iter().take(96).map(|b| format!("{b:02x}")).collect();
    let more = if input.len() > 96 { " ..." } else { "" };
    format!("{} bytes: {}{}", input.len(), shown.join(" "), more)
}

/// Run `target` on `input` in its own thread. Fails the test on a panic or
/// when the case runs past [`CASE_LIMIT`]. A hung thread is left behind and
/// ends with the test process.
fn check_case(label: &str, case: usize, input: Vec<u8>, target: fn(&[u8])) {
    let (done_tx, done_rx) = mpsc::channel();
    let shared = std::sync::Arc::new(input);
    let worker_input = shared.clone();
    let handle = thread::Builder::new()
        .name(format!("{label}-{case}"))
        .stack_size(16 * 1024 * 1024)
        .spawn(move || {
            target(&worker_input);
            let _ = done_tx.send(());
        })
        .expect("spawn case thread");
    match done_rx.recv_timeout(CASE_LIMIT) {
        Ok(()) => {
            let _ = handle.join();
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            let _ = handle.join();
            panic!("{label}: case {case} panicked on input {}", preview(&shared));
        }
        Err(mpsc::RecvTimeoutError::Timeout) => {
            panic!(
                "{label}: case {case} did not finish within {:?} on input {}",
                CASE_LIMIT,
                preview(&shared)
            );
        }
    }
}

/// Generate cases from `seed` until the budget is spent and check each one.
fn run_cases(label: &str, seed: u64, mut generate: impl FnMut(&mut Rng) -> Vec<u8>, target: fn(&[u8])) {
    let mut rng = Rng::new(seed);
    let budget = test_budget();
    let started = Instant::now();
    let mut case = 0;
    while case < MAX_CASES && started.elapsed() < budget {
        let input = generate(&mut rng);
        check_case(label, case, input, target);
        case += 1;
    }
    eprintln!("{label}: {case} cases in {:?}", started.elapsed());
}

const TAGS: &[&str] = &[
    "html", "head", "body", "div", "p", "span", "a", "b", "i", "ul", "li", "table", "thead",
    "tbody", "tr", "td", "th", "select", "option", "form", "input", "textarea", "pre", "script",
    "style", "img", "video", "br", "hr", "meta", "link", "title", "svg", "x-custom", "h1",
];

const ATTRS: &[&str] = &[
    "id", "class", "href", "src", "style", "type", "value", "checked", "selected", "name",
    "action", "method", "integrity", "rel", "content", "http-equiv", "data-x", "onclick",
];

const TEXT: &[&str] = &[
    "hello", " ", "\n", "\t", "&amp;", "&lt;", "&#39;", "&#x1F600;", "&#0;", "&#xD800;",
    "&#99999999999;", "&bogus;", "&", ";", "\u{e9}", "\u{1F600}", "\u{0}", ">", "\"", "'", "=",
    "/", "-->", "]]>", "\r\n",
];

/// Random but tag-shaped HTML.
fn html_soup(rng: &mut Rng) -> String {
    let mut out = String::new();
    for _ in 0..rng.below(60) {
        match rng.below(9) {
            0 | 1 => {
                out.push('<');
                out.push_str(rng.pick(TAGS));
                for _ in 0..rng.below(4) {
                    out.push(' ');
                    out.push_str(rng.pick(ATTRS));
                    match rng.below(4) {
                        0 => {}
                        1 => out.push_str(&format!("=\"{}\"", rng.pick(TEXT))),
                        2 => out.push_str(&format!("='{}'", rng.pick(TEXT))),
                        _ => out.push_str(&format!("={}", rng.pick(TEXT))),
                    }
                }
                if rng.one_in(4) {
                    out.push('/');
                }
                if !rng.one_in(10) {
                    out.push('>');
                }
            }
            2 => out.push_str(&format!("</{}>", rng.pick(TAGS))),
            3 => out.push_str(rng.pick(&["<!-- c -->", "<!--", "<!DOCTYPE html>", "<?x?>", "<!"])),
            _ => out.push_str(rng.pick(TEXT)),
        }
    }
    out
}

/// Parse and then run the read-only queries and an inner-HTML round trip.
fn parse_and_query(input: &[u8]) {
    let html = String::from_utf8_lossy(input);
    let mut doc = parse_html(&html);
    walk(&doc);
    let _ = doc.to_html();
    let _ = doc.title();
    let _ = doc.embedded_css();
    let _ = doc.scripts();
    let _ = doc.images();
    let _ = doc.meta_csp();
    for selector in ["div", "#a", ".b", "div > p", "a[href]", "*", "", ">>", "[", ":not(", "td + td"] {
        let _ = doc.query_selector_all(selector);
    }
    // Reparsing the serialized body must not panic either.
    let body = doc.body();
    let inner = doc.inner_html(body);
    doc.set_inner_html(body, &inner);
    walk(&doc);
}

fn walk(doc: &Document) {
    let mut pending = vec![doc.root()];
    let mut seen = 0usize;
    while let Some(id) = pending.pop() {
        seen += 1;
        assert!(seen <= doc.node_count(), "tree walk visited a node twice");
        let _ = doc.text_content(id);
        if doc.is_select_control(id) {
            let _ = doc.select_value(id);
        }
        pending.extend_from_slice(doc.children(id));
    }
}

#[test]
fn tag_soup_does_not_panic() {
    run_cases("html-soup", 41, |rng| html_soup(rng).into_bytes(), parse_and_query);
}

#[test]
fn random_bytes_do_not_panic() {
    run_cases("html-random", 42, |rng| rng.bytes(256), parse_and_query);
}

#[test]
fn mutated_documents_do_not_panic() {
    run_cases("html-mutated", 43, |rng| {
        let soup = html_soup(rng);
        mutate(rng, soup.as_bytes())
    }, parse_and_query);
}

#[test]
fn deep_and_wide_documents_stay_capped() {
    let deep = "<div>".repeat(5_000) + &"</div>".repeat(5_000);
    let doc = parse_html(&deep);
    walk(&doc);
    let wide = "<br>".repeat(anvil_dom::MAX_DOM_NODES + 100);
    let doc = parse_html(&wide);
    assert!(doc.node_count() <= anvil_dom::MAX_DOM_NODES);
    let attrs: String = (0..1_000).map(|i| format!(" a{i}=x")).collect();
    let doc = parse_html(&format!("<p{attrs}>x</p>"));
    walk(&doc);
}

#[test]
fn stray_less_than_does_not_hang() {
    run_cases("html-stray-lt", 44, |rng| {
        let mut out = html_soup(rng);
        let at = rng.below(out.len() + 1);
        if out.is_char_boundary(at) {
            out.insert_str(at, rng.pick(&["<", "< ", "<1", "<<", "a < b", "<\u{e9}"]));
        }
        out.into_bytes()
    }, parse_and_query);
}

#[test]
fn non_ascii_bytes_inside_a_start_tag_are_skipped() {
    // Each of these once stepped one byte into a multi-byte character.
    for html in ["<a \u{e9}>x</a>", "<p \u{1F600}=1 id=k>y</p>", "<b\u{e9}>z", "<i ==\u{e9}\u{e9}>"] {
        let doc = parse_html(html);
        walk(&doc);
    }
    let doc = parse_html("<p \u{e9} id=k>y</p>");
    assert!(doc.get_element_by_id("k").is_some());
}
