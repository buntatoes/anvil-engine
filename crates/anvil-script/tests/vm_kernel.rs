// SPDX-License-Identifier: LicenseRef-Proprietary
// Copyright (c) 2026 Buntato
//! Checks of the C++ VM request protocol and the pieces the kernel work adds.
//!
//! The C++ part only builds where a C++20 compiler is installed. Without it
//! every script runs on the Rust interpreter, so the checks that need the VM
//! return early.
#![forbid(unsafe_code)]

use anvil_script::{eval_cpp_vm_request, eval_cpp_vm_script, is_cpp_jit_kernel_active};

/// An out buffer large enough for the whole telemetry struct.
const BIG_CAP: u32 = 8192;

#[test]
fn a_run_request_matches_a_plain_script() {
    if !is_cpp_jit_kernel_active() {
        return;
    }
    for source in [
        "let a = 2; let b = a * 3; b;",
        "function sq(x) { return x * x; } let r = sq(7); r;",
        "let s = 0; let i = 0; while (i < 10) { s = s + i; i = i + 1; } s;",
    ] {
        let plain = eval_cpp_vm_script(source).map(|(value, _)| value);
        let request = eval_cpp_vm_request(source, BIG_CAP)
            .ok()
            .map(|(value, _)| value);
        assert_eq!(plain, request, "{source}");
    }
}

#[test]
fn a_short_out_buffer_is_reported() {
    if !is_cpp_jit_kernel_active() {
        return;
    }
    let error = eval_cpp_vm_request("let a = 1; a;", 8).unwrap_err();
    assert_eq!(error.code, 4);
    assert_eq!(error.message, "out buffer too small");
}

#[test]
fn a_request_reports_a_stop_reason() {
    if !is_cpp_jit_kernel_active() {
        return;
    }
    let error = eval_cpp_vm_request("let i = 0; while (i < 1000000) { i = i + 1; } i;", BIG_CAP)
        .unwrap_err();
    assert_eq!(error.message, "step limit reached: 100000 steps");
}

#[test]
fn a_request_runs_a_real_program() {
    if !is_cpp_jit_kernel_active() {
        return;
    }
    let (value, telemetry) =
        eval_cpp_vm_request("let a = 5; let b = a + 1; b;", BIG_CAP).expect("runs");
    assert_eq!(value, 6.0);
    assert!(telemetry.bytecode_ops_executed > 0);
}

#[test]
fn a_plain_script_is_not_read_as_a_request() {
    if !is_cpp_jit_kernel_active() {
        return;
    }
    assert_eq!(
        eval_cpp_vm_script("let a = 41; a + 1;").map(|(value, _)| value),
        Some(42.0)
    );
}

#[test]
fn seeded_random_request_headers_never_crash() {
    if !is_cpp_jit_kernel_active() {
        return;
    }
    let mut state: u64 = 0x9E37_79B9_7F4A_7C15;
    let mut next = || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    for _ in 0..2000 {
        let count = (next() % 40) as usize;
        let mut source = String::from("AVK1");
        for _ in 0..count {
            source.push((b'!' + (next() % 90) as u8) as char);
        }
        // A source that starts with the magic is read as a request. Whatever
        // the header and payload bytes are, it must never crash.
        let _ = eval_cpp_vm_script(&source);
    }
}

#[test]
fn a_configured_step_budget_stops_a_run_sooner() {
    if !is_cpp_jit_kernel_active() {
        return;
    }
    let error = anvil_script::eval_cpp_vm_request_configured(
        1000,
        "let i = 0; while (i < 1000000) { i = i + 1; } i;",
    )
    .unwrap_err();
    assert_eq!(error.message, "step limit reached: 1000 steps");
}

#[test]
fn a_configured_budget_leaves_work_under_it_alone() {
    if !is_cpp_jit_kernel_active() {
        return;
    }
    let (value, _) =
        anvil_script::eval_cpp_vm_request_configured(100000, "let a = 2; let b = a * 3; b;")
            .expect("runs");
    assert_eq!(value, 6.0);
}

// ---- One heap on a byte budget ------------------------------------------

/// Build a string past the old 16 KiB string-pool cap, by doubling with `+=`
/// until it is at least 1 MiB. It used to stop at the fixed pool; now the
/// growable pool in the arena carries it and the run finishes.
#[test]
fn a_big_string_builds_past_the_old_pool_cap() {
    if !is_cpp_jit_kernel_active() {
        return;
    }
    // "x" doubled 20 times is 2^20 = 1,048,576 bytes.
    let source = "let s = \"x\"; for (let i = 0; i < 20; i = i + 1) { s = s + s; } let r = 7; r;";
    let (value, telemetry) = eval_cpp_vm_script(source).expect("runs to the end");
    assert_eq!(value, 7.0);
    assert!(telemetry.stop_reason().is_none());
    // The interned bytes alone are well past the old 16 KiB pool.
    assert!(telemetry.heap_bytes_allocated > 1_000_000);
}

/// Interning still dedups across a string larger than 1 MiB: two copies built
/// the same way are the same interned string, so `===` is true.
#[test]
fn interning_dedups_a_large_string() {
    if !is_cpp_jit_kernel_active() {
        return;
    }
    let source = "let s = \"ab\"; for (let i = 0; i < 19; i = i + 1) { s = s + s; } \
                  let t = \"ab\"; for (let i = 0; i < 19; i = i + 1) { t = t + t; } \
                  let r = (s === t) ? 1 : 0; r;";
    let (value, _) = eval_cpp_vm_script(source).expect("runs");
    assert_eq!(value, 1.0);
}

/// A tiny heap byte budget, set by Configure, stops a string that would
/// otherwise fit, and reports the budget in the reason.
#[test]
fn a_small_heap_budget_reports_the_limit() {
    if !is_cpp_jit_kernel_active() {
        return;
    }
    // 64 KiB budget: building a ~1 MiB string must stop with the heap limit.
    let source = "let s = \"x\"; for (let i = 0; i < 20; i = i + 1) { s = s + s; } let r = 7; r;";
    let error = anvil_script::eval_cpp_vm_request_heap_budget(64 * 1024, source).unwrap_err();
    assert_eq!(error.message, "heap limit reached: 65536 bytes");
}

/// The default budget leaves ordinary scripts alone, and a generous explicit
/// budget runs the same work the same way.
#[test]
fn a_generous_heap_budget_runs_normally() {
    if !is_cpp_jit_kernel_active() {
        return;
    }
    let (value, _) =
        anvil_script::eval_cpp_vm_request_heap_budget(0, "const o = {a: 1, b: 2}; o.a + o.b;")
            .expect("runs on the default budget");
    assert_eq!(value, 3.0);
    let (value, _) = anvil_script::eval_cpp_vm_request_heap_budget(
        128 * 1024 * 1024,
        "const o = {a: 1, b: 2}; o.a + o.b;",
    )
    .expect("runs on a large budget");
    assert_eq!(value, 3.0);
}
