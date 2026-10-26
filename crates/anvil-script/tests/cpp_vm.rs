// SPDX-License-Identifier: LicenseRef-Proprietary
// Copyright (c) 2026 Buntato
//! Checks of the C++ VM and of how pages route scripts to it.
//!
//! The C++ part only builds where a C++20 compiler is installed. Without
//! it every script runs on the Rust interpreter, so the checks that need
//! the VM return early.

use anvil_dom::parse_html;
use anvil_script::{
    ScriptRuntime, eval_cpp_vm_script, eval_cpp_vm_script_bindings, is_cpp_jit_kernel_active,
};

/// Halt value of `source` on the C++ VM, or `None` when it did not run there.
fn cpp(source: &str) -> Option<f64> {
    eval_cpp_vm_script(source).map(|(value, _)| value)
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
