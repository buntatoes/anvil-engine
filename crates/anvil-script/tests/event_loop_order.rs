// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0-OR-Commercial
// Copyright (c) 2026 Buntato
//! Seeded checks of the event loop's order.
//!
//! Each case is a random script of nested `setTimeout`, `setInterval`,
//! `queueMicrotask`, `Promise.resolve().then(...)` chains, and async arrows
//! with `await null`, where every callback logs its own label. A small model
//! of the documented order (the script, then the whole microtask queue,
//! then timers by due time and then by the order they were set, a 4 ms
//! floor for a timer set while a timer callback runs, and a virtual clock)
//! gives the log the runtime must produce, label for label.
//!
//! Other cases check that runaway or huge schedules end within the budget.

use anvil_dom::parse_html;
use anvil_script::ScriptRuntime;
use std::collections::VecDeque;

// ---- shared harness (from the robustness tests, without the mutator) ----

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

/// One step of a generated script.
#[derive(Clone, Debug)]
enum Act {
    Log(usize),
    Timeout(u64, Vec<Act>),
    Micro(Vec<Act>),
    /// `Promise.resolve().then(a).then(b)...`
    Then(Vec<Vec<Act>>),
    /// An async arrow called at once, with `await null` between the parts.
    Async(usize, Vec<Vec<Act>>),
    /// A top-level `setInterval` cleared by its own callback on the given
    /// tick. Each tick logs.
    Interval(usize, u64, u32),
}

struct Gen {
    rng: Rng,
    next_label: usize,
    next_name: usize,
    /// Allow an async arrow to be called from inside another one.
    nested_async: bool,
    in_async: bool,
}

impl Gen {
    fn label(&mut self) -> usize {
        self.next_label += 1;
        self.next_label
    }

    fn name(&mut self) -> usize {
        self.next_name += 1;
        self.next_name
    }

    fn block(&mut self, depth: usize, top: bool) -> Vec<Act> {
        let mut acts = Vec::new();
        for _ in 0..1 + self.rng.below(if depth == 0 { 5 } else { 3 }) {
            let kind = if depth >= 3 { 0 } else { self.rng.below(7) };
            let act = match kind {
                0 | 1 => Act::Log(self.label()),
                2 => {
                    let delay = self.rng.pick(&[0, 0, 1, 3, 4, 5, 10]);
                    Act::Timeout(delay, self.labeled(depth))
                }
                3 => Act::Micro(self.labeled(depth)),
                4 => {
                    let links = (0..1 + self.rng.below(3))
                        .map(|_| self.labeled(depth))
                        .collect();
                    Act::Then(links)
                }
                5 if self.nested_async || !self.in_async => {
                    let name = self.name();
                    let outer = std::mem::replace(&mut self.in_async, true);
                    let parts = (0..1 + self.rng.below(4))
                        .map(|_| self.labeled(depth))
                        .collect();
                    self.in_async = outer;
                    Act::Async(name, parts)
                }
                _ if top => {
                    let name = self.name();
                    let period = self.rng.pick(&[0, 1, 2, 3, 7]);
                    Act::Interval(name, period, 1 + self.rng.below(4) as u32)
                }
                _ => Act::Log(self.label()),
            };
            acts.push(act);
        }
        acts
    }

    /// A nested block that starts with its own label, so every callback
    /// shows up in the log.
    fn labeled(&mut self, depth: usize) -> Vec<Act> {
        let mut acts = vec![Act::Log(self.label())];
        if !self.rng.one_in(3) {
            acts.extend(self.block(depth + 1, false));
        }
        acts
    }
}

fn emit(acts: &[Act], out: &mut String) {
    for act in acts {
        match act {
            Act::Log(label) => out.push_str(&format!("console.log('L{label}'); ")),
            Act::Timeout(delay, body) => {
                out.push_str("setTimeout(() => { ");
                emit(body, out);
                out.push_str(&format!("}}, {delay}); "));
            }
            Act::Micro(body) => {
                out.push_str("queueMicrotask(() => { ");
                emit(body, out);
                out.push_str("}); ");
            }
            Act::Then(links) => {
                out.push_str("Promise.resolve()");
                for body in links {
                    out.push_str(".then(() => { ");
                    emit(body, out);
                    out.push_str("})");
                }
                out.push_str("; ");
            }
            Act::Async(name, parts) => {
                out.push_str(&format!("const a{name} = async () => {{ "));
                for (i, part) in parts.iter().enumerate() {
                    if i > 0 {
                        out.push_str("await null; ");
                    }
                    emit(part, out);
                }
                out.push_str(&format!("}}; a{name}(); "));
            }
            Act::Interval(name, period, ticks) => {
                out.push_str(&format!(
                    "let n{name} = 0; let h{name} = setInterval(() => {{ n{name}++; \
                     console.log('I{name}.' + n{name}); \
                     if (n{name} === {ticks}) {{ clearInterval(h{name}); }} }}, {period}); "
                ));
            }
        }
    }
}

/// Work waiting in a queue.
enum Job {
    Block(Vec<Act>),
    /// Run link `next` of a `then` chain; the later links follow it.
    Link(Vec<Vec<Act>>, usize),
    /// Resume an async arrow at part `next`.
    Resume(Vec<Vec<Act>>, usize),
}

struct Timer {
    due: u64,
    seq: u64,
    job: TimerJob,
}

enum TimerJob {
    Once(Vec<Act>),
    Interval {
        name: usize,
        period: u64,
        ticks: u32,
        done: u32,
    },
}

/// The documented order, as a reference.
struct Model {
    log: Vec<String>,
    micro: VecDeque<Job>,
    timers: Vec<Timer>,
    clock: u64,
    seq: u64,
    in_timer: bool,
}

impl Model {
    fn run(acts: &[Act]) -> Vec<String> {
        let mut m = Model {
            log: Vec::new(),
            micro: VecDeque::new(),
            timers: Vec::new(),
            clock: 0,
            seq: 0,
            in_timer: false,
        };
        m.block(acts);
        m.drain();
        while !m.timers.is_empty() {
            let i = (0..m.timers.len())
                .min_by_key(|&i| (m.timers[i].due, m.timers[i].seq))
                .unwrap();
            let timer = m.timers.remove(i);
            m.clock = m.clock.max(timer.due);
            m.in_timer = true;
            match timer.job {
                TimerJob::Once(body) => m.block(&body),
                TimerJob::Interval {
                    name,
                    period,
                    ticks,
                    done,
                } => {
                    let done = done + 1;
                    m.log.push(format!("I{name}.{done}"));
                    if done < ticks {
                        m.seq += 1;
                        let job = TimerJob::Interval {
                            name,
                            period,
                            ticks,
                            done,
                        };
                        m.timers.push(Timer {
                            due: m.clock + period.max(1),
                            seq: m.seq,
                            job,
                        });
                    }
                }
            }
            m.in_timer = false;
            m.drain();
        }
        m.log
    }

    fn drain(&mut self) {
        while let Some(job) = self.micro.pop_front() {
            match job {
                Job::Block(body) => self.block(&body),
                Job::Link(links, next) => {
                    self.block(&links[next]);
                    // The callback returned, so the next link's promise is
                    // settled and its callback is queued now.
                    if next + 1 < links.len() {
                        self.micro.push_back(Job::Link(links, next + 1));
                    }
                }
                Job::Resume(parts, next) => self.async_from(parts, next),
            }
        }
    }

    fn async_from(&mut self, parts: Vec<Vec<Act>>, at: usize) {
        self.block(&parts[at]);
        if at + 1 < parts.len() {
            self.micro.push_back(Job::Resume(parts, at + 1));
        }
    }

    fn block(&mut self, acts: &[Act]) {
        for act in acts {
            match act {
                Act::Log(label) => self.log.push(format!("L{label}")),
                Act::Timeout(delay, body) => {
                    let delay = if self.in_timer {
                        (*delay).max(4)
                    } else {
                        *delay
                    };
                    self.seq += 1;
                    self.timers.push(Timer {
                        due: self.clock + delay,
                        seq: self.seq,
                        job: TimerJob::Once(body.clone()),
                    });
                }
                Act::Micro(body) => self.micro.push_back(Job::Block(body.clone())),
                Act::Then(links) => self.micro.push_back(Job::Link(links.clone(), 0)),
                Act::Async(_, parts) => self.async_from(parts.clone(), 0),
                Act::Interval(name, period, ticks) => {
                    self.seq += 1;
                    let job = TimerJob::Interval {
                        name: *name,
                        period: (*period).max(1),
                        ticks: *ticks,
                        done: 0,
                    };
                    self.timers.push(Timer {
                        due: self.clock + period,
                        seq: self.seq,
                        job,
                    });
                }
            }
        }
    }
}

fn generate(seed: u64, nested_async: bool) -> (String, Vec<String>) {
    let mut g = Gen {
        rng: Rng::new(seed),
        next_label: 0,
        next_name: 0,
        nested_async,
        in_async: false,
    };
    let acts = g.block(0, true);
    // A script with no event-loop name may run on the numeric VM, which
    // does not keep console output. This empty job keeps every case on the
    // interpreter and logs nothing.
    let mut source = String::from("queueMicrotask(() => {}); ");
    emit(&acts, &mut source);
    (source, Model::run(&acts))
}

fn order_matches(input: &[u8]) {
    let seed = u64::from_le_bytes(input.try_into().expect("8-byte seed"));
    let nested_async = seed >> 32 == 1;
    let (source, want) = generate(seed, nested_async);
    let mut doc = parse_html(&format!("<script>{source}</script>"));
    let mut rt = ScriptRuntime::new();
    rt.run_document_scripts(&mut doc);
    assert!(!rt.was_aborted(), "seed {seed}: aborted\n{source}");
    assert_eq!(rt.console_logs(), &want[..], "seed {seed}:\n{source}");
}

fn run_seeds(label: &str, salt: u64) {
    let budget = test_budget();
    let started = Instant::now();
    let mut case = 0;
    while case < MAX_CASES && started.elapsed() < budget {
        let seed = (salt << 32) | case as u64;
        check_case(label, case, seed.to_le_bytes().to_vec(), order_matches);
        case += 1;
    }
    eprintln!("{label}: {case} cases in {:?}", started.elapsed());
}

/// Async arrows here are only called from outside other async arrows.
#[test]
fn random_schedules_run_in_the_documented_order() {
    run_seeds("event-loop-order", 0);
}

/// The same, with async arrows also called (and not awaited) from inside
/// other async arrows.
#[test]
fn random_schedules_with_nested_async_calls() {
    run_seeds("event-loop-order-nested", 1);
}

fn logs_of(source: &str) -> Vec<String> {
    let mut doc = parse_html(&format!("<script>{source}</script>"));
    let mut rt = ScriptRuntime::new();
    rt.run_document_scripts(&mut doc);
    assert!(!rt.was_aborted(), "aborted: {source}");
    rt.console_logs().to_vec()
}

/// An async function called without `await` from another async function
/// that then awaits must still finish: its rest runs from its own
/// microtask, before the caller resumes.
#[test]
fn an_async_call_inside_an_async_function_finishes() {
    let inner = "const inner = async () => { console.log('x'); await null; console.log('y'); };";
    assert_eq!(
        logs_of(&format!(
            "{inner} const outer = async () => {{ inner(); console.log('z'); await null; console.log('w'); }}; outer();"
        )),
        ["x", "z", "y", "w"]
    );
    assert_eq!(
        logs_of(
            "async function outer() { async function inner() { console.log('x'); await null; console.log('y'); } inner(); console.log('z'); await null; console.log('w'); } outer();"
        ),
        ["x", "z", "y", "w"]
    );
    // Without the caller's await the inner rest already runs.
    assert_eq!(
        logs_of(&format!(
            "{inner} const outer = async () => {{ inner(); console.log('z'); }}; outer();"
        )),
        ["x", "z", "y"]
    );
}

/// Delays that are huge, negative, or not numbers still give a total
/// order: negative and NaN act as 0, huge ones fire last, and the virtual
/// clock does not wrap.
#[test]
fn extreme_delays_keep_their_order() {
    assert_eq!(
        logs_of(
            "setTimeout(() => { console.log('huge'); setTimeout(() => { console.log('after-huge'); }, 1e300); }, 1e300); \
             setTimeout(() => { console.log('inf'); }, Infinity); \
             setTimeout(() => { console.log('neg'); }, -5); \
             setTimeout(() => { console.log('nan'); }, NaN); \
             setTimeout(() => { console.log('ten'); }, 10); \
             queueMicrotask(() => { console.log('micro'); });"
        ),
        ["micro", "neg", "nan", "ten", "huge", "inf", "after-huge"]
    );
}

/// Many timers, a long promise chain, and intervals that never clear end
/// within the case limit (the runtime may stop them with its budget).
#[test]
fn large_schedules_end() {
    let sources = [
        "for (let i = 0; i < 200000; i++) { setTimeout(() => { console.log(i); }, i % 97); }",
        "let p = Promise.resolve(0); for (let i = 0; i < 200000; i++) { p = p.then((v) => v + 1); }",
        "for (let i = 0; i < 1000; i++) { setInterval(() => {}, i % 3); }",
        "const f = async () => { for (let i = 0; i < 100000; i++) { await null; } }; for (let i = 0; i < 50; i++) { f(); }",
        "setInterval(() => { queueMicrotask(() => { setTimeout(() => {}, 0); }); }, 0);",
    ];
    for (index, source) in sources.iter().enumerate() {
        check_case(
            "event-loop-large",
            index,
            source.as_bytes().to_vec(),
            |input| {
                let source = std::str::from_utf8(input).unwrap();
                let mut doc = parse_html(&format!("<script>{source}</script>"));
                let mut rt = ScriptRuntime::new();
                rt.run_document_scripts(&mut doc);
            },
        );
    }
}
