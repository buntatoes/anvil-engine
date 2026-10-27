// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0-OR-Commercial
// Copyright (c) 2026 Buntato
//! Robustness checks for the newer CSS paths, run through the whole pipeline.
//!
//! Inputs come from a seeded generator, so a failure names a case number
//! that reproduces on every run. Each test stops after a short time budget.
//! Every sheet is parsed, cascaded over a small fixed page, laid out, painted,
//! and rasterized into a small framebuffer. Nothing may panic or hang.

use anvil_dom::parse_html;
use anvil_layout::layout_document;
use anvil_paint::paint;
use anvil_raster::{rasterize, rasterize_parallel};
use anvil_style::{Animator, compute_styles_animated, compute_styles_for_viewport, parse_css};

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

/// Property names for the CSS features these tests cover.
const PROPERTIES: &[&str] = &[
    "display",
    "width",
    "height",
    "max-width",
    "left",
    "top",
    "position",
    "font-size",
    "right",
    "bottom",
    "overflow",
    "gap",
    "grid-template-columns",
    "grid-template-rows",
    "grid-auto-rows",
    "align-items",
    "margin",
    "padding",
    "grid-column",
    "grid-row",
    "grid-area",
    "grid-column-start",
    "grid-column-end",
    "grid-row-start",
    "grid-row-end",
    "transform",
    "transform-origin",
    "opacity",
    "color",
    "background",
    "transition",
    "transition-property",
    "transition-duration",
    "transition-delay",
    "transition-timing-function",
    "animation",
    "animation-name",
    "animation-duration",
    "animation-delay",
    "animation-iteration-count",
    "animation-direction",
    "animation-fill-mode",
    "animation-timing-function",
];

/// Values for those properties, valid and broken.
const VALUES: &[&str] = &[
    "grid",
    "0",
    "1px",
    "-5px",
    "1e9px",
    "-1e39px",
    "NaN",
    "inf",
    "50%",
    "1fr",
    "0fr",
    "-1fr",
    "1e30fr",
    "auto",
    "none",
    "minmax(10px, 1fr)",
    "minmax(1fr, 2fr)",
    "minmax(auto, 40px)",
    "minmax(",
    "minmax(,)",
    "minmax(10px)",
    "repeat(3, 1fr)",
    "repeat(0, 1fr)",
    "repeat(99999999999, 1fr)",
    "repeat(2, minmax(5px, auto) 10%)",
    "repeat(",
    "repeat(x, 1px)",
    "calc(100% - 10px)",
    "stretch",
    "center",
    "end",
    "\u{e9}",
    ";",
    ":",
    "{",
    "}",
    "(",
    ")",
    "span 2",
    "span 0",
    "span -3",
    "span",
    "span span",
    "2 span",
    "1 / -1",
    "-1 / 1",
    "-200",
    "3 / span 2",
    "span 99999 / 2",
    "1 / 2 / 3 / 4",
    "1/2/3/4/5",
    "a / b",
    "/",
    "//",
    "-1",
    "100 / -100",
    "span 100 / span 100",
    "10ch",
    "-1ch",
    "1e30ch",
    "chch",
    "ch",
    "0.5CH",
    "sticky",
    "fixed",
    "absolute",
    "relative",
    "hidden",
    "-50%",
    "rotate(45deg)",
    "rotate(1e30turn)",
    "rotate(-0.25turn)",
    "scale(0)",
    "scale(-1, 4)",
    "scale(1e20)",
    "scaleX(1e-20)",
    "skew(89.99deg)",
    "skewY(90deg)",
    "translate(50%, -1e9px)",
    "translateX(10ch)",
    "matrix(1e30, 0, 0, 1e30, 0, 0)",
    "matrix(0, 1, -1, 0, 3, 4)",
    "matrix(NaN, 0, 0, 1, 0, 0)",
    "rotate(",
    "scale()",
    "left",
    "top",
    "right bottom",
    "10px 20%",
    "0.5",
    "-1",
    "1e40",
    "#f00",
    "rgba(0, 0, 255, 0.5)",
    "all",
    "opacity",
    "transform,",
    "1s",
    "0ms",
    "-2s",
    "1e30s",
    "10ms linear",
    "ease-in-out",
    "cubic-bezier(0, 2, 1, -1)",
    "cubic-bezier(",
    "steps(3, jump-both)",
    "steps(99999999999)",
    "step-start",
    ",",
    "width 1ms",
    "k",
    "'k'",
    "k 1s infinite alternate",
    "infinite",
    "1e30",
    "2.5",
    "both",
    "forwards",
    "reverse",
    "alternate-reverse",
    "k 10ms steps(2) -5ms 3 backwards",
    "k, k, k, k, k",
];

/// Random but CSS-shaped text aimed at the grid properties.
fn css_soup(rng: &mut Rng) -> String {
    let mut out = String::new();
    for _ in 0..rng.below(10) {
        out.push_str(rng.pick(&[
            "#g",
            ".i",
            "div",
            "#a",
            "*",
            "@media (min-width: 1px)",
            "@keyframes k { from",
            "@keyframes k { 50%, to } 10%",
            "@keyframes 'k' { 100%",
            "to",
            "@keyframes",
        ]));
        out.push_str(" { ");
        for _ in 0..rng.below(6) {
            out.push_str(rng.pick(PROPERTIES));
            out.push(':');
            for _ in 0..1 + rng.below(4) {
                out.push(' ');
                out.push_str(rng.pick(VALUES));
            }
            out.push(';');
        }
        if !rng.one_in(8) {
            out.push('}');
        }
        out.push(' ');
    }
    out
}

/// A grid with a few children that the soup can target.
const PAGE: &str = "<div id=g style=\"display:grid\"><div id=a class=i>a</div>\
<div class=i>b</div><div class=i style=\"height:7px\"></div><p class=i>c d e</p>\
<div class=i><div class=i>nested</div></div></div>";

/// Parse, cascade, lay out, paint, and rasterize `input` as a style sheet.
fn run_pipeline(input: &[u8]) {
    let css = String::from_utf8_lossy(input);
    let sheet = parse_css(&css);
    let doc = parse_html(PAGE);
    for (w, h) in [(64.0, 48.0), (0.0, 0.0)] {
        let styles = compute_styles_for_viewport(&doc, &sheet, w, h);
        let tree = layout_document(&doc, &styles, w);
        let list = paint(&tree);
        for scroll in [0.0, 7.5, 1e9] {
            let _ = rasterize(&list, 32, 24, scroll);
        }
        let _ = anvil_layout::hit_test_scrolled(
            &tree,
            3.0,
            4.0,
            anvil_layout::Rect {
                x: 0.0,
                y: 5.0,
                width: w,
                height: h,
            },
        );
        let _ = anvil_layout::hit_test(&tree, 10.0, 10.0);
        let _ = rasterize_parallel(&list.scaled(2.5), 32, 140, 3.0);
    }
}

/// Run `input` through animated restyles: its first half, then all of it,
/// so properties change and transitions start, at a spread of times.
fn run_animated(input: &[u8]) {
    let css = String::from_utf8_lossy(input);
    let half = css
        .char_indices()
        .nth(css.chars().count() / 2)
        .map_or(0, |(i, _)| i);
    let first = parse_css(&css[..half]);
    let all = parse_css(&css);
    let doc = parse_html(PAGE);
    let mut animator = Animator::new();
    for (now, sheet) in [
        (0.0, &first),
        (0.0, &all),
        (16.5, &all),
        (f64::NAN, &first),
        (40.0, &first),
        (1e15, &all),
        (-5.0, &all),
    ] {
        animator.tick(now);
        let styles = compute_styles_animated(&doc, sheet, 64.0, 48.0, &mut animator);
        let list = paint(&layout_document(&doc, &styles, 64.0));
        let _ = rasterize(&list, 32, 24, 0.0);
    }
}

#[test]
fn animated_soup_does_not_panic() {
    run_cases(
        "animated-soup",
        73,
        |rng| css_soup(rng).into_bytes(),
        run_animated,
    );
}

#[test]
fn grid_soup_does_not_panic() {
    run_cases(
        "grid-soup",
        71,
        |rng| css_soup(rng).into_bytes(),
        run_pipeline,
    );
}

#[test]
fn mutated_grid_sheets_do_not_panic() {
    run_cases(
        "grid-mutated",
        72,
        |rng| {
            let soup = css_soup(rng);
            mutate(rng, soup.as_bytes())
        },
        run_pipeline,
    );
}
