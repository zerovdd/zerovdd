use std::fs;
use std::path::Path;

use regex::Regex;
use zerovdd_checks::{collect_files, relative_display, repo_root};

fn build_forbidden_patterns() -> Vec<(&'static str, Regex)> {
    vec![
        (
            "task code",
            Regex::new(r"\bT[0-9]+\.[0-9]+\b").expect("valid task code regex"),
        ),
        (
            "batch code",
            Regex::new(r"(?:^|[^A-Za-z0-9])(B[0-9][A-Z])(?:_|[^A-Za-z0-9]|$)")
                .expect("valid batch code regex"),
        ),
        (
            "open decision code",
            Regex::new(r"(?i)(?:^|[^A-Za-z0-9])(OD[0-9]+)(?:_|[^A-Za-z0-9]|$)")
                .expect("valid open decision regex"),
        ),
        (
            "phase ticket code",
            Regex::new(r"\bP[0-9]+-[0-9]+\b").expect("valid phase ticket regex"),
        ),
        (
            "wave label",
            Regex::new(r"(?i)(?:^|[^A-Za-z0-9])(Wave[\s_]*[0-9]+)(?:_|[^A-Za-z0-9]|$)")
                .expect("valid wave label regex"),
        ),
        (
            "batch label",
            Regex::new(r"(?i)(?:^|[^A-Za-z0-9])(Batch[\s_]*[0-9]+[A-Z]?)(?:_|[^A-Za-z0-9]|$)")
                .expect("valid batch label regex"),
        ),
        (
            "phase label",
            Regex::new(r"(?i)(?:^|[^A-Za-z0-9])(Phase[\s_]*[0-9]+)(?:_|[^A-Za-z0-9]|$)")
                .expect("valid phase label regex"),
        ),
    ]
}

fn is_scanned_extension(path: &Path) -> bool {
    let Some(ext) = path.extension().and_then(|s| s.to_str()) else {
        return path
            .parent()
            .and_then(|p| p.file_name())
            .is_some_and(|n| n == ".githooks");
    };
    matches!(
        ext,
        "rs" | "c"
            | "h"
            | "md"
            | "mdx"
            | "mjs"
            | "ts"
            | "css"
            | "toml"
            | "yml"
            | "yaml"
            | "sh"
            | "ps1"
            | "inf"
            | "wxs"
    )
}

fn check_text(path_label: &str, content: &str, patterns: &[(&str, Regex)]) -> Vec<String> {
    let mut violations = Vec::new();
    for (idx, line) in content.lines().enumerate() {
        for (kind, re) in patterns {
            if let Some(m) = re.find(line) {
                violations.push(format!(
                    "{path_label}:{}: forbidden {kind} `{}`",
                    idx + 1,
                    m.as_str().trim()
                ));
            }
        }
    }
    violations
}

#[test]
fn repo_contains_zero_plan_reference_codes_outside_plans_dir() {
    let repo = repo_root();
    let patterns = build_forbidden_patterns();
    let mut violations = Vec::new();

    let files = collect_files(&repo, &|path| {
        let rel = relative_display(&repo, path);
        if rel.starts_with("plans/") || rel == "Cargo.lock" || rel.ends_with("/Cargo.lock") {
            return false;
        }
        is_scanned_extension(path)
    });

    for file in files {
        let rel = relative_display(&repo, &file);
        let Ok(content) = fs::read_to_string(&file) else {
            continue;
        };
        violations.extend(check_text(&rel, &content, &patterns));
    }

    assert!(
        violations.is_empty(),
        "no_plan_reference_codes_lint violations:\n{}",
        violations.join("\n")
    );
}

#[test]
fn rejects_plan_task_batch_decision_phase_wave_and_identifier_codes() {
    let patterns = build_forbidden_patterns();

    // WHY: Construct known-bad plan reference strings dynamically so this test file itself
    // contains zero literal plan/wave/batch codes and requires no file-level exemptions.
    let task_code = format!("// implements {}0.1 bootstrap", "T");
    let batch_code = format!("// from {}1A worker", "B");
    let batch_ident = format!("const {}1A_FLAG: u8 = 0;", "B");
    let decision_code = format!("// decided in {}1", "OD");
    let decision_ident = format!("fn {}1_spike() {{}}", "od");
    let phase_code = format!("// ticket {}0-1", "P");
    let phase_label = format!("// {} 1 milestone", "Phase");
    let wave_label = format!("// shipped in {} 1", "Wave");
    let wave_ident = format!("fn {}1_setup() {{}}", "wave");
    let batch_label = format!("// part of {} 2A", "Batch");

    for bad in [
        task_code,
        batch_code,
        batch_ident,
        decision_code,
        decision_ident,
        phase_code,
        phase_label,
        wave_label,
        wave_ident,
        batch_label,
    ] {
        let violations = check_text("fixture.rs", &bad, &patterns);
        assert!(
            !violations.is_empty(),
            "expected forbidden plan reference code to be rejected: {bad}"
        );
    }

    assert!(
        is_scanned_extension(Path::new(
            "driver-wdk/zerovdd-iddcx-sys/c/iddcx_trampoline.c"
        )),
        "expected .c files to be scanned"
    );
    assert!(
        is_scanned_extension(Path::new(
            "driver-wdk/zerovdd-iddcx-sys/c/iddcx_trampoline.h"
        )),
        "expected .h files to be scanned"
    );
    for doc_ext in [
        "docs/src/content/docs/index.mdx",
        "docs/astro.config.mjs",
        "docs/src/content.config.ts",
        "docs/src/styles/zerovdd-theme.css",
    ] {
        assert!(
            is_scanned_extension(Path::new(doc_ext)),
            "expected `{doc_ext}` to be scanned"
        );
    }
}

#[test]
fn allows_clean_live_code_versions_and_windows_guids() {
    let patterns = build_forbidden_patterns();
    let good = concat!(
        "/// EDID 1.4 and CTA-861-G synthesizer for 3840x2160@119.88Hz HDR10.\n",
        "pub const PROTOCOL_VERSION: (u16, u16, u16) = (1, 0, 0);\n",
        "pub const CONTROL_GUID: &str = \"{12345678-B12F-4000-B04A-1234567890AB}\";\n",
    );
    let violations = check_text("clean.rs", good, &patterns);
    assert!(
        violations.is_empty(),
        "expected clean source and GUIDs to pass, got: {violations:?}"
    );
}
