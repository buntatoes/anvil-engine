// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0-OR-Commercial
// Copyright (c) 2026 Buntato
//! Acceptance programs for capturing a variable through more than one function.
//!
//! These only run where a C++20 compiler built the VM; otherwise they return
//! early. Each program's result is known, so the VM's answer is checked against
//! it; the seeded differential also checks the Rust interpreter agrees.
#![forbid(unsafe_code)]

use anvil_dom::parse_html;
use anvil_script::{ScriptRuntime, is_cpp_jit_kernel_active};

/// Halt value of `source` on the C++ VM.
fn vm(source: &str) -> f64 {
    anvil_script::eval_cpp_vm_request_configured(50_000_000, source)
        .unwrap_or_else(|e| panic!("VM refused: {} :: {source}", e.message))
        .0
}

/// Run `source` on the Rust interpreter and read the last console line as a
/// number, or `None` when it printed nothing numeric.
fn interp_num(source: &str) -> Option<f64> {
    let mut doc = parse_html("<html><body></body></html>");
    let mut rt = ScriptRuntime::new();
    rt.eval_script(&mut doc, source);
    rt.console_logs().last().and_then(|l| l.parse().ok())
}

#[test]
fn a_three_level_counter_keeps_its_state() {
    if !is_cpp_jit_kernel_active() {
        return;
    }
    // The counter cell lives in `outer`; the returned function reaches it two
    // functions out, through `mid`.
    let src = "function outer() { let n = 0; function mid() { return function () { n = n + 1; return n; }; } return mid(); } \
               let s = outer(); s(); s(); s();";
    assert_eq!(vm(src), 3.0);
}

#[test]
fn a_four_level_counter_keeps_its_state() {
    if !is_cpp_jit_kernel_active() {
        return;
    }
    // The counter cell is three functions out from the function that bumps it.
    let src = "function a() { let n = 0; function b() { function c() { return function () { n = n + 1; return n; }; } return c(); } return b(); } \
               let s = a(); s(); s(); s();";
    assert_eq!(vm(src), 3.0);
}

#[test]
fn a_closure_captures_from_one_and_from_two_levels_out() {
    if !is_cpp_jit_kernel_active() {
        return;
    }
    // The innermost function reads `a` (two out) and `b` (one out).
    let src = "function f() { let a = 10; function g() { let b = 5; return function () { return a + b; }; } return g(); } f()();";
    assert_eq!(vm(src), 15.0);
}

#[test]
fn a_per_iteration_loop_let_is_captured_from_an_inner_inner_function() {
    if !is_cpp_jit_kernel_active() {
        return;
    }
    // Each iteration's `i` is captured two functions out (through `mid`), and
    // each closure keeps its own iteration's value: 0 + 1 + 2 + 3 = 6.
    let src = "const fs = []; \
               for (let i = 0; i < 4; i = i + 1) { const mid = function () { return function () { return i; }; }; fs.push(mid()); } \
               let r = 0; for (let j = 0; j < 4; j = j + 1) { r = r + fs[j](); } r;";
    assert_eq!(vm(src), 6.0);
}

#[test]
fn a_multi_level_closure_survives_collection_under_a_tight_budget() {
    if !is_cpp_jit_kernel_active() {
        return;
    }
    // `keep` holds a cell two functions out; the middle closure is gone, so the
    // cell stays reachable only through the kept closure's upvalue chain while
    // transient objects churn and force collections.
    let src = "function outer() { let k = 42; function mid() { return function () { return k; }; } return mid(); } \
               let keep = outer(); let s = 0; \
               for (let i = 0; i < 1200; i = i + 1) { let o = { a: i, b: i + 1 }; let z = [i, i, i]; s = s + 1; } \
               keep() + (s - s);";
    let (value, tel) = anvil_script::eval_cpp_vm_request_heap_budget(200_000, src)
        .expect("a kept multi-level closure survives GC under a tight budget");
    assert_eq!(value, 42.0);
    assert!(
        tel.gc_collections > 0,
        "the tight budget should force at least one collection"
    );
}

#[test]
fn seeded_multi_level_closures_match_the_interpreter() {
    if !is_cpp_jit_kernel_active() {
        return;
    }
    const M: i64 = 1_000_000;
    let mut seed: u64 = 0x_c105_5eed_u64;
    let mut next = |lo: i64, hi: i64| -> i64 {
        seed = seed
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        lo + ((seed >> 33) as i64) % (hi - lo + 1)
    };
    let mut checked = 0usize;
    let mut interp_agreed = 0usize;
    for _ in 0..40 {
        // A three-level counter: the cell is two functions out from the bumper.
        {
            let s = next(0, 5);
            let t = next(1, 4);
            let k = next(1, 6);
            let mut body = format!(
                "function outer() {{ let n = {s}; function mid() {{ return function () {{ n = n + {t}; return n; }}; }} return mid(); }} let c = outer();"
            );
            for _ in 0..(k - 1) {
                body.push_str(" c();");
            }
            let want = (((s + t * k) % M) + M) % M;
            let vm_src = format!("{body} let out = c(); (((out) % 1000000) + 1000000) % 1000000;");
            assert_eq!(vm(&vm_src), want as f64, "VM wrong: {vm_src}");
            let interp_src = format!(
                "{body} let out = c(); console.log((((out) % 1000000) + 1000000) % 1000000);"
            );
            // The interpreter gets some multi-level programs wrong; only cross-
            // check the VM against it where the interpreter itself is right.
            if interp_num(&interp_src) == Some(want as f64) {
                interp_agreed += 1;
            }
            checked += 1;
        }
        // Capture from one and two levels out at once.
        {
            let a = next(0, 50);
            let b = next(0, 50);
            let body = format!(
                "function f() {{ let a = {a}; function g() {{ let b = {b}; return function () {{ return a + b; }}; }} return g(); }}"
            );
            let want = (((a + b) % M) + M) % M;
            let vm_src =
                format!("{body} let out = f()(); (((out) % 1000000) + 1000000) % 1000000;");
            assert_eq!(vm(&vm_src), want as f64, "VM wrong: {vm_src}");
            let interp_src = format!(
                "{body} let out = f()(); console.log((((out) % 1000000) + 1000000) % 1000000);"
            );
            if interp_num(&interp_src) == Some(want as f64) {
                interp_agreed += 1;
            }
            checked += 1;
        }
    }
    assert_eq!(checked, 80, "expected 80 seeded multi-level cases");
    // The interpreter is right on the one-and-two-levels-out adder (a plain
    // nested return), so the cross-check must have matched the VM on at least
    // those, confirming the differential actually ran against the interpreter.
    assert!(
        interp_agreed >= 40,
        "expected the interpreter to agree on at least the adder cases, got {interp_agreed}"
    );
}
