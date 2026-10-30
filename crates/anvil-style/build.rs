// SPDX-License-Identifier: LicenseRef-PolyForm-Noncommercial-1.0.0-OR-Commercial
// Copyright (c) 2026 Buntato
// Builds and links the native C++20 Prism engine static library (`prism_css_engine`)
// from `../../../anvil-browser/engines/prism/cpp/` directly into `anvil-style`.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const CSS_CPP_DIR: &str = "../../../anvil-browser/engines/prism/cpp";
const CSS_CPP_SRC: &str = "../../../anvil-browser/engines/prism/cpp/prism_css_engine.cpp";
const CSS_CPP_HDR: &str = "../../../anvil-browser/engines/prism/cpp/prism_css_engine.hpp";

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
    println!("cargo:rerun-if-changed={CSS_CPP_HDR}");
    println!("cargo:rerun-if-changed={CSS_CPP_SRC}");
    println!("cargo:rustc-check-cfg=cfg(prism_cpp_css)");

    if !Path::new(CSS_CPP_SRC).exists() {
        return;
    }

    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR"));

    if cfg!(target_os = "windows") {
        if let (Some(cl_exe), Some(lib_exe)) = (find_msvc_tool("cl.exe"), find_msvc_tool("lib.exe"))
        {
            let obj_path = out_dir.join("prism_css_engine.obj");
            let cl_ok = Command::new(&cl_exe)
                .args([
                    "/nologo",
                    "/c",
                    "/O2",
                    "/fp:precise",
                    "/std:c++20",
                    "/Gs1048576",
                    "/Zl",
                    &format!("/I{CSS_CPP_DIR}"),
                    CSS_CPP_SRC,
                    &format!("/Fo{}", obj_path.display()),
                ])
                .status()
                .is_ok_and(|s| s.success());
            if !cl_ok || !obj_path.exists() {
                return;
            }
            let lib_path = out_dir.join("prism_css_engine.lib");
            let lib_ok = Command::new(&lib_exe)
                .arg("/NOLOGO")
                .arg(format!("/OUT:{}", lib_path.display()))
                .arg(obj_path.to_str().unwrap())
                .status()
                .is_ok_and(|s| s.success());
            if lib_ok && lib_path.exists() {
                println!("cargo:rustc-link-search=native={}", out_dir.display());
                println!("cargo:rustc-link-lib=static=prism_css_engine");
                println!("cargo:rustc-cfg=prism_cpp_css");
            }
        }
    } else {
        let obj_path = out_dir.join("prism_css_engine.o");
        let gpp_ok = Command::new("g++")
            .args([
                "-c",
                "-O3",
                "-std=c++20",
                "-fno-exceptions",
                "-fno-rtti",
                &format!("-I{CSS_CPP_DIR}"),
                CSS_CPP_SRC,
                "-o",
                obj_path.to_str().unwrap(),
            ])
            .status()
            .is_ok_and(|s| s.success());
        if !gpp_ok || !obj_path.exists() {
            return;
        }
        let lib_path = out_dir.join("libprism_css_engine.a");
        let ar_ok = Command::new("ar")
            .args([
                "rcs",
                lib_path.to_str().unwrap(),
                obj_path.to_str().unwrap(),
            ])
            .status()
            .is_ok_and(|s| s.success());
        if ar_ok && lib_path.exists() {
            println!("cargo:rustc-link-search=native={}", out_dir.display());
            println!("cargo:rustc-link-lib=static=prism_css_engine");
            println!("cargo:rustc-cfg=prism_cpp_css");
        }
    }
}
