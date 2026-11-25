//! Repository filesystem traversal, manifest discovery, and portable shell lookup helpers.

use std::fs;
use std::path::{Path, PathBuf};

/// Returns the repository root directory containing the workspace `Cargo.toml`.
pub fn repo_root() -> PathBuf {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest_dir
        .parent()
        .and_then(Path::parent)
        .expect("zerovdd-checks must reside at crates/zerovdd-checks")
        .to_path_buf()
}

/// Recursively collects files under `root` matching `predicate`, skipping build and VCS directories.
pub fn collect_files<F>(root: &Path, predicate: &F) -> Vec<PathBuf>
where
    F: Fn(&Path) -> bool,
{
    let mut results = Vec::new();
    if !root.exists() {
        return results;
    }
    collect_files_inner(root, predicate, &mut results);
    results.sort();
    results
}

fn collect_files_inner<F>(dir: &Path, predicate: &F, out: &mut Vec<PathBuf>)
where
    F: Fn(&Path) -> bool,
{
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => return,
    };

    for entry in entries.flatten() {
        let path = entry.path();
        let file_name = entry.file_name();
        let name = file_name.to_string_lossy();
        if path.is_dir() {
            if matches!(
                name.as_ref(),
                ".git" | "target" | "node_modules" | ".astro" | "dist"
            ) {
                continue;
            }
            collect_files_inner(&path, predicate, out);
        } else if predicate(&path) {
            out.push(path);
        }
    }
}

/// Collects all `.rs` source files across `crates/` and `driver-wdk/`.
pub fn collect_rust_files(repo: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for subdir in ["crates", "driver-wdk"] {
        let base = repo.join(subdir);
        files.extend(collect_files(&base, &|p| {
            p.extension().is_some_and(|ext| ext == "rs")
        }));
    }
    files.sort();
    files
}

/// Collects all crate `Cargo.toml` manifests directly under `crates/*/` and `driver-wdk/*/`.
pub fn collect_crate_manifests(repo: &Path) -> Vec<PathBuf> {
    let mut manifests = Vec::new();
    for subdir in ["crates", "driver-wdk"] {
        let base = repo.join(subdir);
        let Ok(entries) = fs::read_dir(&base) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                let manifest = path.join("Cargo.toml");
                if manifest.is_file() {
                    manifests.push(manifest);
                }
            }
        }
    }
    manifests.sort();
    manifests
}

/// Normalizes a path relative to `repo` using forward slashes for cross-platform diagnostics.
pub fn relative_display(repo: &Path, path: &Path) -> String {
    path.strip_prefix(repo)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

/// Locates a usable POSIX/Git Bash executable, preferring Git Bash on Windows over WSL bash.
pub fn find_bash_executable() -> PathBuf {
    #[cfg(windows)]
    {
        let mut candidates = vec![
            PathBuf::from(r"C:\Program Files\Git\bin\bash.exe"),
            PathBuf::from(r"C:\Program Files\Git\usr\bin\bash.exe"),
        ];

        for env_var in ["ProgramFiles", "ProgramFiles(x86)"] {
            if let Ok(base) = std::env::var(env_var) {
                let base_path = PathBuf::from(base);
                candidates.push(base_path.join(r"Git\bin\bash.exe"));
                candidates.push(base_path.join(r"Git\usr\bin\bash.exe"));
            }
        }

        if let Ok(output) = std::process::Command::new("git")
            .arg("--exec-path")
            .output()
        {
            if output.status.success() {
                let exec_path = PathBuf::from(String::from_utf8_lossy(&output.stdout).trim());
                for ancestor in exec_path.ancestors().take(4) {
                    candidates.push(ancestor.join("bin").join("bash.exe"));
                    candidates.push(ancestor.join("usr").join("bin").join("bash.exe"));
                }
            }
        }

        for candidate in candidates {
            if candidate.is_file() {
                return candidate;
            }
        }
    }
    PathBuf::from("bash")
}
