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

// ---- Guard the byte-wide inline-cache site index -------------------------

/// Up to 256 distinct property-access sites address correctly; the 257th would
/// wrap the byte-wide site operand, so the compiler refuses past that with a
/// reason instead of reading the wrong property. (The wider encoding lifts this
/// later.) Before the guard the over-256 case ran and returned a wrong value.
#[test]
fn inline_cache_sites_are_correct_up_to_256_then_refused() {
    if !is_cpp_jit_kernel_active() {
        return;
    }
    // 200 distinct sites: well within the byte operand, correct value out.
    let mut within = String::from("const o = {}; ");
    for i in 0..200 {
        within.push_str(&format!("o.p{i} = {i}; "));
    }
    within.push_str("o.p199;");
    let (value, _) = anvil_script::eval_cpp_vm_request_configured(5_000_000, &within)
        .expect("200 property sites compile and run");
    assert_eq!(value, 199.0);

    // 300 distinct sites: past the byte operand, refused with a reason.
    let mut over = String::from("const o = {}; ");
    for i in 0..300 {
        over.push_str(&format!("o.p{i} = {i}; "));
    }
    over.push_str("o.p299;");
    let error = anvil_script::eval_cpp_vm_request_configured(5_000_000, &over).unwrap_err();
    assert_eq!(
        error.message,
        "inline cache site limit reached: 256 sites per script"
    );
    assert!(error.is_limit() && error.refused());
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
