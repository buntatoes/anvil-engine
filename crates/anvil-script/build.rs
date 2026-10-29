// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0-OR-Commercial
// Copyright (c) 2026 Buntato
// Builds and links the native C++20 Anvil-JS engine (`AnvilVM`: NaN-boxed heap,
// hidden classes, Inline Caches, and register VM) from `../../../anvil-browser/engines/anvil-js/cpp/`
// directly into `anvil-script`.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const ENGINE_CPP_DIR: &str = "../../../anvil-browser/engines/anvil-js/cpp";
const LOCAL_CPP_DIR: &str = "cpp";

const CPP_STEMS: &[&str] = &[
    "anvil_vm_heap",
    "anvil_vm_ic",
    "anvil_vm_compiler",
    "anvil_jit_accel",
    "anvil_jit_native",
];

fn find_msvc_tool(tool_name: &str) -> Option<PathBuf> {
    let roots = [
        r"C:\Program Files\Microsoft Visual Studio",
        r"C:\Program Files (x86)\Microsoft Visual Studio",
    ];
    for root in roots {
        let root_path = Path::new(root);
        if !root_path.exists() {
            continue;
        }
        if let Some(found) = walk_for_host_x64_tool(root_path, tool_name, 0) {
            return Some(found);
        }
    }
    None
}

fn walk_for_host_x64_tool(dir: &Path, tool_name: &str, depth: usize) -> Option<PathBuf> {
    if depth > 12 {
        return None;
    }
    let entries = fs::read_dir(dir).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if let Some(found) = walk_for_host_x64_tool(&path, tool_name, depth + 1) {
                return Some(found);
            }
        } else if path
            .file_name()
            .is_some_and(|n| n.eq_ignore_ascii_case(tool_name))
        {
            let full = path.to_string_lossy().replace('/', "\\");
            if full.contains("Hostx64\\x64\\") {
                return Some(path);
            }
        }
    }
    None
}

fn main() {
    let cpp_dir = if Path::new(ENGINE_CPP_DIR).exists() {
        ENGINE_CPP_DIR
    } else {
        LOCAL_CPP_DIR
    };
    println!("cargo:rerun-if-changed={cpp_dir}/anvil_vm.hpp");
    let mut units = Vec::with_capacity(CPP_STEMS.len());
    for stem in CPP_STEMS {
        let src = format!("{cpp_dir}/{stem}.cpp");
        println!("cargo:rerun-if-changed={src}");
        if !Path::new(&src).exists() {
            return;
        }
        units.push((src, *stem));
    }
    println!("cargo:rustc-check-cfg=cfg(anvil_cpp_jit)");

    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR"));

    if cfg!(target_os = "windows") {
        if let (Some(cl_exe), Some(lib_exe)) = (find_msvc_tool("cl.exe"), find_msvc_tool("lib.exe"))
        {
            let mut obj_paths = Vec::with_capacity(units.len());
            for (src, stem) in &units {
                let obj_path = out_dir.join(format!("{stem}.obj"));
                let cl_status = Command::new(&cl_exe)
                    .args([
                        "/nologo",
                        "/c",
                        "/O2",
                        "/fp:precise",
                        "/std:c++20",
                        "/Gs1048576",
                        "/Zl",
                        &format!("/I{cpp_dir}"),
                        src.as_str(),
                        &format!("/Fo{}", obj_path.display()),
                    ])
                    .status();
                if !cl_status.is_ok_and(|s| s.success()) || !obj_path.exists() {
                    return;
                }
                obj_paths.push(obj_path);
            }

            let lib_path = out_dir.join("anvil_jit_accel.lib");
            let mut lib_cmd = Command::new(&lib_exe);
            lib_cmd
                .arg("/NOLOGO")
                .arg(format!("/OUT:{}", lib_path.display()));
            for obj in &obj_paths {
                lib_cmd.arg(obj.to_str().unwrap());
            }
            if lib_cmd.status().is_ok_and(|s| s.success()) && lib_path.exists() {
                println!("cargo:rustc-link-search=native={}", out_dir.display());
                println!("cargo:rustc-link-lib=static=anvil_jit_accel");
                println!("cargo:rustc-link-lib=dylib=kernel32");
                println!("cargo:rustc-cfg=anvil_cpp_jit");
            }
        }
    } else {
        let mut obj_paths = Vec::with_capacity(units.len());
        for (src, stem) in &units {
            let obj_path = out_dir.join(format!("{stem}.o"));
            let gpp_ok = Command::new("g++")
                .args([
                    "-c",
                    "-O3",
                    "-std=c++20",
                    "-fno-exceptions",
                    "-fno-rtti",
                    &format!("-I{cpp_dir}"),
                    src.as_str(),
                    "-o",
                    obj_path.to_str().unwrap(),
                ])
                .status()
                .is_ok_and(|s| s.success());
            if !gpp_ok || !obj_path.exists() {
                return;
            }
            obj_paths.push(obj_path);
        }

        let lib_path = out_dir.join("libanvil_jit_accel.a");
        let mut ar_cmd = Command::new("ar");
        ar_cmd.arg("rcs").arg(lib_path.to_str().unwrap());
        for obj in &obj_paths {
            ar_cmd.arg(obj.to_str().unwrap());
        }
        if ar_cmd.status().is_ok_and(|s| s.success()) && lib_path.exists() {
            println!("cargo:rustc-link-search=native={}", out_dir.display());
            println!("cargo:rustc-link-lib=static=anvil_jit_accel");
            println!("cargo:rustc-cfg=anvil_cpp_jit");
        }
    }
}
