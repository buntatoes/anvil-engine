// SPDX-License-Identifier: LicenseRef-Proprietary
// Copyright (c) 2026 Buntato
//! Checks of the C++ VM and of how pages route scripts to it.
//!
//! The C++ part only builds where a C++20 compiler is installed. Without
//! it every script runs on the Rust interpreter, so the checks that need
//! the VM return early.

use anvil_script::{eval_cpp_vm_script, eval_cpp_vm_script_bindings, is_cpp_jit_kernel_active};

/// Halt value of `source` on the C++ VM, or `None` when it did not run there.
fn cpp(source: &str) -> Option<f64> {
    eval_cpp_vm_script(source).map(|(value, _)| value)
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
