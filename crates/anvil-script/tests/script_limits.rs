// SPDX-License-Identifier: LicenseRef-Proprietary
// Copyright (c) 2026 Buntato
//! Interpreter limits stop a page script with a reason on the console, and
//! the scripts after it still run.

use anvil_dom::{Document, parse_html};
use anvil_script::{MAX_SCRIPT_SOURCE, ScriptRuntime};

/// Run each source as its own `<script>` on one page. Deep recursion in
/// an unoptimized build needs more than the default test-thread stack, so
/// the page runs on a thread with a 64 MiB stack.
fn run_page(scripts: &[&str]) -> (Document, ScriptRuntime) {
    let mut html = String::from("<html><body><p id=\"out\"></p>");
    for script in scripts {
        html.push_str("<script>");
        html.push_str(script);
        html.push_str("</script>");
    }
    html.push_str("</body></html>");
    std::thread::Builder::new()
        .stack_size(64 << 20)
        .spawn(move || {
            let mut doc = parse_html(&html);
            let mut rt = ScriptRuntime::new();
            rt.run_document_scripts(&mut doc);
            (doc, rt)
        })
        .expect("spawn page thread")
        .join()
        .expect("page thread")
}

fn out_text(doc: &Document) -> String {
    let out = doc.get_element_by_id("out").expect("out");
    doc.text_content(out)
}

const WRITE_DONE: &str = "document.getElementById('out').textContent = 'done';";

#[test]
fn a_stopped_script_does_not_stop_the_next_one() {
    let (doc, rt) = run_page(&["function r() { return r(); } r();", WRITE_DONE]);
    assert_eq!(out_text(&doc), "done");
    assert_eq!(
        rt.console_logs(),
        ["[error] Script stopped: call depth limit reached: 64 nested calls"]
    );
    assert!(rt.was_aborted());
    assert_eq!(
        rt.abort_reason(),
        Some("call depth limit reached: 64 nested calls")
    );
}

#[test]
fn an_endless_loop_stops_at_the_step_budget() {
    let (doc, rt) = run_page(&["let n = 0; while (true) { n = n + 1; }", WRITE_DONE]);
    assert_eq!(out_text(&doc), "done");
    assert_eq!(
        rt.console_logs(),
        ["[error] Script stopped: step budget exhausted"]
    );
}

#[test]
fn loops_run_past_ten_thousand_passes() {
    for source in [
        "let n = 0; let s = ''; while (n < 12000) { n = n + 1; s = 'x'; } \
         document.getElementById('out').textContent = n;",
        "let n = 0; let s = ''; for (let i = 0; i < 12000; i++) { n = n + 1; s = 'y'; } \
         document.getElementById('out').textContent = n;",
    ] {
        let (doc, rt) = run_page(&[source]);
        assert_eq!(out_text(&doc), "12000", "{source}");
        assert!(rt.console_logs().is_empty(), "{source}");
        assert!(!rt.was_aborted(), "{source}");
    }
}

#[test]
fn arrays_past_the_limit_stop_with_a_reason() {
    for source in [
        "let a = []; for (let i = 0; i < 10001; i++) { a.push(i); }",
        "let a = []; a[10000] = 1;",
        "let a = new Array(20000);",
        "let a = []; for (let i = 0; i < 6000; i++) { a.push(i); } let b = a.concat(a);",
    ] {
        let (doc, rt) = run_page(&[source, WRITE_DONE]);
        assert_eq!(out_text(&doc), "done", "{source}");
        assert_eq!(
            rt.console_logs(),
            ["[error] Script stopped: array length limit reached: 10000 elements"],
            "{source}"
        );
    }
    let (doc, rt) = run_page(&[
        "let a = []; for (let i = 0; i < 10000; i++) { a.push(i); } \
         document.getElementById('out').textContent = a.length;",
    ]);
    assert_eq!(out_text(&doc), "10000");
    assert!(rt.console_logs().is_empty());
}

#[test]
fn writing_past_the_end_of_an_array_grows_it() {
    let (doc, _) = run_page(&[
        "let a = [1]; a[3] = 4; document.getElementById('out').textContent = a.length + ' ' + a[3];",
    ]);
    assert_eq!(out_text(&doc), "4 4");
}

#[test]
fn long_strings_stop_with_a_reason() {
    let (doc, rt) = run_page(&[
        "let s = 'x'.repeat(1024); while (true) { s = s + s; }",
        WRITE_DONE,
    ]);
    assert_eq!(out_text(&doc), "done");
    assert_eq!(
        rt.console_logs(),
        ["[error] Script stopped: string length limit reached: 1048576 bytes"]
    );
    let (doc, _) = run_page(&[
        "let s = 'ab'.repeat(2000); document.getElementById('out').textContent = s.length;",
    ]);
    assert_eq!(out_text(&doc), "4000");
}

#[test]
fn uncaught_throws_are_logged() {
    let (doc, rt) = run_page(&["throw new Error('boom');", WRITE_DONE]);
    assert_eq!(out_text(&doc), "done");
    assert_eq!(rt.console_logs(), ["[error] Uncaught Error: boom"]);
    let (_, rt) = run_page(&["throw 'plain';"]);
    assert_eq!(rt.console_logs(), ["[error] Uncaught plain"]);
}

#[test]
fn a_limit_caught_by_try_leaves_no_stale_reason() {
    let (doc, rt) = run_page(&[
        "let caught = 0; try { function r() { return r(); } r(); } catch (e) { caught = 1; } \
         document.getElementById('out').textContent = caught;",
        "let a = []; a[10000] = 1;",
    ]);
    assert_eq!(out_text(&doc), "1");
    assert_eq!(
        rt.console_logs(),
        ["[error] Script stopped: array length limit reached: 10000 elements"]
    );
}

#[test]
fn oversized_scripts_say_they_did_not_run() {
    let big = "var x = 1;".repeat(MAX_SCRIPT_SOURCE / 8);
    let (doc, rt) = run_page(&[&big, WRITE_DONE]);
    assert_eq!(out_text(&doc), "done");
    assert_eq!(rt.console_logs().len(), 1);
    assert!(
        rt.console_logs()[0].starts_with("[warn] Script not run: "),
        "{:?}",
        rt.console_logs()
    );
    assert!(rt.console_logs()[0].ends_with("over the 262144 byte limit"));
}

#[test]
fn console_output_past_the_limit_is_noted() {
    let (_, rt) = run_page(&["for (let i = 0; i < 300; i++) { console.log(i); }"]);
    let logs = rt.console_logs();
    assert_eq!(logs.len(), 257);
    assert_eq!(logs[255], "255");
    assert_eq!(
        logs[256],
        "[warn] console limit reached: 256 lines, later lines are dropped"
    );
}
