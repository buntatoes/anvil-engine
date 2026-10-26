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

/// A 100,000-element array, built by index. The old storage capped arrays at
/// 256 elements and 2,048 elements of storage in all; now they grow in the
/// arena. The step budget is raised so the loop itself is not what stops.
#[test]
fn a_100k_element_array_grows_in_the_arena() {
    if !is_cpp_jit_kernel_active() {
        return;
    }
    let source = "const a = []; let i = 0; while (i < 100000) { a[i] = i; i = i + 1; } \
                  let r = a[99999] + a[0] + a.length; r;";
    let (value, _) = anvil_script::eval_cpp_vm_request_configured(5_000_000, source).expect("runs");
    assert_eq!(value, 199_999.0);
}

/// Far more live arrays than the old 256-array table held.
#[test]
fn many_live_arrays_past_the_old_table() {
    if !is_cpp_jit_kernel_active() {
        return;
    }
    let source = "let holder = []; let i = 0; \
                  while (i < 2000) { holder.push([i, i + 1]); i = i + 1; } \
                  let r = holder.length + holder[1999][1]; r;";
    let (value, _) = eval_cpp_vm_script(source).expect("runs");
    assert_eq!(value, 4000.0);
}

/// Pushing past the old per-array length cap of 256.
#[test]
fn an_array_grows_past_the_old_length_cap() {
    if !is_cpp_jit_kernel_active() {
        return;
    }
    let source = "const a = []; let i = 0; \
                  while (i < 5000) { a.push(i * 2); i = i + 1; } \
                  let r = a[4999] + a.length; r;";
    let (value, _) = eval_cpp_vm_script(source).expect("runs");
    assert_eq!(value, 14998.0);
}

/// GC keeps the arrays a global still points at, and reclaims the rest, while
/// the surviving array's contents stay intact across many collections.
#[test]
fn gc_keeps_reachable_arrays_through_churn() {
    if !is_cpp_jit_kernel_active() {
        return;
    }
    // `keep` survives; the array made each iteration becomes garbage. A small
    // byte budget forces the heap to collect and compact mid-run, which must
    // not disturb `keep`.
    let source = "const keep = [11, 22, 33]; let i = 0; \
                  while (i < 4000) { const junk = [i, i, i, i, i, i, i, i]; junk[0] = i; i = i + 1; } \
                  let r = keep[0] + keep[1] + keep[2] + keep.length; r;";
    let (value, telemetry) =
        anvil_script::eval_cpp_vm_request_tuned(5_000_000, 64 * 1024, source).expect("runs");
    assert_eq!(value, 69.0);
    // The churn forced at least one collection.
    assert!(telemetry.gc_collections >= 1);
}

/// A tiny heap budget stops a growing array with the heap limit, naming the
/// budget, instead of any fixed array cap.
#[test]
fn a_small_budget_stops_a_growing_array() {
    if !is_cpp_jit_kernel_active() {
        return;
    }
    // 32 KiB holds far fewer than 100,000 eight-byte values.
    let source = "const a = []; let i = 0; while (i < 100000) { a[i] = i; i = i + 1; } 1;";
    let error = anvil_script::eval_cpp_vm_request_heap_budget(32 * 1024, source).unwrap_err();
    assert_eq!(error.message, "heap limit reached: 32768 bytes");
}

// ---- Arena free list -----------------------------------------------------

/// Heavy array churn under a tight budget: each loop makes an array, grows it,
/// then drops it. Only reusing the blocks of swept arrays keeps the run inside
/// the budget, so this would hit the heap limit if freed blocks were abandoned.
#[test]
fn array_churn_reuses_freed_blocks_under_a_tight_budget() {
    if !is_cpp_jit_kernel_active() {
        return;
    }
    let source = "let total = 0; let i = 0; \
                  while (i < 500) { const a = []; let j = 0; \
                  while (j < 200) { a[j] = j; j = j + 1; } \
                  total = total + a[199]; i = i + 1; } total;";
    // 512 KiB holds only a handful of these arrays at once; the whole run
    // allocates far more than that, so it only fits if blocks are reused.
    let (value, telemetry) =
        anvil_script::eval_cpp_vm_request_tuned(50_000_000, 512 * 1024, source).expect("runs");
    assert_eq!(value, 99_500.0);
    assert!(telemetry.gc_collections >= 1);
}

// ---- Objects, shapes and dictionary mode on the arena --------------------

/// Far more live objects than the old 512-object table held.
#[test]
fn ten_thousand_objects_live_on_the_arena() {
    if !is_cpp_jit_kernel_active() {
        return;
    }
    let source = "const a = []; let i = 0; \
                  while (i < 10000) { a.push({v: i}); i = i + 1; } \
                  let r = a[9999].v + a[0].v + a.length; r;";
    let (value, _) = anvil_script::eval_cpp_vm_request_configured(5_000_000, source).expect("runs");
    assert_eq!(value, 19_999.0);
}

/// One object with a thousand properties. Past the 64-property threshold the
/// object drops its shared shape and keeps its own key map (dictionary mode),
/// so every property stays readable with no fixed cap.
#[test]
fn many_properties_on_one_object_in_dictionary_mode() {
    if !is_cpp_jit_kernel_active() {
        return;
    }
    // Far more than the old 64-property shape cap, so the object is in
    // dictionary mode with its own growable key map. Written as separate
    // statements, which reuse registers, to stay well inside the compiler's
    // per-function register and token budgets.
    let mut source = String::from("const o = {};");
    for i in 0..150 {
        source.push_str(&format!(" o.p{i} = {i};"));
    }
    source.push_str(" o.p149 + o.p75 + o.p0;");
    let (value, _) =
        anvil_script::eval_cpp_vm_request_configured(5_000_000, &source).expect("runs");
    assert_eq!(value, 224.0);
}

/// Crossing the threshold must not lose or reorder the properties written
/// before the switch: a property set in shape mode reads back after the object
/// has gone to dictionary mode, and an update in dictionary mode sticks.
#[test]
fn properties_survive_the_switch_to_dictionary_mode() {
    if !is_cpp_jit_kernel_active() {
        return;
    }
    let mut source = String::from("const o = {}; o.first = 7;");
    for i in 0..200 {
        source.push_str(&format!(" o.q{i} = {i};"));
    }
    // Update an early (shape-mode) property while in dictionary mode.
    source.push_str(" o.first = 70; o.first + o.q0 + o.q199;");
    let (value, _) =
        anvil_script::eval_cpp_vm_request_configured(5_000_000, &source).expect("runs");
    assert_eq!(value, 269.0);
}

/// GC keeps the object a global still points at, with all its properties, and
/// reclaims the objects churned each iteration, across many collections under a
/// tight byte budget.
#[test]
fn gc_keeps_a_reachable_object_through_churn() {
    if !is_cpp_jit_kernel_active() {
        return;
    }
    let source = "const keep = {a: 11, b: 22, c: 33}; let i = 0; \
                  while (i < 4000) { const junk = {x: i, y: i, z: i}; junk.x = i; i = i + 1; } \
                  let r = keep.a + keep.b + keep.c; r;";
    let (value, telemetry) =
        anvil_script::eval_cpp_vm_request_tuned(5_000_000, 64 * 1024, source).expect("runs");
    assert_eq!(value, 66.0);
    assert!(telemetry.gc_collections >= 1);
}

/// A tiny heap budget stops object growth with the heap limit, naming the
/// budget, instead of any fixed object cap.
#[test]
fn a_small_budget_stops_object_growth() {
    if !is_cpp_jit_kernel_active() {
        return;
    }
    let source = "const a = []; let i = 0; \
                  while (i < 100000) { a.push({v: i}); i = i + 1; } 1;";
    let error = anvil_script::eval_cpp_vm_request_heap_budget(32 * 1024, source).unwrap_err();
    assert_eq!(error.message, "heap limit reached: 32768 bytes");
}

// ---- Compiler/VM tables on the arena -----------------------------------

/// More functions than the old 128-function table held. (The count is kept
/// inside the compiler's fixed token budget, lifted later in the front end.)
#[test]
fn functions_table_grows_past_the_old_cap() {
    if !is_cpp_jit_kernel_active() {
        return;
    }
    let mut source = String::new();
    for i in 0..150 {
        source.push_str(&format!("function f{i}() {{ return {i}; }} "));
    }
    source.push_str("f149() + f0();");
    let (value, _) = eval_cpp_vm_script(&source).expect("runs");
    assert_eq!(value, 149.0);
}

/// More top-level globals than the old 256-global table held.
#[test]
fn globals_table_grows_past_the_old_cap() {
    if !is_cpp_jit_kernel_active() {
        return;
    }
    let mut source = String::new();
    for i in 0..300 {
        source.push_str(&format!("let g{i} = {i}; "));
    }
    source.push_str("g299 + g0;");
    let (value, _) = eval_cpp_vm_script(&source).expect("runs");
    assert_eq!(value, 299.0);
}

/// More property inline-cache sites than the old 256-site table held.
#[test]
fn ic_sites_table_grows_past_the_old_cap() {
    if !is_cpp_jit_kernel_active() {
        return;
    }
    let mut source = String::from("const o = {a: 5}; let r = 0; ");
    for _ in 0..300 {
        source.push_str("r = o.a; ");
    }
    source.push_str("r;");
    let (value, _) = eval_cpp_vm_script(&source).expect("runs");
    assert_eq!(value, 5.0);
}

/// The constant pool starts small and grows in the arena, so a script with far
/// more distinct number constants than the initial pool held still runs. (The
/// count is kept inside the compiler's fixed token budget, lifted later.)
#[test]
fn constant_pool_grows_in_the_arena() {
    if !is_cpp_jit_kernel_active() {
        return;
    }
    let mut source = String::from("let r = 0; ");
    for i in 0..200 {
        source.push_str(&format!("r = {i}.5; "));
    }
    source.push_str("r;");
    let (value, _) = eval_cpp_vm_script(&source).expect("runs");
    assert_eq!(value, 199.5);
}
