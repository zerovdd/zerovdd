use std::fs;
use std::path::Path;

use zerovdd_checks::{
    collect_crate_manifests, collect_files, collect_rust_files, relative_display, repo_root,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExpectedEol {
    Lf,
    Crlf,
}

fn expected_eol_for_path(path: &Path) -> Option<ExpectedEol> {
    if path
        .parent()
        .and_then(|p| p.file_name())
        .is_some_and(|n| n == ".githooks")
    {
        return Some(ExpectedEol::Lf);
    }
    let ext = path.extension().and_then(|e| e.to_str())?;
    match ext {
        "rs" | "toml" | "yml" | "yaml" | "md" | "mdx" | "mjs" | "ts" | "css" | "svg" | "json"
        | "sh" => Some(ExpectedEol::Lf),
        "inf" | "wxs" | "ps1" | "cmd" | "bat" => Some(ExpectedEol::Crlf),
        _ => None,
    }
}

fn check_eol_bytes(path_label: &str, bytes: &[u8], expected: ExpectedEol) -> Option<String> {
    match expected {
        ExpectedEol::Lf => {
            if bytes.contains(&b'\r') {
                return Some(format!(
                    "{path_label}: expected LF line endings, found CR (`\\r`)"
                ));
            }
        }
        ExpectedEol::Crlf => {
            for (idx, &b) in bytes.iter().enumerate() {
                if b == b'\n' && (idx == 0 || bytes[idx - 1] != b'\r') {
                    return Some(format!(
                        "{path_label}: expected CRLF line endings, found bare LF at byte {idx}"
                    ));
                }
            }
        }
    }
    None
}

fn check_lib_doctest_flag(path_label: &str, manifest_toml: &str) -> Option<String> {
    let parsed: toml::Value = match toml::from_str(manifest_toml) {
        Ok(v) => v,
        Err(err) => return Some(format!("{path_label}: failed to parse Cargo.toml: {err}")),
    };
    let has_doctest_setting = parsed
        .get("lib")
        .and_then(|lib| lib.get("doctest"))
        .and_then(toml::Value::as_bool)
        .is_some();
    if !has_doctest_setting {
        return Some(format!(
            "{path_label}: library crate must explicitly declare `[lib] doctest = false` (or `true` when maintaining doctests)"
        ));
    }
    None
}

#[test]
fn repo_enforces_2018_modules_lib_doctest_and_line_endings() {
    let repo = repo_root();
    let mut violations = Vec::new();

    for rs_file in collect_rust_files(&repo) {
        if rs_file.file_name().and_then(|n| n.to_str()) == Some("mod.rs") {
            violations.push(format!(
                "{}: legacy `mod.rs` layout is forbidden; use Rust 2018+ `foo.rs` + `foo/`",
                relative_display(&repo, &rs_file)
            ));
        }
    }

    for manifest in collect_crate_manifests(&repo) {
        let Some(crate_dir) = manifest.parent() else {
            continue;
        };
        if crate_dir.join("src/lib.rs").is_file() {
            let rel = relative_display(&repo, &manifest);
            let content = fs::read_to_string(&manifest).expect("read crate Cargo.toml");
            if let Some(err) = check_lib_doctest_flag(&rel, &content) {
                violations.push(err);
            }
        }
    }

    let files = collect_files(&repo, &|path| {
        let rel = relative_display(&repo, path);
        if rel.starts_with("plans/") {
            return false;
        }
        expected_eol_for_path(path).is_some()
    });

    for file in files {
        let Some(expected) = expected_eol_for_path(&file) else {
            continue;
        };
        let rel = relative_display(&repo, &file);
        let bytes = fs::read(&file).expect("read file bytes for EOL check");
        if let Some(err) = check_eol_bytes(&rel, &bytes, expected) {
            violations.push(err);
        }
    }

    assert!(
        violations.is_empty(),
        "layout_and_eol_lint violations:\n{}",
        violations.join("\n")
    );
}

#[test]
fn rejects_crlf_in_lf_file_bare_lf_in_crlf_file_and_missing_lib_doctest() {
    assert!(check_eol_bytes("src/lib.rs", b"fn main() {}\r\n", ExpectedEol::Lf).is_some());
    assert!(check_eol_bytes("src/lib.rs", b"fn main() {}\n", ExpectedEol::Lf).is_none());

    assert!(check_eol_bytes("driver.inf", b"[Version]\n", ExpectedEol::Crlf).is_some());
    assert!(check_eol_bytes("driver.inf", b"[Version]\r\n", ExpectedEol::Crlf).is_none());

    let missing_lib = "[package]\nname = \"demo\"\nversion = \"0.1.0\"\n";
    assert!(check_lib_doctest_flag("crates/demo/Cargo.toml", missing_lib).is_some());

    let with_lib = "[package]\nname = \"demo\"\nversion = \"0.1.0\"\n\n[lib]\ndoctest = false\n";
    assert!(check_lib_doctest_flag("crates/demo/Cargo.toml", with_lib).is_none());
}
