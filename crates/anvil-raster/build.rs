// Optional C++20 scanline and compositor library for anvil-raster.
// Author: Buntos
//
// Compiles and links cpp/anvil_raster_accel.cpp and cpp/anvil_raster_compositor.cpp
// when a C++20 compiler is available, emitting cfg(anvil_cpp_accel).

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const CPP_UNITS: &[(&str, &str)] = &[
    ("cpp/anvil_raster_accel.cpp", "anvil_raster_accel"),
    ("cpp/anvil_raster_compositor.cpp", "anvil_raster_compositor"),
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
    // Hostx64\x64 sits deeper than eight directories under the Visual Studio root.
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
    println!("cargo:rerun-if-changed=cpp/anvil_raster_accel.hpp");
    for (src, _) in CPP_UNITS {
        println!("cargo:rerun-if-changed={src}");
        if !Path::new(src).exists() {
            return;
        }
    }
    println!("cargo:rustc-check-cfg=cfg(anvil_cpp_accel)");

    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR"));

    if cfg!(target_os = "windows") {
        if let (Some(cl_exe), Some(lib_exe)) = (find_msvc_tool("cl.exe"), find_msvc_tool("lib.exe"))
        {
            let mut obj_paths = Vec::with_capacity(CPP_UNITS.len());
            for (src, stem) in CPP_UNITS {
                let obj_path = out_dir.join(format!("{stem}.obj"));
                let cl_status = Command::new(&cl_exe)
                    .args([
                        "/nologo",
                        "/c",
                        "/O2",
                        "/fp:fast",
                        "/std:c++20",
                        "/Gs1048576",
                        "/Zl",
                        "/Icpp",
                        src,
                        &format!("/Fo{}", obj_path.display()),
                    ])
                    .status();
                if !cl_status.is_ok_and(|s| s.success()) || !obj_path.exists() {
                    return;
                }
                obj_paths.push(obj_path);
            }

            let lib_path = out_dir.join("anvil_raster_accel.lib");
            let mut lib_cmd = Command::new(&lib_exe);
            lib_cmd
                .arg("/NOLOGO")
                .arg(format!("/OUT:{}", lib_path.display()));
            for obj in &obj_paths {
                lib_cmd.arg(obj.to_str().unwrap());
            }
            if lib_cmd.status().is_ok_and(|s| s.success()) && lib_path.exists() {
                println!("cargo:rustc-link-search=native={}", out_dir.display());
                println!("cargo:rustc-link-lib=static=anvil_raster_accel");
                println!("cargo:rustc-cfg=anvil_cpp_accel");
            }
        }
    } else {
        let mut obj_paths = Vec::with_capacity(CPP_UNITS.len());
        for (src, stem) in CPP_UNITS {
            let obj_path = out_dir.join(format!("{stem}.o"));
            let gpp_ok = Command::new("g++")
                .args([
                    "-c",
                    "-O3",
                    "-std=c++20",
                    "-fno-exceptions",
                    "-fno-rtti",
                    "-Icpp",
                    src,
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

        let lib_path = out_dir.join("libanvil_raster_accel.a");
        let mut ar_cmd = Command::new("ar");
        ar_cmd.arg("rcs").arg(lib_path.to_str().unwrap());
        for obj in &obj_paths {
            ar_cmd.arg(obj.to_str().unwrap());
        }
        if ar_cmd.status().is_ok_and(|s| s.success()) && lib_path.exists() {
            println!("cargo:rustc-link-search=native={}", out_dir.display());
            println!("cargo:rustc-link-lib=static=anvil_raster_accel");
            println!("cargo:rustc-cfg=anvil_cpp_accel");
        }
    }
}
