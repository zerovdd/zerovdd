use std::fs;

use zerovdd_checks::{collect_crate_manifests, collect_files, relative_display, repo_root};

fn check_manifest_workspace_lints(path_label: &str, toml_content: &str) -> Vec<String> {
    let mut violations = Vec::new();
    let parsed: toml::Value = match toml::from_str(toml_content) {
        Ok(v) => v,
        Err(err) => {
            violations.push(format!("{path_label}: failed to parse Cargo.toml: {err}"));
            return violations;
        }
    };

    let has_workspace_lints = parsed
        .get("lints")
        .and_then(|l| l.get("workspace"))
        .and_then(toml::Value::as_bool)
        .unwrap_or(false);

    if !has_workspace_lints {
        violations.push(format!(
            "{path_label}: crate must declare `[lints] workspace = true`"
        ));
    }

    violations
}

fn check_driver_workspace_lints_mirror(root_toml: &str, driver_toml: &str) -> Vec<String> {
    let mut violations = Vec::new();
    let root: toml::Value = toml::from_str(root_toml).expect("parse root Cargo.toml");
    let driver: toml::Value = match toml::from_str(driver_toml) {
        Ok(v) => v,
        Err(err) => {
            violations.push(format!(
                "driver-wdk/Cargo.toml: failed to parse TOML: {err}"
            ));
            return violations;
        }
    };

    let root_clippy = root
        .get("workspace")
        .and_then(|w| w.get("lints"))
        .and_then(|l| l.get("clippy"))
        .and_then(toml::Value::as_table)
        .expect("root Cargo.toml missing [workspace.lints.clippy]");

    let driver_clippy = driver
        .get("workspace")
        .and_then(|w| w.get("lints"))
        .and_then(|l| l.get("clippy"))
        .and_then(toml::Value::as_table);

    let Some(driver_clippy) = driver_clippy else {
        violations
            .push("driver-wdk/Cargo.toml must mirror root [workspace.lints.clippy]".to_string());
        return violations;
    };

    for (lint_name, expected_val) in root_clippy {
        if driver_clippy.get(lint_name) != Some(expected_val) {
            violations.push(format!(
                "driver-wdk/Cargo.toml [workspace.lints.clippy] drifted on `{lint_name}` (expected {expected_val})"
            ));
        }
    }

    violations
}

fn check_static_lint_registration(main_rs: &str, lint_module_names: &[String]) -> Vec<String> {
    let mut violations = Vec::new();
    for module in lint_module_names {
        let decl = format!("mod {module};");
        if !main_rs.lines().any(|line| line.trim() == decl) {
            violations.push(format!(
                "static_lints/main.rs is missing registration `{decl}`"
            ));
        }
    }
    violations
}

#[test]
fn repo_registers_all_lints_and_enforces_workspace_lints_and_single_clippy_toml() {
    let repo = repo_root();
    let mut violations = Vec::new();

    for manifest in collect_crate_manifests(&repo) {
        let rel = relative_display(&repo, &manifest);
        let content = fs::read_to_string(&manifest).expect("failed to read crate Cargo.toml");
        violations.extend(check_manifest_workspace_lints(&rel, &content));
    }

    let driver_ws_manifest = repo.join("driver-wdk/Cargo.toml");
    if driver_ws_manifest.is_file() {
        let root_toml = fs::read_to_string(repo.join("Cargo.toml")).expect("read root Cargo.toml");
        let driver_toml =
            fs::read_to_string(&driver_ws_manifest).expect("read driver-wdk/Cargo.toml");
        violations.extend(check_driver_workspace_lints_mirror(
            &root_toml,
            &driver_toml,
        ));
    }

    for subdir in ["crates", "driver-wdk"] {
        let shadowing = collect_files(&repo.join(subdir), &|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n == "clippy.toml" || n == ".clippy.toml")
        });
        for file in shadowing {
            violations.push(format!(
                "{}: per-crate clippy.toml shadowing is forbidden; use root clippy.toml",
                relative_display(&repo, &file)
            ));
        }
    }

    let lint_dir = repo.join("crates/zerovdd-checks/tests/static_lints");
    let mut lint_modules = Vec::new();
    for entry in fs::read_dir(&lint_dir)
        .expect("read static_lints dir")
        .flatten()
    {
        let path = entry.path();
        if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
            if stem.ends_with("_lint") {
                lint_modules.push(stem.to_string());
            }
        }
    }
    lint_modules.sort();
    assert!(
        lint_modules.len() >= 12,
        "expected all 12 static lint modules, found {}",
        lint_modules.len()
    );

    let main_rs = fs::read_to_string(lint_dir.join("main.rs")).expect("read static_lints/main.rs");
    violations.extend(check_static_lint_registration(&main_rs, &lint_modules));

    let mise_content = fs::read_to_string(repo.join("mise.toml")).expect("read mise.toml");
    let mise_parsed: toml::Value = toml::from_str(&mise_content).expect("parse mise.toml");
    let tasks = mise_parsed
        .get("tasks")
        .and_then(toml::Value::as_table)
        .expect("mise.toml missing [tasks]");

    for required_task in [
        "setup",
        "fmt",
        "fmt:check",
        "lint",
        "check:win",
        "check:nostd",
        "lints:static",
        "test:scripts",
        "test",
        "driver:build",
        "machete",
        "audit:rust",
        "deny:rust",
        "ci",
        "verify:agent",
        "release:preflight",
    ] {
        if !tasks.contains_key(required_task) {
            violations.push(format!(
                "mise.toml is missing required task `{required_task}`"
            ));
        }
    }

    if mise_content.contains("git diff --exit-code HEAD") {
        violations.push(
            "mise.toml must never run blanket `git diff --exit-code HEAD` inside verify:agent"
                .to_string(),
        );
    }

    let repo_lints_sh =
        fs::read_to_string(repo.join("scripts/repo-lints.sh")).expect("read scripts/repo-lints.sh");
    for required_ref in [
        "--test static_lints",
        "--test script_tests",
        "check-emdash-diff.sh",
    ] {
        if !repo_lints_sh.contains(required_ref) {
            violations.push(format!(
                "scripts/repo-lints.sh is missing registration for `{required_ref}`"
            ));
        }
    }

    assert!(
        violations.is_empty(),
        "repo_lints_registration_lint violations:\n{}",
        violations.join("\n")
    );
}

#[test]
fn rejects_crate_missing_workspace_lints_and_driver_workspace_lint_drift() {
    let bad_manifest = concat!(
        "[package]\n",
        "name = \"example\"\n",
        "version = \"0.1.0\"\n",
        "edition = \"2021\"\n",
    );
    assert!(!check_manifest_workspace_lints("crates/example/Cargo.toml", bad_manifest).is_empty());

    let root = concat!(
        "[workspace.lints.clippy]\n",
        "allow_attributes_without_reason = \"deny\"\n",
        "todo = \"deny\"\n",
    );
    let bad_driver = concat!(
        "[workspace.lints.clippy]\n",
        "allow_attributes_without_reason = \"warn\"\n",
    );
    assert!(
        !check_driver_workspace_lints_mirror(root, bad_driver).is_empty(),
        "expected drifted driver-wdk [workspace.lints.clippy] to be rejected"
    );
}

#[test]
fn rejects_unregistered_static_lint_module() {
    let main_rs = "mod alpha_lint;\n";
    let modules = vec!["alpha_lint".to_string(), "orphan_lint".to_string()];
    let violations = check_static_lint_registration(main_rs, &modules);
    assert!(
        !violations.is_empty(),
        "expected unregistered lint module to be rejected"
    );
}
