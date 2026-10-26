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
        "let r = 0; do { r = r + 1; } while (r < 3); r;",
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
    let cases: [(&str, u32, &str); 9] = [
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
            "call depth limit reached: 15 nested calls",
        ),
        (
            "let head = null; let i = 0; while (i < 600) { head = {next: head}; i = i + 1; } 1;",
            3,
            "object limit reached: 512 live objects",
        ),
        (
            "let head = null; let i = 0; while (i < 300) { head = [head]; i = i + 1; } 1;",
            4,
            "array limit reached: 256 live arrays",
        ),
        (
            "let k = [[], [], [], [], [], [], [], [], [], [], []]; let i = 0; \
             while (i < 200) { let j = 0; while (j < 11) { k[j][i] = i; j = j + 1; } i = i + 1; } 1;",
            5,
            "array storage limit reached: 2048 elements",
        ),
        (
            "let a = []; let i = 0; while (i < 300) { a[i] = i; i = i + 1; } 1;",
            6,
            "array length limit reached: 256 elements per array",
        ),
        (
            "let s = 'x'; let i = 0; while (i < 20) { s = s + s; i = i + 1; } 1;",
            7,
            "string pool limit reached: 16384 bytes",
        ),
        (
            "let i = 0; while (i < 512) { let o = {}; \
             if (i & 1) { o.a = 1; } if (i & 2) { o.b = 1; } if (i & 4) { o.c = 1; } \
             if (i & 8) { o.d = 1; } if (i & 16) { o.e = 1; } if (i & 32) { o.f = 1; } \
             if (i & 64) { o.g = 1; } if (i & 128) { o.h = 1; } if (i & 256) { o.k = 1; } \
             i = i + 1; } 1;",
            9,
            "shape limit reached: 256 shapes",
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
    let mut props = String::from("let o = {};");
    for i in 0..33 {
        props.push_str(&format!(" o.p{i} = {i};"));
    }
    props.push_str(" 1;");
    let error = vm_error(&props);
    assert_eq!(error.code, 8);
    assert_eq!(
        error.message,
        "property limit reached: 32 properties per object"
    );
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
