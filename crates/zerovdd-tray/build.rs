//! Build script embedding `assets/zerovdd.ico` into `zerovdd-tray.exe` on Windows targets.

use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=assets/zerovdd.ico");
    println!("cargo:rerun-if-changed=assets/zerovdd.rc");

    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    if !cfg!(target_os = "windows") {
        return;
    }

    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap_or_default());
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap_or_default());
    let rc_input = manifest_dir.join("assets").join("zerovdd.rc");
    let res_output = out_dir.join("zerovdd.res");

    let Some((compiler, is_llvm)) = locate_rc_compiler() else {
        println!(
            "cargo:warning=Could not locate rc.exe or llvm-rc.exe; zerovdd-tray.exe will use default shell icon fallback."
        );
        return;
    };

    let mut cmd = Command::new(&compiler);
    if is_llvm {
        cmd.arg("/fo").arg(&res_output).arg(&rc_input);
    } else {
        cmd.arg("/nologo")
            .arg("/fo")
            .arg(&res_output)
            .arg(&rc_input);
    }
    let status = cmd.status();
    match status {
        Ok(st) if st.success() && res_output.is_file() => {
            println!("cargo:rustc-link-arg-bins={}", res_output.display());
        }
        Ok(st) => {
            panic!(
                "resource compiler `{}` failed with exit status {st}",
                compiler.display()
            );
        }
        Err(err) => {
            panic!("failed to spawn `{}`: {err}", compiler.display());
        }
    }
}

fn find_exe_recursive(root: &Path, filename: &str) -> Option<PathBuf> {
    let entries = std::fs::read_dir(root).ok()?;
    let mut dirs = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            dirs.push(path);
        } else if path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.eq_ignore_ascii_case(filename))
        {
            return Some(path);
        }
    }
    dirs.sort();
    dirs.reverse();
    for dir in dirs {
        if let Some(found) = find_exe_recursive(&dir, filename) {
            return Some(found);
        }
    }
    None
}

fn locate_rc_compiler() -> Option<(PathBuf, bool)> {
    let mut kit_roots = Vec::new();
    if let Ok(wdk) = env::var("WDKContentRoot") {
        if !wdk.trim().is_empty() {
            kit_roots.push(PathBuf::from(wdk));
        }
    }
    if let Ok(local) = env::var("LOCALAPPDATA") {
        kit_roots.push(PathBuf::from(&local).join("zerovdd-wdk").join("kit"));
    }
    kit_roots.push(PathBuf::from(r"C:\Program Files (x86)\Windows Kits\10"));

    for root in kit_roots {
        let bin_dir = root.join("bin");
        if bin_dir.is_dir() {
            if let Some(rc) = find_exe_recursive(&bin_dir, "rc.exe") {
                return Some((rc, false));
            }
        }
    }

    if let Ok(local) = env::var("LOCALAPPDATA") {
        let llvm_rc = PathBuf::from(&local)
            .join("zerovdd-wdk")
            .join(r"llvm\SourceDir\LLVM\bin\llvm-rc.exe");
        if llvm_rc.is_file() {
            return Some((llvm_rc, true));
        }
    }

    if let Ok(path_var) = env::var("PATH") {
        for dir in env::split_paths(&path_var) {
            let rc = dir.join("rc.exe");
            if rc.is_file() {
                return Some((rc, false));
            }
            let llvm_rc = dir.join("llvm-rc.exe");
            if llvm_rc.is_file() {
                return Some((llvm_rc, true));
            }
        }
    }

    None
}
