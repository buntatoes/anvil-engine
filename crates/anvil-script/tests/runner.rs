// SPDX-License-Identifier: LicenseRef-Proprietary
// Copyright (c) 2026 Buntato
//! Checks of the `anvil-js-run` file runner: its stdout, its stderr, and the
//! exit codes it returns. The success checks need the C++ VM, so they return
//! early when it is not linked.

use std::path::PathBuf;
use std::process::Command;

use anvil_script::is_cpp_jit_kernel_active;

/// Write `source` to a unique temp file and return its path.
fn temp_file(tag: &str, source: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "anvil-js-run-{tag}-{}-{nanos}.js",
        std::process::id()
    ));
    std::fs::write(&path, source).unwrap();
    path
}

/// Run the binary on `path` and return (stdout, stderr, exit code).
fn run(path: &PathBuf) -> (String, String, i32) {
    let output = Command::new(env!("CARGO_BIN_EXE_anvil-js-run"))
        .arg(path)
        .output()
        .expect("spawn anvil-js-run");
    (
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
        output.status.code().unwrap_or(-1),
    )
}

#[test]
fn prints_console_output_to_stdout_and_exits_zero() {
    if !is_cpp_jit_kernel_active() {
        return;
    }
    let path = temp_file(
        "ok",
        "console.log(1 + 1);\nfor (let i = 0; i < 3; i++) { console.log(i); }\nconsole.log(true, null);\n",
    );
    let (stdout, stderr, code) = run(&path);
    assert_eq!(stdout, "2\n0\n1\n2\ntrue null\n");
    assert_eq!(stderr, "");
    assert_eq!(code, 0);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn a_script_with_no_output_prints_nothing_and_exits_zero() {
    if !is_cpp_jit_kernel_active() {
        return;
    }
    let path = temp_file("silent", "let x = 1 + 2;\n");
    let (stdout, stderr, code) = run(&path);
    assert_eq!(stdout, "");
    assert_eq!(stderr, "");
    assert_eq!(code, 0);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn a_refused_script_is_reported_on_stderr_and_exits_nonzero() {
    if !is_cpp_jit_kernel_active() {
        return;
    }
    // A string argument to console.log is not supported yet, so the compiler
    // refuses the script rather than running it some other way.
    let path = temp_file("refused", "console.log(\"hi\");\n");
    let (stdout, stderr, code) = run(&path);
    assert_eq!(stdout, "");
    assert!(stderr.starts_with("anvil-js-run: "), "{stderr}");
    assert!(stderr.contains("string is not supported"), "{stderr}");
    assert!(stderr.contains("code "), "{stderr}");
    assert_ne!(code, 0);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn a_runtime_stop_is_reported_on_stderr_and_exits_nonzero() {
    if !is_cpp_jit_kernel_active() {
        return;
    }
    // Calling a non-function stops the run; the reason reaches stderr.
    let path = temp_file("stop", "let x = 5;\nx();\n");
    let (stdout, stderr, code) = run(&path);
    assert_eq!(stdout, "");
    assert!(stderr.starts_with("anvil-js-run: "), "{stderr}");
    assert!(stderr.contains("code "), "{stderr}");
    assert_ne!(code, 0);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn a_large_file_runs_and_prints_every_line() {
    if !is_cpp_jit_kernel_active() {
        return;
    }
    // Many statements, well past any small source cap: the runner caps the
    // source only by the heap budget, so it all runs.
    let mut source = String::new();
    for i in 0..5000 {
        source.push_str(&format!("console.log({i});\n"));
    }
    let path = temp_file("large", &source);
    let (stdout, stderr, code) = run(&path);
    assert_eq!(stderr, "");
    assert_eq!(code, 0);
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines.len(), 5000);
    assert_eq!(lines[0], "0");
    assert_eq!(lines[4999], "4999");
    let _ = std::fs::remove_file(&path);
}

#[test]
fn output_before_a_stop_still_reaches_stdout() {
    if !is_cpp_jit_kernel_active() {
        return;
    }
    // console.log runs, then calling a non-function stops the run: the line
    // printed first reaches stdout, the reason reaches stderr, exit nonzero.
    let path = temp_file(
        "partial",
        "console.log(1);
let x = 5;
x();
console.log(2);
",
    );
    let (stdout, stderr, code) = run(&path);
    assert_eq!(stdout, "1\n");
    assert!(stderr.starts_with("anvil-js-run: "), "{stderr}");
    assert_ne!(code, 0);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn no_argument_is_a_usage_error() {
    let output = Command::new(env!("CARGO_BIN_EXE_anvil-js-run"))
        .output()
        .expect("spawn anvil-js-run");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("usage"), "{stderr}");
}

#[test]
fn a_missing_file_is_reported_and_exits_nonzero() {
    let output = Command::new(env!("CARGO_BIN_EXE_anvil-js-run"))
        .arg("/no/such/anvil-js-run-file.js")
        .output()
        .expect("spawn anvil-js-run");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("cannot read"), "{stderr}");
}
