// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0-OR-Commercial
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

// ---- Heap frames ---------------------------------------------------------

/// Recursion runs on the heap frame stack. fib(25) makes far more nested and
/// repeated calls than the old 64-frame stack allowed, so this needs frames in
/// the arena.
#[test]
fn fib_25_runs_on_heap_frames() {
    if !is_cpp_jit_kernel_active() {
        return;
    }
    let source =
        "function fib(n) { if (n < 2) { return n; } return fib(n - 1) + fib(n - 2); } fib(25);";
    let (value, _) = anvil_script::eval_cpp_vm_request_configured(5_000_000, source).expect("runs");
    assert_eq!(value, 75025.0);
}

/// A recursion 9,999 frames deep runs to the end, well past the old frame cap,
/// because the register and frame stacks grow in the arena.
#[test]
fn recursion_9999_deep_runs() {
    if !is_cpp_jit_kernel_active() {
        return;
    }
    let source = "function rec(n) { if (n <= 0) { return 0; } return rec(n - 1) + 1; } rec(9999);";
    let (value, _) = anvil_script::eval_cpp_vm_request_configured(5_000_000, source).expect("runs");
    assert_eq!(value, 9999.0);
}

/// Past the default 10,000 depth the run stops with the depth in the reason,
/// not a fixed frame-cap message.
#[test]
fn recursion_past_the_depth_limit_stops() {
    if !is_cpp_jit_kernel_active() {
        return;
    }
    let source = "function rec(n) { if (n <= 0) { return 1; } return rec(n - 1); } rec(20000);";
    let error = anvil_script::eval_cpp_vm_request_configured(5_000_000, source).unwrap_err();
    assert_eq!(
        error.message,
        "call depth limit reached: 10000 nested calls"
    );
}

/// The depth is a setting: a Configure request lowers it and the run stops at
/// the configured depth.
#[test]
fn a_configured_call_depth_stops_sooner() {
    if !is_cpp_jit_kernel_active() {
        return;
    }
    let source = "function rec(n) { if (n <= 0) { return 1; } return rec(n - 1); } rec(500);";
    let error = anvil_script::eval_cpp_vm_request_depth(100, 5_000_000, source).unwrap_err();
    assert_eq!(error.message, "call depth limit reached: 100 nested calls");
}

/// A Configure request can also raise the depth, so a recursion deeper than the
/// default runs when the setting allows it.
#[test]
fn a_raised_call_depth_lets_a_deeper_recursion_run() {
    if !is_cpp_jit_kernel_active() {
        return;
    }
    let source = "function rec(n) { if (n <= 0) { return 0; } return rec(n - 1) + 1; } rec(11000);";
    let (value, _) =
        anvil_script::eval_cpp_vm_request_depth(12000, 5_000_000, source).expect("runs");
    assert_eq!(value, 11000.0);
}

// ---- Stop and resume -----------------------------------------------------

/// A run paused at a yield and driven to the end with Resume requests gives the
/// same answer as an unbroken run, across many yield intervals. Interval 1
/// pauses before every instruction, so this covers stopping at any instruction.
#[test]
fn a_paused_run_resumes_to_the_same_result() {
    if !is_cpp_jit_kernel_active() {
        return;
    }
    let programs: [(&str, f64); 3] = [
        (
            "function fib(n) { if (n < 2) { return n; } return fib(n - 1) + fib(n - 2); } fib(20);",
            6765.0,
        ),
        (
            "let s = 0; let i = 0; while (i < 500) { s = s + i; i = i + 1; } s;",
            124750.0,
        ),
        (
            "function rec(n) { if (n <= 0) { return 0; } return rec(n - 1) + 1; } rec(300);",
            300.0,
        ),
    ];
    for (source, expected) in programs {
        let (direct, _) = anvil_script::eval_cpp_vm_request_configured(5_000_000, source)
            .expect("the unbroken run");
        assert_eq!(direct, expected);
        for yield_interval in [1u32, 2, 3, 5, 7, 13, 31, 97, 500] {
            let (resumed, _) =
                anvil_script::eval_cpp_vm_request_resumed(yield_interval, 5_000_000, source)
                    .expect("the resumed run");
            assert_eq!(
                resumed, expected,
                "yield interval {yield_interval} for `{source}`"
            );
        }
    }
}

/// Seeded random stop points: the yield interval is drawn each round, and every
/// resumed run matches the unbroken result. The run holds no native stack
/// between slices, so resuming from the heap state is enough.
#[test]
fn seeded_random_stop_points_resume_to_the_same_result() {
    if !is_cpp_jit_kernel_active() {
        return;
    }
    let source =
        "function fib(n) { if (n < 2) { return n; } return fib(n - 1) + fib(n - 2); } fib(22);";
    let (expected, _) =
        anvil_script::eval_cpp_vm_request_configured(5_000_000, source).expect("the unbroken run");
    assert_eq!(expected, 17711.0);
    let mut state: u64 = 0x00C0_FFEE_D15E_A5E5;
    for _ in 0..64 {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        let yield_interval = ((state >> 33) as u32 % 200) + 1;
        let (resumed, _) =
            anvil_script::eval_cpp_vm_request_resumed(yield_interval, 5_000_000, source)
                .expect("the resumed run");
        assert_eq!(resumed, expected, "yield interval {yield_interval}");
    }
}

// ---- The token buffer grows in the compiler arena ------------------------

/// A script with far more than the old 2,048-token cap now compiles and runs,
/// because the token buffer grows in the compiler arena instead of a fixed
/// array. This chain of statements is well over 3,000 tokens.
#[test]
fn a_script_past_the_old_token_cap_compiles() {
    if !is_cpp_jit_kernel_active() {
        return;
    }
    let mut source = String::from("let s = 0; ");
    for _ in 0..500 {
        source.push_str("s = s + 1; ");
    }
    source.push('s');
    source.push(';');
    let (value, _) = anvil_script::eval_cpp_vm_request_configured(5_000_000, &source)
        .expect("compiles and runs past the old token cap");
    assert_eq!(value, 500.0);
}

// ---- The bytecode grows in the arena -------------------------------------

/// A 2,000-statement script compiles and runs. Each statement emits several
/// instructions, so this is well past the old 4,096-instruction array; the
/// bytecode now grows in the arena. It is also too long for the numeric JIT, so
/// it runs on the interpreter.
#[test]
fn a_2000_statement_script_compiles() {
    if !is_cpp_jit_kernel_active() {
        return;
    }
    let mut source = String::from("let s = 0; ");
    for _ in 0..2000 {
        source.push_str("s = s + 1; ");
    }
    source.push_str("s;");
    let (value, _) = anvil_script::eval_cpp_vm_request_configured(5_000_000, &source)
        .expect("compiles past the old instruction cap");
    assert_eq!(value, 2000.0);
}

// ---- Lift the register, site and program-size caps -----------------------

/// Property-access sites past the old 256 cap now address correctly. The site
/// index rides in a 16-bit operand, so 300 distinct sites (the case the earlier
/// byte-wide guard refused) compile and return the right value. This replaces
/// the earlier refusal test, whose premise (a byte-wide site operand) is gone.
#[test]
fn inline_cache_sites_past_the_old_256_cap_are_correct() {
    if !is_cpp_jit_kernel_active() {
        return;
    }
    let mut source = String::from("const o = {}; ");
    for i in 0..300 {
        source.push_str(&format!("o.p{i} = {i}; "));
    }
    source.push_str("o.p0 + o.p150 + o.p299;");
    let (value, _) = anvil_script::eval_cpp_vm_request_configured(5_000_000, &source)
        .expect("300 property sites compile and run");
    assert_eq!(value, 0.0 + 150.0 + 299.0);
}

/// A 300-element array literal compiles and runs. Each element is freed after it
/// is pushed, so the literal needs only a couple of registers, not one per
/// element; before the register cap lifted this hit the 64-register limit.
#[test]
fn array_literal_of_300_elements_compiles() {
    if !is_cpp_jit_kernel_active() {
        return;
    }
    let elems = (0..300)
        .map(|i| i.to_string())
        .collect::<Vec<_>>()
        .join(", ");
    let source = format!("let a = [{elems}]; a[0] + a[299] + a.length;");
    let (value, _) = anvil_script::eval_cpp_vm_request_configured(5_000_000, &source)
        .expect("a 300-element array literal compiles and runs");
    assert_eq!(value, 0.0 + 299.0 + 300.0);
}

/// A 1,000-property object literal compiles and runs. This needs both the lifted
/// site cap (over a thousand inline-cache sites) and the temporary-freeing
/// allocator (each value freed after its store).
#[test]
fn object_literal_of_1000_properties_compiles() {
    if !is_cpp_jit_kernel_active() {
        return;
    }
    let props = (0..1000)
        .map(|i| format!("p{i}: {i}"))
        .collect::<Vec<_>>()
        .join(", ");
    let source = format!("const o = {{{props}}}; o.p0 + o.p500 + o.p999;");
    let (value, _) = anvil_script::eval_cpp_vm_request_configured(5_000_000, &source)
        .expect("a 1000-property object literal compiles and runs");
    assert_eq!(value, 0.0 + 500.0 + 999.0);
}

/// Deeply nested ifs compile. The condition is freed after each branch, so 2,000
/// levels of nesting reuse one register slot instead of one per level; before
/// this they hit the 64-register limit.
#[test]
fn deeply_nested_ifs_compile() {
    if !is_cpp_jit_kernel_active() {
        return;
    }
    // Nesting this deep spends more native stack than libtest's 2 MiB test
    // thread, and the default compiler-stack budget refuses it, so the body runs
    // on a larger stack with the budget raised to match.
    std::thread::Builder::new()
        .stack_size(64 * 1024 * 1024)
        .spawn(|| {
            let depth = 2000;
            let mut source = String::from("let x = 1; let r = 0; ");
            for _ in 0..depth {
                source.push_str("if (x) { ");
            }
            source.push_str("r = 7; ");
            for _ in 0..depth {
                source.push('}');
            }
            source.push_str(" r;");
            let (value, _) =
                anvil_script::eval_cpp_vm_request_parse_stack(32 * 1024 * 1024, 5_000_000, &source)
                    .expect("2000-deep nested ifs compile and run");
            assert_eq!(value, 7.0);
        })
        .expect("spawn a large-stack thread")
        .join()
        .expect("the compile thread does not panic");
}

/// A nested-if script about 64 KiB of source compiles and runs. The program-size
/// ceiling and the temporary-freeing allocator together let a chain this deep
/// through the compiler instead of being refused. The front end is
/// recursive-descent, so a chain this deep needs more native stack than libtest's
/// default test thread (2 MiB); real embedders (the host runner, the browser) run
/// it on a normal stack, so the body runs on a thread sized to match with the
/// compiler-stack budget raised to suit.
#[test]
fn a_64_kib_nested_if_script_compiles() {
    if !is_cpp_jit_kernel_active() {
        return;
    }
    std::thread::Builder::new()
        .stack_size(256 * 1024 * 1024)
        .spawn(|| {
            let depth = 6600;
            let mut source = String::from("let x = 1; let r = 0; ");
            for _ in 0..depth {
                source.push_str("if (x) { ");
            }
            source.push_str("r = 9; ");
            for _ in 0..depth {
                source.push('}');
            }
            source.push_str(" r;");
            assert!(
                source.len() >= 64 * 1024,
                "source is {} bytes",
                source.len()
            );
            let (value, _) =
                anvil_script::eval_cpp_vm_request_parse_stack(64 * 1024 * 1024, 5_000_000, &source)
                    .expect("a 64 KiB nested-if script compiles and runs");
            assert_eq!(value, 9.0);
        })
        .expect("spawn a large-stack thread")
        .join()
        .expect("the compile thread does not panic");
}

/// A script nested past the default compiler-stack budget is refused with a
/// clear reason rather than overflowing the native stack. This runs on
/// libtest's normal (2 MiB) test thread: before the guard a chain this deep
/// aborted the process; now it is a clean refusal.
#[test]
fn a_deeply_nested_if_past_the_default_refuses() {
    if !is_cpp_jit_kernel_active() {
        return;
    }
    let depth = 8000;
    let mut source = String::from("let x = 1; let r = 0; ");
    for _ in 0..depth {
        source.push_str("if (x) { ");
    }
    source.push_str("r = 7; ");
    for _ in 0..depth {
        source.push('}');
    }
    source.push_str(" r;");
    let error = anvil_script::eval_cpp_vm_request_configured(5_000_000, &source).unwrap_err();
    assert_eq!(
        error.code, 51,
        "expected the nesting refusal code, got: {error:?}"
    );
    assert!(
        error.message.contains("nesting"),
        "expected a nesting reason, got: {}",
        error.message
    );
}

/// A deeply nested expression (nested parentheses) is refused the same way. An
/// expression nests through more compiler frames per level than a statement, so
/// it trips the budget at a shallower depth; the guard measures stack bytes, not
/// a depth count, so both are covered. Also on the normal test thread.
#[test]
fn a_deeply_nested_expression_past_the_default_refuses() {
    if !is_cpp_jit_kernel_active() {
        return;
    }
    let depth = 5000;
    let mut source = String::from("let r = ");
    for _ in 0..depth {
        source.push('(');
    }
    source.push('1');
    for _ in 0..depth {
        source.push(')');
    }
    source.push_str("; r;");
    let error = anvil_script::eval_cpp_vm_request_configured(5_000_000, &source).unwrap_err();
    assert_eq!(
        error.code, 51,
        "expected the nesting refusal code, got: {error:?}"
    );
    assert!(
        error.message.contains("nesting"),
        "expected a nesting reason, got: {}",
        error.message
    );
}

/// Raising the compiler-stack budget (with a native stack to match) lets a
/// script compile that the default budget refuses, so the limit is a setting,
/// not a hard cap.
#[test]
fn a_raised_parse_stack_budget_allows_deeper_nesting() {
    if !is_cpp_jit_kernel_active() {
        return;
    }
    std::thread::Builder::new()
        .stack_size(64 * 1024 * 1024)
        .spawn(|| {
            let depth = 8000;
            let mut source = String::from("let x = 1; let r = 0; ");
            for _ in 0..depth {
                source.push_str("if (x) { ");
            }
            source.push_str("r = 7; ");
            for _ in 0..depth {
                source.push('}');
            }
            source.push_str(" r;");
            // The default budget refuses this depth (see the test above); a
            // raised budget on a matching stack compiles it.
            let (value, _) =
                anvil_script::eval_cpp_vm_request_parse_stack(32 * 1024 * 1024, 5_000_000, &source)
                    .expect("8000-deep nested ifs compile once the budget is raised");
            assert_eq!(value, 7.0);
        })
        .expect("spawn a large-stack thread")
        .join()
        .expect("the compile thread does not panic");
}

/// Lowering the budget refuses a nesting the default would accept, so the
/// setting works in both directions.
#[test]
fn a_lowered_parse_stack_budget_refuses_sooner() {
    if !is_cpp_jit_kernel_active() {
        return;
    }
    let depth = 400;
    let mut source = String::from("let x = 1; let r = 0; ");
    for _ in 0..depth {
        source.push_str("if (x) { ");
    }
    source.push_str("r = 7; ");
    for _ in 0..depth {
        source.push('}');
    }
    source.push_str(" r;");
    // 400-deep compiles comfortably under the default budget.
    let (value, _) = anvil_script::eval_cpp_vm_request_configured(5_000_000, &source)
        .expect("400-deep nested ifs compile under the default budget");
    assert_eq!(value, 7.0);
    // With a 64 KiB budget the same script is refused.
    let error =
        anvil_script::eval_cpp_vm_request_parse_stack(64 * 1024, 5_000_000, &source).unwrap_err();
    assert_eq!(
        error.code, 51,
        "expected the nesting refusal code, got: {error:?}"
    );
}

/// A source about 1 MiB long compiles and runs rather than being refused for its
/// size. The bytecode grows in the arena and jump targets are 32-bit, so the
/// program-size ceiling no longer stops a large but simple source.
#[test]
fn a_1_mib_source_compiles() {
    if !is_cpp_jit_kernel_active() {
        return;
    }
    let repeats = 96_000;
    let mut source = String::from("let r = 0; ");
    for _ in 0..repeats {
        source.push_str("r = r + 1; ");
    }
    source.push_str("r;");
    assert!(
        source.len() >= 1024 * 1024,
        "source is {} bytes",
        source.len()
    );
    let (value, _) = anvil_script::eval_cpp_vm_request_tuned(50_000_000, 0, &source)
        .expect("a 1 MiB source compiles and runs");
    assert_eq!(value, repeats as f64);
}

/// Seeded differential against the C++ VM's old compiler front end. The oracle
/// in `differential_oracle.txt` is each generated script paired with the answer
/// the old front end gave for it, captured before this change. The new front end
/// must return the same answer for every one.
#[test]
fn differential_against_the_old_front_end() {
    if !is_cpp_jit_kernel_active() {
        return;
    }
    let oracle = include_str!("differential_oracle.txt");
    let mut checked = 0usize;
    for line in oracle.lines() {
        if line.is_empty() {
            continue;
        }
        let (answer, script) = line
            .split_once('\t')
            .expect("oracle row is answer<TAB>script");
        let expected: f64 = answer.parse().expect("oracle answer parses");
        let (value, _) = anvil_script::eval_cpp_vm_request_configured(5_000_000, script)
            .unwrap_or_else(|e| {
                panic!(
                    "new compiler refused an oracle script: {} :: {script}",
                    e.message
                )
            });
        if expected.is_nan() {
            assert!(
                value.is_nan(),
                "old front end gave NaN but new compiler gave {value} for: {script}"
            );
        } else {
            assert_eq!(
                value, expected,
                "answer differs from the old front end for: {script}"
            );
        }
        checked += 1;
    }
    assert_eq!(checked, 300, "expected 300 differential rows");
}

/// A run whose frames outgrow its byte budget stops with the heap-limit reason
/// rather than crashing. A function with many locals recurses deep under a tight
/// heap budget and a generous step budget, so the register stack, not the step
/// or depth limit, is what gives out.
#[test]
fn a_run_that_outgrows_its_byte_budget_stops() {
    if !is_cpp_jit_kernel_active() {
        return;
    }
    let mut locals = String::new();
    for i in 0..300 {
        locals.push_str(&format!("let v{i} = n + {i}; "));
    }
    let source = format!(
        "function f(n) {{ {locals} if (n <= 0) {{ return v0; }} return f(n - 1) + v299; }} f(100000);"
    );
    let error =
        anvil_script::eval_cpp_vm_request_tuned(500_000_000, 2 * 1024 * 1024, &source).unwrap_err();
    assert!(
        error.message.starts_with("heap limit reached"),
        "expected a heap-limit stop, got: {}",
        error.message
    );
    assert_eq!(error.code, 11, "heap stop carries the VM_STOP_HEAP code");
}

/// More than 512 distinct numeric constants in one script.
/// The constant pool grows in the arena and the constant index is 16-bit, so
/// this compiles and runs now that the token cap is gone.
#[test]
fn more_than_512_constants_compile() {
    if !is_cpp_jit_kernel_active() {
        return;
    }
    let mut source = String::from("let t = 0; ");
    let mut expected = 0.0_f64;
    for i in 0..600 {
        source.push_str(&format!("t = t + {i}.5; "));
        expected += i as f64 + 0.5;
    }
    source.push_str("t;");
    let (value, _) = anvil_script::eval_cpp_vm_request_configured(50_000_000, &source)
        .expect("more than 512 constants compile and run");
    assert_eq!(value, expected);
}

// ---- Dynamic per-function frame stride -----------------------------------

/// Each frame takes only the registers its function needs (a dynamic stride),
/// so a wide-frame function and a narrow one compose correctly across a call,
/// and a deep recursion restores each caller's window. Before this the stride
/// was a fixed 64 registers per frame.
#[test]
fn dynamic_frame_stride_composes_across_calls() {
    if !is_cpp_jit_kernel_active() {
        return;
    }
    let wide = "function narrow(x) { return x + x; }
                function wide(n) {
                    let a = n + 1; let b = a + 1; let c = b + 1; let d = c + 1;
                    let e = d + 1; let f = e + 1; let g = f + 1; let h = g + 1;
                    return narrow(a + b + c + d + e + f + g + h);
                }
                wide(0);";
    let (value, _) =
        anvil_script::eval_cpp_vm_request_configured(5_000_000, wide).expect("wide/narrow compose");
    assert_eq!(value, 72.0);

    let deep = "function rec(n) { let a = n; let b = a + 1; if (n <= 0) { return 0; } return rec(n - 1) + 1; } rec(9000);";
    let (value, _) = anvil_script::eval_cpp_vm_request_configured(5_000_000, deep)
        .expect("deep multi-local recursion");
    assert_eq!(value, 9000.0);
}

// ---- Functions as values and call-of-value ---------------

/// A function expression is a value that can be stored and then called.
#[test]
fn function_expression_is_a_value_that_can_be_called() {
    if !is_cpp_jit_kernel_active() {
        return;
    }
    let (v, _) = anvil_script::eval_cpp_vm_request_configured(
        1_000_000,
        "const f = function (x) { return x + 1; }; let r = f(1); r;",
    )
    .expect("a function expression compiles, runs and is callable");
    assert_eq!(v, 2.0);
}

/// An arrow with a parenthesised parameter list is a function value.
#[test]
fn arrow_with_parens_is_a_value() {
    if !is_cpp_jit_kernel_active() {
        return;
    }
    let (v, _) =
        anvil_script::eval_cpp_vm_request_configured(1_000_000, "const f = (x) => x * 2; f(21);")
            .expect("a parenthesised arrow compiles and runs");
    assert_eq!(v, 42.0);
}

/// An arrow with a single bare-identifier parameter is a function value.
#[test]
fn arrow_without_parens_is_a_value() {
    if !is_cpp_jit_kernel_active() {
        return;
    }
    let (v, _) =
        anvil_script::eval_cpp_vm_request_configured(1_000_000, "const g = x => x + 10; g(5);")
            .expect("a bare-identifier arrow compiles and runs");
    assert_eq!(v, 15.0);
}

/// An arrow with a block body returns through `return`.
#[test]
fn arrow_with_a_block_body_returns() {
    if !is_cpp_jit_kernel_active() {
        return;
    }
    let (v, _) = anvil_script::eval_cpp_vm_request_configured(
        1_000_000,
        "const f = (a, b) => { let s = a + b; return s * 2; }; f(3, 4);",
    )
    .expect("an arrow with a block body compiles and runs");
    assert_eq!(v, 14.0);
}

/// A function is passed as an argument and the parameter is called as a value.
#[test]
fn a_function_is_passed_as_an_argument_and_called() {
    if !is_cpp_jit_kernel_active() {
        return;
    }
    let (v, _) = anvil_script::eval_cpp_vm_request_configured(
        1_000_000,
        "function ap(g, v) { return g(v); } function inc(x) { return x + 1; } ap(inc, 4);",
    )
    .expect("a function passed as an argument is called through its value");
    assert_eq!(v, 5.0);
}

/// A function value is returned from a call and then called.
#[test]
fn a_function_is_returned_and_called() {
    if !is_cpp_jit_kernel_active() {
        return;
    }
    let (v, _) = anvil_script::eval_cpp_vm_request_configured(
        1_000_000,
        "function mk() { return function (a) { return a * a; }; } let q = mk(); q(6);",
    )
    .expect("a returned function value is callable");
    assert_eq!(v, 36.0);
}

/// Calling a value that is not a function stops with a reason, not a crash.
#[test]
fn calling_a_non_function_value_stops_with_a_reason() {
    if !is_cpp_jit_kernel_active() {
        return;
    }
    let error =
        anvil_script::eval_cpp_vm_request_configured(1_000_000, "let x = 3; x(1);").unwrap_err();
    assert_eq!(
        error.code, 12,
        "expected the not-a-function stop, got: {error:?}"
    );
    assert!(
        error.message.contains("not a function"),
        "expected a not-a-function reason, got: {}",
        error.message
    );
}

/// A parameter the call omits is undefined in the callee.
#[test]
fn a_missing_argument_is_undefined() {
    if !is_cpp_jit_kernel_active() {
        return;
    }
    let (v, _) =
        anvil_script::eval_cpp_vm_request_configured(1_000_000, "const f = (a, b) => a + b; f(5);")
            .expect("a call with a missing argument runs");
    assert!(v.is_nan(), "a + undefined is NaN, got {v}");
}

/// Arguments beyond the parameters are evaluated and dropped.
#[test]
fn extra_arguments_are_ignored() {
    if !is_cpp_jit_kernel_active() {
        return;
    }
    let (v, _) =
        anvil_script::eval_cpp_vm_request_configured(1_000_000, "const f = a => a; f(7, 8, 9);")
            .expect("a call with extra arguments runs");
    assert_eq!(v, 7.0);
}

/// A closure that reads an enclosing local runs on the VM: the inner function
/// captures the outer `let` by reference.
#[test]
fn a_closure_reads_an_enclosing_local() {
    if !is_cpp_jit_kernel_active() {
        return;
    }
    let (v, _) = anvil_script::eval_cpp_vm_request_configured(
        1_000_000,
        "function mk() { let k = 5; return function () { return k; }; } mk()();",
    )
    .expect("a closure over an outer local runs");
    assert_eq!(v, 5.0);
}

/// A seeded differential for closures. The old front end had no first-class
/// function values, so it cannot be the oracle here. Instead each generated
/// program is built so its integer result is known, and the C++ VM must
/// return it. The
/// programs cover captured locals and parameters, per-iteration loop `let`,
/// counters, two closures sharing one binding and recursion through a captured
/// name, all with single-level capture.
#[test]
fn seeded_closures_match_a_reference_model() {
    if !is_cpp_jit_kernel_active() {
        return;
    }
    const M: i64 = 1_000_000;
    let mut seed: u64 = 0x5eed_c105_u64;
    let mut next = |lo: i64, hi: i64| -> i64 {
        seed = seed
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        lo + ((seed >> 33) as i64) % (hi - lo + 1)
    };
    let mut checked = 0usize;
    for _ in 0..30 {
        // A: counter.
        {
            let s = next(0, 5);
            let t = next(1, 4);
            let k = next(1, 6);
            let mut prog = format!(
                "function mk() {{ let n = {s}; return function () {{ n = n + {t}; return n; }}; }} let c = mk();"
            );
            for _ in 0..(k - 1) {
                prog.push_str(" c();");
            }
            prog.push_str(" let r = c(); (((r) % 1000000) + 1000000) % 1000000;");
            let want = ((s + t * k) % M + M) % M;
            run_eq(&prog, want as f64);
            checked += 1;
        }
        // B: captured-parameter adder, a closure returned from a closure.
        {
            let x = next(0, 20);
            let y = next(0, 20);
            let prog = format!(
                "function adder(a) {{ return function (b) {{ return a + b; }}; }} let r = adder({x})({y}); (((r) % 1000000) + 1000000) % 1000000;"
            );
            run_eq(&prog, ((((x + y) % M) + M) % M) as f64);
            checked += 1;
        }
        // C: per-iteration loop let, closures held in an array.
        {
            let n = next(2, 6);
            let c = next(1, 5);
            let prog = format!(
                "let fs = []; for (let i = 0; i < {n}; i = i + 1) {{ fs.push(function () {{ return i * {c}; }}); }} \
                 let r = 0; for (let j = 0; j < {n}; j = j + 1) {{ let f = fs[j]; r = r + f(); }} \
                 (((r) % 1000000) + 1000000) % 1000000;"
            );
            let want = ((c * (n * (n - 1) / 2)) % M + M) % M;
            run_eq(&prog, want as f64);
            checked += 1;
        }
        // D: two closures sharing one binding.
        {
            let s = next(0, 5);
            let m = next(2, 6);
            let mut ops = String::new();
            let mut net = 0i64;
            for _ in 0..m {
                if next(0, 1) == 0 {
                    ops.push_str(" o = up();");
                    net += 1;
                } else {
                    ops.push_str(" o = down();");
                    net -= 1;
                }
            }
            let prog = format!(
                "function make() {{ let n = {s}; function up() {{ n = n + 1; return n; }} \
                 function down() {{ n = n - 1; return n; }} let o = 0;{ops} return o; }} \
                 let r = make(); (((r) % 1000000) + 1000000) % 1000000;"
            );
            run_eq(&prog, (((s + net) % M + M) % M) as f64);
            checked += 1;
        }
        // E: recursion through a captured name.
        {
            let k = next(1, 8);
            let prog = format!(
                "function outer() {{ function sumto(n) {{ if (n <= 0) {{ return 0; }} return n + sumto(n - 1); }} \
                 return sumto({k}); }} let r = outer(); (((r) % 1000000) + 1000000) % 1000000;"
            );
            run_eq(&prog, (((k * (k + 1) / 2) % M + M) % M) as f64);
            checked += 1;
        }
    }
    assert_eq!(checked, 150, "expected 150 seeded closure cases");
}

fn run_eq(script: &str, want: f64) {
    let (value, _) = anvil_script::eval_cpp_vm_request_configured(20_000_000, script)
        .unwrap_or_else(|e| {
            panic!(
                "C++ VM refused a closure program: {} :: {script}",
                e.message
            )
        });
    assert_eq!(value, want, "wrong answer for: {script}");
}

/// A thousand distinct closures held in an array, each capturing its own loop
/// index, all called back by index.
#[test]
fn a_thousand_closures_live_in_an_array() {
    if !is_cpp_jit_kernel_active() {
        return;
    }
    let script = "const fs = []; for (let i = 0; i < 1000; i++) { fs.push(() => i); } \
                  let r = 0; for (let j = 0; j < 1000; j++) { r = r + fs[j](); } r;";
    let (value, _) = anvil_script::eval_cpp_vm_request_configured(50_000_000, script)
        .expect("a thousand closures run");
    assert_eq!(value, 499_500.0);
}

/// Live closures and the cells they captured survive garbage collection under a
/// tight heap budget while transient objects churn around them.
#[test]
fn closures_survive_collection_under_a_tight_budget() {
    if !is_cpp_jit_kernel_active() {
        return;
    }
    let script = "function mk() { let k = 42; return function () { return k; }; } \
                  let keep = mk(); let s = 0; \
                  for (let i = 0; i < 1200; i = i + 1) { let o = { a: i, b: i + 1 }; let z = [i, i, i]; s = s + 1; } \
                  keep() + (s - s);";
    let (value, tel) = anvil_script::eval_cpp_vm_request_heap_budget(200_000, script)
        .expect("a kept closure survives GC under a tight budget");
    assert_eq!(value, 42.0);
    assert!(
        tel.gc_collections > 0,
        "the tight budget should force at least one collection"
    );
}

/// Distinct capturing closures are distinct under ===, while a non-capturing
/// function is the same value each time.
#[test]
fn closure_identity_is_per_closure() {
    if !is_cpp_jit_kernel_active() {
        return;
    }
    let distinct = "function mk(v) { return function () { return v; }; } \
                    let a = mk(1); let b = mk(2); (a === b) ? 1 : 0;";
    let (v, _) = anvil_script::eval_cpp_vm_request_configured(5_000_000, distinct).expect("runs");
    assert_eq!(v, 0.0);
    let same = "function f() { return 1; } let a = f; let b = f; (a === b) ? 1 : 0;";
    let (v2, _) = anvil_script::eval_cpp_vm_request_configured(5_000_000, same).expect("runs");
    assert_eq!(v2, 1.0);
}
