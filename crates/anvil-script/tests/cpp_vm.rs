// SPDX-License-Identifier: LicenseRef-Proprietary
// Copyright (c) 2026 Buntato
//! Checks of the C++ VM and of how pages route scripts to it.
//!
//! The C++ part only builds where a C++20 compiler is installed. Without
//! it every script runs on the Rust interpreter, so the checks that need
//! the VM return early.

use anvil_dom::parse_html;
use anvil_script::{
    ScriptRuntime, VmError, eval_cpp_vm_script, eval_cpp_vm_script_bindings,
    eval_cpp_vm_script_checked, is_cpp_jit_kernel_active,
};

/// Halt value of `source` on the C++ VM, or `None` when it did not run there.
fn cpp(source: &str) -> Option<f64> {
    eval_cpp_vm_script(source).map(|(value, _)| value)
}

/// Why the VM refused or stopped `source`. Panics when it ran to the end.
fn vm_error(source: &str) -> VmError {
    match eval_cpp_vm_script_checked(source) {
        Ok((value, _)) => panic!("ran to the end with {value}: {source}"),
        Err(error) => error,
    }
}

/// Run `source` as a page script and return the runtime.
fn page(source: &str) -> ScriptRuntime {
    let mut doc = parse_html("<html><body><p id=\"out\"></p></body></html>");
    let mut rt = ScriptRuntime::new();
    rt.eval_script(&mut doc, source);
    rt
}

#[test]
fn functions_read_and_write_top_level_variables() {
    if !is_cpp_jit_kernel_active() {
        return;
    }
    assert_eq!(
        cpp("let g = 1; function wr() { g = 9; } wr(); g;"),
        Some(9.0)
    );
    assert_eq!(
        cpp("let g = 7; function rd() { return g; } g = 8; rd();"),
        Some(8.0)
    );
    let (_, globals, _) = eval_cpp_vm_script_bindings(
        "let s = 0; let i = 1; while (i <= 10) { s = s + i; i = i + 1; }",
    )
    .expect("runs on the VM");
    assert!(globals.contains(&("s".to_string(), 55.0)), "{globals:?}");
    assert!(globals.contains(&("i".to_string(), 11.0)), "{globals:?}");
}

#[test]
fn temporaries_are_reused_between_statements() {
    if !is_cpp_jit_kernel_active() {
        return;
    }
    let source = format!("let a = 0; {} a;", "a = a + 1; ".repeat(100));
    assert_eq!(cpp(&source), Some(100.0));
    let body = "t = t + n * 2; ".repeat(60);
    let source = format!("function f(n) {{ let t = 0; {body} return t; }} let r = f(1); r;");
    assert_eq!(cpp(&source), Some(120.0));
}

#[test]
fn objects_held_only_by_top_level_variables_survive_collection() {
    if !is_cpp_jit_kernel_active() {
        return;
    }
    let source = "let keep = {v: 7}; let i = 0; \
                  while (i < 600) { let t = {v: i}; i = i + 1; } keep.v;";
    assert_eq!(cpp(source), Some(7.0));
    let source = "let keep = [7]; let i = 0; \
                  while (i < 300) { let t = [i]; i = i + 1; } keep[0];";
    assert_eq!(cpp(source), Some(7.0));
}

#[test]
fn console_log_is_not_dropped() {
    let rt = page("let x = 1; console.log(x);");
    assert_eq!(rt.console_logs(), ["1"]);
    assert!(!rt.last_script_used_vm());
}

#[test]
fn scripts_with_unknown_names_are_left_to_the_interpreter() {
    if !is_cpp_jit_kernel_active() {
        return;
    }
    for source in [
        "foo(1); 2;",
        "let r = page_total + 1; r;",
        "let r = 1; done = r; r;",
        "let r = Math.max(1, 5); r;",
        "let a = [1, 2]; a.pop(); a.length;",
        "let o = {a: 1}; let r = o.a; o.toString(); r;",
        "function f(x) { return x; } let g = 1; g(2);",
        "let r = 1; (r)(2); r;",
        "let r = 0; try { r = 1; } finally { r = 2; } r;",
        "let r = 0; switch (r) { case 0: r = 1; } r;",
        "let f = (x) => x; 1;",
        "let s = 'a\\nb'; 1;",
        "let n = 1e3; n;",
        "return 1;",
    ] {
        assert_eq!(cpp(source), None, "{source}");
    }
}

#[test]
fn plain_numeric_scripts_still_run_on_the_vm() {
    if !is_cpp_jit_kernel_active() {
        return;
    }
    assert_eq!(cpp("let a = 2; let b = a * 3; b;"), Some(6.0));
    assert_eq!(
        cpp("function sq(x) { return x * x; } let r = sq(7); r;"),
        Some(49.0)
    );
    let rt = page("let a = 2; let b = a * 3;");
    assert!(rt.last_script_used_vm());
}

#[test]
fn run_limits_stop_with_a_reason() {
    if !is_cpp_jit_kernel_active() {
        return;
    }
    let cases: [(&str, u32, &str); 3] = [
        (
            // Numeric, so the native loop counts the steps.
            "let i = 0; while (i < 1000000) { i = i + 1; } i;",
            1,
            "step limit reached: 100000 steps",
        ),
        (
            "let o = {n: 0}; while (o.n < 1000000) { o.n = o.n + 1; } o.n;",
            1,
            "step limit reached: 100000 steps",
        ),
        (
            "function d(n) { return d(n); } d(1);",
            2,
            "call depth limit reached: 63 nested calls",
        ),
    ];
    for (source, code, message) in cases {
        let error = vm_error(source);
        assert_eq!(
            (error.code, error.message.as_str()),
            (code, message),
            "{source}"
        );
        assert!(error.is_limit() && !error.refused(), "{source}");
    }
}

#[test]
fn work_under_the_limits_still_runs() {
    if !is_cpp_jit_kernel_active() {
        return;
    }
    // Strings were cut at 512 bytes when joined, so these two compared equal.
    let source = "let s = 'abcdefghij'; let i = 0; while (i < 6) { s = s + s; i = i + 1; } \
                  let a = s + 'x'; let b = s + 'y'; let r = 0; if (a === b) { r = 1; } r;";
    assert_eq!(cpp(source), Some(0.0));
    // Grown arrays leave their old space behind; it is reused.
    let source = "let a = []; let i = 0; while (i < 250) { a[i] = i; i = i + 1; } a[249];";
    assert_eq!(cpp(source), Some(249.0));
    let mut props = String::from("let o = {};");
    for i in 0..32 {
        props.push_str(&format!(" o.p{i} = {i};"));
    }
    props.push_str(" o.p31;");
    assert_eq!(cpp(&props), Some(31.0));
}

#[test]
fn compile_limits_refuse_with_a_reason() {
    if !is_cpp_jit_kernel_active() {
        return;
    }
    let nested = format!("{}1{};", "(1 + ".repeat(70), ")".repeat(70));
    let mut functions = String::new();
    let mut globals = String::new();
    for i in 0..129 {
        functions.push_str(&format!("function f{i}() {{}} "));
    }
    for i in 0..257 {
        globals.push_str(&format!("let g{i} = 0; "));
    }
    let sites = format!("let o = {{a: 1}}; let r = 0; {}", "r = o.a; ".repeat(257));
    let tokens = format!("let r = [{}];", vec!["1"; 1100].join(", "));
    let mut constants = String::from("let r = 0; ");
    for i in 0..257 {
        constants.push_str(&format!("r = {}.5 + {}.5; ", 2 * i, 2 * i + 1));
    }
    let cases = [
        (
            nested,
            40,
            "register limit reached: 64 registers per function",
        ),
        (functions, 44, "function limit reached: 128 functions"),
        (globals, 45, "global limit reached: 256 globals"),
        (sites, 46, "property site limit reached: 256 sites"),
        (tokens, 41, "token limit reached: 2048 tokens"),
        (constants, 43, "constant limit reached: 512 constants"),
    ];
    for (source, code, message) in cases {
        let error = vm_error(&source);
        assert_eq!((error.code, error.message.as_str()), (code, message));
        assert!(error.is_limit() && error.refused());
    }
}

#[test]
fn refused_scripts_say_why() {
    if !is_cpp_jit_kernel_active() {
        return;
    }
    let cases = [
        ("let t = typeof 1; 1;", "unsupported word 'typeof'"),
        (
            "let x = 1; console.log(x);",
            "unsupported method call 'log'",
        ),
        (
            "foo(1); 2;",
            "call to a function not declared in this script 'foo'",
        ),
        (
            "let r = page_total + 1; r;",
            "name not declared in this script 'page_total'",
        ),
        (
            "let r = Math.max(1, 5); r;",
            "unsupported Math function 'max'",
        ),
        ("let n = 1e3; n;", "unsupported number '1e3'"),
        ("let f = (x) => x; 1;", "unsupported operator '=>'"),
        ("return 1;", "return outside a function near '1'"),
        ("let s = 'abc", "unterminated string ''abc'"),
        ("let r = 1; }", "unexpected near '}'"),
        (
            "let x = 1; { let x = 2; } x;",
            "name declared again in an inner block 'x'",
        ),
    ];
    for (source, message) in cases {
        let error = vm_error(source);
        assert!(error.refused() && !error.is_limit(), "{source}");
        assert_eq!(error.message, message, "{source}");
    }
}

#[test]
fn a_vm_limit_leaves_the_script_to_the_interpreter() {
    if !is_cpp_jit_kernel_active() {
        return;
    }
    let mut doc = parse_html("<html><body><p id=\"out\"></p></body></html>");
    let mut rt = ScriptRuntime::new();
    rt.eval_script(
        &mut doc,
        "let o = {n: 0}; while (o.n < 9000) { o.n = o.n + 1; } let total = o.n;",
    );
    assert!(!rt.last_script_used_vm());
    assert_eq!(
        rt.last_vm_error().map(|error| error.message.as_str()),
        Some("step limit reached: 100000 steps")
    );
    assert!(!rt.was_aborted());
    rt.eval_script(
        &mut doc,
        "document.getElementById('out').textContent = 'total ' + total;",
    );
    assert!(rt.last_vm_error().is_none());
    let out = doc.get_element_by_id("out").expect("out");
    assert_eq!(doc.text_content(out), "total 9000");
}

#[test]
fn calls_pass_their_arguments() {
    if !is_cpp_jit_kernel_active() {
        return;
    }
    let cases = [
        ("function add(a, b) { return a + b; } add(2, 3);", 5.0),
        (
            "function d(n) { if (n === 0) { return 0; } return 1 + d(n - 1); } d(50);",
            50.0,
        ),
        (
            "function add(a, b) { return a + b; } add(add(1, 2), add(3, 4));",
            10.0,
        ),
        (
            "function fib(n) { if (n < 2) { return n; } return fib(n - 1) + fib(n - 2); } fib(15);",
            610.0,
        ),
        (
            "function sub(a, b) { return a - b; } let x = 10; sub(x * 2, x - 3);",
            13.0,
        ),
        ("function first(a, b) { return a; } first(4);", 4.0),
        ("function first(a) { return a; } first(1, 2, 3);", 1.0),
        ("function none() { return 7; } none(1);", 7.0),
    ];
    for (source, want) in cases {
        assert_eq!(cpp(source), Some(want), "{source}");
    }
}

#[test]
fn calls_nest_until_the_frame_stack_is_full() {
    if !is_cpp_jit_kernel_active() {
        return;
    }
    let deep = "function d(n) { if (n === 0) { return 0; } return 1 + d(n - 1); } d(62);";
    assert_eq!(cpp(deep), Some(62.0));
    let error =
        vm_error("function d(n) { if (n === 0) { return 0; } return 1 + d(n - 1); } d(63);");
    assert_eq!(error.message, "call depth limit reached: 63 nested calls");
}

#[test]
fn functions_can_be_called_before_they_are_declared() {
    if !is_cpp_jit_kernel_active() {
        return;
    }
    assert_eq!(
        cpp("let r = sq(6); function sq(x) { return x * x; } r;"),
        Some(36.0)
    );
    let even_odd = "function even(n) { if (n === 0) { return 1; } return odd(n - 1); } \
                    function odd(n) { if (n === 0) { return 0; } return even(n - 1); } \
                    even(10) + odd(7);";
    assert_eq!(cpp(even_odd), Some(2.0));
}

#[test]
fn function_declarations_the_vm_cannot_scope_are_refused() {
    if !is_cpp_jit_kernel_active() {
        return;
    }
    let cases = [
        (
            "function f() { function g() { return 1; } return g(); } f();",
            "function declared inside a block or function 'g'",
        ),
        (
            "let r = 0; if (r === 0) { function g() { return 1; } } r;",
            "function declared inside a block or function 'g'",
        ),
        (
            "function f() { return 1; } function f() { return 2; } f();",
            "function declared twice 'f'",
        ),
        (
            "let f = 1; function f() { return 2; } f;",
            "name used for a variable and a function 'f'",
        ),
    ];
    for (source, message) in cases {
        assert_eq!(vm_error(source).message, message, "{source}");
    }
}

#[test]
fn later_scripts_see_what_a_vm_script_declared() {
    if !is_cpp_jit_kernel_active() {
        return;
    }
    let mut doc = parse_html("<html><body><p id=\"out\"></p></body></html>");
    let mut rt = ScriptRuntime::new();
    rt.eval_script(
        &mut doc,
        "let total = 2 + 3; let name = 'a' + 'b'; let flag = total > 4; let nothing = null; \
         let half = 1 / 2; function twice(x) { return x * 2; }",
    );
    assert!(rt.last_script_used_vm());
    rt.eval_script(
        &mut doc,
        "document.getElementById('out').textContent = \
         total + ' ' + name + ' ' + flag + ' ' + nothing + ' ' + half + ' ' + twice(total);",
    );
    let out = doc.get_element_by_id("out").expect("out");
    assert_eq!(doc.text_content(out), "5 ab true null 0.5 10");
}

#[test]
fn objects_in_top_level_variables_leave_the_script_to_the_interpreter() {
    if !is_cpp_jit_kernel_active() {
        return;
    }
    let mut doc = parse_html("<html><body><p id=\"out\"></p></body></html>");
    let mut rt = ScriptRuntime::new();
    rt.eval_script(&mut doc, "let o = {a: 4}; let list = [1, 2]; let n = o.a;");
    assert!(!rt.last_script_used_vm());
    assert_eq!(
        rt.last_vm_error().map(|error| error.message.as_str()),
        Some("top-level 'o' holds an object, array or function, which later scripts could not see")
    );
    rt.eval_script(
        &mut doc,
        "document.getElementById('out').textContent = o.a + list[1] + n;",
    );
    let out = doc.get_element_by_id("out").expect("out");
    assert_eq!(doc.text_content(out), "10");
}

#[test]
fn top_level_variables_too_big_to_hand_over_leave_the_script_to_the_interpreter() {
    if !is_cpp_jit_kernel_active() {
        return;
    }
    let mut doc = parse_html("<html><body><p id=\"out\"></p></body></html>");
    let mut rt = ScriptRuntime::new();
    rt.eval_script(
        &mut doc,
        "let s = 'x'; let i = 0; while (i < 12) { s = s + s; i = i + 1; }",
    );
    assert!(!rt.last_script_used_vm());
    assert_eq!(
        rt.last_vm_error().map(|error| error.message.as_str()),
        Some("the top-level variables do not fit in 4096 bytes")
    );
    rt.eval_script(
        &mut doc,
        "document.getElementById('out').textContent = s.length + ' ' + i;",
    );
    let out = doc.get_element_by_id("out").expect("out");
    assert_eq!(doc.text_content(out), "4096 12");
}

#[test]
fn undefined_is_a_value_of_its_own() {
    if !is_cpp_jit_kernel_active() {
        return;
    }
    let cases = [
        ("let u; let r = 0; if (u === undefined) { r = 1; } r;", 1.0),
        ("let u; let r = 0; if (u === null) { r = 1; } r;", 0.0),
        (
            "function f(a, b) { if (b === undefined) { return a; } return a + b; } f(4);",
            4.0,
        ),
        (
            "function f() { return; } let r = 0; if (f() === undefined) { r = 1; } r;",
            1.0,
        ),
        (
            "function f() { let x = 1; } let r = 0; if (f() === undefined) { r = 1; } r;",
            1.0,
        ),
    ];
    for (source, want) in cases {
        assert_eq!(cpp(source), Some(want), "{source}");
    }
    let nan = cpp("let u; let r = u + 1; r;").expect("ran");
    assert!(nan.is_nan());
}

#[test]
fn numbers_follow_javascript() {
    if !is_cpp_jit_kernel_active() {
        return;
    }
    // Each runs once through objects, which keeps it off the native loops,
    // and once as plain numbers.
    let cases = [
        ("1 / 0", f64::INFINITY),
        ("-1 / 0", f64::NEG_INFINITY),
        ("7 % 3", 1.0),
        ("-7 % 3", -1.0),
        ("5.5 % 2", 1.5),
        ("4294967296 | 0", 0.0),
        ("4294967297 | 0", 1.0),
        ("-1 >>> 0", 4294967295.0),
        ("1 << 31", -2147483648.0),
        ("Math.floor(-2.5)", -3.0),
        ("Math.round(2.5)", 3.0),
        ("Math.round(-2.5)", -2.0),
        ("Math.sqrt(10000000000)", 100000.0),
        ("Math.floor(100000000000000000000)", 1e20),
    ];
    for (expr, want) in cases {
        let plain = format!("let r = {expr}; r;");
        assert_eq!(cpp(&plain), Some(want), "{plain}");
        let boxed = format!("let o = {{v: 0}}; o.v = {expr}; o.v;");
        assert_eq!(cpp(&boxed), Some(want), "{boxed}");
    }
    let neg_zero = cpp("let z = -1 * 0; z;").expect("ran");
    assert!(neg_zero == 0.0 && neg_zero.is_sign_negative());
    assert!(cpp("let r = 0 / 0; r;").expect("ran").is_nan());
    assert!(cpp("let r = 5 % 0; r;").expect("ran").is_nan());
    assert_eq!(
        cpp("let n = 0 / 0; let r = 0; if (n === n) { r = 1; } r;"),
        Some(0.0)
    );
    assert_eq!(
        cpp("let t = true; let r = 0; if (t === 1) { r = 1; } r;"),
        Some(0.0)
    );
    assert_eq!(
        cpp("let s = ''; let r = 0; if (s) { r = 1; } r;"),
        Some(0.0)
    );
}

#[test]
fn values_the_vm_cannot_convert_go_to_the_interpreter() {
    if !is_cpp_jit_kernel_active() {
        return;
    }
    for source in [
        "let s = 'a'; let r = s + 1; 1;",
        "let s = 'a'; let r = s < 'b'; 1;",
        "let s = 'abc'; let n = s.length; n;",
        "let o = {a: 1}; let k = 'a'; let v = o[k]; 1;",
        "let a = [1]; a[-1] = 2; 1;",
        "let a = [1]; a[0.5] = 2; 1;",
        "let n = 1; n.x = 2; 1;",
    ] {
        let error = vm_error(source);
        assert_eq!(error.code, 10, "{source}: {}", error.message);
    }
    let mut doc = parse_html("<html><body><p id=\"out\"></p></body></html>");
    let mut rt = ScriptRuntime::new();
    rt.eval_script(&mut doc, "let s = 'abc'; let n = s.length + 1;");
    assert!(!rt.last_script_used_vm());
    rt.eval_script(&mut doc, "document.getElementById('out').textContent = n;");
    let out = doc.get_element_by_id("out").expect("out");
    assert_eq!(doc.text_content(out), "4");
}

#[test]
fn loops_can_break_continue_and_run_do_while() {
    if !is_cpp_jit_kernel_active() {
        return;
    }
    let cases = [
        (
            "let s = 0; for (let i = 0; i < 10; i++) { if (i === 5) { break; } s += i; } s;",
            10.0,
        ),
        (
            "let s = 0; for (let i = 0; i < 10; i++) { if (i % 2 === 0) { continue; } s += i; } s;",
            25.0,
        ),
        (
            "let s = 0; let i = 0; while (i < 10) { i++; if (i === 3) { continue; } if (i === 8) { break; } s += i; } s;",
            25.0,
        ),
        ("let n = 0; do { n++; } while (n < 5); n;", 5.0),
        ("let n = 10; do { n++; } while (n < 5); n;", 11.0),
        (
            "let n = 0; let s = 0; do { n++; if (n === 2) { continue; } if (n > 4) { break; } s += n; } while (n < 100); s;",
            8.0,
        ),
        (
            "let s = 0; for (let i = 0; i < 4; i++) { for (let j = 0; j < 4; j++) { if (j > i) { break; } s += 1; } } s;",
            10.0,
        ),
        (
            "let s = 0; while (true) { s++; if (s >= 7) { break; } } s;",
            7.0,
        ),
        (
            "function f(n) { let c = 0; while (true) { if (n <= 1) { break; } n = n / 2; c++; } return c; } f(64);",
            6.0,
        ),
        (
            "let o = {n: 0}; for (let i = 0; i < 50; i++) { if (i === 20) { break; } o.n = o.n + 1; } o.n;",
            20.0,
        ),
    ];
    for (source, want) in cases {
        assert_eq!(cpp(source), Some(want), "{source}");
    }
}

#[test]
fn loop_jumps_the_vm_cannot_place_are_refused() {
    if !is_cpp_jit_kernel_active() {
        return;
    }
    let error = vm_error("let x = 1; break;");
    assert_eq!(error.code, 32);
    assert!(
        error.message.contains("break outside a loop"),
        "{}",
        error.message
    );
    let error = vm_error("let x = 1; continue;");
    assert!(
        error.message.contains("continue outside a loop"),
        "{}",
        error.message
    );
    let error = vm_error("let i = 0; while (i < 3) { i++; break outer; }");
    assert_eq!(error.code, 33);
    assert!(error.message.contains("labels"), "{}", error.message);

    let mut nested = String::from("let n = 0; ");
    for _ in 0..17 {
        nested.push_str("while (n < 1) { ");
    }
    nested.push_str("n++; ");
    for _ in 0..17 {
        nested.push_str("} ");
    }
    let error = vm_error(&nested);
    assert_eq!(error.code, 49);
    assert_eq!(error.message, "loop nesting limit reached: 16 loops");
    assert!(error.is_limit());

    let mut jumps = String::from("let i = 0; while (i < 3) { i++; ");
    for _ in 0..33 {
        jumps.push_str("if (i > 5) { break; } ");
    }
    jumps.push('}');
    let error = vm_error(&jumps);
    assert_eq!(error.message, "break limit reached: 32 jumps per loop");
}

#[test]
fn loose_equality_postfix_and_push_values() {
    if !is_cpp_jit_kernel_active() {
        return;
    }
    let cases = [
        ("let a = 1; let r = 0; if (a == 1) { r = 1; } r;", 1.0),
        ("let a = 1; let r = 0; if (a != 2) { r = 1; } r;", 1.0),
        ("let u; let r = 0; if (u == null) { r = 1; } r;", 1.0),
        (
            "let n = null; let r = 0; if (n != undefined) { r = 1; } r;",
            0.0,
        ),
        ("let n = null; let r = 0; if (n == 0) { r = 1; } r;", 0.0),
        ("let s = 'a'; let r = 0; if (s == 'a') { r = 1; } r;", 1.0),
        ("let b = 0; let c = b++; c * 10 + b;", 1.0),
        ("let b = 5; let c = b--; c * 10 + b;", 54.0),
        (
            "function f(x) { let y = x++; return y * 10 + x; } f(3);",
            34.0,
        ),
        ("let a = []; let n = a.push(7); n = a.push(8); n;", 2.0),
        ("let b = 3; b++;", 3.0),
        ("let b = 3; b--; b;", 2.0),
        ("function f() { let k = 0; k++; k++; return k; } f();", 2.0),
        (
            "let s = 0; for (let i = 0; i < 1000; i++) { if (i % 3 == 0) { s += i; } } s;",
            166833.0,
        ),
    ];
    for (source, want) in cases {
        assert_eq!(cpp(source), Some(want), "{source}");
    }
    let u = cpp("let u; let v = u++; v;").expect("ran");
    assert!(u.is_nan());

    // Pairs that convert before comparing go to the interpreter.
    for source in [
        "let a = 1; let r = 0; if (a == '1') { r = 1; } r;",
        "let t = true; let r = 0; if (t == 1) { r = 1; } r;",
    ] {
        let error = vm_error(source);
        assert_eq!(error.code, 10, "{source}: {}", error.message);
        assert_eq!(error.message, "== between values of different types");
    }
    let error = vm_error("let o = {n: 1}; o.n++; o.n;");
    assert_eq!(error.code, 33);
    assert!(error.message.contains("++ or --"), "{}", error.message);
}
