// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0-OR-Commercial
// Copyright (c) 2026 Buntato
//! Robustness checks for the CSS parser and the cascade.
//!
//! Inputs come from a seeded generator, so a failure names a case number
//! that reproduces on every run. Each test stops after a short time budget.
//! Any input is a valid style sheet to this parser. It must not panic or hang,
//! and the cascade over a small fixed document must not panic either.

use anvil_dom::parse_html;
use anvil_style::{
    Color, MediaQuery, compute_styles, compute_styles_for_viewport, parse_css, parse_declarations,
};

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
        if n == 0 {
            0
        } else {
            (self.next_u64() % n as u64) as usize
        }
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
            panic!(
                "{label}: case {case} panicked on input {}",
                preview(&shared)
            );
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
fn run_cases(
    label: &str,
    seed: u64,
    mut generate: impl FnMut(&mut Rng) -> Vec<u8>,
    target: fn(&[u8]),
) {
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

const SELECTORS: &[&str] = &[
    "div",
    "p",
    "*",
    "#main",
    ".note",
    "div > p",
    "ul li",
    "a[href]",
    "a[href^=\"http\"]",
    "input[type=checkbox]:checked",
    "li:first-child",
    "li:nth-child(2n+1)",
    ":not(p)",
    "p::before",
    "h1, h2",
    "td + td",
    "td ~ td",
    ":hover",
    ":root",
    "[",
    "]",
    "::",
    "#",
    ".",
    ">",
    "+",
];

const PROPERTIES: &[&str] = &[
    "color",
    "background",
    "background-color",
    "display",
    "margin",
    "padding",
    "border",
    "border-radius",
    "width",
    "height",
    "min-width",
    "max-height",
    "font-size",
    "font-weight",
    "line-height",
    "position",
    "top",
    "left",
    "z-index",
    "flex",
    "flex-direction",
    "flex-wrap",
    "justify-content",
    "align-items",
    "gap",
    "grid-template-columns",
    "overflow",
    "opacity",
    "box-shadow",
    "transform",
    "text-align",
    "white-space",
    "--custom",
    "",
];

const VALUES: &[&str] = &[
    "0",
    "1px",
    "-1px",
    "2.5em",
    "3rem",
    "50%",
    "100vw",
    "100vh",
    "10vmin",
    "1e9px",
    "-1e39px",
    "NaN",
    "inf",
    "red",
    "#fff",
    "#ffffffff",
    "#12",
    "rgb(1,2,3)",
    "rgba(1, 2, 3, 0.5)",
    "rgb(300,-1,1e9)",
    "hsl(120, 50%, 50%)",
    "transparent",
    "linear-gradient(red, blue)",
    "linear-gradient(",
    "calc(100% - 10px)",
    "calc(",
    "var(--custom)",
    "repeat(3, 1fr)",
    "minmax(10px, 1fr)",
    "auto",
    "none",
    "inherit",
    "initial",
    "flex",
    "grid",
    "block",
    "0 0 4px rgba(0,0,0,.5)",
    "1px solid red",
    "!important",
    "\"\u{e9}\"",
    "url(x.png)",
    "\\",
    "\u{0}",
    ";",
    ":",
    "{",
    "}",
    "/*",
    "*/",
];

/// Random but CSS-shaped text: rules, declarations, `@media`, and other at-rules.
fn css_soup(rng: &mut Rng) -> String {
    let mut out = String::new();
    for _ in 0..rng.below(12) {
        match rng.below(6) {
            0 => out.push_str(&format!("@media (max-width: {}px) {{ ", rng.below(2000))),
            1 => {
                out.push_str(rng.pick(&["@import x;", "@font-face { src: x }", "@media", "}", "{"]))
            }
            _ => {
                out.push_str(rng.pick(SELECTORS));
                out.push_str(" { ");
                for _ in 0..rng.below(6) {
                    out.push_str(rng.pick(PROPERTIES));
                    out.push(':');
                    for _ in 0..1 + rng.below(3) {
                        out.push(' ');
                        out.push_str(rng.pick(VALUES));
                    }
                    out.push(';');
                }
                if !rng.one_in(8) {
                    out.push('}');
                }
            }
        }
        out.push(' ');
    }
    out
}

const PAGE: &str = "<html><head><title>t</title></head><body><div id=main class=note>\
<p>a</p><p>b</p><ul><li>1</li><li>2</li><li>3</li></ul><a href=\"http://x\">x</a>\
<input type=checkbox checked><table><tr><td>1</td><td>2</td></tr></table></div></body></html>";

/// Parse the sheet and cascade it over [`PAGE`] at a few viewport sizes.
fn parse_and_cascade(input: &[u8]) {
    let css = String::from_utf8_lossy(input);
    let sheet = parse_css(&css);
    let doc = parse_html(PAGE);
    let _ = compute_styles(&doc, &sheet);
    let _ = compute_styles_for_viewport(&doc, &sheet, 0.0, 0.0);
    let _ = compute_styles_for_viewport(&doc, &sheet, -5.0, f32::MAX);
    let _ = parse_declarations(&css);
    let _ = Color::parse(&css);
    let _ = MediaQuery::parse(&css).matches(320.0);
}

#[test]
fn css_soup_does_not_panic() {
    run_cases(
        "css-soup",
        51,
        |rng| css_soup(rng).into_bytes(),
        parse_and_cascade,
    );
}

#[test]
fn random_bytes_do_not_panic() {
    run_cases("css-random", 52, |rng| rng.bytes(256), parse_and_cascade);
}

#[test]
fn mutated_sheets_do_not_panic() {
    run_cases(
        "css-mutated",
        53,
        |rng| {
            let soup = css_soup(rng);
            mutate(rng, soup.as_bytes())
        },
        parse_and_cascade,
    );
}

#[test]
fn single_values_do_not_panic() {
    run_cases(
        "css-values",
        54,
        |rng| {
            let mut out = String::new();
            for _ in 0..1 + rng.below(4) {
                out.push_str(rng.pick(VALUES));
            }
            out.into_bytes()
        },
        |input| {
            let value = String::from_utf8_lossy(input);
            let _ = Color::parse(&value);
            for property in PROPERTIES {
                let _ = parse_declarations(&format!("{property}: {value}"));
            }
            let style =
                format!("<p style=\"color: {value}; width: {value}; margin: {value}\">x</p>");
            let doc = parse_html(&style);
            let _ = compute_styles(&doc, &parse_css(""));
        },
    );
}

#[test]
fn deeply_nested_blocks_do_not_overflow() {
    for depth in [100usize, 2_000] {
        let media =
            "@media (min-width: 1px) { ".repeat(depth) + "p { color: red }" + &"}".repeat(depth);
        let braces = "p { ".repeat(depth) + &"}".repeat(depth);
        let unclosed = "a { b { c { ".repeat(depth);
        for css in [media, braces, unclosed] {
            check_case("css-nesting", depth, css.into_bytes(), parse_and_cascade);
        }
    }
}

#[test]
fn non_ascii_lengths_do_not_panic() {
    // Byte 5 of these values falls inside a multi-byte character, which the
    // `calc(` prefix check used to slice through.
    for value in [
        "calc\u{e9}x)",
        "abcd\u{e9})",
        "1\u{1F600}px",
        "calc(\u{e9})",
    ] {
        let css = format!(
            "p {{ width: {value}; grid-template-columns: {value} {value}; margin: {value} }}"
        );
        parse_and_cascade(css.as_bytes());
    }
}
