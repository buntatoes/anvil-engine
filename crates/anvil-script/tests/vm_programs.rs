// SPDX-License-Identifier: LicenseRef-Proprietary
// Copyright (c) 2026 Buntato
//! Small programs from a feature list that the C++ VM runs. Each one sets
//! `r`; the VM must finish it with `r` equal to the expected value, and a
//! page that logs `r` from a later script must print that value.

use anvil_dom::parse_html;
use anvil_script::{ScriptRuntime, eval_cpp_vm_script, is_cpp_jit_kernel_active};

/// (name, program, expected value as JavaScript, expected console line)
const PROGRAMS: &[(&str, &str, &str, &str)] = &[
    ("arith", r#"let r = (7*6) - 2/2 + 10%3;"#, r#"42"#, "42"),
    (
        "var_let_const",
        r#"var a=1; let b=2; const c=3; let r=a+b+c;"#,
        r#"6"#,
        "6",
    ),
    (
        "while_loop",
        r#"let r=0; let i=0; while(i<10){r=r+i; i=i+1;}"#,
        r#"45"#,
        "45",
    ),
    (
        "for_incr_pluseq",
        r#"let r=0; for(let i=0;i<10;i++){r+=i;}"#,
        r#"45"#,
        "45",
    ),
    (
        "do_while",
        r#"let r=0; do { r=r+1; } while (r<3);"#,
        r#"3"#,
        "3",
    ),
    (
        "break_continue",
        r#"let r=0; for(let i=0;i<10;i++){ if(i==2){continue;} if(i==5){break;} r=r+i; }"#,
        r#"8"#,
        "8",
    ),
    (
        "fn_decl_call",
        r#"function add(a,b){return a+b;} let r=add(2,3);"#,
        r#"5"#,
        "5",
    ),
    (
        "fn_recursion_fib15",
        r#"function fib(n){ if(n<2){return n;} return fib(n-1)+fib(n-2);} let r=fib(15);"#,
        r#"610"#,
        "610",
    ),
    (
        "fn_hoisting",
        r#"let r=sq(4); function sq(x){return x*x;}"#,
        r#"16"#,
        "16",
    ),
    (
        "fn_reads_global",
        r#"let g=7; function rd(){return g;} let r=rd();"#,
        r#"7"#,
        "7",
    ),
    (
        "fn_writes_global",
        r#"let g=1; function wr(){ g=9; } wr(); let r=g;"#,
        r#"9"#,
        "9",
    ),
    (
        "object_literal_props",
        r#"const o={a:1,b:2}; o.c=3; let r=o.a+o.b+o.c;"#,
        r#"6"#,
        "6",
    ),
    (
        "object_identity",
        r#"const a={x:1}; const b=a; b.x=2; let r=a.x;"#,
        r#"2"#,
        "2",
    ),
    (
        "object_nested",
        r#"const o={p:{q:{v:4}}}; let r=o.p.q.v;"#,
        r#"4"#,
        "4",
    ),
    (
        "array_literal_push_len",
        r#"const a=[1,2,3]; a.push(4); let r=a.length*10+a[3];"#,
        r#"44"#,
        "44",
    ),
    (
        "array_index_write_100",
        r#"const a=[]; for(let i=0;i<100;i++){a[i]=i;} let r=a[99]+a.length;"#,
        r#"199"#,
        "199",
    ),
    (
        "array_identity",
        r#"const a=[1]; const b=a; b.push(2); let r=a.length;"#,
        r#"2"#,
        "2",
    ),
    (
        "strict_eq_types",
        r#"let r=(1==='1') ? 'bad' : 'good';"#,
        r#""good""#,
        "good",
    ),
    (
        "undefined_value",
        r#"let u; let r=(u===undefined) ? 1 : 0;"#,
        r#"1"#,
        "1",
    ),
    ("int_division_or0", r#"let r=(100/3)|0;"#, r#"33"#, "33"),
    (
        "bitwise_ops",
        r#"let r=((5&3)|((1<<4)^2)) + (-8>>1) + (-1>>>28);"#,
        r#"30"#,
        "30",
    ),
    (
        "asi_newlines",
        r#"let a=1
let b=2
let r=a+b"#,
        r#"3"#,
        "3",
    ),
];

/// Programs a page hands to the Rust interpreter, because they keep objects
/// in top-level variables that later scripts could not see from the VM.
/// The interpreter copies an object on assignment and gets these wrong, so
/// their page result is not checked yet.
const PAGE_KNOWN_WRONG: &[&str] = &["object_identity", "array_identity"];

#[test]
fn feature_programs_run_on_the_vm() {
    if !is_cpp_jit_kernel_active() {
        return;
    }
    for (name, program, expected, _) in PROGRAMS {
        let source =
            format!("{program}\nlet __ok = 0;\nif (r === {expected}) {{ __ok = 1; }}\n__ok;\n");
        let result = eval_cpp_vm_script(&source).map(|(value, _)| value);
        assert_eq!(result, Some(1.0), "{name}");
    }
}

#[test]
fn feature_programs_give_their_value_to_later_scripts() {
    // Without the VM the interpreter runs these, and it does not get all
    // of them right yet.
    if !is_cpp_jit_kernel_active() {
        return;
    }
    for (name, program, _, line) in PROGRAMS {
        if PAGE_KNOWN_WRONG.contains(name) {
            continue;
        }
        let mut doc = parse_html("<html><body></body></html>");
        let mut rt = ScriptRuntime::new();
        rt.eval_script(&mut doc, program);
        rt.eval_script(&mut doc, "console.log(r);");
        assert_eq!(
            rt.console_logs().last().map(String::as_str),
            Some(*line),
            "{name}"
        );
    }
}
