// SPDX-License-Identifier: LicenseRef-Proprietary
// Copyright (c) 2026 Buntato
#![forbid(unsafe_code)]
//! A small, safe-Rust file runner for the C++ AnvilVM engine.
//!
//! Usage: `anvil-js-run <file.js>`
//!
//! It reads the file, runs it on the VM, prints each `console.log` line to
//! stdout and, if the run is refused or stops, prints the reason to stderr.
//! It exits 0 when the script ran to the end and nonzero otherwise. There is
//! no fallback to another engine: a script the VM compiler refuses is reported
//! on stderr with its reason, not run some other way.

use std::io::Write;
use std::process::ExitCode;

use anvil_script::eval_cpp_vm_with_console_configured;

const PROG: &str = "anvil-js-run";

fn main() -> ExitCode {
    let mut args = std::env::args_os();
    let _bin = args.next();
    let path = match (args.next(), args.next()) {
        (Some(path), None) => path,
        _ => {
            eprintln!("{PROG}: usage: {PROG} <file.js>");
            return ExitCode::from(2);
        }
    };

    let source = match std::fs::read_to_string(&path) {
        Ok(source) => source,
        Err(err) => {
            eprintln!("{PROG}: cannot read {}: {err}", path.to_string_lossy());
            return ExitCode::FAILURE;
        }
    };

    // A generous step budget lets a large file finish; the source size itself
    // is capped only by the heap byte budget. The heap and call-depth budgets
    // stay at the engine's built-in defaults.
    match eval_cpp_vm_with_console_configured(&source, 1_000_000_000, 0, 0) {
        Ok(run) => {
            let stdout = std::io::stdout();
            let mut out = stdout.lock();
            for line in &run.output {
                // Each console.log makes one output line, even an empty one.
                if writeln!(out, "{line}").is_err() {
                    return ExitCode::FAILURE;
                }
            }
            ExitCode::SUCCESS
        }
        Err(failure) => {
            // Print whatever the run managed before it stopped, then the
            // reason. A stable one-line error format: the message and the VM's
            // code. Codes from 32 up mean the compiler refused the script;
            // lower codes mean the run stopped (a non-function call, a limit,
            // and so on).
            let stdout = std::io::stdout();
            let mut out = stdout.lock();
            for line in &failure.output {
                if writeln!(out, "{line}").is_err() {
                    return ExitCode::FAILURE;
                }
            }
            drop(out);
            eprintln!(
                "{PROG}: {} (code {})",
                failure.error.message, failure.error.code
            );
            ExitCode::FAILURE
        }
    }
}
