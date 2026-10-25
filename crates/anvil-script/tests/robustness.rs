// SPDX-License-Identifier: LicenseRef-Proprietary
// Copyright (c) 2026 Buntato
//! Robustness checks for the script runtime.
//!
//! Inputs come from a seeded generator, so a failure names a case number
//! that reproduces on every run. Each test stops after a short time budget.
//! A script may throw, abort, or do nothing. Running it must not panic or
//! hang, and the step budget must stop runaway loops and recursion.
//!
//! Each case runs on its own thread with a 16 MiB stack, so these tests do
//! not depend on the default test-thread stack size.

use anvil_dom::parse_html;
use anvil_script::ScriptRuntime;

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

const TOKENS: &[&str] = &[
    "let ", "const ", "var ", "x", "y", "i", "f", "o", "a", "=", "==", "===", "!=", "+", "-", "*",
    "/", "%", "**", "<", ">", "<=", "&&", "||", "!", "?", ":", "(", ")", "{", "}", "[", "]", ";",
    ",", ".", "=>", "0", "1", "-1", "1e308", "0.1", "NaN", "Infinity", "\"s\"", "'t'", "`u${x}`",
    "\"\u{e9}\u{1F600}\"", "true", "false", "null", "undefined", "if ", "else ", "while ",
    "for ", "return ", "break", "continue", "function ", "new ", "typeof ", "this", "try ",
    "catch ", "finally ", "throw ", "async ", "await ", "document", "getElementById('p')",
    "querySelector('div')", "textContent", "innerHTML", "createElement('b')", "appendChild",
    "console.log", "JSON.parse", "JSON.stringify", "Math.max", "parseInt", "setTimeout",
    "Promise.resolve()", "localStorage", "setItem", "getItem", "fetch('/x')", "length", "push",
    "map", "join", "split", " ", "\n", "\\", "/*", "*/", "//",
];

const SAMPLES: &[&str] = &[
    "let total = 0; for (let i = 0; i < 20; i++) { total = total + i; } total",
    "function f(n) { if (n < 2) { return n; } return f(n - 1) + f(n - 2); } f(10)",
    "let p = document.getElementById('p'); p.textContent = 'x' + JSON.stringify({a: [1, 2]});",
    "const a = [3, 1, 2].map(x => x * 2).join(','); console.log(a);",
    "let o = JSON.parse('{\"k\": [1, {\"m\": null}]}'); o.k.length",
    "localStorage.setItem('k', 'v'); localStorage.getItem('k')",
    "setTimeout(() => { document.querySelector('div').innerHTML = '<b>t</b>'; }, 0);",
    "try { throw 'e'; } catch (err) { console.log(err); } finally { 1; }",
    "let s = ''; while (s.length < 50) { s = s + 'ab'; } s.split('a').length",
];

const PAGE: &str = "<div id=d><p id=p>text</p><button id=b onclick=\"x = 1\">go</button></div>";

/// Sources that exercise promises, async/await, and the timer queue.
const ASYNC_SAMPLES: &[&str] = &[
    "Promise.resolve(1).then(x => x + 1).then(x => { document.getElementById('p').textContent = x; });",
    "async function f() { let a = await Promise.resolve(2); return a + 1; } f().then(v => { document.getElementById('p').textContent = v; });",
    "let id = setInterval(() => { clearInterval(id); }, 0);",
    "Promise.all([Promise.resolve(1), Promise.reject(2)]).catch(e => e);",
    "Promise.allSettled([Promise.resolve(1), Promise.reject(2)]).then(a => a.length);",
    "queueMicrotask(() => { setTimeout(() => { document.getElementById('p').textContent = 'q'; }, 0); });",
    "Promise.any([Promise.reject(1), Promise.resolve(2)]).then(v => v);",
    "Promise.race([Promise.resolve('a'), Promise.resolve('b')]).then(v => v);",
    "let later = new Promise(res => { setTimeout(() => { res(5); }, 0); }); later.then(v => v);",
    "async function g() { for (let i = 0; i < 3; i++) { await Promise.resolve(i); } } g();",
    "async function h() { try { await Promise.reject('e'); } catch (err) { return err; } } h();",
    "function sleep(ms) { return new Promise(r => setTimeout(r, ms)); } async function w(n) { for (let i = 0; i < n; i++) { await sleep(i); } return n; } w(2); w(3);",
    "async function k() { for await (const v of [1, Promise.resolve(2)]) { if (v > 1) { break; } } try { await null; } finally { await 1; } } k();",
    "const a = async (x) => { while (x > 0) { await null; x--; } return x; }; a(3).then(v => v);",
    "setTimeout(async () => { await Promise.reject(1); }, 0); queueMicrotask(async () => { await 2; });",
    "async function m() { let { q, r: [s] } = { q: 1, r: [await 2] }; return q + s; } m();",
];

/// Run `source` as an inline script, then as a direct eval, an expression,
/// a click handler, and pending timers, each on a fresh document.
fn run_script(input: &[u8]) {
    let source = String::from_utf8_lossy(input);

    let mut doc = parse_html(PAGE);
    let mut runtime = ScriptRuntime::new();
    runtime.eval_script(&mut doc, &source);
    let _ = runtime.eval_expr(&mut doc, &source);
    let _ = runtime.run_microtasks(&mut doc);
    let _ = runtime.run_timers(&mut doc);
    let _ = runtime.console_logs();
    if let Some(button) = doc.get_element_by_id("b") {
        let _ = runtime.dispatch_click(&mut doc, button);
    }

    let mut rust_only = ScriptRuntime::with_jit_enabled(false);
    let mut doc = parse_html(PAGE);
    rust_only.eval_script(&mut doc, &source);
}

fn token_soup(rng: &mut Rng) -> Vec<u8> {
    let mut out = String::new();
    for _ in 0..rng.below(40) {
        out.push_str(rng.pick(TOKENS));
    }
    out.into_bytes()
}

#[test]
fn token_soup_does_not_panic() {
    run_cases("script-tokens", 61, token_soup, run_script);
}

#[test]
fn mutated_samples_do_not_panic() {
    run_cases("script-mutated", 62, |rng| {
        let sample = rng_sample(rng);
        mutate(rng, sample.as_bytes())
    }, run_script);
}

#[test]
fn spliced_samples_do_not_panic() {
    run_cases("script-spliced", 63, |rng| {
        // Cut two samples at random char boundaries and join the halves.
        let left = rng_sample(rng);
        let right = rng_sample(rng);
        let cut_left = boundary(left, rng.below(left.len() + 1));
        let cut_right = boundary(right, rng.below(right.len() + 1));
        format!("{}{}", &left[..cut_left], &right[cut_right..]).into_bytes()
    }, run_script);
}

#[test]
fn runaway_scripts_are_stopped_by_the_budget() {
    let runaway = [
        "while (true) {}",
        "for (;;) { let x = 1; }",
        "function r() { r(); } r();",
        "function a() { b(); } function b() { a(); } a();",
        "let f = () => f(); f();",
        "let s = 'a'; while (true) { s = s + s; }",
        "let a = []; while (true) { a.push(a.length); }",
        "function t() { setTimeout(t, 0); } t();",
        "function m() { Promise.resolve().then(m); } m();",
    ];
    for (index, source) in runaway.iter().enumerate() {
        check_case("script-runaway", index, source.as_bytes().to_vec(), run_script);
    }
}

fn rng_sample(rng: &mut Rng) -> &'static str {
    rng.pick(SAMPLES)
}

fn rng_async_sample(rng: &mut Rng) -> &'static str {
    rng.pick(ASYNC_SAMPLES)
}

#[test]
fn mutated_async_samples_do_not_panic() {
    run_cases(
        "script-async",
        64,
        |rng| {
            let sample = rng_async_sample(rng);
            mutate(rng, sample.as_bytes())
        },
        run_script,
    );
}

#[test]
fn async_and_timer_runaways_are_stopped_by_the_budget() {
    let runaway = [
        "function m() { Promise.resolve().then(m); } m();",
        "function t() { setTimeout(t, 0); } t();",
        "function iv() { let id = setInterval(iv, 0); } iv();",
        "async function a() { while (true) { await Promise.resolve(1); } } a();",
        "let p = Promise.resolve(0); while (true) { p = p.then(x => x + 1); }",
        "new Promise(res => { while (true) { res(1); } });",
    ];
    for (index, source) in runaway.iter().enumerate() {
        check_case(
            "script-async-runaway",
            index,
            source.as_bytes().to_vec(),
            run_script,
        );
    }
}

fn boundary(text: &str, mut at: usize) -> usize {
    while !text.is_char_boundary(at) {
        at -= 1;
    }
    at
}

#[test]
fn string_growth_is_capped() {
    for source in [
        "let s = 'a'; while (true) { s = s + s; }",
        "let s = 'a'; while (true) { s += s; }",
        "let s = 'abcdefgh'; while (true) { s = `${s}${s}`; }",
        "let s = 'abcdefgh'; while (true) { s = s.repeat(1024); }",
        "let s = 'ab'; let a = [s, s, s, s]; while (true) { s = a.join(s); a = [s, s, s, s]; }",
    ] {
        let mut doc = parse_html(PAGE);
        let mut runtime = ScriptRuntime::with_jit_enabled(false);
        runtime.eval_script(&mut doc, source);
        assert!(runtime.was_aborted(), "not aborted: {source}");
    }
    // Ordinary concatenation is untouched.
    let mut doc = parse_html(PAGE);
    let mut runtime = ScriptRuntime::with_jit_enabled(false);
    runtime.eval_script(&mut doc, "let s = 'a' + 'b'; s += `c${s}`; document.getElementById('p').textContent = s;");
    assert!(!runtime.was_aborted());
    let p = doc.get_element_by_id("p").unwrap();
    assert_eq!(doc.text_content(p), "abcab");
}

#[test]
fn stray_closing_brace_does_not_stall_the_compiler() {
    // The numeric compiler's top-level loop once sat forever on a `}` with
    // no open block. Such a script must be rejected and fall back.
    let sources = ["}", "1 }", "let s = 1; }", "let s nnerHTML = '<b>t</b>'; }, 0);", "}}{"];
    for (index, source) in sources.iter().enumerate() {
        check_case("script-brace", index, source.as_bytes().to_vec(), |input| {
            let source = String::from_utf8_lossy(input);
            let _ = anvil_script::eval_cpp_vm_script(&source);
            run_script(input);
        });
    }
}
