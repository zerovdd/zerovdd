use std::fs;

use regex::Regex;
use zerovdd_checks::{collect_rust_files, relative_display, repo_root};

fn build_boundary_patterns() -> Vec<(&'static str, Regex)> {
    vec![
        (
            "include_str!/include_bytes! reading repository source/config",
            Regex::new(
                r#"(?s)include_(?:str|bytes)!\s*\([^;]{0,300}?(/src/|\.rs["']|Cargo\.toml|\.github/|mise\.toml)"#,
            )
            .expect("valid include macro regex"),
        ),
        (
            "read_to_string/fs::read inspecting repository source files",
            Regex::new(
                r#"(?s)(?:read_to_string|fs::read)\s*\([^;]{0,300}?(/src/|\.rs["']|Cargo\.toml|\.github/)"#,
            )
            .expect("valid fs read regex"),
        ),
    ]
}

fn line_number_at_offset(source: &str, offset: usize) -> usize {
    source[..offset].bytes().filter(|&b| b == b'\n').count() + 1
}

fn check_test_source(path_label: &str, source: &str, patterns: &[(&str, Regex)]) -> Vec<String> {
    let mut violations = Vec::new();
    for (desc, re) in patterns {
        for m in re.find_iter(source) {
            let line = line_number_at_offset(source, m.start());
            violations.push(format!(
                "{path_label}:{line}: {desc}; move source-text inspection into crates/zerovdd-checks"
            ));
        }
    }
    violations
}

#[test]
fn source_text_inspection_tests_live_only_in_zerovdd_checks() {
    let repo = repo_root();
    let patterns = build_boundary_patterns();
    let mut violations = Vec::new();

    for file in collect_rust_files(&repo) {
        let rel = relative_display(&repo, &file);
        if rel.starts_with("crates/zerovdd-checks/") {
            continue;
        }
        let is_test_file = rel.contains("/tests/");
        let source = fs::read_to_string(&file).expect("failed to read rust source file");
        let has_cfg_test = source.contains("#[cfg(test)]");
        if is_test_file || has_cfg_test {
            violations.extend(check_test_source(&rel, &source, &patterns));
        }
    }

    assert!(
        violations.is_empty(),
        "source_text_test_boundary_lint violations:\n{}",
        violations.join("\n")
    );
}

#[test]
fn rejects_single_and_multiline_source_text_inspection_outside_checks_crate() {
    let patterns = build_boundary_patterns();
    let bad_single = concat!(
        "#[test]\n",
        "fn inspects_own_src() {\n",
        "    let src = std::fs::read_to_string(concat!(env!(\"CARGO_MANIFEST_DIR\"), \"/src/lib.rs\")).unwrap();\n",
        "    assert!(src.contains(\"pub fn\"));\n",
        "}\n",
    );
    assert!(!check_test_source("crates/example/tests/it.rs", bad_single, &patterns).is_empty());

    let bad_multiline = concat!(
        "#[test]\n",
        "fn inspects_multiline() {\n",
        "    let src = std::fs::read_to_string(\n",
        "        concat!(env!(\"CARGO_MANIFEST_DIR\"), \"/src/lib.rs\"),\n",
        "    )\n",
        "    .unwrap();\n",
        "    assert!(!src.is_empty());\n",
        "}\n",
    );
    assert!(
        !check_test_source("crates/example/tests/it.rs", bad_multiline, &patterns).is_empty(),
        "expected rustfmt-wrapped multi-line read_to_string to be rejected"
    );

    let bad_fs_read = concat!(
        "#[test]\n",
        "fn reads_source_bytes() {\n",
        "    let bytes = std::fs::read(\"src/lib.rs\").unwrap();\n",
        "    assert!(!bytes.is_empty());\n",
        "}\n",
    );
    assert!(
        !check_test_source("crates/example/tests/it.rs", bad_fs_read, &patterns).is_empty(),
        "expected fs::read on .rs source to be rejected"
    );

    let bad_include_bytes = concat!(
        "#[test]\n",
        "fn includes_source_bytes() {\n",
        "    let bytes = include_bytes!(\"../src/lib.rs\");\n",
        "    assert!(!bytes.is_empty());\n",
        "}\n",
    );
    assert!(
        !check_test_source("crates/example/tests/it.rs", bad_include_bytes, &patterns).is_empty(),
        "expected include_bytes! on /src/ to be rejected"
    );
}

#[test]
fn allows_loading_non_source_test_data_fixtures() {
    let patterns = build_boundary_patterns();
    let good = concat!(
        "#[test]\n",
        "fn parses_fixture_json() {\n",
        "    let data = include_str!(\"fixtures/topology_snapshot.json\");\n",
        "    assert!(!data.is_empty());\n",
        "}\n",
    );
    let violations = check_test_source("crates/example/tests/it.rs", good, &patterns);
    assert!(
        violations.is_empty(),
        "expected data fixture test to pass, got: {violations:?}"
    );
}
